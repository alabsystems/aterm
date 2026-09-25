// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Host adapter for first-party native tab applications.
//!
//! Reducers emit typed effects; this module is the only layer allowed to turn
//! them into config persistence, updater work, clipboard access, external opens,
//! tab presentation invalidation, and redraws.

use crate::native_app::{
    AppEffect, AppEvent, ClipboardOutcome, ClipboardRequest, ConfigEditorOutcome,
    ConfigPatchOutcome, DamageRegion, EventResult, ExpectedConfigValue, ExternalOpenOutcome,
    PackagesOutcome, PackagesRequest, TextInputEvent, UpdateOutcome, UpdateRequest, ViewCx,
};
use crate::native_config_service::{
    ConfigKeyEdit, ConfigPatchRequest, ConfigPatchResult, ExpectedValue,
};
use crate::native_updater_service::CheckCompletion;
use crate::native_updater_service::{
    ApplyDecision, ApplyMode, ApplyPreflightStart, CheckBlock, CheckStart, ClosePreflight,
    DurableStageDisposition, DurableUpdateStatus, InstalledUpdate, NativeUpdaterService,
    ReturnedApplyDisposition, ReturnedApplyFacts, UpdaterPhase, UpdaterWorkTicket,
};
use crate::packages_screen::{PackagesBusy, PackagesCommandOutcome, PackagesWorkerCompletion};
use crate::{App, Wake, WindowId};

/// Settings ▸ Messages is republished at most this often while the center
/// moves (design §4.2): a downloading update restates at the tailer's 10 Hz,
/// and the page need not repaint at that rate — 2 Hz is the eye's.
pub(crate) const MESSAGES_PUBLISH_MIN_GAP: std::time::Duration =
    std::time::Duration::from_millis(500);
/// …and at least this often while a view is on the route with nothing
/// moving, so the relative times tick (`3 min ago` becomes `4 min ago`).
pub(crate) const MESSAGES_PUBLISH_TICK: std::time::Duration = std::time::Duration::from_secs(60);

static NEXT_CONTROL_SETTINGS_REQUEST: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

fn next_control_settings_request() -> u64 {
    NEXT_CONTROL_SETTINGS_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn control_serious_mode_intent(key: &str, value: Option<&str>) -> Option<bool> {
    if key != crate::prefs::EDIT_SERIOUS_MODE {
        return None;
    }
    match value {
        // Removing the authored field restores Config's documented default.
        None => Some(false),
        Some(value) => value.trim().parse::<bool>().ok(),
    }
}

fn control_settings_completion_reply(
    key: &str,
    value: Option<&str>,
    outcome: &ConfigPatchOutcome,
    synchronization_error: Option<&str>,
) -> Result<String, String> {
    match outcome {
        ConfigPatchOutcome::Applied { undo, .. } => {
            let mut status = if undo.is_some() {
                format!("saved: {key} = {}", value.unwrap_or(""))
            } else {
                format!("{key}: unchanged")
            };
            if let Some(error) = synchronization_error {
                // The worker produced a durable proof, so this remains a successful
                // save; the service gate is nevertheless closed until a later stable
                // observation. State that explicitly instead of hiding reconciliation.
                status.push_str("; reconciliation required: ");
                status.push_str(error);
            }
            Ok(status)
        }
        ConfigPatchOutcome::Conflict { revision } => {
            let mut error = format!(
                "save conflict for {key} at config revision {revision}; current aterm.toml was kept"
            );
            if let Some(reconcile) = synchronization_error {
                error.push_str("; reconciliation required: ");
                error.push_str(reconcile);
            }
            Err(error)
        }
        ConfigPatchOutcome::Indeterminate { message } => {
            let mut error = format!(
                "publication unverified for {key}: {message}; reload aterm.toml before retrying"
            );
            if let Some(reconcile) = synchronization_error {
                error.push_str("; reconciliation required: ");
                error.push_str(reconcile);
            }
            Err(error)
        }
        ConfigPatchOutcome::Rejected { message } => {
            let mut error = format!("save failed: {message}");
            if let Some(reconcile) = synchronization_error {
                error.push_str("; reconciliation required: ");
                error.push_str(reconcile);
            }
            Err(error)
        }
    }
}

#[cfg(test)]
thread_local! {
    static UPDATE_FACT_PROBES_ON_THREAD: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
}

/// Opaque evidence that the main-thread native document/restore preflight was Ready.
/// The constructor is private to this module; process replacement must carry the token
/// rather than recreating the safety boolean at its final admission gate.
pub(crate) struct NativeUpdateSafetyToken {
    _private: (),
}

impl NativeUpdateSafetyToken {
    #[must_use]
    pub(crate) fn is_certified(&self) -> bool {
        true
    }
}

/// Disk facts collected off the event loop after an asynchronous handoff stops.
/// Reduction remains main-thread owned, but neither ledger I/O nor the installed
/// bundle probe belongs on the latency-sensitive completion callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct NativeUpdateReconcileTicket {
    request_sequence: u64,
}

impl NativeUpdateReconcileTicket {
    #[cfg(test)]
    #[must_use]
    pub(crate) fn request_sequence(self) -> u64 {
        self.request_sequence
    }

    /// Test-only: a ticket for hand-built worker facts, so a conformance bind
    /// outside this module can feed the real reducer the disk a failed
    /// candidate leaves behind (plan P0-6's `BundleSwap`).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test(request_sequence: u64) -> Self {
        Self { request_sequence }
    }
}

/// What the event loop should do only after worker-collected facts have been accepted.
/// Trigger payloads never carry update authority; the effective reducer stage does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateReconcilePurpose {
    Startup,
    StageAvailable,
    ApplyControl,
    /// Re-read the ledger with no announcement semantics: the background check's
    /// health hook fired (a persistent failure streak), or the user opened
    /// Settings ▸ Software Update. Either way the screen must show the CURRENT
    /// verdict, not the last thing a stage or startup happened to import. Before
    /// this existed a failing check staged nothing and requested nothing, so the
    /// panel kept saying "You're up to date" while `health.toml` counted up.
    Refresh,
}

#[must_use]
fn merge_reconcile_purpose(
    left: NativeUpdateReconcilePurpose,
    right: NativeUpdateReconcilePurpose,
) -> NativeUpdateReconcilePurpose {
    use NativeUpdateReconcilePurpose::{ApplyControl, Refresh, StageAvailable, Startup};
    match (left, right) {
        (ApplyControl, _) | (_, ApplyControl) => ApplyControl,
        (StageAvailable, _) | (_, StageAvailable) => StageAvailable,
        (Startup, _) | (_, Startup) => Startup,
        (Refresh, Refresh) => Refresh,
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativeUpdateReconcileFacts {
    pub(crate) _ticket: NativeUpdateReconcileTicket,
    /// Assigned by the sole facts worker immediately before the read. This, not
    /// request dispatch order, is the freshness authority.
    pub(crate) observation_sequence: u64,
    /// When the worker BEGAN reading the disk. The sequence orders observations
    /// against each other; this orders them against the reducer's own stage
    /// IMPORTS: a read that began before a check staged its build describes a disk
    /// without that stage, however late its wake lands (the read spans a
    /// codesign), and must not retire what the check imported.
    pub(crate) observed_at: std::time::Instant,
    pub(crate) durable: Option<DurableUpdateStatus>,
    pub(crate) installed: Option<InstalledUpdate>,
}

enum NativeUpdateFactDestination {
    Wake {
        purpose: NativeUpdateReconcilePurpose,
        proxy: winit::event_loop::EventLoopProxy<Wake>,
    },
    Reply(std::sync::mpsc::SyncSender<NativeUpdateReconcileFacts>),
}

pub(crate) struct NativeUpdateReconcileRequest {
    ticket: NativeUpdateReconcileTicket,
    current_build: u64,
    destination: NativeUpdateFactDestination,
}

pub(crate) enum NativeUpdateWorkerRequest {
    Reconcile(NativeUpdateReconcileRequest),
    ConfirmBootHealth { current_build: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateDispatch {
    Queued,
    Saturated,
    Unavailable,
}

pub(crate) type NativeUpdateReconcileSender =
    std::sync::mpsc::SyncSender<NativeUpdateWorkerRequest>;

/// One buffered item plus the worker's active item is sufficient; further UI
/// intents are retained by the purpose coalescer. Keeping this capacity named
/// binds the shipping constructor to the Tier-1 Full/drain/retry boundary and
/// prevents stale disk work from accumulating ahead of user-visible updates.
pub(crate) const NATIVE_UPDATE_WORKER_CAPACITY: usize = 1;

/// Result of reducing worker-collected disk facts on the event loop. Active updater
/// work defers the owned facts without rereading disk; otherwise presentation must use
/// only the returned effective reducer stage, never the stale trigger payload.
///
/// `Deferred` carries its facts BOXED. The parked observation is the rare arm — it
/// happens only while a check or an apply is already in flight — and the whole
/// [`NativeUpdateReconcileFacts`] payload is several hundred bytes of durable-status
/// and installed-bundle strings, which every `IgnoredStale`/`Reduced` return would
/// otherwise have to move too. One allocation on the rare path in exchange for a
/// small return value on the common ones.
pub(crate) enum NativeUpdateFactsResult {
    IgnoredStale,
    Deferred(Box<NativeUpdateReconcileFacts>),
    Reduced {
        effective_stage: Option<crate::native_updater_service::StagedUpdate>,
    },
}

#[must_use]
fn read_native_update_reconcile_facts(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    current_build: u64,
) -> NativeUpdateReconcileFacts {
    #[cfg(test)]
    UPDATE_FACT_PROBES_ON_THREAD.with(|count| count.set(count.get().saturating_add(1)));
    read_native_update_reconcile_facts_with(
        ticket,
        observation_sequence,
        || aterm_update::status(current_build).map(durable_update_status),
        || {
            aterm_update::installed_update_facts()
                // A YANKED bundle newer than this process is not an activation
                // candidate: reporting it as installed would retire a good download
                // for an activation every handoff then refuses at the floor
                // (2026-08-19 round-3 audit). A yanked bundle at or below the running
                // build is still the bundle we run from and stays reported.
                .filter(|installed| !(installed.yanked && installed.build_number > current_build))
                .map(|installed| InstalledUpdate {
                    build: installed.build_number,
                    commit: installed.git_commit,
                    version: installed.version,
                    receipt_build: installed.receipt_build_number,
                    receipt_dmg_sha256: installed.receipt_dmg_sha256,
                })
        },
    )
}

fn read_native_update_reconcile_facts_with(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    read_durable: impl FnOnce() -> Option<DurableUpdateStatus>,
    read_installed: impl FnOnce() -> Option<InstalledUpdate>,
) -> NativeUpdateReconcileFacts {
    // Load the ready marker before canonical bundle identity. Shipping order swaps
    // the bundle first and removes ready afterward, so this observes ready+old,
    // ready+new, or missing+new; exact installed proof safely dominates either
    // surviving-marker case.
    let observed_at = std::time::Instant::now();
    let durable = read_durable();
    let installed = read_installed();
    NativeUpdateReconcileFacts {
        _ticket: ticket,
        observation_sequence,
        observed_at,
        durable,
        installed,
    }
}

/// Start the one process-wide, bounded FIFO facts worker. Every ledger/provenance
/// observation is serialized here, and freshness is stamped at read time.
pub(crate) fn spawn_native_update_reconcile_worker(
    drain_proxy: winit::event_loop::EventLoopProxy<Wake>,
) -> Result<NativeUpdateReconcileSender, String> {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel::<NativeUpdateWorkerRequest>(NATIVE_UPDATE_WORKER_CAPACITY);
    std::thread::Builder::new()
        .name("aterm-update-facts".to_string())
        .spawn(move || {
            // QoS (port of 61a6c8b62): reconcile facts feed a settings screen,
            // not a frame; results return through the proxy, no lock is held.
            crate::qos::set_self(crate::qos::Role::Background);
            let health_proxy = drain_proxy.clone();
            run_native_update_worker(
                receiver,
                read_native_update_reconcile_facts,
                move |confirmed| {
                    let _ = health_proxy
                        .send_event(Wake::NativeBootHealthConfirmationFinished { confirmed });
                },
                || {
                    let _ = drain_proxy.send_event(Wake::NativeUpdateWorkerDrained);
                },
            );
        })
        .map_err(|error| format!("could not start updater facts worker: {error}"))?;
    Ok(sender)
}

fn run_native_update_worker(
    receiver: std::sync::mpsc::Receiver<NativeUpdateWorkerRequest>,
    mut read: impl FnMut(NativeUpdateReconcileTicket, u64, u64) -> NativeUpdateReconcileFacts,
    mut health_finished: impl FnMut(bool),
    mut drained: impl FnMut(),
) {
    let mut next_observation_sequence = 1_u64;
    while let Ok(work) = receiver.recv() {
        let request = match work {
            NativeUpdateWorkerRequest::Reconcile(request) => request,
            NativeUpdateWorkerRequest::ConfirmBootHealth { current_build } => {
                let confirmed = aterm_update::confirm_boot_health_exact(
                    current_build,
                    crate::build_info::GIT_COMMIT,
                );
                health_finished(confirmed);
                drained();
                continue;
            }
        };
        let observation_sequence = next_observation_sequence;
        let Some(next) = observation_sequence.checked_add(1) else {
            break;
        };
        next_observation_sequence = next;
        let facts = read(request.ticket, observation_sequence, request.current_build);
        match request.destination {
            NativeUpdateFactDestination::Wake { purpose, proxy } => {
                let _ = proxy.send_event(Wake::NativeUpdateReconcileFinished { purpose, facts });
            }
            NativeUpdateFactDestination::Reply(reply) => {
                let _ = reply.send(facts);
            }
        }
        // A Reply or boot-health item need not emit any other UI event. This edge
        // guarantees a Full-queue pending latch gets another nonblocking chance.
        drained();
    }
}

/// Queue one handoff-worker observation on the same FIFO and wait off the event
/// loop for its ordered result.
pub(crate) fn collect_native_update_reconcile_facts(
    worker: &NativeUpdateReconcileSender,
    ticket: NativeUpdateReconcileTicket,
    current_build: u64,
) -> Option<NativeUpdateReconcileFacts> {
    let (reply, result) = std::sync::mpsc::sync_channel(1);
    worker
        .send(NativeUpdateWorkerRequest::Reconcile(
            NativeUpdateReconcileRequest {
                ticket,
                current_build,
                destination: NativeUpdateFactDestination::Reply(reply),
            },
        ))
        .ok()?;
    result.recv().ok()
}

const MAX_AUTOMATIC_UPDATE_CYCLES: u8 = 3;

/// A refusal about the installed environment, distinct from an exhausted
/// artifact budget. Only a later verified observation can release this latch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyEnvironmentBlock {
    build: u64,
    dmg_sha256: [u8; 32],
    blocked_at: std::time::Instant,
}

/// The activity-revoked retry ladder has this many distinct rungs (2 s → 30 s,
/// [`automatic_retry_delay`]); past the last one every further revocation waits
/// the last rung again. Activity is never a reason to stop: on the machine this
/// feature exists for — a daily driver with an agent streaming shell output into
/// it — revocation is the NORMAL outcome of an attempt, not evidence of a
/// problem, and the ladder in `native_update_auto_intent` is what makes the
/// next attempt land. Every attempt is a lossless launch/park/paint round trip
/// (a successor boot, ~1.3 s measured), so the spacing is what bounds the cost
/// — at most two a minute once saturated — and it stays inside
/// `LANDS_WITHIN`: a spacing of minutes would break the one-minute promise on
/// its own.
const ACTIVITY_REVOKED_LADDER_RUNGS: u8 = 5;

/// Physical handoff failures get a SMALL budget on a LONG leash — not the zero
/// they used to get.
///
/// The old policy was "never repeat a physical failure from a timer", and the
/// reasoning was sound as far as it went: a returned handoff really did park
/// readers, spawn a child and checkpoint sessions, so repeating it is not free.
/// What that reasoning missed is that `TimedOut` is classified physical, and the
/// handoff deadline (15 s, `app_input.rs`) has to cover an entire cold boot of the
/// old image, a blocking `flock`, re-verification, the bundle swap, a second exec,
/// a boot of the NEW image, PTY adoption and a full repaint. Measured on the
/// author's machine: 4.52 s — comfortable, until the page cache is cold or
/// `codesign` is not warm. So the commonest physical failure is ENVIRONMENTAL and
/// transient, and the penalty for hitting it once was permanent: automatic
/// in-session apply disabled for that build until a new one shipped, with the
/// staged update sitting there applying only on the next relaunch. That is the
/// exact symptom this whole feature exists to remove.
///
/// Two retries, spaced in tens of minutes, is the compromise WITHIN one epoch: a
/// slow moment gets another chance at a calmer one. Convergence for a STRUCTURAL
/// failure is NOT this constant's job — three failures inside forty minutes cannot
/// tell a broken pair of builds from a busy afternoon, which is why the lane it
/// ends is bounded by [`MAX_PHYSICAL_FAILURE_EPOCHS`] instead, after the whole
/// schedule has been replayed in three separate epochs spread across ~14 hours.
///
/// …AND WHY THIS SCHEDULE IS NOT THE ONLY ONE. A failure that says so in its own
/// right — two builds whose adoption proof genuinely cannot agree — never enters
/// this schedule at all now: it is charged
/// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] and converges without ever reaching an
/// epoch boundary. Everything above is the argument for the TRANSIENT member of
/// the set, and it was being spent on all four.
const MAX_PHYSICAL_FAILURE_CYCLES: u8 = 2;

/// Physical failures counted in ONE epoch: the initial failure plus its
/// [`MAX_PHYSICAL_FAILURE_CYCLES`] retries.
pub(crate) const PHYSICAL_FAILURES_PER_EPOCH: u8 = MAX_PHYSICAL_FAILURE_CYCLES + 1;

/// How many whole epochs an artifact gets before automatic apply gives up on
/// those exact bytes.
///
/// THE FINDING THIS CONSTANT EXISTS FOR: without it, a spent epoch stood down for
/// six hours, the stand-down outlasted the replenish window BY DESIGN, and so the
/// cycle counter reset and the artifact got a full fresh budget — forever. The
/// measured cost was ~10 park/spawn/paint round trips and ~10 "Update delayed"
/// pills a day, on a machine where nothing was ever going to change, while the
/// prose one screen up claimed the lane "converges to manual-only quickly". The
/// code and the claim disagreed and the claim was the nicer of the two.
///
/// Owner instruction: a transient BLOCK deserves a cooldown; a STRUCTURAL failure
/// does not deserve unbounded retries — bound it so it genuinely converges, keep
/// the transient lane retrying quietly, and do not notify a user on a schedule
/// for a failure that is not going to fix itself.
///
/// Three epochs is where "the machine was having a bad afternoon" stops being a
/// credible explanation. The full schedule is 9 failures spread over roughly
/// 14 hours (40 min of epoch + 6 h stand-down, three times over), so the last
/// epoch necessarily samples the machine on a different side of a night's idle
/// time from the first. An artifact that cannot hand off in any of them is
/// evidence about the BYTES, and the honest answer is the one the user can act
/// on: stop spending round trips, latch manual-only, and say so once.
const MAX_PHYSICAL_FAILURE_EPOCHS: u8 = 3;

/// Total physical failures an artifact may cost before automatic apply converges.
/// Nine, and after the ninth [`App::spend_physical_failure_budget`] answers
/// [`PhysicalFailureSchedule::Converged`] — for this TRANSIENT shape a quiet
/// re-sample one [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] out (plan P1-1(d)); only a
/// STRUCTURAL convergence is the deadline-less latch, and that type's doc says
/// exactly what can release it.
pub(crate) const PHYSICAL_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * MAX_PHYSICAL_FAILURE_EPOCHS;

/// Total physical failures a STRUCTURALLY-shaped artifact may cost before
/// automatic apply converges on those exact bytes.
///
/// THE FINDING THIS CONSTANT EXISTS FOR: the worker's four returned failure kinds
/// were classified precisely and then charged identically. They are not the same
/// event, and every word of the schedule above is an argument about ONE of them.
/// [`MAX_PHYSICAL_FAILURE_CYCLES`] is sized for `TimedOut` — a 15 s deadline
/// missed on a cold page cache, an accident of the machine's afternoon — and its
/// generosity (nine round trips, three independent epochs, ~14 hours) is bought
/// entirely by the claim that the next sample genuinely might succeed.
///
/// Nothing in that argument survives contact with an `AdoptionMismatch`: the
/// parent and its candidate disagreed about the adopted PTY set, which is a
/// property of the two IMAGES. Six hours of stand-down does not change a build's
/// proof format, and a quiet terminal does not change a bundle that fails
/// `codesign`. Charging a structural failure the transient schedule spends eight
/// further park/spawn/paint round trips, and most of a day of the automatic
/// lane's attention, re-learning what the first failure already said.
///
/// TWO, NOT ONE, and the second attempt is a genuine confirmation rather than
/// politeness: the class is not perfectly separable at this seam. A
/// `PreparationFailed` is the staged bundle failing pre-park verification
/// (structural, certain to recur) — and, until the 2026-09-22/23 update audit
/// split this process's own failures out as `ProducerFailed` (plan P1-2), it was
/// also a full disk, an `EMFILE`, or a screen-carry digest losing a race with a
/// resize. An `AdoptionMismatch` can still be a cross-version race. One retry,
/// ten minutes out, separates those at a cost of one round trip. A third would be
/// spending round trips to re-confirm a verdict already confirmed.
pub(crate) const STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS: u8 = 2;

/// Whole epochs an artifact gets when its handoff failure is UNEXPLAINED — the
/// candidate closed the readiness channel and the parent could observe nothing
/// about why (see [`crate::ChildDeathEvidence::Unobserved`]).
///
/// THIS IS THE LANE A `ChildDied` LANDS IN WHENEVER THE EVIDENCE RUNS OUT, and on
/// the shipping macOS lane that is still the commonest outcome: a candidate that
/// refuses closes the readiness channel and then keeps running for as long as it
/// takes to unwind, so the pre-kill look often finds it alive and the status that
/// comes back afterwards is this process's own SIGKILL. Sizing it is therefore
/// sizing the DEFAULT, not an exotic corner.
///
/// TWO, AND THE SECOND ONE IS THE ENTIRE POINT. An epoch is a re-sample of the
/// MACHINE, and the machine is exactly what an unexplained `ChildDied` might be
/// about: the field case that produced this classification was a child starved on
/// a desk carrying a load average of 140-160, and the retry that finally worked
/// was the one taken after that load stopped — HOURS later, not the 600 s and
/// 1800 s rungs inside the first epoch, which sampled the same pathological hour
/// three times. One [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] is the minimum that can
/// separate "this machine was having a bad hour" from "this successor will not
/// boot", and it is also the maximum worth spending on a verdict this weak: a
/// third epoch would be re-asking a question two independent samples have already
/// answered.
const UNEXPLAINED_FAILURE_EPOCHS: u8 = 2;

/// Total physical failures an artifact may cost while every one of them is
/// UNEXPLAINED. Six, across ~7.7 hours (40 min of epoch, one 6 h stand-down, then
/// 40 min more), after which automatic apply is done with those exact bytes.
///
/// STRICTLY BETWEEN THE OTHER TWO, WHICH IS THE WHOLE OF THE POLICY. An
/// unexplained failure is not a `TimedOut` — nothing about it says the machine
/// merely missed a deadline, and it may perfectly well be a successor that refuses
/// to boot — so it does not get the transient lane's nine attempts and fourteen
/// hours. Nor is it an `AdoptionMismatch` — no observation supports converging
/// after one confirming retry — so it does not get the structural lane's two. The
/// asserts below are what stop a later edit from collapsing it into either.
///
/// AND IT FORGIVES ITS COUNTED TRIAL LAUNCH, which is what makes six attempts
/// safe to spend at all: `MAX_BOOT_ATTEMPTS` is 3, so a shape that both retried
/// six times AND kept every counted launch would revert the bundle and poison the
/// build on its third attempt — the precise defect `forgive_trial_launch_if_advanced`
/// exists to prevent, arriving through the new lane instead of the old one. Only
/// [`PhysicalFailureShape::Structural`] keeps its count, and the assert above ties
/// that budget to `MAX_BOOT_ATTEMPTS` so the two can never diverge again.
pub(crate) const UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * UNEXPLAINED_FAILURE_EPOCHS;

/// A `ChildDied` this lane calls STRUCTURAL keeps its counted boot-trial launch
/// (`app_update_handoff`'s reject path forgives every other shape), so its whole
/// lifetime budget is spent against the SAME counter the boot sentinel reverts on.
/// Converging strictly sooner than `MAX_BOOT_ATTEMPTS` is what makes those two
/// budgets composable instead of adversarial: without it a retry schedule can walk
/// a build to the revert threshold and poison bytes that never failed, which is
/// exactly what `forgive_trial_launch_if_advanced` was added to prevent.
#[cfg(target_os = "macos")]
const _: () = assert!(
    (STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS as u32) < aterm_update::MAX_BOOT_ATTEMPTS,
    "a structural handoff failure keeps its counted trial launch, so its budget \
     must converge before the boot sentinel would revert the bundle"
);

const _: () = assert!(
    STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS < PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
    "the structural lane exists to converge SOONER than the transient one; at \
     parity the classification is decoration"
);

const _: () = assert!(
    STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS < UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS
        && UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS < PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
    "an unexplained failure is a weaker verdict than either of the ones the \
     parent can actually prove; its budget has to sit between them or the \
     three-way classification is decoration"
);

const _: () = assert!(
    UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS > PHYSICAL_FAILURES_PER_EPOCH,
    "an unexplained failure must be able to REACH the stand-down. Its whole \
     premise is that the machine may be what failed, and only a second epoch \
     samples a different machine; a budget that converges inside the first epoch \
     re-asks the same bad hour and calls the answer evidence"
);

const _: () = assert!(
    STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS <= PHYSICAL_FAILURES_PER_EPOCH,
    "a structural failure must be finished inside its FIRST epoch. It may never \
     reach the stand-down, whose whole premise — that six hours make the machine \
     an independent sample — is a statement about the machine, and a structural \
     failure is not about the machine"
);

/// Idle gap after which an artifact's PHYSICAL-failure counter starts over.
///
/// DELIBERATELY NOT [`crate::ACTIVITY_RETRY_BUDGET_REPLENISH`], which this lane
/// used to borrow. That window is 30 minutes and the second physical retry waits
/// exactly 30 minutes, so the third failure ALWAYS landed at or past the
/// replenish threshold and reset `cycles` to zero. The budget therefore never
/// reached [`MAX_PHYSICAL_FAILURE_CYCLES`]: a structurally broken pair of builds
/// alternated 10-minute and 30-minute park/spawn/paint round trips forever. A
/// budget whose cap cannot be reached is not a budget.
///
/// IT MUST NOW OUTLAST A WHOLE EPOCH, WHICH IS THE OPPOSITE OF WHAT IT USED TO DO,
/// and the inversion is the fix. The previous window (4 h) was deliberately
/// SHORTER than the 6 h stand-down so that "the next epoch starts from a full
/// budget" — which is exactly how the counter forgave itself on the very cadence
/// it prescribed and made the lane unbounded. Epochs can only be counted by a
/// counter that survives the gap between them, so the window has to clear the
/// longest gap the schedule itself can produce: one stand-down
/// ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`], 6 h) plus the in-epoch spacing
/// (600 s + 1800 s). Twelve hours clears that with room for a late failure that
/// waited behind other updater work.
///
/// The forgiveness property the window was borrowed for is intact and now means
/// something sharper: half a day with no physical failure at all for these exact
/// bytes — which, once the lane has converged, only a person's deliberate Version
/// menu retry can produce — starts the schedule over.
const PHYSICAL_RETRY_BUDGET_REPLENISH: std::time::Duration =
    std::time::Duration::from_secs(12 * 60 * 60);

/// How long automatic apply stands down after an artifact spends one epoch's
/// PHYSICAL retry budget.
///
/// A spent EPOCH is not proof that the artifact is broken — it is proof that
/// three handoffs inside forty minutes did not land, and the commonest cause
/// (`TimedOut`: the 15 s handoff deadline missed on a cold page cache or a cold
/// `codesign`) is a fact about the machine's afternoon, not about the bytes. So a
/// spent epoch is a LONG WAIT and nothing more; only a spent
/// [`MAX_PHYSICAL_FAILURE_EPOCHS`] is permitted to end the lane.
///
/// Six hours is what makes the epochs independent samples: it is long enough that
/// the page cache, `codesign`'s state, and the machine's load are unrelated to
/// what they were, and short enough that three of them fit inside a day, so a
/// user whose machine had one bad morning still gets the update that evening
/// rather than the following week. The worst case an artifact that can NEVER hand
/// off can cost is now finite and stated: 9 round trips over ~14 h, then silence.
const PHYSICAL_FAILURE_EPOCH_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(6 * 60 * 60);

const _: () = assert!(
    PHYSICAL_RETRY_BUDGET_REPLENISH.as_secs()
        > PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs() + 600 + 1800,
    "the replenish window must outlast a whole epoch — one stand-down plus the \
     in-epoch spacing — or the counter forgives itself between epochs, the epoch \
     cap is unreachable, and a structurally broken artifact retries forever"
);

/// What [`App::spend_physical_failure_budget`] says about the attempt that just
/// came back.
///
/// The three cases are kept apart rather than flattened into
/// `(Instant, bool exhausted)` because they mean three different things to a user
/// and the old pair could only say two of them: a mid-epoch retry is invisible, a
/// stand-down is a long quiet wait, and convergence is the only state where
/// reaching for the Version menu beats waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhysicalFailureSchedule {
    /// Another automatic attempt, inside the current epoch.
    Retry(std::time::Instant),
    /// This epoch is spent; the next one begins after a long stand-down.
    StandDown(std::time::Instant),
    /// This shape's [`PhysicalFailureShape::lifetime_attempts`] are spent (for the
    /// transient shape, [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] across
    /// [`MAX_PHYSICAL_FAILURE_EPOCHS`] independent epochs). The schedule is done
    /// and says so ONCE, loudly ([`App::announce_automatic_apply_stranded`]).
    ///
    /// `resample_at` is what happens next, and since the 2026-09-22/23 update
    /// audit (plan P1-1(d)) it is only `None` for a STRUCTURAL failure — the
    /// one verdict about the candidate's bytes. Nothing automatic releases that
    /// latch, and what does depends on which side of the bundle swap it sits
    /// on (corrected in the 2026-09-24 review; this said "a strictly newer
    /// build" moves it, which is only half true):
    ///
    /// * on a DOWNLOAD (the fork lane, or a failure before any swap) the latch
    ///   names those exact bytes, so a strictly newer build arms on its own;
    /// * on the installed ACTIVATION — where every launched-lane failure ends
    ///   up, because the candidate installs its bundle before it dials and the
    ///   latch follows the swap (P0-6) — NO newer release moves it: the
    ///   activation latch covers every artifact of its build, the activation
    ///   outranks any newer download while the bundle is newer than this
    ///   process (`reconcile_native_update_facts`), and its successor never
    ///   boot-applies past the build it was authorized as
    ///   (`aterm_update::apply_staged_if_ready`). The Version menu's apply, or
    ///   a relaunch into the installed bundle, is what moves it — which is
    ///   why convergence posts the health notice at once
    ///   ([`App::announce_automatic_apply_stranded`]) and says so.
    ///
    /// A TRANSIENT or
    /// UNEXPLAINED lifetime ends in a quiet re-sample one
    /// [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] out, every time, instead of a latch
    /// that never lapses: nine timed-out successors on a desk carrying a load
    /// average of 140-160 are a fact about THAT day, and the old `None` kept a
    /// healthy build off the machine until someone relaunched it. Those two
    /// shapes forgive their trial launch, so re-sampling cannot walk the boot
    /// sentinel toward a revert.
    Converged {
        resample_at: Option<std::time::Instant>,
    },
}

impl PhysicalFailureSchedule {
    /// Convergence for `shape` at `now`: the silent re-sample one epoch cooldown
    /// out for the shapes that are about the machine, none for the one that is
    /// about the bytes (plan P1-1(d)).
    #[must_use]
    fn converged(shape: PhysicalFailureShape, now: std::time::Instant) -> Self {
        Self::Converged {
            resample_at: (shape != PhysicalFailureShape::Structural)
                .then(|| now + PHYSICAL_FAILURE_EPOCH_COOLDOWN),
        }
    }

    /// The deadline to stamp on [`crate::AutoApplyManualOnly`]. `None` is the
    /// latch `lapse_expired_auto_apply_manual_only` will never release — minted
    /// ONLY on STRUCTURAL convergence, never for a single unlucky handoff and
    /// never for a failure that was about the machine's afternoon.
    #[must_use]
    fn retry_at(self) -> Option<std::time::Instant> {
        match self {
            Self::Retry(at) | Self::StandDown(at) => Some(at),
            Self::Converged { resample_at } => resample_at,
        }
    }
}

/// How long the automatic lane waits between attempts once an artifact's cheap
/// PREFLIGHT-BLOCK budget is spent. A probe is screen-silent
/// (`ClosePreflightVisibility::Quiet`), so this is retry spacing on a retained
/// intent and nothing more; a minute means a user who saves or closes the
/// Settings edit that blocked the update gets it within a minute, the same
/// bound the ladder promises.
const PREFLIGHT_BLOCK_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// How the close preflight's per-person blockers open (`native_update_close_preflight`)
/// — the shapes [`App::update_blocker_for_person`] recognises as work a person
/// has to save, so the reason and its reader cannot drift apart.
const SETTINGS_DRAFTS_BLOCK: &str = "Review Settings Drafts:";
const DIRTY_DOCUMENTS_BLOCK: &str = "Checkpoint Drafts:";
const FAILED_CHECKPOINTS_BLOCK: &str = "Retry:";

const _: () = assert!(PREFLIGHT_BLOCK_COOLDOWN.as_secs() <= 60);

/// How often an intent held by a capture REFUSAL re-reads the desk to see
/// whether the refusing state has moved on (the 2026-09-22/23 update audit,
/// plan P0-3). A read is a handful of `try_lock`s and a hash, and nothing is
/// launched unless the fingerprint changed — the whole point: v0.91 relaunched
/// a successor into the same refusal every fifteen minutes. Thirty seconds is
/// short enough that closing the offending tab or leaving the TUI lands the
/// update about as soon as the ladder would, and long enough that a desk being
/// resized continuously is not relaunched at on every drag.
pub(crate) const CAPTURE_REFUSAL_PROBE: std::time::Duration = std::time::Duration::from_secs(30);

/// The backstop for an intent held by a capture refusal: after an hour with the
/// desk unchanged the lane tries once more anyway, in case the refusal was
/// about something the fingerprint does not see. A refusal is recorded as an
/// apply failure at every attempt, so the backstop is also what keeps
/// `failing_applies` climbing (and the persistent-failure notice reachable)
/// for a desk that never moves — loud, bounded, and never a latch.
pub(crate) const CAPTURE_REFUSAL_BACKSTOP: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);

/// The updater's "a newer build has waited too long" notice is sized off this
/// lane's own promise (the 2026-09-22/23 update audit, plan P1-1(b)): far past
/// the ladder's bound and the switch after it (at least four times — the
/// ladder lands "within a minute" since main's 2026-09-23 retune, where the
/// audit sized it against the old fifteen), AND past the physical lane's first
/// retry (600 s), so a healthy lane — or one ordinary physical failure and
/// its scheduled retry — lands long before it, while a stuck one is said out
/// loud within the hour. Tied at compile time so neither number can move
/// without the other being looked at.
const _: () = assert!(
    aterm_update::PENDING_UPDATE_OVERDUE_SECS
        >= 4 * (crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
            + crate::native_update_auto_intent::SWITCH_ALLOWANCE.as_secs())
        && aterm_update::PENDING_UPDATE_OVERDUE_SECS
            > 600
                + crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
                + crate::native_update_auto_intent::SWITCH_ALLOWANCE.as_secs()
        && aterm_update::PENDING_UPDATE_OVERDUE_SECS <= 60 * 60,
    "the overdue notice must fire well past the apply ladder's bound and its \
     first physical retry — sooner and a healthy ladder trips it — and within \
     the hour, or a stranded machine waits in silence again"
);

/// WHICH LANE'S BUDGET A RETURNED HANDOFF FAILURE MAY SPEND, carried from the
/// [`crate::native_updater_service::ApplyMode`] the attempt was authorized under.
///
/// THE FINDING THIS TYPE EXISTS FOR: the completion path took `pending.mode`,
/// used it for the activity classification, and then DROPPED it — so every
/// returned failure, including one a person asked for from the Version menu or
/// `aterm-ctl update apply`, was charged to the AUTOMATIC lane. Two consequences,
/// both user-visible and both the wrong way round:
///   * a person's retry pushed the automatic artifact toward
///     [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`], so clicking Install three times on a
///     bad afternoon could converge the background lane to manual-only — the exact
///     "staged, applies on relaunch" state the seamless lane exists to delete;
///   * it also stamped `auto_apply_physical_retry` MICROSECONDS before surfacing,
///     which is precisely the freshness window the pill rule of the day used to
///     recognise the automatic lane's own quiet retries — so the person who just
///     asked for the update got silence.
///
/// A person's failure therefore charges NOTHING: no budget, no manual-only latch,
/// no retirement of a live automatic intent. It is surfaced (loudly, by the
/// caller) and that is all. The two automatic causes keep their existing separate
/// clocks.
/// WHAT A RETURNED PHYSICAL FAILURE IS EVIDENCE ABOUT: the machine's MOMENT, or
/// the two IMAGES. Carried from the worker's typed [`crate::UpdateHandoffOutcome`]
/// and never from its message string, exactly like the activity classification
/// beside it.
///
/// The distinction is not cosmetic — it decides how many park/spawn/paint round
/// trips an artifact may cost and how long the automatic lane keeps promising the
/// user it "retries on its own". See [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalFailureShape {
    /// The attempt lost a race the next one may win. Rides the full
    /// [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] schedule.
    Transient,
    /// The candidate could not become this process's successor, for a reason that
    /// belongs to the bytes rather than to the afternoon. Converges after
    /// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`].
    Structural,
    /// SOMETHING ENDED THE CANDIDATE AND THE PARENT DID NOT SEE WHAT. Not a third
    /// kind of failure — a failure whose kind is UNKNOWN, which is a different
    /// thing and must not be filed as either of the two the parent can prove.
    /// Rides the epoch machinery (the machine is one of the things it may be) on
    /// the shorter [`UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS`] budget (a successor
    /// that will not boot is the other).
    Unexplained,
}

impl PhysicalFailureShape {
    /// Classify one worker outcome, with whatever the worker OBSERVED about a
    /// candidate that died.
    ///
    /// STRUCTURAL, and why each one earns it:
    ///   * `AdoptionMismatch` — the child proved a PTY set the parent does not
    ///     recognise. Every instance this tree has recorded was a cross-version
    ///     disagreement about what the proof covers (a re-serialized manifest, a
    ///     screen digest taken over different bytes, a key count off by one);
    ///     none of them cared how busy the machine was;
    ///   * `PreparationFailed` — raised entirely BEFORE the spawn, and since the
    ///     2026-09-22/23 update audit (plan P1-2) its ONLY producer is the staged
    ///     candidate failing pre-park verification. A bundle that fails
    ///     `codesign` fails it again in six hours. Everything else that used to
    ///     share the name — a full disk, an `EMFILE`, a 63rd tab, no private
    ///     directory — is `ProducerFailed`, and TRANSIENT: this process's
    ///     afternoon, not the candidate's bytes.
    ///
    /// TRANSIENT is the rest, and deliberately includes the two that are facts
    /// about a moment: `TimedOut` (a 15 s deadline covering a cold boot, a
    /// blocking `flock`, a bundle swap, a second exec and a full repaint — 4.5 s
    /// measured, until the page cache is cold) and a non-activity-shaped
    /// `Rejected` (a commit-time re-check of state that moves on its own).
    /// `ProofReady` cannot reach a failure lane at all; it fails closed to the
    /// forgiving shape rather than converging an artifact on a state nobody
    /// understands. Nor can `CaptureRefused`, which is neither shape: a
    /// deterministic refusal of the DESK, which [`HandoffFailureLane::classify`]
    /// routes to [`HandoffFailureLane::Refused`] before any shape is asked for
    /// (plan P0-3) — here it fails closed the same way, never to Structural.
    ///
    /// `ChildDied` IS NOT IN EITHER LIST ANY MORE, and that is this function's
    /// finding. It used to be the third structural member, arguing that a
    /// candidate which exited before writing its readiness proof was "the successor
    /// image refusing to boot as a successor … the strongest statement about the
    /// new bytes available at this seam". The field refuted it as a UNIVERSAL
    /// reading (see [`crate::ChildDeathEvidence`]): a child starved of CPU also
    /// exits before writing a readiness proof — it never ran — and the same bytes
    /// that produced two `ChildDied` verdicts under a load average of 140-160
    /// applied with no intervention once the load stopped. The doc immediately
    /// above had ALREADY conceded the general point for `TimedOut`; `ChildDied` was
    /// simply never revisited. So the outcome no longer decides on its own —
    /// [`Self::of_child_death`] reads the evidence, and the absence of evidence is
    /// itself a state rather than a verdict.
    #[must_use]
    fn of_outcome(outcome: crate::UpdateHandoffOutcome, death: crate::ChildDeathEvidence) -> Self {
        match outcome {
            crate::UpdateHandoffOutcome::AdoptionMismatch
            | crate::UpdateHandoffOutcome::PreparationFailed => Self::Structural,
            crate::UpdateHandoffOutcome::ChildDied => Self::of_child_death(death),
            crate::UpdateHandoffOutcome::TimedOut
            | crate::UpdateHandoffOutcome::ProducerFailed
            | crate::UpdateHandoffOutcome::Rejected
            | crate::UpdateHandoffOutcome::ActivityRevoked
            | crate::UpdateHandoffOutcome::CaptureRefused
            | crate::UpdateHandoffOutcome::ProofReady => Self::Transient,
        }
    }

    /// THE ONE QUESTION THE EVIDENCE HAS TO ANSWER: did the successor IMAGE run
    /// and DECIDE, or did something end a candidate that never got that far?
    ///
    ///   * `Exited` — the candidate reached an `exit` instruction, which a starved
    ///     process never does. The CODE is not read: this tree's commonest refusal
    ///     is a clean `0` (`main_entry` returns without a window when the overlap
    ///     authority is incomplete), so treating a non-zero code as the structural
    ///     one would classify exactly the wrong half. STRUCTURAL.
    ///   * `Signalled` — split by WHO raised it. A fault is the image executing
    ///     itself into a wall, which is the bytes; every other signal arrives from
    ///     outside the image (macOS jetsam SIGKILLs under memory pressure, which is
    ///     precisely the pathological-load state this classification exists for),
    ///     which is the machine.
    ///   * `Unobserved` — no statement either way. UNEXPLAINED: retried, bounded,
    ///     converging.
    ///
    /// THERE IS NO ARM FOR "THE IMAGE STARTED", and its absence is deliberate. The
    /// only fact the shipping macOS lane can gather without a witnessed status is
    /// the boot sentinel's launch counter, and that counter moves at the FIRST
    /// statement of the target's `main` — long before the rendezvous dial that a
    /// `ChildDied` on that lane presupposes. It is therefore true of a starved
    /// candidate and a refusing one alike, and filing it as STRUCTURAL reproduced
    /// the field defect exactly. See [`crate::ChildDeathEvidence`].
    ///
    /// ALSO THE FORGIVENESS RULE. `app_update_handoff`'s reject path keeps a
    /// counted boot-trial launch only for the deaths this function calls
    /// `Structural` — the ones the bytes answer for — and the compile-time assert
    /// beside [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] keeps that budget strictly
    /// under `MAX_BOOT_ATTEMPTS`, so no retry schedule can walk a healthy build
    /// into a revert.
    #[must_use]
    pub(crate) fn of_child_death(death: crate::ChildDeathEvidence) -> Self {
        match death {
            crate::ChildDeathEvidence::Unobserved => Self::Unexplained,
            crate::ChildDeathEvidence::Exited { .. } => Self::Structural,
            crate::ChildDeathEvidence::Signalled { signal } => {
                if signal_is_the_image_faulting(signal) {
                    Self::Structural
                } else {
                    Self::Transient
                }
            }
        }
    }

    /// How many failures for one artifact this shape may cost before automatic
    /// apply is done with those exact bytes. The counter these are compared against
    /// is shared and shape-blind (see [`App::spend_physical_failure_budget`]); only
    /// the CEILING is a property of what the parent proved.
    #[must_use]
    pub(crate) const fn lifetime_attempts(self) -> u8 {
        match self {
            Self::Transient => PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
            Self::Structural => STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
            Self::Unexplained => UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
        }
    }
}

/// Whether `signal` is one an image raises ON ITSELF by executing — a fault —
/// rather than one delivered to it from outside.
///
/// THE DISTINCTION IS THE WHOLE CLASSIFICATION, so it is drawn by NAME and not by
/// number: `SIGBUS` is 10 on macOS and 7 on Linux, and 7 on macOS is `SIGEMT`. A
/// hand-rolled numeric set would have silently swapped "the image faulted" for
/// "something killed it" on one of the two platforms this crate builds for.
///
/// The set is the synchronous exceptions both platforms name: an illegal
/// instruction, a trap, an `abort()`, a floating-point exception, a bad memory
/// access, an invalid address, and a bad system call. (`SIGEMT` is deliberately
/// absent — libc defines it only for mips/sparc Linux, so naming it would cost the
/// portability this function exists for, and no x86/ARM image raises it.)
///
/// Everything else — `SIGKILL` above all, which is what macOS jetsam sends a
/// process it is reclaiming memory from, and which no process can raise on itself
/// — is somebody else's decision about a process that may never have executed an
/// instruction of its own.
#[cfg(unix)]
#[must_use]
fn signal_is_the_image_faulting(signal: i32) -> bool {
    matches!(
        signal,
        libc::SIGILL
            | libc::SIGTRAP
            | libc::SIGABRT
            | libc::SIGFPE
            | libc::SIGBUS
            | libc::SIGSEGV
            | libc::SIGSYS
    )
}

/// Unreachable off unix — [`crate::ChildDeathEvidence::Signalled`] is produced only
/// by the unix worker, and no other platform has a handoff candidate to reap. The
/// answer is the FORGIVING one for the same reason `ProofReady` is: a shape
/// invented for a state nobody understands must not be the one that converges an
/// artifact.
#[cfg(not(unix))]
#[must_use]
fn signal_is_the_image_faulting(_signal: i32) -> bool {
    false
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HandoffFailureLane {
    /// AUTOMATIC, and the lossless rollback was caused by user/terminal activity:
    /// spends [`AutomaticRetryKind::ActivityRevoked`] budget.
    ActivityRevoked,
    /// AUTOMATIC, and the failure is evidence about the artifact or the machine:
    /// spends the converging physical budget, on the schedule its
    /// [`PhysicalFailureShape`] earns.
    Physical(PhysicalFailureShape),
    /// A PERSON asked for this apply (the Version menu, the palette, a control
    /// request, or an install-on-clean-quit gesture). Charges nothing.
    Manual,
    /// AUTOMATIC, and the park's capture REFUSED the desk
    /// ([`crate::UpdateHandoffOutcome::CaptureRefused`]) — the arm that is
    /// neither a physical shape nor activity (the 2026-09-22/23 update audit,
    /// plan P0-3). Not the machine's moment, so it spends no activity spacing
    /// and is never re-parked; not the candidate's bytes, so it spends no
    /// physical budget and never latches. It is RECORDED as an apply failure
    /// naming the session (so `failing_applies` moves and `update status` says
    /// `apply_failure=`) and retried once the desk changes, or after
    /// [`CAPTURE_REFUSAL_BACKSTOP`]. v0.91 filed it as `ActivityRevoked` and
    /// relaunched a successor every fifteen minutes into the same refusal.
    Refused,
}

impl HandoffFailureLane {
    /// Classify one returned completion from the two typed facts it carries: the
    /// mode the apply was authorized under, and the worker's `outcome`.
    ///
    /// `revoked_by_activity` is the MAIN THREAD's half of the activity
    /// observation (an activity-shaped rejection it recorded against this
    /// attempt); the worker's half is the `ActivityRevoked` outcome, and either
    /// one is enough. Both are meaningful only for a background attempt — a
    /// person's apply deliberately does not WAIT for a quiet epoch first. (It
    /// still arms the revocation watcher: `Immediate` stamps `activity_epoch`,
    /// hands the worker a live `cancel`, and gates Commit on `exact_activity`
    /// exactly like every other lane.)
    ///
    /// THE OUTCOME IS NOW READ RATHER THAN DISCARDED. It used to reach this
    /// decision as a single `activity_revoked: bool` and stop there, so all four
    /// physical kinds were charged one schedule — the one written for the
    /// transient member of the set. See [`PhysicalFailureShape`].
    ///
    /// AND `death` IS THE SAME CORRECTION ONE LEVEL FURTHER DOWN. The outcome is a
    /// total classification of what the WORKER did, but `ChildDied` is not a
    /// classification of anything — it is proof EOF, which a refusing successor, a
    /// faulting one and a starved one all produce identically. The evidence the
    /// worker gathered at that instant is what separates them; see
    /// [`crate::ChildDeathEvidence`]. Every other outcome ignores it, because every
    /// other outcome describes a candidate THIS process ended.
    ///
    /// `CleanQuit` counts as person-initiated: it exists only because a human just
    /// quit the app, nothing re-attempts it on a timer, and the process is on its
    /// way out — so spending a budget that dies with it could only ever damage the
    /// NEXT session's automatic lane through the durable side effects the latch
    /// drives.
    #[must_use]
    pub(crate) fn classify(
        mode: crate::native_updater_service::ApplyMode,
        outcome: crate::UpdateHandoffOutcome,
        death: crate::ChildDeathEvidence,
        revoked_by_activity: bool,
    ) -> Self {
        if !mode.is_automatic() {
            return Self::Manual;
        }
        // FIRST, and over the main thread's activity flag too: the park typed
        // this stand-down itself, and re-filing a refusal as activity is the
        // exact loop the lane exists to end (`RefusalNeverRetriesAsActivity`).
        if outcome == crate::UpdateHandoffOutcome::CaptureRefused {
            return Self::Refused;
        }
        if revoked_by_activity || outcome == crate::UpdateHandoffOutcome::ActivityRevoked {
            Self::ActivityRevoked
        } else {
            Self::Physical(PhysicalFailureShape::of_outcome(outcome, death))
        }
    }

    /// Whether this failure is allowed to touch the automatic lane's scheduling
    /// state at all (its budgets, its latch, its live intent).
    #[must_use]
    fn charges_the_automatic_lane(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

/// The FORK LANE's answer to a capture that refused the desk, by who asked —
/// the same verdict [`HandoffFailureLane::classify`] gives the launched lane's
/// completion for the same fact. Automatic: [`UpdateOutcome::CaptureRefused`],
/// whose caller arms the desk-fingerprint retry it promises. A person's press
/// (`Manual` there): [`UpdateOutcome::Failed`], as
/// `abort_reaped_native_apply_before_reconcile` answers it — nothing arms a
/// retry for a press, so Settings must not say "retries when it changes".
fn fork_capture_refusal_outcome(
    mode: crate::native_updater_service::ApplyMode,
    message: String,
) -> UpdateOutcome {
    if mode.is_automatic() {
        UpdateOutcome::CaptureRefused { message }
    } else {
        UpdateOutcome::Failed { message }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutomaticRetryKind {
    PreflightBlocked,
    /// The overlap attempt parked, spawned, and was then revoked by USER OR
    /// TERMINAL ACTIVITY before Commit. The rollback is proven lossless (kill,
    /// reap, resume readers; zero bytes consumed by the parent post-park), so
    /// — unlike `PhysicalFailure` — repeating later is as safe as the first
    /// attempt. Spaced (2 s growing to 30 s) so a busy terminal is not launched
    /// at continuously; never exhausted, because activity is never a reason to
    /// stop (the ladder decides when the next attempt lands).
    ActivityRevoked,
    PhysicalFailure,
}

/// A bounded retry plan for cheap ordering/preflight races and — on a
/// deliberately short budget and long leash — for physical handoff failures,
/// most of which are a missed deadline rather than a broken pair of builds (see
/// [`MAX_PHYSICAL_FAILURE_CYCLES`]); and an UNBOUNDED, saturating spacing for
/// lossless activity-revoked overlap returns.
///
/// For [`AutomaticRetryKind::PhysicalFailure`] the `cycles` argument is the index
/// WITHIN the current epoch, not the artifact's lifetime failure count: `None`
/// here means "this epoch is spent", and whether that ends the epoch or the whole
/// lane is [`App::spend_physical_failure_budget`]'s decision. For
/// [`AutomaticRetryKind::ActivityRevoked`] the answer is never `None`.
#[must_use]
fn automatic_retry_delay(cycles: u8, kind: AutomaticRetryKind) -> Option<std::time::Duration> {
    let budget = match kind {
        AutomaticRetryKind::ActivityRevoked => None,
        AutomaticRetryKind::PhysicalFailure => Some(MAX_PHYSICAL_FAILURE_CYCLES),
        AutomaticRetryKind::PreflightBlocked => Some(MAX_AUTOMATIC_UPDATE_CYCLES),
    };
    if budget.is_some_and(|budget| cycles >= budget) {
        return None;
    }
    let seconds = match (kind, cycles.min(ACTIVITY_REVOKED_LADDER_RUNGS - 1)) {
        (AutomaticRetryKind::PreflightBlocked, 0 | 1) => 5,
        (AutomaticRetryKind::PreflightBlocked, _) => 15,
        // Spacing that saturates at 30 s: each revoked attempt costs a
        // launch/park/paint round trip, so back off while the terminal stays
        // busy — but in seconds, because the lane promises a landing within a
        // minute. The ladder's phase decides what the next attempt waits for,
        // and the spacing starts over after a long idle gap.
        (AutomaticRetryKind::ActivityRevoked, 0) => 2,
        (AutomaticRetryKind::ActivityRevoked, 1) => 5,
        (AutomaticRetryKind::ActivityRevoked, 2) => 10,
        (AutomaticRetryKind::ActivityRevoked, 3) => 20,
        (AutomaticRetryKind::ActivityRevoked, _) => 30,
        // Kept explicit so this helper stays fail-closed if the early return above is
        // Tens of minutes, not seconds. A physical retry costs a real
        // park/spawn/paint round trip, and the failure it is recovering from is a
        // missed deadline — so wait long enough that the machine is plausibly in a
        // different state (page cache warm, codesign warm, load down) rather than
        // re-running the same losing race immediately. The budget above stops this
        // after two retries per epoch; [`MAX_PHYSICAL_FAILURE_EPOCHS`] stops the
        // epochs.
        (AutomaticRetryKind::PhysicalFailure, 0) => 600,
        (AutomaticRetryKind::PhysicalFailure, _) => 1800,
    };
    Some(std::time::Duration::from_secs(seconds))
}

/// Parse the durable artifact identity without allocating. Reducer-imported stages
/// are already canonicalized, but automatic application fails closed if a future
/// producer ever hands this layer malformed identity bytes.
#[must_use]
pub(crate) fn decode_dmg_sha256(digest: &str) -> Option<[u8; 32]> {
    fn nibble(byte: u8) -> Option<u8> {
        match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            b'A'..=b'F' => Some(byte - b'A' + 10),
            _ => None,
        }
    }

    let bytes = digest.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        decoded[index] = nibble(pair[0])?.checked_shl(4)? | nibble(pair[1])?;
    }
    Some(decoded)
}

/// Seed the process service from the same durable status marker every existing
/// update surface reads. A missing marker still produces an honest enabled/idle
/// (or disabled) service; no network work occurs here.
pub(crate) fn load_native_updater_service() -> NativeUpdaterService {
    let build = crate::build_info::BUILD_NUMBER.parse::<u64>().unwrap_or(0);
    let version = crate::build_info::version_display();
    // Startup ledger reconciliation is posted by an off-loop worker. Construction
    // stays filesystem/process-spawn free so first input cannot queue behind TOML or
    // PlistBuddy work on the event loop.
    NativeUpdaterService::new(build, version, aterm_update::enabled())
}

/// Bounded, owned result suitable for a typed event-loop wake.
///
/// Runs ONLY on a worker thread (the facts worker and the check worker), which is
/// what lets it take the second ledger read below. `aterm_update` deliberately keeps
/// the apply lane's REASONS out of `UpdateStatus` — the check lane and the apply lane
/// fail independently — and exposes them through `apply_lane_report` instead. Every
/// consumer that needs both is expected to read them side by side; this is that read,
/// done once, here, so the event loop and every surface downstream see one owned value
/// carrying the failure COUNT and the failure REASON together. Without it the window
/// could say "two applies failed" but never why (2026-08-21 field report).
pub(crate) fn durable_update_status(status: aterm_update::UpdateStatus) -> DurableUpdateStatus {
    let apply_lane = aterm_update::apply_lane_report(status.current_build).unwrap_or_default();
    DurableUpdateStatus {
        linux_host: cfg!(target_os = "linux"),
        linux: status.linux,
        enabled: status.enabled,
        current_build: status.current_build,
        staged_build: status.staged_build,
        staged_version: status.staged_version,
        staged_commit: status.staged_commit,
        staged_dmg_sha256: status.staged_dmg_sha256,
        changelog: status.changelog,
        outcome: status.outcome,
        failing_checks: status.failing_checks,
        failing_persistent: status.failing_persistent,
        failing_kind: status.failing_kind,
        failing_applies: status.failing_applies,
        apply_failure: apply_lane.last_failure,
        apply_failure_build: apply_lane.last_failure_target_build,
        apply_failures_for_target: apply_lane.failures_for_target,
        installable: status.installable,
        channel_unreadable: status.channel_unreadable,
        checked_at: aterm_update_core::pkg_check::rfc3339_to_unix(&status.updated_at),
    }
}

fn failed_update_status(build: u64, message: String) -> DurableUpdateStatus {
    DurableUpdateStatus {
        linux_host: cfg!(target_os = "linux"),
        linux: None,
        enabled: true,
        current_build: build,
        staged_build: None,
        staged_version: None,
        staged_commit: None,
        staged_dmg_sha256: None,
        changelog: None,
        outcome: message,
        failing_checks: 1,
        failing_persistent: false,
        failing_kind: String::new(),
        failing_applies: 0,
        // Likewise: a worker that could not start says nothing about the apply lane,
        // and inventing a reason here would attach it to a streak of zero.
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        // A worker failure says nothing about the bundle; the installable claim is
        // only ever made by a real observation.
        installable: true,
        // Likewise: only a completed check can pronounce the channel unreadable.
        channel_unreadable: false,
        checked_at: None,
    }
}

fn union_native_damage(first: DamageRegion, second: DamageRegion) -> DamageRegion {
    let (
        DamageRegion::Rect {
            x: first_x,
            y: first_y,
            width: first_width,
            height: first_height,
        },
        DamageRegion::Rect {
            x: second_x,
            y: second_y,
            width: second_width,
            height: second_height,
        },
    ) = (first, second)
    else {
        return DamageRegion::All;
    };
    let x = first_x.min(second_x);
    let y = first_y.min(second_y);
    let right = first_x
        .saturating_add(first_width)
        .max(second_x.saturating_add(second_width));
    let bottom = first_y
        .saturating_add(first_height)
        .max(second_y.saturating_add(second_height));
    DamageRegion::Rect {
        x,
        y,
        width: right.saturating_sub(x),
        height: bottom.saturating_sub(y),
    }
}

/// Identity of every input that can change the active native app's compiled UI.
/// A cached frame is observable only while this stamp still matches the live
/// controller, service, document, and window geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct NativeUiCompileStamp {
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) geometry: u64,
    pub(crate) config_revision: u64,
    pub(crate) update_revision: u64,
    pub(crate) document_seq: Option<u64>,
    pub(crate) presentation_revision: u64,
    /// Theme and chrome-font identity consumed only by raster lowering. A
    /// paint-input change must never reuse a reducer-local rectangle.
    pub(crate) paint_revision: u64,
}

fn native_appearance_revision(preferences: crate::native_appearance::AppearancePreferences) -> u64 {
    use std::hash::{Hash, Hasher};

    let preferences = preferences.normalized();
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    preferences.high_contrast.hash(&mut revision);
    preferences.reduced_transparency.hash(&mut revision);
    preferences.text_scale.to_bits().hash(&mut revision);
    revision.finish()
}

/// Paint identity for the host-owned motion facts supplied to every native app.
/// These facts are intentionally not reducer state: focus, OS Reduce Motion,
/// recording-watch focus, load shedding, OS appearance, the process-wide serious
/// override and the GPU capability (a device lost, an intent redeemed) can all
/// change while the native view generation remains stable.
/// They must nevertheless invalidate retained pixels because Settings previews
/// resolve animation, automatic window appearance, static effect output, and their
/// static/live badge from this exact context.
fn native_motion_revision(motion: crate::native_app::ViewMotionCx) -> u8 {
    (motion.system_reduced as u8)
        | ((motion.focused as u8) << 1)
        | ((motion.performance_reduced as u8) << 2)
        | ((motion.serious as u8) << 3)
        | ((motion.system_dark as u8) << 4)
        | ((motion.backend_gpu as u8) << 5)
}

impl NativeUiCompileStamp {
    /// Regional reducer damage remains sound only when the view-local reducer
    /// generation is the sole stamp input that changed. Geometry, services,
    /// document content, or presentation changes conservatively widen to All.
    pub(crate) fn accepts_regional_damage_from(self, previous: Self) -> bool {
        self.instance == previous.instance
            && self.view == previous.view
            && self.geometry == previous.geometry
            && self.config_revision == previous.config_revision
            && self.update_revision == previous.update_revision
            && self.document_seq == previous.document_seq
            && self.presentation_revision == previous.presentation_revision
            && self.paint_revision == previous.paint_revision
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeCompiledPhase {
    Staged,
    Presented,
}

/// The exact semantic/layout product lowered into the native tray for a frame.
/// Keeping this beside the raster makes control inspection an observer of the
/// same artifact as pixels, hit testing, and AccessKit rather than a second
/// speculative compiler invocation.
#[derive(Clone, Debug)]
pub(crate) struct NativeCompiledFrame {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) phase: NativeCompiledPhase,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Retained semantic scene for exactly one native split leaf. This is the
/// backend-neutral handoff: raster today consumes `compiled.tray`, while a direct
/// GPU/CPU `UiScene` backend can consume the same view/stamp/viewport without
/// changing tab geometry, cache identity, hit testing or accessibility.
#[derive(Clone, Debug)]
pub(crate) struct NativeLeafScene {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) viewport: crate::native_ui::LogicalRect,
    /// View-local logical-pixel damage declared by the reducer. The retained
    /// tray adapter patches the outward-rounded device tile when every other
    /// compile-stamp input is stable, without widening to the leaf or window.
    pub(crate) damage: DamageRegion,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Immutable view of one compositor-retained native leaf. The full live compile
/// stamp, lifecycle generation, semantic tree, and exact raw-window destination
/// have already been cross-checked by the resolver that returns it.
pub(crate) struct RetainedNativeLeafArtifact<'a> {
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) compiled: &'a crate::native_ui::CompiledUi,
    /// Signed window-space destination. A centred transient surface crop can
    /// place the retained leaf partly above/left of the physical client area.
    pub(crate) device_x: i64,
    pub(crate) device_y: i64,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) scale: f64,
}

struct NativeConfigPersistenceJob {
    plan: crate::native_config_service::ConfigPersistencePlan,
    undo: Option<u64>,
    origin: NativeConfigOrigin,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

struct NativeConfigReconciliationJob {
    path: std::path::PathBuf,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
    pending_sequence: u64,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

struct NativeConfigExternalPreparationJob {
    observation: crate::native_config_service::ConfigDiskObservation,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
}

enum NativeConfigJob {
    Persist(NativeConfigPersistenceJob),
    Reconcile(NativeConfigReconciliationJob),
    PrepareExternal(NativeConfigExternalPreparationJob),
}

#[derive(Clone, Debug)]
pub(crate) struct NativeConfigPersistenceCompletion {
    pub(crate) outcome: ConfigPatchOutcome,
    pub(crate) observation: Result<crate::native_config_service::PreparedConfigObservation, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeConfigReconciliationCompletion {
    pub(crate) pending_sequence: u64,
    pub(crate) observation: Result<crate::native_config_service::PreparedConfigObservation, String>,
}

#[derive(Clone, Debug)]
pub(crate) struct NativeConfigExternalPreparationCompletion {
    pub(crate) observation: crate::native_config_service::ConfigDiskObservation,
    pub(crate) result: Result<crate::native_config_service::PreparedConfigObservation, String>,
}

pub(crate) enum NativeConfigWork {
    Patch(crate::native_app::ConfigPatch),
    Undo(u64),
    /// A process command is a semantic intent until it reaches the head of the
    /// serialized config lane.  Materializing its OCC patch any earlier would
    /// give several rapid toggles the same base revision/expected value, so the
    /// third click could conflict with the second one's durable completion.
    SeriousMode(bool),
    /// Legacy control-protocol `settings set|unset` is an absolute semantic
    /// intent, materialized against the newest service revision only when it
    /// reaches the serialized lane head.  It must never run the old standalone
    /// read/edit/write helper beside native Settings.
    ControlField {
        key: String,
        value: Option<String>,
    },
}

/// Completion authority for the one serialized config lane. Native Settings
/// patches return to their typed reducer; process commands have no fabricated
/// view identity and complete through their own App-owned policy transition.
#[derive(Debug)]
pub(crate) enum NativeConfigOrigin {
    View {
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
    },
    SeriousMode {
        desired: bool,
    },
    /// A View ▸ Presence Band / Rim toggle's durable write (round 19): `key` is
    /// the `[presence]` leaf (`presence.band` / `presence.rim`), `desired` the
    /// bit the human chose. The LIVE bit was flipped at the click
    /// (`App::user_toggle_presence`); this completion only adopts the durable
    /// truth or, on a refused write, says so and reverts the live bit to it.
    Presence {
        key: &'static str,
        desired: bool,
    },
    /// Completion sink for the stable `settings set|unset` wire command. The
    /// control thread remains blocked on this one-shot while the main loop stays
    /// free to receive the worker completion. Non-success outcomes are returned
    /// as `Err`, so the wire formatter cannot accidentally prefix them with OK.
    Control {
        request_id: u64,
        key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    },
}

pub(crate) struct NativeConfigRequest {
    origin: NativeConfigOrigin,
    work: NativeConfigWork,
}

pub(crate) enum DeferredNativeConfigGeneration {
    Prepared(Box<crate::native_font_catalog::PreparedConfigGeneration>),
    Observation(Box<crate::native_config_service::PreparedConfigObservation>),
}

impl DeferredNativeConfigGeneration {
    fn baseline(&self) -> &crate::native_document_host::AtomicFileBaseline {
        match self {
            Self::Prepared(generation) => &generation.observation.baseline,
            Self::Observation(prepared) => &prepared.observation.baseline,
        }
    }

    fn themes(&self) -> &std::sync::Arc<crate::app_config::ThemeCatalog> {
        match self {
            Self::Prepared(generation) => &generation.assets.themes,
            Self::Observation(prepared) => &prepared.assets.themes,
        }
    }
}

fn native_config_queue() -> Result<&'static std::sync::mpsc::Sender<NativeConfigJob>, String> {
    static QUEUE: std::sync::OnceLock<Result<std::sync::mpsc::Sender<NativeConfigJob>, String>> =
        std::sync::OnceLock::new();
    QUEUE
        .get_or_init(|| {
            let (sender, receiver) = std::sync::mpsc::channel::<NativeConfigJob>();
            std::thread::Builder::new()
                .name("aterm-native-config".to_string())
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        match job {
                            NativeConfigJob::Persist(job) => {
                                let completion =
                                    execute_native_config_persistence(&job.plan, job.undo);
                                let _ = job.proxy.send_event(Wake::NativeConfigFinished {
                                    origin: job.origin,
                                    completion,
                                });
                            }
                            NativeConfigJob::Reconcile(job) => {
                                let completion = NativeConfigReconciliationCompletion {
                                    pending_sequence: job.pending_sequence,
                                    observation: observe_and_prepare_native_config(
                                        &job.path, job.themes,
                                    ),
                                };
                                let _ = job
                                    .proxy
                                    .send_event(Wake::NativeConfigReconciled { completion });
                            }
                            NativeConfigJob::PrepareExternal(job) => {
                                let observation = job.observation;
                                let result =
                                    crate::native_config_service::VersionedConfigService::prepare_observation(
                                        observation.clone(),
                                        job.themes,
                                    );
                                let completion = NativeConfigExternalPreparationCompletion {
                                    observation,
                                    result,
                                };
                                let _ = job.proxy.send_event(
                                    Wake::NativeConfigExternalPrepared { completion },
                                );
                            }
                        }
                    }
                })
                .map_err(|error| format!("could not start config worker: {error}"))?;
            Ok(sender)
        })
        .as_ref()
        .map_err(Clone::clone)
}

fn observe_and_prepare_native_config(
    path: &std::path::Path,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
) -> Result<crate::native_config_service::PreparedConfigObservation, String> {
    let observation =
        crate::native_config_service::VersionedConfigService::observe_path(path, true)?;
    crate::native_config_service::VersionedConfigService::prepare_observation(observation, themes)
}

pub(crate) fn execute_native_config_persistence(
    plan: &crate::native_config_service::ConfigPersistencePlan,
    undo: Option<u64>,
) -> NativeConfigPersistenceCompletion {
    let saved = crate::prefs::save_prefs_snapshot_observed(plan);
    let outcome = match &saved.outcome {
        crate::prefs::SaveOutcome::Saved | crate::prefs::SaveOutcome::Unchanged => {
            ConfigPatchOutcome::Applied {
                revision: plan.snapshot.revision,
                undo,
            }
        }
        crate::prefs::SaveOutcome::Conflict { .. } => ConfigPatchOutcome::Conflict {
            revision: plan.snapshot.revision,
        },
        crate::prefs::SaveOutcome::PublishedUnverified { stage, message, .. } => {
            ConfigPatchOutcome::Indeterminate {
                message: format!(
                    "config publication at {stage:?} could not be verified: {message}; reload before retrying"
                ),
            }
        }
        crate::prefs::SaveOutcome::Error(message) => ConfigPatchOutcome::Rejected {
            message: message.clone(),
        },
    };
    let observation = if let Some(baseline) = saved.observed {
        Ok(crate::native_config_service::ConfigDiskObservation {
            text: plan.snapshot.text.to_string(),
            baseline,
        })
    } else {
        plan.baseline
            .as_ref()
            .map(|baseline| baseline.target.logical_path().to_path_buf())
            .or_else(|| plan.logical_path.clone())
            .or_else(crate::app_config::config_path)
            .ok_or_else(|| "no config path (HOME/XDG unset)".to_string())
            .and_then(|path| {
                crate::native_config_service::VersionedConfigService::observe_path(&path, true)
            })
    }
    .and_then(|observation| {
        crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            std::sync::Arc::clone(&plan.snapshot.assets.themes),
        )
    });
    NativeConfigPersistenceCompletion {
        outcome,
        observation,
    }
}

impl App {
    /// Main-thread projection consumed by the control socket's front-input
    /// authorization fence. Overlay identity wins because it is the immediate
    /// event consumer; otherwise native Settings is distinguished from every
    /// other native app instead of being collapsed into "no overlay".
    pub(crate) fn front_control_surface(&self) -> crate::control::FrontControlSurface {
        if let Some(kind) = self
            .front()
            .and_then(|window| window.overlay())
            .map(|overlay| overlay.kind())
        {
            return crate::control::FrontControlSurface::Overlay(kind);
        }
        let Some(wid) = self.frontmost_window else {
            return crate::control::FrontControlSurface::None;
        };
        let Some((instance, _)) = self.active_native_view(wid) else {
            return crate::control::FrontControlSurface::None;
        };
        if self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
        {
            crate::control::FrontControlSurface::NativeSettings
        } else {
            crate::control::FrontControlSurface::OtherNative
        }
    }

    fn ensure_native_update_reconcile_worker(&mut self) -> Option<NativeUpdateReconcileSender> {
        if let Some(worker) = self.native_update_reconcile_worker.clone() {
            return Some(worker);
        }
        let proxy = self.proxy.clone()?;
        match spawn_native_update_reconcile_worker(proxy) {
            Ok(worker) => {
                self.native_update_reconcile_worker = Some(worker.clone());
                Some(worker)
            }
            Err(error) => {
                aterm_log::warn!("native updater worker restart failed: {error}");
                None
            }
        }
    }

    /// Mint one monotonic identity before dispatching any durable updater read.
    /// Exhaustion fails closed: sequence reuse could let an old completion outrank
    /// a newer artifact observation.
    pub(crate) fn mint_native_update_reconcile_ticket(
        &mut self,
    ) -> Option<NativeUpdateReconcileTicket> {
        let request_sequence = self.next_native_update_reconcile_sequence;
        self.next_native_update_reconcile_sequence = request_sequence.checked_add(1)?;
        Some(NativeUpdateReconcileTicket { request_sequence })
    }

    #[must_use]
    pub(crate) fn native_update_reconcile_worker(&self) -> Option<NativeUpdateReconcileSender> {
        self.native_update_reconcile_worker.clone()
    }

    fn try_dispatch_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> NativeUpdateDispatch {
        let Some(worker) = self.ensure_native_update_reconcile_worker() else {
            return NativeUpdateDispatch::Unavailable;
        };
        let Some(ticket) = self.mint_native_update_reconcile_ticket() else {
            aterm_log::warn!("native updater reconciliation identity space exhausted");
            return NativeUpdateDispatch::Unavailable;
        };
        let current_build = self.native_updater_service.snapshot().current_build;
        let work = NativeUpdateWorkerRequest::Reconcile(NativeUpdateReconcileRequest {
            ticket,
            current_build,
            destination: destination(purpose),
        });
        match worker.try_send(work) {
            Ok(()) => NativeUpdateDispatch::Queued,
            Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => {
                self.native_update_reconcile_worker = None;
                let Some(restarted) = self.ensure_native_update_reconcile_worker() else {
                    return NativeUpdateDispatch::Unavailable;
                };
                match restarted.try_send(work) {
                    Ok(()) => NativeUpdateDispatch::Queued,
                    Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        self.native_update_reconcile_worker = None;
                        NativeUpdateDispatch::Unavailable
                    }
                }
            }
        }
    }

    fn request_native_update_reconcile_with(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> bool {
        if !self.native_updater_service.snapshot().enabled {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        }
        let effective = self
            .pending_native_update_reconcile_purpose
            .take()
            .map_or(purpose, |pending| merge_reconcile_purpose(pending, purpose));
        match self.try_dispatch_native_update_reconcile(effective, destination) {
            NativeUpdateDispatch::Queued => true,
            NativeUpdateDispatch::Saturated => {
                self.pending_native_update_reconcile_purpose = Some(effective);
                true
            }
            NativeUpdateDispatch::Unavailable => {
                self.pending_native_update_reconcile_purpose = None;
                false
            }
        }
    }

    /// Queue status + installed-bundle probes on the sole process worker. `try_send`
    /// is nonblocking, so a saturated worker can never stall input. Saturation is an
    /// accepted, coalesced request: it remains pending until a later event-loop turn
    /// enters it into the FIFO.
    pub(crate) fn request_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
    ) -> bool {
        if !self.native_updater_service.snapshot().enabled {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        }
        let Some(proxy) = self.proxy.clone() else {
            self.pending_native_update_reconcile_purpose = None;
            return false;
        };
        self.request_native_update_reconcile_with(purpose, move |effective| {
            NativeUpdateFactDestination::Wake {
                purpose: effective,
                proxy,
            }
        })
    }

    fn retry_pending_native_update_reconcile_with(
        &mut self,
        destination: impl FnOnce(NativeUpdateReconcilePurpose) -> NativeUpdateFactDestination,
    ) -> NativeUpdateDispatch {
        // The event loop calls this at every park point. Establish that there is
        // real work BEFORE consulting service state or constructing a destination:
        // on macOS, cloning the destination EventLoopProxy installs a CFRunLoop
        // source. Doing that while idle turns a no-op park into a self-waking hot
        // loop (and, when updates are disabled, one warning per iteration).
        let Some(purpose) = self.pending_native_update_reconcile_purpose.take() else {
            return NativeUpdateDispatch::Queued;
        };
        if !self.native_updater_service.snapshot().enabled {
            return NativeUpdateDispatch::Unavailable;
        }
        let outcome = self.try_dispatch_native_update_reconcile(purpose, destination);
        if outcome == NativeUpdateDispatch::Saturated {
            self.pending_native_update_reconcile_purpose = Some(purpose);
        } else if outcome == NativeUpdateDispatch::Unavailable {
            self.pending_native_update_reconcile_purpose = None;
        }
        outcome
    }

    /// Pure guard for the event-loop retry wrapper. It is intentionally kept
    /// separate from proxy materialization so Tier-1 can prove that an idle park
    /// cannot clone/wake an `EventLoopProxy` or enter the unavailable/log path.
    #[must_use]
    fn has_pending_native_update_reconcile(&self) -> bool {
        self.pending_native_update_reconcile_purpose.is_some()
    }

    /// Injectable wrapper around proxy materialization. Keeping the factory
    /// behind the pending-work guard lets Tier-1 count the exact side effect the
    /// historical macOS hot loop performed, rather than testing only the worker
    /// dispatch below it.
    fn retry_pending_native_update_reconcile_via(
        &mut self,
        materialize_proxy: impl FnOnce(&Self) -> Option<winit::event_loop::EventLoopProxy<crate::Wake>>,
    ) {
        if !self.has_pending_native_update_reconcile() {
            return;
        }
        let Some(proxy) = materialize_proxy(self) else {
            return;
        };
        let outcome = self.retry_pending_native_update_reconcile_with(move |purpose| {
            NativeUpdateFactDestination::Wake { purpose, proxy }
        });
        if outcome == NativeUpdateDispatch::Unavailable {
            aterm_log::warn!("native updater reconcile worker is unavailable; retry stopped");
        }
    }

    /// Retry one coalesced request after the worker has had an opportunity to drain.
    /// No filesystem work and no blocking operation occurs on the event loop.
    pub(crate) fn retry_pending_native_update_reconcile(&mut self) {
        self.retry_pending_native_update_reconcile_via(|app| app.proxy.clone());
    }

    /// Queue boot-sentinel confirmation/GC behind already-requested fact reads.
    /// This is nonblocking and performs no filesystem work on the event loop.
    pub(crate) fn request_native_boot_health_confirmation(&mut self) -> NativeUpdateDispatch {
        let Some(worker) = self.ensure_native_update_reconcile_worker() else {
            return NativeUpdateDispatch::Unavailable;
        };
        let work = NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: self.native_updater_service.snapshot().current_build,
        };
        match worker.try_send(work) {
            Ok(()) => NativeUpdateDispatch::Queued,
            Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
            Err(std::sync::mpsc::TrySendError::Disconnected(work)) => {
                self.native_update_reconcile_worker = None;
                let Some(restarted) = self.ensure_native_update_reconcile_worker() else {
                    return NativeUpdateDispatch::Unavailable;
                };
                match restarted.try_send(work) {
                    Ok(()) => NativeUpdateDispatch::Queued,
                    Err(std::sync::mpsc::TrySendError::Full(_)) => NativeUpdateDispatch::Saturated,
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        self.native_update_reconcile_worker = None;
                        NativeUpdateDispatch::Unavailable
                    }
                }
            }
        }
    }

    pub(crate) fn finish_native_boot_health_confirmation(
        &mut self,
        confirmed: bool,
        now: std::time::Instant,
    ) {
        if confirmed {
            self.boot_health_confirmation_dispatched = true;
            self.boot_health_confirmation_retry_at = None;
        } else {
            self.boot_health_confirmation_dispatched = false;
            self.boot_health_confirmation_retry_at = Some(
                now.checked_add(std::time::Duration::from_secs(1))
                    .unwrap_or(now),
            );
        }
    }

    /// Accept one ordered fact completion, then derive every presentation/action from
    /// the effective reducer stage. The original stage wake's build/version are never
    /// trusted as authority.
    pub(crate) fn finish_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        facts: NativeUpdateReconcileFacts,
    ) {
        // Read BEFORE reducing: `reconcile_native_update_facts` publishes the new
        // stage into `self.relaunch` on its way out, so a comparison made after it
        // always found the notice already naming the stage and `newly_announced`
        // was false for every real import — no "Update ready" toast, no level-up,
        // and the first preflight-block pill suppressed (2026-08-19 audit).
        let announced_before = self.relaunch.as_ref().map(|notice| notice.build);
        // A DEFERRED PURPOSE RIDES THE NEXT NEWER FACTS. A request parked while the
        // reducer was busy (a control `update apply` deferred behind an attempt)
        // used to wait for the idle backstop's replay — and if any newer observation
        // (the failed attempt's own Startup facts) was reduced first, that replay was
        // IgnoredStale and the request vanished with nothing surfaced. Merge it here,
        // into whichever facts are newest, so the purpose is never dropped.
        let (purpose, facts) = match self.deferred_native_update_reconcile.take() {
            Some((deferred_purpose, deferred_facts))
                if deferred_facts.observation_sequence <= facts.observation_sequence =>
            {
                (merge_reconcile_purpose(deferred_purpose, purpose), facts)
            }
            Some(newer) => {
                self.deferred_native_update_reconcile = Some(newer);
                (purpose, facts)
            }
            None => (purpose, facts),
        };
        match self.reconcile_native_update_facts(facts) {
            NativeUpdateFactsResult::IgnoredStale => {
                // The FACTS were stale, the PURPOSE is not: a control apply whose read
                // began before this process imported its stage must be re-observed,
                // not dropped with its "OK apply requested" already sent. Bounded: a
                // facts worker whose sequence restarted below the last reduced one
                // answers stale forever, and one control request must not become a
                // hot loop — after a few tries the request is refused, loudly.
                const MAX_CONTROL_APPLY_STALE_RETRIES: u8 = 3;
                if purpose == NativeUpdateReconcilePurpose::ApplyControl
                    && (self.control_apply_stale_retries >= MAX_CONTROL_APPLY_STALE_RETRIES || {
                        self.control_apply_stale_retries += 1;
                        !self.request_native_update_reconcile(purpose)
                    })
                {
                    self.control_apply_stale_retries = 0;
                    let reason = "Updater facts could not be collected safely";
                    aterm_update::record_apply_refusal(
                        self.native_updater_service.snapshot().current_build,
                        reason,
                    );
                    self.surface_update_apply_outcome(
                        "control request",
                        UpdateOutcome::Blocked {
                            reasons: vec![reason.to_string()],
                        },
                        false,
                    );
                }
            }
            NativeUpdateFactsResult::Deferred(facts) => {
                let facts = *facts;
                self.deferred_native_update_reconcile =
                    Some(match self.deferred_native_update_reconcile.take() {
                        None => (purpose, facts),
                        Some((old_purpose, old_facts)) => {
                            let merged = merge_reconcile_purpose(old_purpose, purpose);
                            let newest =
                                if facts.observation_sequence > old_facts.observation_sequence {
                                    facts
                                } else {
                                    old_facts
                                };
                            (merged, newest)
                        }
                    });
            }
            NativeUpdateFactsResult::Reduced { effective_stage } => {
                if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                    self.control_apply_stale_retries = 0;
                }
                let newly_announced = effective_stage
                    .as_ref()
                    .is_some_and(|stage| announced_before != Some(stage.build));
                self.publish_native_update_state();

                if let Some(stage) = effective_stage {
                    if newly_announced
                        && matches!(
                            purpose,
                            NativeUpdateReconcilePurpose::Startup
                                | NativeUpdateReconcilePurpose::StageAvailable
                        )
                    {
                        // A NEWLY STAGED BUILD IS ANNOUNCED BY THE STATUS BAR
                        // (2026-09-07): "Updating to aterm vX", live for as long
                        // as the automatic lane is armed ("aterm vX is ready ·
                        // click to install" where a press installs it). The floating
                        // "Update ready" card and the stage-time border glow are
                        // retired: the glow is the UPGRADE's, and fires when the
                        // apply actually runs.
                        //
                        // ANNOUNCED FROM HERE, NOT FROM THE DOWNLOADER (2026-09-09).
                        // The updater's own `Progress::Staged` report is gated on
                        // this process having actually DOWNLOADED the artifact
                        // (`take_download_began`), so a build staged by a previous
                        // launch — or by a sibling aterm that won the download race
                        // — reported no progress at all and the row was never
                        // opened. That is the ordinary way a ready build is met: it
                        // is already on disk when the app starts. The reducer knows
                        // the stage is newly announced and knows its posture, so the
                        // row is posted here, where the announcement actually is.
                        // `note_update_progress` is idempotent for a build whose row
                        // is already up: it repaints the same words.
                        aterm_log::info!(
                            "update staged: build {} (v{}) announced on the status bar",
                            stage.build,
                            stage.version
                        );
                        self.note_update_progress(&aterm_update::Progress::Staged {
                            version: stage.version.clone(),
                            build: stage.build,
                        });
                        self.restate_staged_bar_posture(stage.build);
                        self.request_redraw_all_windows();
                    }

                    if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                        let outcome = self.apply_native_update(ApplyMode::Immediate);
                        let landed = matches!(outcome, UpdateOutcome::Accepted);
                        self.surface_update_apply_outcome("control request", outcome, false);
                        // A control apply that did not land (preflight blocked, deferred)
                        // leaves the stage exactly as armed as any other import would:
                        // the automatic lane picks it up instead of waiting for the
                        // background thread's next announcement.
                        if !landed {
                            self.arm_native_auto_apply(stage.build, &stage.dmg_sha256);
                        }
                    } else {
                        self.arm_native_auto_apply(stage.build, &stage.dmg_sha256);
                        self.try_pending_native_auto_apply(newly_announced);
                    }
                } else if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                    self.surface_update_apply_outcome(
                        "control request",
                        UpdateOutcome::Blocked {
                            reasons: vec!["No newer verified update is staged".to_string()],
                        },
                        false,
                    );
                }
            }
        }
    }

    /// Retry already-collected facts after service-owned check/apply work releases the
    /// reducer. No disk is reread and only the newest deferred sequence survives.
    /// Reduce the disk facts a RETURNED apply carried, through the same door every
    /// other reconcile uses, so whatever they import is also ARMED. The five
    /// returned-apply arms used to `let _ = reconcile_native_update_facts(facts)`
    /// and then replay a deferred purpose against facts that were by then stale:
    /// an activation imported after our own child swapped the bundle sat un-armed
    /// ("activates at the next quiet moment" — nothing scheduled), and a control
    /// `update apply` deferred behind the attempt was dropped as IgnoredStale.
    /// A pending deferred purpose rides these newer facts; otherwise this is a
    /// plain refresh (arms, never announces).
    fn reduce_returned_apply_facts(&mut self, facts: NativeUpdateReconcileFacts) {
        // A pending deferred purpose merges in (and the newest observation wins) —
        // `finish_native_update_reconcile` does that for every caller now.
        self.finish_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh, facts);
        self.finish_deferred_native_update_reconcile();
    }

    pub(crate) fn finish_deferred_native_update_reconcile(&mut self) {
        if self.native_updater_service.snapshot().active.is_none()
            && self.native_updater_service.snapshot().phase != UpdaterPhase::Applying
            && let Some((purpose, facts)) = self.deferred_native_update_reconcile.take()
        {
            self.finish_native_update_reconcile(purpose, facts);
        }
    }

    pub(crate) fn native_ui_full_viewport(
        &self,
        wid: WindowId,
    ) -> Result<crate::native_ui::LogicalRect, String> {
        let Some(ws) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let scale = ws.scale.max(f64::EPSILON) as f32;
        Ok(crate::native_ui::LogicalRect::new(
            0.0,
            0.0,
            usize::from(ws.cols)
                .saturating_mul(cw)
                .saturating_add(pad.saturating_mul(2)) as f32
                / scale,
            usize::from(ws.rows).saturating_mul(ch).saturating_add(pad) as f32 / scale,
        ))
    }

    /// Resolve the logical viewport for one exact native presentation, even
    /// when another leaf is focused or its containing tab is inactive. This is
    /// the geometry counterpart of stable view addressing used by semantic
    /// inspection; it must never borrow the focused leaf's rectangle.
    pub(crate) fn native_ui_viewport_for(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
    ) -> Result<crate::native_ui::LogicalRect, String> {
        if !matches!(
            self.view_store.get(view),
            Some(crate::tab_model::View::Native(_))
        ) {
            return Err("view is not a native presentation".to_string());
        }
        let Some(window) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let Some(tab) = window
            .tab_set
            .tabs()
            .iter()
            .find(|tab| tab.root.contains(view))
        else {
            return Err("native view is not in this window".to_string());
        };
        if tab.root.len() <= 1 {
            return self.native_ui_full_viewport(wid);
        }
        let plan = self
            .visible_leaf_plan_for_tab(wid, tab.id)
            .ok_or_else(|| "native view tab has no layout".to_string())?;
        let leaf = plan
            .leaf(view)
            .ok_or_else(|| "native view is hidden by pane zoom".to_string())?;
        // Zoom presents one leaf as the whole native content surface. Preserve
        // the full viewport's padding exactly as the ordinary single-leaf path.
        if plan.leaves.len() <= 1 {
            return self.native_ui_full_viewport(wid);
        }
        let (cw, ch) = self.win_cell_size(wid);
        let scale = window.scale.max(f64::EPSILON) as f32;
        Ok(crate::native_ui::LogicalRect::new(
            0.0,
            0.0,
            leaf.rect.size.width * cw as f32 / scale,
            leaf.rect.size.height * ch as f32 / scale,
        ))
    }

    pub(crate) fn native_ui_viewport(
        &self,
        wid: WindowId,
    ) -> Result<crate::native_ui::LogicalRect, String> {
        if let Some((_, view)) = self.active_native_view(wid) {
            self.native_ui_viewport_for(wid, view)
        } else {
            self.native_ui_full_viewport(wid)
        }
    }

    /// Device-pixel Y where native content begins: effective top pad + OS head band +
    /// in-frame chrome rows (tab strip + status bars). The painter and pointer
    /// projection share this one origin so a native card can neither cover the
    /// chrome nor hit-test one pad off.
    pub(crate) fn native_content_origin_y(&self, wid: WindowId) -> usize {
        let (_, ch) = self.win_cell_size(wid);
        self.win_pad_top(wid)
            .saturating_add(self.win_head(wid))
            .saturating_add(usize::from(self.chrome_rows(wid)).saturating_mul(ch))
    }

    pub(crate) fn native_ui_compile_stamp(
        &self,
        wid: WindowId,
    ) -> Result<NativeUiCompileStamp, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        self.native_ui_compile_stamp_for(wid, instance, view, self.native_ui_viewport(wid)?)
    }

    pub(crate) fn native_ui_compile_stamp_for(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
    ) -> Result<NativeUiCompileStamp, String> {
        use std::hash::{Hash, Hasher};

        let Some(ws) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let generation = self
            .native_runtime
            .view_generation(view)
            .ok_or_else(|| "native view is no longer live".to_string())?;
        let presentation_revision = self
            .native_runtime
            .view_state(view)
            .map_or(0, |state| state.common().presentation_revision);
        let document_seq = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document))
            .map(|snapshot| snapshot.seq.0);
        let (cw, ch) = self.win_cell_size(wid);
        let mut geometry = std::collections::hash_map::DefaultHasher::new();
        ws.cols.hash(&mut geometry);
        ws.rows.hash(&mut geometry);
        cw.hash(&mut geometry);
        ch.hash(&mut geometry);
        self.win_pad(wid).hash(&mut geometry);
        self.win_pad_top(wid).hash(&mut geometry);
        self.win_head(wid).hash(&mut geometry);
        ws.scale.to_bits().hash(&mut geometry);
        viewport.x.to_bits().hash(&mut geometry);
        viewport.y.to_bits().hash(&mut geometry);
        viewport.width.to_bits().hash(&mut geometry);
        viewport.height.to_bits().hash(&mut geometry);
        let mut paint = std::collections::hash_map::DefaultHasher::new();
        self.theme.fg.hash(&mut paint);
        self.theme.bg.hash(&mut paint);
        self.theme.cursor.hash(&mut paint);
        self.theme.selection.hash(&mut paint);
        // The CHROME-resolved palette is a second paint input (Linux config
        // `window_theme` can force it off the terminal theme —
        // `chrome_palette_theme`); hashing the RESOLVED fields means a
        // `window_theme` edit re-lowers retained native pages exactly when it
        // moved their pixels, and costs nothing where the two themes coincide.
        let chrome_theme = self.chrome_palette_theme();
        chrome_theme.fg.hash(&mut paint);
        chrome_theme.bg.hash(&mut paint);
        chrome_theme.cursor.hash(&mut paint);
        chrome_theme.selection.hash(&mut paint);
        self.win_font_px(wid).to_bits().hash(&mut paint);
        self.font_family.hash(&mut paint);
        self.font_config.styled_paths.hash(&mut paint);
        self.font_config.synthetic_style.hash(&mut paint);
        self.font_config.fallback_fonts.hash(&mut paint);
        self.font_config.symbol_font.hash(&mut paint);
        self.font_config.emoji_font.hash(&mut paint);
        for (tag, value) in &self.font_variations {
            tag.hash(&mut paint);
            value.to_bits().hash(&mut paint);
        }
        self.font_weight_dark_nudge.to_bits().hash(&mut paint);
        native_appearance_revision(crate::native_appearance::current_preferences())
            .hash(&mut paint);
        native_motion_revision(self.native_view_motion_cx(wid, view)).hash(&mut paint);
        Ok(NativeUiCompileStamp {
            instance,
            view,
            generation,
            geometry: geometry.finish(),
            config_revision: self.native_config_service.snapshot().revision,
            update_revision: self.native_updater_service.snapshot().revision,
            document_seq,
            presentation_revision,
            paint_revision: paint.finish(),
        })
    }

    /// Return the compiled artifact retained for glass only while every live
    /// input still matches the stamp captured with it.
    pub(crate) fn cached_native_ui(&self, wid: WindowId) -> Option<&NativeCompiledFrame> {
        let stamp = self.native_ui_compile_stamp(wid).ok()?;
        self.windows
            .get(&wid)?
            .native_ui_compiled
            .as_ref()
            .filter(|frame| frame.stamp == stamp)
    }

    pub(crate) fn invalidate_native_ui_cache(&mut self, wid: WindowId) {
        if let Some(window) = self.windows.get_mut(&wid) {
            window.native_ui_compiled = None;
            for cache in window.leaf_render_cache.values_mut() {
                cache.native = None;
                cache.native_damage = Some(DamageRegion::All);
            }
        }
    }

    /// Mark exactly one retained native leaf dirty. Damage is leaf-local and
    /// never invalidates a sibling's scene; repeated regions are conservatively
    /// unioned until the renderer consumes them.
    pub(crate) fn invalidate_native_view_cache(
        &mut self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        damage: DamageRegion,
    ) {
        let Some(window) = self.windows.get_mut(&wid) else {
            return;
        };
        if window
            .native_ui_compiled
            .as_ref()
            .is_some_and(|frame| frame.stamp.view == view)
        {
            window.native_ui_compiled = None;
        }
        let cache = window.leaf_render_cache.entry(view).or_default();
        cache.native_damage = Some(match cache.native_damage {
            Some(existing) => union_native_damage(existing, damage),
            None => damage,
        });
        window.last_present = None;
    }

    /// Compile the active native app's one semantic tree for inspection or hit
    /// testing. Paint uses this exact compiler in `redraw_native_window`.
    pub(crate) fn compiled_native_ui(
        &self,
        wid: WindowId,
    ) -> Result<crate::native_ui::CompiledUi, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let viewport = self.native_ui_viewport(wid)?;
        self.compiled_native_ui_for(wid, instance, view, viewport)
    }

    pub(crate) fn compiled_native_ui_for(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
    ) -> Result<crate::native_ui::CompiledUi, String> {
        let document = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document));
        let animation_phase_ms =
            u64::try_from(self.lat_epoch.elapsed().as_millis()).unwrap_or(u64::MAX);
        let semantic_font = self.prepare_native_semantic_font(wid, view, animation_phase_ms);
        let tree = self
            .native_runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    config_revision: self.native_config_service.snapshot().revision,
                    update_revision: self.native_updater_service.snapshot().revision,
                    animation_phase_ms,
                    motion: self.native_view_motion_cx(wid, view),
                    terminal_font_px: self.win_font_px(wid),
                    terminal_theme: self.theme,
                    semantic_font,
                    document: document.as_ref(),
                },
            )
            .map_err(|error| format!("native render failed: {error:?}"))?;
        let compiled = tree
            .compile(viewport)
            .map_err(|error| format!("native compile failed: {error:?}"))?;
        compiled
            .validate_parity()
            .map_err(|error| format!("native observer parity failed: {error:?}"))?;
        Ok(compiled)
    }

    /// Build one independently cacheable native leaf scene. No active/focused
    /// window assumption enters this seam.
    pub(crate) fn build_native_leaf_scene(
        &self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        viewport: crate::native_ui::LogicalRect,
        damage: DamageRegion,
    ) -> Result<NativeLeafScene, String> {
        Ok(NativeLeafScene {
            stamp: self.native_ui_compile_stamp_for(wid, instance, view, viewport)?,
            instance,
            view,
            viewport,
            damage,
            compiled: self.compiled_native_ui_for(wid, instance, view, viewport)?,
        })
    }

    pub(crate) fn native_view_motion_cx(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
    ) -> crate::native_app::ViewMotionCx {
        let focused_leaf = self
            .active_native_view(wid)
            .is_some_and(|(_, active)| active == view);
        let focused_window = self.windows.get(&wid).is_some_and(|window| window.focused);
        crate::native_app::ViewMotionCx {
            system_reduced: self.system_reduce_motion,
            focused: self.motion_focus(wid, focused_leaf && focused_window),
            performance_reduced: self.perf_reduced,
            system_dark: self.os_appearance == aterm_types::Appearance::Dark,
            serious: self.serious_mode_enabled(),
            backend_gpu: self.gpu_capable(),
        }
    }

    /// Resolve one exact retained native artifact. Paint, pointer routing,
    /// accessibility and inspection consume this same semantic tree and device
    /// destination. Any lifecycle, geometry, service, theme, or document drift
    /// changes the compile stamp and therefore fails closed.
    #[cfg(test)]
    pub(crate) fn retained_native_leaf_artifact(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        require_presented: bool,
    ) -> Option<RetainedNativeLeafArtifact<'_>> {
        let plan = self.active_visible_leaf_plan(wid)?;
        self.retained_native_leaf_artifact_from_plan(wid, view, require_presented, &plan)
    }

    pub(crate) fn retained_native_leaf_artifact_from_plan(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        require_presented: bool,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Option<RetainedNativeLeafArtifact<'_>> {
        let window = self.windows.get(&wid)?;
        if window.overlay.is_some() {
            return None;
        }
        let leaf = plan.leaf(view)?;
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let generation = self.native_runtime.view_generation(view)?;
        let (cw, ch) = self.win_cell_size(wid);
        let scale = window.scale.max(f64::EPSILON);
        let viewport = if plan.leaves.len() == 1 {
            self.native_ui_full_viewport(wid).ok()?
        } else {
            crate::native_ui::LogicalRect::new(
                0.0,
                0.0,
                leaf.rect.size.width * cw as f32 / scale as f32,
                leaf.rect.size.height * ch as f32 / scale as f32,
            )
        };
        let expected_stamp = self
            .native_ui_compile_stamp_for(wid, native.instance, view, viewport)
            .ok()?;
        let raster = window.leaf_render_cache.get(&view)?.native.as_ref()?;
        if raster.stamp != expected_stamp
            || raster.stamp.generation != generation
            || require_presented && !raster.presented
        {
            return None;
        }
        let card = window.settings_card.as_ref()?;
        if raster.presented_x.checked_add(raster.width)? > card.pw
            || raster.presented_y.checked_add(raster.height)? > card.ph
            || usize::try_from(card.pw)
                .ok()?
                .checked_mul(usize::try_from(card.ph).ok()?)?
                .checked_mul(4)?
                != card.rgba.len()
        {
            return None;
        }
        let (frame_x, frame_y) = self.frame_origin(wid);
        let device_x = frame_x
            .checked_add(i64::from(card.dx))?
            .checked_add(i64::from(raster.presented_x))?;
        let device_y = frame_y
            .checked_add(i64::from(card.dy))?
            .checked_add(i64::from(raster.presented_y))?;
        if let Some(size) = window.win_px {
            let right = device_x.checked_add(i64::from(raster.width))?;
            let bottom = device_y.checked_add(i64::from(raster.height))?;
            // Partial intersection is the normal centred-crop case. Reject only
            // a truly offscreen retained destination (or a torn empty extent).
            if raster.width == 0
                || raster.height == 0
                || right <= 0
                || bottom <= 0
                || device_x >= i64::from(size.width)
                || device_y >= i64::from(size.height)
            {
                return None;
            }
        }
        Some(RetainedNativeLeafArtifact {
            instance: native.instance,
            view,
            generation,
            compiled: &raster.compiled,
            device_x,
            device_y,
            width: raster.width,
            height: raster.height,
            scale,
        })
    }

    /// Resolve the retained leaf containing a raw window-space pointer and map
    /// it into that leaf's canonical logical coordinates.
    pub(crate) fn retained_native_leaf_at_pointer(
        &self,
        wid: WindowId,
        x: f64,
        y: f64,
    ) -> Option<(RetainedNativeLeafArtifact<'_>, f32, f32)> {
        if !x.is_finite() || !y.is_finite() {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        for leaf in &plan.leaves {
            let Some(artifact) =
                self.retained_native_leaf_artifact_from_plan(wid, leaf.view, true, &plan)
            else {
                continue;
            };
            let local_x = x - artifact.device_x as f64;
            let local_y = y - artifact.device_y as f64;
            if local_x >= 0.0
                && local_y >= 0.0
                && local_x < f64::from(artifact.width)
                && local_y < f64::from(artifact.height)
            {
                let logical_x = (local_x / artifact.scale) as f32;
                let logical_y = (local_y / artifact.scale) as f32;
                return Some((artifact, logical_x, logical_y));
            }
        }
        None
    }

    /// Convert a window-space pointer into the focused native app's retained
    /// content coordinates.
    #[cfg(test)]
    pub(crate) fn native_content_point(&self, wid: WindowId, x: f64, y: f64) -> Option<(f32, f32)> {
        let (_, focused) = self.active_native_view(wid)?;
        let (artifact, x, y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
        (artifact.view == focused).then_some((x, y))
    }

    /// Dispatch through the active app reducer, execute every typed effect, then
    /// refresh canonical tab chrome. This is shared by human pointer/keyboard
    /// input and the control `act` path.
    pub(crate) fn reconcile_active_editor_viewport(
        &mut self,
        wid: WindowId,
    ) -> Result<bool, String> {
        let Some((instance, view)) = self.active_native_view(wid) else {
            return Ok(false);
        };
        if !self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Editor)
        {
            return Ok(false);
        }
        let palette_candidates = self
            .native_runtime
            .view_state(view)
            .and_then(|state| match state {
                crate::native_app::AppViewState::Editor(state) => state.buffer.as_ref(),
                _ => None,
            })
            .and_then(|buffer| match &buffer.minibuffer {
                crate::native_editor::Minibuffer::Command { query, .. } => {
                    Some(crate::native_editor::command_completions(query).len())
                }
                _ => None,
            })
            .unwrap_or(0);
        let visible_lines = crate::native_ui::editor_visible_line_capacity_with_palette(
            self.native_ui_viewport(wid)?,
            palette_candidates,
        );
        if matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Editor(state))
                if state.buffer.as_ref().is_some_and(|buffer| {
                    buffer.viewport_lines() == visible_lines
                })
        ) {
            return Ok(false);
        }
        let Some(_) = self.dispatch_editor_event(
            instance,
            view,
            &AppEvent::EditorViewportChanged { visible_lines },
        )?
        else {
            return Ok(false);
        };
        self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        Ok(true)
    }

    pub(crate) fn dispatch_native_event(
        &mut self,
        wid: WindowId,
        event: AppEvent,
    ) -> Result<EventResult, String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let kind = self
            .native_runtime
            .app(instance)
            .map(crate::native_app::NativeApp::kind)
            .ok_or_else(|| "active native app disappeared".to_string())?;
        if kind == crate::native_app::AppKind::Editor
            && !matches!(event, AppEvent::EditorViewportChanged { .. })
        {
            let _ = self.reconcile_active_editor_viewport(wid)?;
        }
        if matches!(
            kind,
            crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor
        ) && let AppEvent::Action(invocation) = &event
            && crate::command_registry::native_document_action(invocation.id.as_str()).is_none()
        {
            return Err(format!(
                "unregistered native document command: {}",
                invocation.id.as_str()
            ));
        }
        if let AppEvent::EditorCommand(command) = &event {
            let _ = crate::command_registry::editor_command(command);
        }
        // Palette/buttons and the Emacs keymap lower into the same typed editor
        // reducer. The command registry above classifies authority before this
        // adapter is allowed to interpret an ActionId.
        let event = if kind == crate::native_app::AppKind::Markdown {
            match event {
                AppEvent::ScrollLines(lines) => {
                    let viewport = self.native_ui_viewport(wid)?;
                    AppEvent::MarkdownScroll {
                        lines,
                        viewport_width: viewport.width,
                        viewport_height: viewport.height,
                    }
                }
                AppEvent::MarkdownPage {
                    direction,
                    viewport_width: _,
                    viewport_height: _,
                } => {
                    let viewport = self.native_ui_viewport(wid)?;
                    AppEvent::MarkdownPage {
                        direction,
                        viewport_width: viewport.width,
                        viewport_height: viewport.height,
                    }
                }
                event => event,
            }
        } else if kind == crate::native_app::AppKind::Editor {
            match &event {
                AppEvent::Action(invocation) => match invocation.id.as_str() {
                    "editor/save" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Save)
                    }
                    "editor/undo" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Undo)
                    }
                    "editor/redo" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::Redo)
                    }
                    "editor/find" => AppEvent::EditorCommand(
                        crate::native_editor::EditorCommand::IncrementalSearch,
                    ),
                    "editor/goto-line" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::GotoLine)
                    }
                    "editor/commands" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::ExecuteCommand)
                    }
                    "editor/revert" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::RevertBuffer)
                    }
                    action if action.starts_with("editor/completion/") => invocation
                        .id
                        .as_str()
                        .strip_prefix("editor/completion/")
                        .and_then(|index| index.parse::<usize>().ok())
                        .map(|index| {
                            AppEvent::EditorCompletion(
                                crate::native_editor::EditorCompletionAction::Choose(index),
                            )
                        })
                        .unwrap_or_else(|| event.clone()),
                    action if action.starts_with("editor/config-page/") => action
                        .strip_prefix("editor/config-page/")
                        .and_then(|suffix| suffix.split_once('/'))
                        .and_then(|(target, candidates)| {
                            Some((
                                target.parse::<usize>().ok()?,
                                candidates.parse::<usize>().ok()?,
                            ))
                        })
                        .and_then(|(target, candidates)| {
                            self.editor_config_completion_context(instance, view)
                                .map(|context| AppEvent::EditorConfigNavigate {
                                    navigation: crate::native_app::ConfigCompletionNavigation::Page(
                                        target,
                                    ),
                                    candidates,
                                    context,
                                })
                        })
                        .unwrap_or_else(|| event.clone()),
                    "editor/config-problem-next" => {
                        AppEvent::EditorConfigDiagnosticNavigate { previous: false }
                    }
                    "editor/config-problem-previous" => {
                        AppEvent::EditorConfigDiagnosticNavigate { previous: true }
                    }
                    action
                        if action.starts_with(
                            crate::native_config_language::CONFIG_COMPLETION_ACTION_PREFIX,
                        ) =>
                    {
                        self.editor_config_completion(instance, view, action)
                            .map_or(
                                AppEvent::EditorConfigCompletionRejected,
                                AppEvent::EditorConfigCompletion,
                            )
                    }
                    _ => event,
                },
                _ => event,
            }
        } else {
            event
        };
        if let Some(result) = self.dispatch_editor_event(instance, view, &event)? {
            // The editor workspace is an independent reducer and currently has
            // no regional-damage effect lane. Invalidate its exact leaf only.
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            self.refresh_native_presentation(wid, instance, view);
            return Ok(result);
        }
        let outcome = self
            .native_runtime
            .dispatch(instance, view, event)
            .map_err(|error| format!("native dispatch failed: {error:?}"))?;
        // A reducer that mutates without asking for repaint still fails safe,
        // but only its own leaf is widened to full damage.
        if !outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, AppEffect::RepaintSelf(_)))
        {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        }
        for effect in outcome.effects {
            self.execute_native_effect(wid, instance, view, effect)?;
        }
        self.refresh_native_presentation(wid, instance, view);
        Ok(outcome.result)
    }

    /// Dispatch to one exact native view, refusing a stale control target or a
    /// view that is no longer active in the requested window. Human input uses
    /// [`Self::dispatch_native_event`]; semantic control actions use this form so
    /// an intervening tab switch can never redirect an action to the wrong app.
    pub(crate) fn dispatch_native_view_event(
        &mut self,
        wid: WindowId,
        expected_view: crate::tab_model::ViewId,
        event: AppEvent,
    ) -> Result<EventResult, String> {
        let (_, active_view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        if active_view != expected_view {
            return Err("native app target is stale or no longer active".to_string());
        }
        self.dispatch_native_event(wid, event)
    }

    /// Native tabs are an input boundary: keyboard/text events are reduced by
    /// the app and never fall through to a hidden PTY. Geometry and focus are
    /// deliberately returned to the host seam because they remain window
    /// properties even while a native tab is active.
    pub(crate) fn native_input_event(
        &mut self,
        wid: WindowId,
        event: &crate::input::InputEvent,
    ) -> bool {
        use crate::input::{InputEvent, ScrollIntent};
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        let Some((instance, active_view)) = self.active_native_view(wid) else {
            return false;
        };
        // THE KEYPAD IS ITS MAIN-BLOCK TWIN ON A NATIVE PAGE. The seam hands
        // this reducer the same engine key it hands the PTY encoder, and since
        // `keymap::build_key_input` began keeping the keypad identity (so
        // DECKPAM and kitty disambiguate can tell KP_5 from 5) that key is
        // `Numpad5`, `NumpadEnter`, `NumpadEnd` — which the lowering below
        // matched as nothing: KP_5 into a focused Settings field typed
        // nothing, a NumLock-off KP_1 in the editor moved nowhere. No page
        // needs the keypad identity, so a keypad digit COMMITS its glyph and
        // the keypad's Enter/arrows/Home/End/Page/Insert/Delete drive the
        // main-block arms. `InputEvent::keypad_folded` is the one fold, shared
        // with the seam's press classifier; a controller's `key kpenter` takes
        // the same road. (KP_Begin has no twin and lowers to nothing, as it
        // always did.)
        let folded = event.keypad_folded();
        let event = folded.as_ref().unwrap_or(event);
        let editor_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Editor);
        let editor_chord_pending = editor_active
            && matches!(
                self.native_runtime.view_state(active_view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state
                        .buffer
                        .as_ref()
                        .is_some_and(crate::native_editor::EditorBufferView::chord_pending)
            );
        let editor_command_palette_active = editor_active
            && matches!(
                self.native_runtime.view_state(active_view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state.buffer.as_ref().is_some_and(|buffer| {
                        matches!(
                            buffer.minibuffer,
                            crate::native_editor::Minibuffer::Command { .. }
                        )
                    })
            );
        let editor_minibuffer_active = editor_active
            && matches!(
                self.native_runtime.view_state(active_view),
                Some(crate::native_app::AppViewState::Editor(state))
                    if state
                        .buffer
                        .as_ref()
                        .is_some_and(crate::native_editor::EditorBufferView::minibuffer_active)
            );
        let (
            editor_config_completion_count,
            editor_config_completion_selected,
            editor_config_completion_context,
            editor_config_completion_interacting,
            editor_config_assist_present,
            editor_config_assist_dismissed,
        ) = if editor_active && !editor_minibuffer_active && !editor_chord_pending {
            self.editor_config_assist(instance, active_view)
                .and_then(|(context, assist)| {
                    let crate::native_app::AppViewState::Editor(state) =
                        self.native_runtime.view_state(active_view)?
                    else {
                        return None;
                    };
                    let dismissed = state.config_completion_dismissed == Some(context);
                    let interacting = state.config_completion_interaction == Some(context);
                    let selected = if interacting {
                        state.config_completion_selected
                    } else {
                        0
                    };
                    let count = assist.completions.len();
                    let present = assist.help.is_some() || count > 0;
                    Some((
                        count,
                        selected,
                        Some(context),
                        interacting,
                        present,
                        dismissed,
                    ))
                })
                .unwrap_or((0, 0, None, false, false, false))
        } else {
            (0, 0, None, false, false, false)
        };
        let editor_config_completion_active =
            !editor_config_assist_dismissed && editor_config_completion_count > 0;
        let editor_config_assist_visible =
            !editor_config_assist_dismissed && editor_config_assist_present;
        let settings_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings);
        let markdown_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Markdown);

        if editor_config_assist_present
            && (!editor_config_completion_interacting || editor_config_assist_dismissed)
            && !editor_chord_pending
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Space),
                mods,
                event_type: KeyEventType::Press,
                ..
            } = event
            && mods.contains(Modifiers::CTRL)
            && !mods.intersects(
                Modifiers::ALT
                    | Modifiers::SUPER
                    | Modifiers::HYPER
                    | Modifiers::META
                    | Modifiers::SHIFT,
            )
            && let Some(context) = editor_config_completion_context
        {
            let _ = self.dispatch_native_event(
                wid,
                AppEvent::EditorConfigNavigate {
                    navigation: crate::native_app::ConfigCompletionNavigation::Page(
                        editor_config_completion_selected,
                    ),
                    candidates: editor_config_completion_count,
                    context,
                },
            );
            return true;
        }

        if !editor_chord_pending
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Tab),
                mods,
                event_type: KeyEventType::Press,
                ..
            } = event
        {
            if editor_command_palette_active {
                let _ = self.dispatch_native_event(
                    wid,
                    AppEvent::EditorCompletion(
                        crate::native_editor::EditorCompletionAction::Complete,
                    ),
                );
                return true;
            }
            if editor_config_completion_active && !mods.contains(Modifiers::SHIFT) {
                let Some(context) = editor_config_completion_context else {
                    return true;
                };
                let _ = self.dispatch_native_event(
                    wid,
                    AppEvent::EditorConfigNavigate {
                        navigation: crate::native_app::ConfigCompletionNavigation::Page(
                            editor_config_completion_selected,
                        ),
                        candidates: editor_config_completion_count,
                        context,
                    },
                );
                if editor_config_completion_interacting
                    && self.activate_native_focus(wid).unwrap_or(false)
                {
                    return true;
                }
                // First Tab explicitly enters the completion list without
                // mutating the document. Arrows can now choose any candidate;
                // Enter or a second Tab accepts it.
                return true;
            }
            let _ = self.move_native_focus(wid, mods.contains(Modifiers::SHIFT));
            return true;
        }
        let editor_config_completion_focused = editor_config_completion_interacting
            && editor_active
            && self
                .native_runtime
                .view_state(active_view)
                .and_then(|state| state.common().last_focus.as_ref())
                .is_some_and(|key| key.as_str().starts_with("editor/config-completion/"));
        if editor_config_assist_visible
            && !editor_chord_pending
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Escape),
                event_type: KeyEventType::Press,
                ..
            } = event
            && let Some(context) = editor_config_completion_context
        {
            let _ = self.dispatch_native_event(wid, AppEvent::EditorConfigDismiss { context });
            return true;
        }
        if editor_config_completion_active
            && editor_config_completion_focused
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Enter),
                event_type: KeyEventType::Press,
                ..
            } = event
        {
            let Some(context) = editor_config_completion_context else {
                return true;
            };
            let _ = self.dispatch_native_event(
                wid,
                AppEvent::EditorConfigNavigate {
                    navigation: crate::native_app::ConfigCompletionNavigation::Page(
                        editor_config_completion_selected,
                    ),
                    candidates: editor_config_completion_count,
                    context,
                },
            );
            if self.activate_native_focus(wid).unwrap_or(false) {
                return true;
            }
        }
        if editor_config_completion_focused
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Space),
                event_type: KeyEventType::Press,
                ..
            } = event
            && self.activate_native_focus(wid).unwrap_or(false)
        {
            return true;
        }
        let editor_config_diagnostic_count =
            self.native_runtime.config_editor_analysis(instance).map_or(
                0,
                crate::native_config_language::ConfigAnalysis::diagnostic_count,
            );
        if editor_active
            && editor_config_diagnostic_count > 0
            && let InputEvent::Key {
                key: Key::Named(NamedKey::F8),
                mods,
                event_type: KeyEventType::Press,
                ..
            } = event
        {
            let _ = self.dispatch_native_event(
                wid,
                AppEvent::EditorConfigDiagnosticNavigate {
                    previous: mods.contains(Modifiers::SHIFT),
                },
            );
            return true;
        }
        if !editor_active
            && !self.native_text_field_has_focus(wid)
            && let InputEvent::Key {
                key: Key::Named(key @ (NamedKey::Enter | NamedKey::Space)),
                event_type: KeyEventType::Press,
                ..
            } = event
        {
            if self.activate_native_focus(wid).unwrap_or(false) {
                return true;
            }
            // Nothing holds keyboard focus: Return falls back to the page's
            // DEFAULT button (the highlighted Primary — "Update to Latest Now",
            // "Copy Build Information"), the native default-button convention.
            // Space never does; on macOS it only activates the focused control.
            // KP_Enter — the physical key, which `keymap::build_key_input`
            // delivers as `NumpadEnter`, or a controller's `key kpenter` — was
            // folded onto Enter at the top of this function, so it activates
            // the default button as Return does.
            if matches!(key, NamedKey::Enter) && self.activate_native_default(wid).unwrap_or(false)
            {
                return true;
            }
        }

        let app_event = match event {
            // Geometry and focus are window-level facts, not text input: a native
            // view never consumes them, so they fall through to the ordinary path.
            // `ResizeWindowPx` belongs here for the same reason and doubly so — it
            // carries no engine state at all, only a request to the OS window.
            InputEvent::Resize { .. }
            | InputEvent::ResizeWindowPx { .. }
            | InputEvent::Focus(_) => return false,
            InputEvent::Text(text) | InputEvent::Paste(text, _) => {
                Some(AppEvent::TextInput(TextInputEvent::Commit(text.clone())))
            }
            InputEvent::Key {
                event_type: KeyEventType::Release,
                ..
            } => None,
            InputEvent::Key {
                key,
                mods,
                event_type: KeyEventType::Press | KeyEventType::Repeat,
                ..
            } => {
                let command = mods.intersects(Modifiers::SUPER | Modifiers::CTRL);
                let extend = mods.contains(Modifiers::SHIFT);
                if settings_active
                    && mods.contains(Modifiers::SUPER)
                    && matches!(key, Key::Character('f' | 'F'))
                {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("settings/search"),
                        value: None,
                    }))
                } else if editor_active
                    && mods.contains(Modifiers::SUPER)
                    && matches!(key, Key::Character('s' | 'S'))
                {
                    Some(AppEvent::EditorCommand(
                        crate::native_editor::EditorCommand::Save,
                    ))
                } else if markdown_active && command && matches!(key, Key::Character('[')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/back"),
                        value: None,
                    }))
                } else if markdown_active && command && matches!(key, Key::Character(']')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/forward"),
                        value: None,
                    }))
                } else if markdown_active && command && matches!(key, Key::Character('e' | 'E')) {
                    Some(AppEvent::Action(crate::native_app::ActionInvocation {
                        id: crate::native_ui::ActionId::new("markdown/edit"),
                        value: None,
                    }))
                } else if editor_active
                    && (editor_chord_pending
                        || mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META))
                {
                    let key = match key {
                        Key::Character(character) => {
                            Some(character.to_ascii_lowercase().to_string())
                        }
                        Key::Named(NamedKey::Space) => Some("space".to_string()),
                        Key::Named(NamedKey::Backspace) => Some("backspace".to_string()),
                        Key::Named(NamedKey::Delete) => Some("delete".to_string()),
                        Key::Named(NamedKey::Enter) => Some("enter".to_string()),
                        Key::Named(NamedKey::Escape) => Some("escape".to_string()),
                        Key::Named(NamedKey::Tab) => Some("tab".to_string()),
                        // WORD MOTION on the arrows (2026-07-24). These fell to
                        // `_ => None` and the event was DROPPED, so in the
                        // native editor ⌥← / ⌥→ were dead keys while `M-b` /
                        // `M-f` worked — the same command, reachable only by
                        // the emacs spelling. Plain arrows are unaffected: this
                        // arm is only entered when a modifier is held.
                        Key::Named(NamedKey::ArrowLeft) => Some("left".to_string()),
                        Key::Named(NamedKey::ArrowRight) => Some("right".to_string()),
                        _ => None,
                    };
                    key.map(|key| {
                        AppEvent::EditorChord(crate::native_editor::KeyChord {
                            control: mods.contains(Modifiers::CTRL),
                            meta: mods.intersects(Modifiers::ALT | Modifiers::META),
                            shift: mods.contains(Modifiers::SHIFT),
                            key,
                        })
                    })
                } else {
                    // READLINE caret bindings for the Settings text fields (the
                    // search + free-form value fields): Ctrl-A/E home/end, Ctrl-B/F
                    // left/right, Ctrl-D delete forward, Ctrl-K/U kill to end/start,
                    // Ctrl-W word back — the macOS system Emacs set. CTRL-only
                    // (never ⌘/⌥), so ⌘A Select-All and the chords above are
                    // untouched, and matched BEFORE the `command` arms because
                    // `command` deliberately folds CTRL in. Scoped to Settings so
                    // other native apps keep their existing Ctrl shortcuts.
                    let readline = settings_active
                        && mods.contains(Modifiers::CTRL)
                        && !mods.intersects(
                            Modifiers::SUPER | Modifiers::ALT | Modifiers::HYPER | Modifiers::META,
                        );
                    // The ⌥ twin of `readline`, for word motion only.
                    let alt_word = settings_active
                        && mods.intersects(Modifiers::ALT | Modifiers::META)
                        && !mods.intersects(Modifiers::CTRL | Modifiers::SUPER | Modifiers::HYPER);
                    // Off macOS, CTRL is the platform word modifier — Ctrl+←/→
                    // walks words in every Windows/GTK text box — so it joins the
                    // ⌥ twin for the NAMED-key arms below (its predicate is
                    // exactly `readline`). The CTRL+letter emacs set is
                    // untouched: those Character arms match FIRST, so a Ctrl
                    // chord can only reach an `alt_word` arm on an arrow (or the
                    // ⌫ arm gated beside them). macOS keeps ⌥ alone — Ctrl+arrow
                    // is not the word idiom there, and this twin must stay
                    // byte-identical with `app_input::field_edit_action`'s
                    // cfg'd `by_word` so the find/rename fields and these
                    // Settings fields cannot disagree about one chord.
                    #[cfg(not(target_os = "macos"))]
                    let alt_word = alt_word || readline;
                    match key {
                        Key::Character('a' | 'A') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Home { extend }))
                        }
                        Key::Character('e' | 'E') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::End { extend }))
                        }
                        Key::Character('b' | 'B') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Left { extend }))
                        }
                        Key::Character('f' | 'F') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Right { extend }))
                        }
                        Key::Character('d' | 'D') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::Delete))
                        }
                        Key::Character('k' | 'K') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::KillToEnd))
                        }
                        Key::Character('u' | 'U') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::KillToStart))
                        }
                        Key::Character('w' | 'W') if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::DeleteWordBackward))
                        }
                        // WORD MOTION in Settings fields (2026-07-24 audit:
                        // they had none at all). ⌥←/⌥→ is what a Mac user
                        // reaches for; ⌥B/⌥F is the emacs spelling of the same
                        // pair, matching the terminal's own ESC-b/ESC-f. ⌥-only
                        // so ⌘← (line start) and the CTRL readline set above
                        // are untouched.
                        Key::Named(NamedKey::ArrowLeft) if alt_word => {
                            Some(AppEvent::TextInput(TextInputEvent::WordLeft { extend }))
                        }
                        Key::Named(NamedKey::ArrowRight) if alt_word => {
                            Some(AppEvent::TextInput(TextInputEvent::WordRight { extend }))
                        }
                        Key::Character('b' | 'B') if alt_word => {
                            Some(AppEvent::TextInput(TextInputEvent::WordLeft { extend }))
                        }
                        Key::Character('f' | 'F') if alt_word => {
                            Some(AppEvent::TextInput(TextInputEvent::WordRight { extend }))
                        }
                        // Ctrl+⌫ = backward-kill-word, the other half of the
                        // Windows word-motion reflex (⌥⌫ keeps today's meaning
                        // everywhere — this arm exists only off macOS and only
                        // for CTRL, ahead of the plain ⌫ arm below). Ctrl+⌦ has
                        // no event to ride (`TextInputEvent` has no
                        // DeleteWordForward), so forward word delete stays out
                        // of scope rather than growing the native-input surface.
                        #[cfg(not(target_os = "macos"))]
                        Key::Named(NamedKey::Backspace) if readline => {
                            Some(AppEvent::TextInput(TextInputEvent::DeleteWordBackward))
                        }
                        Key::Character('a' | 'A') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::SelectAll))
                        }
                        Key::Character('z' | 'Z') if command && extend => {
                            Some(AppEvent::TextInput(TextInputEvent::Redo))
                        }
                        Key::Character('z' | 'Z') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::Undo))
                        }
                        Key::Character('y' | 'Y') if command => {
                            Some(AppEvent::TextInput(TextInputEvent::Redo))
                        }
                        Key::Character(character)
                            if !mods.intersects(
                                Modifiers::CTRL
                                    | Modifiers::ALT
                                    | Modifiers::SUPER
                                    | Modifiers::HYPER
                                    | Modifiers::META,
                            ) =>
                        {
                            Some(AppEvent::TextInput(TextInputEvent::Commit(
                                character.to_string(),
                            )))
                        }
                        Key::Named(NamedKey::Space)
                            if !mods.intersects(
                                Modifiers::CTRL
                                    | Modifiers::ALT
                                    | Modifiers::SUPER
                                    | Modifiers::HYPER
                                    | Modifiers::META,
                            ) =>
                        {
                            Some(AppEvent::TextInput(TextInputEvent::Commit(" ".to_string())))
                        }
                        Key::Named(NamedKey::Backspace) => {
                            Some(AppEvent::TextInput(TextInputEvent::Backspace))
                        }
                        Key::Named(NamedKey::Delete) => {
                            Some(AppEvent::TextInput(TextInputEvent::Delete))
                        }
                        Key::Named(NamedKey::ArrowLeft) => {
                            Some(AppEvent::TextInput(TextInputEvent::Left { extend }))
                        }
                        Key::Named(NamedKey::ArrowRight) => {
                            Some(AppEvent::TextInput(TextInputEvent::Right { extend }))
                        }
                        Key::Named(NamedKey::ArrowUp) if editor_active => {
                            if editor_command_palette_active {
                                Some(AppEvent::EditorCompletion(
                                    crate::native_editor::EditorCompletionAction::Previous,
                                ))
                            } else if editor_config_completion_active
                                && editor_config_completion_focused
                            {
                                editor_config_completion_context.map(|context| {
                                    AppEvent::EditorConfigNavigate {
                                        navigation:
                                            crate::native_app::ConfigCompletionNavigation::Previous,
                                        candidates: editor_config_completion_count,
                                        context,
                                    }
                                })
                            } else {
                                Some(AppEvent::EditorCommand(
                                    crate::native_editor::EditorCommand::MoveLineUp,
                                ))
                            }
                        }
                        Key::Named(NamedKey::ArrowDown) if editor_active => {
                            if editor_command_palette_active {
                                Some(AppEvent::EditorCompletion(
                                    crate::native_editor::EditorCompletionAction::Next,
                                ))
                            } else if editor_config_completion_active
                                && editor_config_completion_focused
                            {
                                editor_config_completion_context.map(|context| {
                                    AppEvent::EditorConfigNavigate {
                                        navigation:
                                            crate::native_app::ConfigCompletionNavigation::Next,
                                        candidates: editor_config_completion_count,
                                        context,
                                    }
                                })
                            } else {
                                Some(AppEvent::EditorCommand(
                                    crate::native_editor::EditorCommand::MoveLineDown,
                                ))
                            }
                        }
                        Key::Named(NamedKey::Home) if editor_active => {
                            Some(AppEvent::EditorCommand(
                                crate::native_editor::EditorCommand::MoveLineStart,
                            ))
                        }
                        Key::Named(NamedKey::End) if editor_active => {
                            Some(AppEvent::EditorCommand(
                                crate::native_editor::EditorCommand::MoveLineEnd,
                            ))
                        }
                        Key::Named(NamedKey::Enter) => {
                            Some(AppEvent::TextInput(TextInputEvent::Submit))
                        }
                        Key::Named(NamedKey::Escape) => {
                            Some(AppEvent::TextInput(TextInputEvent::Cancel))
                        }
                        Key::Named(NamedKey::ArrowUp) => Some(AppEvent::ScrollLines(-1)),
                        Key::Named(NamedKey::ArrowDown) => Some(AppEvent::ScrollLines(1)),
                        Key::Named(NamedKey::PageUp) if markdown_active => {
                            Some(AppEvent::MarkdownPage {
                                direction: -1,
                                viewport_width: 0.0,
                                viewport_height: 0.0,
                            })
                        }
                        Key::Named(NamedKey::PageDown) if markdown_active => {
                            Some(AppEvent::MarkdownPage {
                                direction: 1,
                                viewport_width: 0.0,
                                viewport_height: 0.0,
                            })
                        }
                        Key::Named(NamedKey::PageUp) => Some(AppEvent::ScrollLines(-8)),
                        Key::Named(NamedKey::PageDown) => Some(AppEvent::ScrollLines(8)),
                        Key::Named(NamedKey::Home) if markdown_active || command => {
                            Some(AppEvent::ScrollLines(-10_000))
                        }
                        Key::Named(NamedKey::End) if markdown_active || command => {
                            Some(AppEvent::ScrollLines(10_000))
                        }
                        _ => None,
                    }
                }
            }
            InputEvent::Key { .. } => None,
            // A native view scrolls ONE vertical list, so a horizontal wheel
            // (audit I7) maps to no event at all rather than being folded into
            // the vertical delta — `vertical_up()` returning `None` IS the
            // "nothing to do here" answer.
            InputEvent::Wheel { dir, lines, .. } => dir.vertical_up().map(|up| {
                AppEvent::ScrollLines(if up {
                    -(*lines).max(1)
                } else {
                    (*lines).max(1)
                })
            }),
            InputEvent::ScrollView(ScrollIntent::Up | ScrollIntent::PrevPrompt)
                if markdown_active =>
            {
                Some(AppEvent::MarkdownPage {
                    direction: -1,
                    viewport_width: 0.0,
                    viewport_height: 0.0,
                })
            }
            InputEvent::ScrollView(ScrollIntent::Down | ScrollIntent::NextPrompt)
                if markdown_active =>
            {
                Some(AppEvent::MarkdownPage {
                    direction: 1,
                    viewport_width: 0.0,
                    viewport_height: 0.0,
                })
            }
            InputEvent::ScrollView(intent) => Some(AppEvent::ScrollLines(match intent {
                ScrollIntent::By(lines) => -*lines,
                ScrollIntent::Up | ScrollIntent::PrevPrompt => -8,
                ScrollIntent::Down | ScrollIntent::NextPrompt => 8,
                ScrollIntent::Top => -10_000,
                ScrollIntent::Bottom => 10_000,
            })),
            InputEvent::KeySequence(_)
            | InputEvent::MouseButton { .. }
            | InputEvent::MouseMove { .. } => None,
        };

        if let Some(event) = app_event {
            let _ = self.dispatch_native_event(wid, event);
        }
        true
    }

    fn move_native_focus(&mut self, wid: WindowId, backwards: bool) -> Result<(), String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let compiled = self.compiled_native_ui(wid)?;
        if compiled.focus_order.is_empty() {
            return Ok(());
        }
        let current = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref());
        let next = current
            .and_then(|key| {
                compiled
                    .focus_order
                    .iter()
                    .position(|candidate| candidate == key)
            })
            .map_or_else(
                || {
                    if backwards {
                        compiled.focus_order.len() - 1
                    } else {
                        0
                    }
                },
                |index| {
                    if backwards {
                        index
                            .checked_sub(1)
                            .unwrap_or(compiled.focus_order.len() - 1)
                    } else {
                        (index + 1) % compiled.focus_order.len()
                    }
                },
            );
        let key = compiled.focus_order[next].clone();
        self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        let outcome = self
            .native_runtime
            .dispatch(instance, view, AppEvent::FocusChanged(Some(key)))
            .map_err(|error| format!("native focus failed: {error:?}"))?;
        for effect in outcome.effects {
            self.execute_native_effect(wid, instance, view, effect)?;
        }
        if let Some(state) = self.native_runtime.view_state_mut(view) {
            state.common_mut().focus_visible = true;
        }
        self.request_redraw_all_windows();
        Ok(())
    }

    fn native_text_field_has_focus(&self, wid: WindowId) -> bool {
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        let Some(key) = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.as_ref())
        else {
            return false;
        };
        self.compiled_native_ui(wid).ok().and_then(|compiled| {
            compiled.semantic(key).map(|semantic| {
                semantic.role == crate::native_ui::SemanticRole::TextField
                    && semantic.state.is_none_or(|state| state.enabled)
            })
        }) == Some(true)
    }

    fn activate_native_focus(&mut self, wid: WindowId) -> Result<bool, String> {
        let (_, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let Some(key) = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
        else {
            return Ok(false);
        };
        let compiled = self.compiled_native_ui(wid)?;
        let Some(semantic) = compiled.semantic(&key) else {
            return Ok(false);
        };
        if semantic.state.is_some_and(|state| !state.enabled) {
            return Ok(true);
        }
        let Some(action) = semantic.action.clone() else {
            return Ok(false);
        };
        self.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: action,
                value: None,
            }),
        )?;
        Ok(true)
    }

    /// Activate the page's DEFAULT button (`CompiledUi::default_action`) — the
    /// bare-Return fallback when no control holds keyboard focus. `false` when
    /// the page declares no enabled Primary button; the key then flows on to
    /// the text-input lowering exactly as before.
    fn activate_native_default(&mut self, wid: WindowId) -> Result<bool, String> {
        let compiled = self.compiled_native_ui(wid)?;
        let Some((_, action)) = compiled.default_action else {
            return Ok(false);
        };
        self.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: action,
                value: None,
            }),
        )?;
        Ok(true)
    }

    fn execute_native_effect(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        effect: AppEffect,
    ) -> Result<(), String> {
        match effect {
            AppEffect::ConfigPatch { patch, reply } => {
                self.native_config_pending.push_back(NativeConfigRequest {
                    origin: NativeConfigOrigin::View {
                        instance,
                        view,
                        reply,
                    },
                    work: NativeConfigWork::Patch(patch),
                });
                self.pump_native_config()?;
            }
            AppEffect::ConfigUndo { token, reply } => {
                self.native_config_pending.push_back(NativeConfigRequest {
                    origin: NativeConfigOrigin::View {
                        instance,
                        view,
                        reply,
                    },
                    work: NativeConfigWork::Undo(token),
                });
                self.pump_native_config()?;
            }
            AppEffect::OpenExternal { request, reply } => {
                let safe = crate::is_safe_url(&request.uri);
                let outcome = if request.user_initiated && safe {
                    crate::app_mouse::open_url_external(&request.uri);
                    ExternalOpenOutcome::Opened
                } else if !safe {
                    ExternalOpenOutcome::Denied {
                        message: "unsupported or unsafe URL scheme".to_string(),
                    }
                } else {
                    ExternalOpenOutcome::Denied {
                        message: "external opens require a user gesture".to_string(),
                    }
                };
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ExternalOpenFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::OpenConfigEditor { target, reply } => {
                let outcome =
                    match self.ensure_and_open_config_editor_at_in_window(wid, target.as_ref()) {
                        Ok(canonical_uri) => ConfigEditorOutcome::Opened { canonical_uri },
                        Err(message) => ConfigEditorOutcome::Failed { message },
                    };
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ConfigEditorFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Clipboard { request, reply } => {
                let copied = match request {
                    ClipboardRequest::CopyText { text, .. } => crate::control::pbcopy(&text),
                    ClipboardRequest::CopyDocumentRange {
                        document, range, ..
                    } => self
                        .document_store
                        .snapshot(document)
                        .is_some_and(|snapshot| {
                            range.start < range.end
                                && range.end <= snapshot.text.len()
                                && snapshot.text.is_char_boundary(range.start)
                                && snapshot.text.is_char_boundary(range.end)
                                && crate::control::pbcopy(&snapshot.text[range])
                        }),
                };
                let outcome = if copied {
                    ClipboardOutcome::Copied
                } else {
                    ClipboardOutcome::Failed {
                        message: "clipboard unavailable".to_string(),
                    }
                };
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ClipboardFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Recovery { request, reply } => {
                let outcome = self.execute_recovery_request(wid, request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::RecoveryFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Update { request, reply } => {
                let outcome = self.execute_native_update(request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::UpdateFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::Packages { request, reply } => {
                let outcome = self.execute_native_packages(request);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::PackagesFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            // The page named an entry and a button; the host resolves both
            // and performs the intent in THIS window (`perform_message_act`).
            AppEffect::MessageAct { id, action, reply } => {
                let outcome = self.perform_message_act(wid, id, action);
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::MessageActFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::OpenDocumentEditor { document } => {
                let uri = self
                    .document_store
                    .canonical_uri(document)
                    .ok_or_else(|| "Markdown document disappeared before edit".to_string())?
                    .to_string();
                self.open_document_tab_in_window(wid, crate::native_app::AppKind::Editor, &uri)?;
                self.dispatch_native_completion(
                    wid,
                    instance,
                    view,
                    AppEvent::DocumentEditorOpened { document },
                )?;
            }
            AppEffect::ChooseWallpaperImage => {
                // The modal picker runs its own nested loop on the main thread
                // (the document-open pattern). Only an affirmative selection
                // writes config — through the SAME versioned lane the control
                // `settings set` verb uses, so the image is re-decoded and every
                // Settings view converges on the one admitted verdict.
                if let Some(path) =
                    crate::menu::choose_local_file("Choose a wallpaper image", "Set Wallpaper")
                {
                    let value = path.to_string_lossy().into_owned();
                    let (reply, outcome) = std::sync::mpsc::channel();
                    self.queue_control_settings_field(
                        crate::prefs::EDIT_WALLPAPER.to_string(),
                        Some(value),
                        reply,
                    );
                    // The lane replies asynchronously after persistence; only a
                    // synchronously-known failure is worth surfacing here.
                    if let Ok(Err(error)) = outcome.try_recv() {
                        aterm_log::warn!("wallpaper picker: {error}");
                    }
                }
            }
            AppEffect::OpenLogFolder => {
                // The host's own log directory — never a path from the view — opened
                // through NSWorkspace like Open Log: no shell, no Terminal.
                match crate::logging::log_dir() {
                    Some(dir) if crate::menu::open_file_in_workspace(&dir) => {}
                    Some(dir) => {
                        aterm_log::warn!("could not open the log folder {}", dir.display());
                    }
                    None => aterm_log::warn!("the log folder has no directory to live in"),
                }
            }
            AppEffect::OpenPackagesLog => {
                // atpkg's own resolution of where it writes — never a path from the view.
                match atpkg::packages_log::log_path() {
                    Some(path) if crate::menu::open_file_in_workspace(&path) => {}
                    Some(path) => {
                        aterm_log::warn!("could not open the package log {}", path.display());
                    }
                    None => aterm_log::warn!("the package log has no directory to live in"),
                }
            }
            AppEffect::RequestCloseSelf => {
                self.close_active_native_tab(wid)?;
            }
            AppEffect::InvalidateOwnPresentation => {
                self.refresh_native_presentation(wid, instance, view);
            }
            AppEffect::RepaintSelf(damage) => {
                self.invalidate_native_view_cache(wid, view, damage);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.last_present = None;
                    // The native tray is a retained full-page raster. Dropping it
                    // here makes a repaint effect fail closed: neither a control
                    // capture nor the next glass present can reuse pixels from the
                    // previous route while the new semantic tree is already live.
                    ws.settings_card = None;
                }
                if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    window.request_redraw();
                }
            }
        }
        Ok(())
    }

    /// Enqueue one stable-protocol `settings set|unset` command into the exact
    /// serialized/versioned persistence lane used by native Settings.  Validation,
    /// reconciliation, OCC, atomic publication, and follow-up observation therefore
    /// have one owner.  The main loop never blocks on the disk worker: completion
    /// carries the socket's one-shot reply back through [`NativeConfigOrigin::Control`].
    pub(crate) fn queue_control_settings_field(
        &mut self,
        key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) {
        let Some(canonical_key) = crate::prefs::editable_fields(&self.config)
            .into_iter()
            .find(|field| field.key == key)
            .map(|field| field.key.to_string())
        else {
            let _ = reply.send(Err(format!(
                "unknown key {key:?} (search Settings or use Manual for the complete schema)"
            )));
            return;
        };
        if self.proxy.is_none() {
            let _ = reply.send(Err(
                "save failed: config persistence needs an event-loop proxy".to_string(),
            ));
            return;
        }
        if let Err(error) = native_config_queue() {
            let _ = reply.send(Err(format!("save failed: {error}")));
            return;
        }

        let request_id = self.enqueue_control_settings_field_intent(canonical_key, value, reply);
        if let Err(error) = self.pump_native_config()
            && let Some(position) = self.native_config_pending.iter().position(|request| {
                matches!(
                    &request.origin,
                    NativeConfigOrigin::Control {
                        request_id: queued,
                        ..
                    } if *queued == request_id
                )
            })
            && let Some(NativeConfigRequest {
                origin: NativeConfigOrigin::Control { reply, .. },
                ..
            }) = self.native_config_pending.remove(position)
        {
            // Reconciliation/capability failure happened before this request was
            // reduced. Remove only this caller's uniquely-tagged request so the
            // control thread receives a bounded ERR instead of waiting forever;
            // older UI intents remain queued for the normal reconciliation retry.
            let _ = reply.send(Err(format!("save failed: {error}")));
            self.refresh_serious_mode_queued_projection();
        }
    }

    fn enqueue_control_settings_field_intent(
        &mut self,
        canonical_key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) -> u64 {
        let request_id = next_control_settings_request();
        let serious_mode_intent = control_serious_mode_intent(&canonical_key, value.as_deref());
        self.native_config_pending.push_back(NativeConfigRequest {
            origin: NativeConfigOrigin::Control {
                request_id,
                key: canonical_key.clone(),
                value: value.clone(),
                reply,
            },
            work: NativeConfigWork::ControlField {
                key: canonical_key,
                value,
            },
        });
        // This legacy command shares the same semantic queue as the native
        // Serious Mode command. Project a valid absolute control intent before
        // pumping so a rapid following toggle composes against the value that
        // will precede it, not the still-live durable value. Invalid bool text
        // deliberately has no projection: reduction will reject it without
        // changing the service.
        if let Some(desired) = serious_mode_intent {
            self.serious_mode_queued_projection = Some(desired);
        }
        request_id
    }

    /// Enqueue the menu/keybinding/palette Serious Mode command in the same
    /// versioned transaction lane used by native Settings. Capability preflight
    /// happens before the request becomes visible. The semantic desired value
    /// is materialized as an exact OCC patch only when it reaches the head of
    /// that lane, against the last completed/optimistic service revision. The
    /// live policy is left untouched until durable completion returns.
    pub(crate) fn queue_serious_mode_toggle(&mut self) -> Result<(), String> {
        self.proxy
            .as_ref()
            .ok_or_else(|| "config persistence needs an event-loop proxy".to_string())?;
        let _ = native_config_queue()?;
        self.enqueue_serious_mode_intent()?;
        let result = self.pump_native_config();
        if result.is_err() {
            self.refresh_serious_mode_queued_projection();
        }
        result
    }

    /// Compose a new click against the newest queued intent, not just the live
    /// durable policy. Kept as one shipping seam so any rapid toggle sequence
    /// retains its parity while earlier completions are in flight.
    fn enqueue_serious_mode_intent(&mut self) -> Result<(), String> {
        let desired = !self
            .serious_mode_queued_projection
            .unwrap_or_else(|| self.serious_mode_enabled());
        self.native_config_pending.push_back(NativeConfigRequest {
            origin: NativeConfigOrigin::SeriousMode { desired },
            work: NativeConfigWork::SeriousMode(desired),
        });
        self.serious_mode_queued_projection = Some(desired);
        Ok(())
    }

    /// Recompute the semantic projection after a completion or a failed queue
    /// admission. The newest valid intent wins; malformed legacy bool text is
    /// ignored because its reduction is guaranteed to reject without a write.
    fn refresh_serious_mode_queued_projection(&mut self) {
        self.serious_mode_queued_projection =
            self.native_config_pending
                .iter()
                .rev()
                .find_map(|request| match &request.origin {
                    NativeConfigOrigin::SeriousMode { desired } => Some(*desired),
                    NativeConfigOrigin::Control { key, value, .. } => {
                        control_serious_mode_intent(key, value.as_deref())
                    }
                    NativeConfigOrigin::View { .. } | NativeConfigOrigin::Presence { .. } => None,
                });
    }

    /// A presence toggle's durable write completed (round 19). The live bit
    /// was flipped at the click; an APPLIED write only adopts the durable
    /// `[presence]` table into `self.config` (so a later resolver read agrees),
    /// while a refused one — a conflict with a hand edit, an indeterminate or
    /// rejected write — says so on the notice and reverts the live bit to the
    /// durable truth, exactly as Serious Mode's completion reports on itself.
    fn publish_presence_completion(
        &mut self,
        key: &'static str,
        desired: bool,
        outcome: &ConfigPatchOutcome,
        authoritative: Option<&crate::native_config_service::ConfigSnapshot>,
    ) {
        let label = crate::app_fabric_menu::presence_key_label(key);
        // `(cause, indeterminate)`: the toggle reverted, or may not have
        // persisted (design §10.3 C17).
        let feedback = match outcome {
            ConfigPatchOutcome::Applied { .. } => {
                if let Some(snapshot) = authoritative {
                    self.config.presence = snapshot.config.presence.clone();
                }
                None
            }
            ConfigPatchOutcome::Conflict { .. } => Some((
                "aterm.toml changed first; its current value was kept".to_string(),
                false,
            )),
            ConfigPatchOutcome::Indeterminate { message } => Some((message.clone(), true)),
            other => Some((
                control_settings_completion_reply(key, Some(&desired.to_string()), other, None)
                    .err()
                    .unwrap_or_else(|| "the write was refused".to_string()),
                false,
            )),
        };
        if let Some((cause, indeterminate)) = feedback {
            // The durable truth wins over the click: re-read it from the
            // authoritative snapshot when there is one, else from the config
            // the App last adopted.
            let durable = authoritative.map_or_else(
                || crate::app_fabric_menu::presence_key_resolve(key, &self.config),
                |snapshot| crate::app_fabric_menu::presence_key_resolve(key, &snapshot.config),
            );
            self.set_presence_bit(key, durable);
            self.post_message(crate::message_reporters::presence_not_saved(
                key,
                label,
                &cause,
                indeterminate,
            ));
        }
    }

    /// Queue the durable write for a presence toggle (round 19): the leaf key
    /// `presence.band` / `presence.rim` set to `desired`, through the same
    /// serialized lane and OCC materialization Serious Mode uses — the value is
    /// composed at dequeue against the newest service revision, so rapid
    /// clicks cannot conflict with each other's completions.
    pub(crate) fn queue_presence_write(
        &mut self,
        key: &'static str,
        desired: bool,
    ) -> Result<(), String> {
        self.proxy
            .as_ref()
            .ok_or_else(|| "config persistence needs an event-loop proxy".to_string())?;
        let _ = native_config_queue()?;
        self.native_config_pending.push_back(NativeConfigRequest {
            origin: NativeConfigOrigin::Presence { key, desired },
            work: NativeConfigWork::ControlField {
                key: key.to_string(),
                value: Some(desired.to_string()),
            },
        });
        self.pump_native_config()
    }

    /// Build the exact compare-and-swap request for a Serious Mode intent at
    /// dequeue time. Every earlier request has already reduced into the
    /// service, so both the base revision and expected value are current.
    fn serious_mode_patch_request(
        &self,
        desired: bool,
    ) -> Result<crate::native_app::ConfigPatch, String> {
        let snapshot = self.native_config_service.snapshot();
        let expected = snapshot.values()?.remove(crate::prefs::EDIT_SERIOUS_MODE);
        Ok(crate::native_app::ConfigPatch {
            base_revision: snapshot.revision,
            edits: vec![crate::native_app::ConfigEdit {
                key: crate::prefs::EDIT_SERIOUS_MODE.to_string(),
                expected: ExpectedConfigValue::Exact(expected),
                value: Some(desired.to_string()),
            }],
        })
    }

    fn control_field_patch_request(
        &self,
        key: String,
        value: Option<String>,
    ) -> Result<crate::native_app::ConfigPatch, String> {
        let snapshot = self.native_config_service.snapshot();
        let expected = snapshot.values()?.remove(&key);
        Ok(crate::native_app::ConfigPatch {
            base_revision: snapshot.revision,
            edits: vec![crate::native_app::ConfigEdit {
                key,
                expected: ExpectedConfigValue::Exact(expected),
                value,
            }],
        })
    }

    fn reduce_native_config_work(
        &mut self,
        work: NativeConfigWork,
    ) -> Result<ConfigPatchResult, String> {
        let work = match work {
            NativeConfigWork::SeriousMode(desired) => {
                NativeConfigWork::Patch(self.serious_mode_patch_request(desired)?)
            }
            NativeConfigWork::ControlField { key, value } => {
                NativeConfigWork::Patch(self.control_field_patch_request(key, value)?)
            }
            work => work,
        };
        Ok(match work {
            NativeConfigWork::Patch(patch) => {
                self.native_config_service.patch(ConfigPatchRequest {
                    base_revision: patch.base_revision,
                    edits: patch
                        .edits
                        .into_iter()
                        .map(|edit| ConfigKeyEdit {
                            key: edit.key,
                            expected: match edit.expected {
                                ExpectedConfigValue::Any => ExpectedValue::Any,
                                ExpectedConfigValue::Exact(value) => ExpectedValue::Exact(value),
                            },
                            value: edit.value,
                        })
                        .collect(),
                })
            }
            NativeConfigWork::Undo(token) => self
                .native_config_service
                .undo(crate::native_config_service::UndoToken::from_stored(token)),
            NativeConfigWork::SeriousMode(_) | NativeConfigWork::ControlField { .. } => {
                unreachable!("semantic config work is materialized above")
            }
        })
    }

    pub(crate) fn pump_native_config(&mut self) -> Result<(), String> {
        while !self.native_config_inflight {
            // A watcher candidate owns file authority from receipt through
            // parse/assets/font preparation. Hold semantic writes until that
            // exact baseline is either admitted or rejected. Once a complete
            // generation is retained, reconciliation may run to order it
            // against a concurrent durable write.
            if self.config_watch_admission_pending()
                && self.native_config_external_pending.is_none()
            {
                return Ok(());
            }
            // A conflict or post-publication proof failure invalidates the
            // previous disk baseline. Dispatch a bounded worker observation
            // before even popping the next semantic request; the event loop
            // never opens the pathname or resolves referenced assets.
            if self.native_config_service.reconciliation_required() {
                let proxy = self.proxy.clone().ok_or_else(|| {
                    "native config reconciliation needs an event-loop proxy".to_string()
                })?;
                let queue = native_config_queue()?;
                let path = self
                    .native_config_service
                    .bound_logical_path()
                    .map(std::path::Path::to_path_buf)
                    .or_else(crate::app_config::config_path)
                    .ok_or_else(|| "no config path (HOME/XDG unset)".to_string())?;
                let job = NativeConfigJob::Reconcile(NativeConfigReconciliationJob {
                    path,
                    themes: std::sync::Arc::clone(
                        &self.native_config_service.snapshot().assets.themes,
                    ),
                    pending_sequence: self.native_config_external_sequence,
                    proxy,
                });
                queue.send(job).map_err(|_| {
                    "native config worker stopped during reconciliation".to_string()
                })?;
                self.native_config_inflight = true;
                // Park here; do NOT fail the caller. Parking is bounded because
                // the probe just dispatched always reports back, and BOTH of its
                // outcomes settle this queue: a successful sample clears the
                // fence and re-pumps (`finish_native_config_reconciliation`), so
                // the request is reduced normally; a failed one runs
                // `fail_native_config_reconciliation`, which answers every queued
                // origin with a bounded rejection. The wait is one worker file
                // read, not the 30s wire deadline that this defect was reported
                // as -- that deadline came from a queue nothing ever drained, and
                // draining it is the actual fix.
                //
                // REJECTED: failing fast here on a remembered "the last probe
                // came back broken". It bought no boundedness on top of the
                // drain, and cost three things. (1) It answered with the PREVIOUS
                // sample's reason while the fresh one dispatched above -- which
                // may well succeed -- was still in flight. (2) It converted any
                // TRANSIENT failure (a momentary lock or AV hold, a half-written
                // aterm.toml caught mid-save by the watcher) into a hard ERR for
                // the next `settings set`, where waiting one read would have
                // succeeded, and made recovery cost an extra command. (3) The
                // resulting Err is not even truthful for every origin: the
                // Control escape hatch below can withdraw its own uniquely-tagged
                // request before replying, but a queued `SeriousMode` intent has
                // no such tag, so it stayed queued while the user was told
                // "Serious Mode was not changed" -- and the probe this very call
                // dispatched could then go on to apply it.
                return Ok(());
            }
            if self.native_config_pending.is_empty() {
                return Ok(());
            }
            // Acquire every fallible host capability before reducing the queued
            // request. Otherwise an unavailable event-loop proxy/worker could
            // advance the in-memory revision without ever making it durable.
            let proxy = self
                .proxy
                .clone()
                .ok_or_else(|| "native config persistence needs an event-loop proxy".to_string())?;
            let queue = native_config_queue()?;
            let request = self
                .native_config_pending
                .pop_front()
                .expect("queue was checked before capability acquisition");
            let NativeConfigRequest { origin, work } = request;
            let result = match self.reduce_native_config_work(work) {
                Ok(result) => result,
                Err(message) => {
                    let authoritative = self.native_config_service.snapshot();
                    self.publish_native_config_origin(
                        origin,
                        ConfigPatchOutcome::Rejected { message },
                        Some(authoritative),
                        false,
                        None,
                    );
                    continue;
                }
            };
            match result {
                ConfigPatchResult::Applied { snapshot, undo } => {
                    let plan = self.native_config_service.persistence_plan(snapshot);
                    let job = NativeConfigJob::Persist(NativeConfigPersistenceJob {
                        plan,
                        undo: Some(undo.get()),
                        origin,
                        proxy,
                    });
                    if let Err(error) = queue.send(job) {
                        // The reducer has advanced but no worker owns the candidate.
                        // Restore from durable authority before completing the
                        // initiating request; a control-origin request owns a blocked
                        // socket reply and must not be silently dropped here.
                        let NativeConfigJob::Persist(job) = error.0 else {
                            unreachable!("persistence send returned a reconciliation job")
                        };
                        let restored = self.native_config_service.restore_durable_snapshot();
                        let (authoritative, message) = match restored {
                            Ok(snapshot) => {
                                (Some(snapshot), "native config worker stopped".to_string())
                            }
                            Err(error) => (
                                None,
                                format!(
                                    "native config worker stopped; in-memory durable rollback failed: {error}"
                                ),
                            ),
                        };
                        self.publish_native_config_origin(
                            job.origin,
                            ConfigPatchOutcome::Rejected {
                                message: message.clone(),
                            },
                            authoritative,
                            true,
                            Some(message),
                        );
                        continue;
                    }
                    self.native_config_inflight = true;
                    return Ok(());
                }
                ConfigPatchResult::Unchanged { snapshot } => {
                    self.publish_native_config_origin(
                        origin,
                        ConfigPatchOutcome::Applied {
                            revision: snapshot.revision,
                            undo: None,
                        },
                        Some(snapshot),
                        false,
                        None,
                    );
                }
                ConfigPatchResult::Conflict { snapshot, .. } => {
                    self.publish_native_config_origin(
                        origin,
                        ConfigPatchOutcome::Conflict {
                            revision: snapshot.revision,
                        },
                        Some(snapshot),
                        false,
                        None,
                    );
                }
                ConfigPatchResult::Rejected { snapshot, message } => {
                    self.publish_native_config_origin(
                        origin,
                        ConfigPatchOutcome::Rejected { message },
                        Some(snapshot),
                        false,
                        None,
                    );
                }
            }
        }
        Ok(())
    }

    /// Main-thread half of the config worker protocol. The durable transaction
    /// already completed even if its initiating view/instance closed; revision
    /// publication fans out to every currently live Settings view.
    pub(crate) fn finish_native_config_write(
        &mut self,
        origin: NativeConfigOrigin,
        completion: NativeConfigPersistenceCompletion,
    ) {
        self.native_config_inflight = false;
        let NativeConfigPersistenceCompletion {
            outcome,
            observation,
        } = completion;
        if matches!(
            &outcome,
            ConfigPatchOutcome::Conflict { .. } | ConfigPatchOutcome::Indeterminate { .. }
        ) {
            self.native_config_service.mark_reconciliation_required();
        }
        let before_revision = self.native_config_service.snapshot().revision;
        let mut runtime_observation = None;
        let (authoritative, synchronization_error) = match observation {
            Ok(prepared) => {
                let prepared = self.rebase_prepared_config_themes(prepared);
                match self
                    .native_config_service
                    .synchronize_prepared_observation(prepared.clone())
                {
                    Ok(snapshot) => {
                        runtime_observation = Some(prepared);
                        (Some(snapshot), None)
                    }
                    Err(error) => {
                        self.native_config_service.mark_reconciliation_required();
                        let restored = if matches!(&outcome, ConfigPatchOutcome::Applied { .. }) {
                            None
                        } else {
                            self.native_config_service.restore_durable_snapshot().ok()
                        };
                        (restored, Some(error))
                    }
                }
            }
            Err(error) => {
                self.native_config_service.mark_reconciliation_required();
                let restored = if matches!(&outcome, ConfigPatchOutcome::Applied { .. }) {
                    None
                } else {
                    self.native_config_service.restore_durable_snapshot().ok()
                };
                (restored, Some(error))
            }
        };
        let reconciled_changed = authoritative
            .as_ref()
            .is_some_and(|snapshot| snapshot.revision != before_revision);
        // The wire reply already distinguishes these outcomes; the log line and
        // the in-app notice below did not, and claimed a save for every closed
        // gate. A `Rejected` outcome can fail its preflight BEFORE a single byte
        // reaches disk, so "Config was saved" was flatly untrue there and sent a
        // real investigation looking for a phantom partial write. Decide the
        // wording from the outcome, while it is still in hand.
        let saved_wording = match &outcome {
            ConfigPatchOutcome::Applied { .. } => "Config was saved, but its exact disk generation",
            // The worker lost the race or never produced a proof; either way the
            // requested edit is not what is on disk now.
            ConfigPatchOutcome::Indeterminate { .. } => {
                "Config publication could not be verified, and the current disk generation"
            }
            ConfigPatchOutcome::Conflict { .. } | ConfigPatchOutcome::Rejected { .. } => {
                "Config was NOT saved, and the current disk generation"
            }
        };
        self.publish_native_config_origin(
            origin,
            outcome,
            authoritative,
            reconciled_changed,
            synchronization_error.clone(),
        );
        if let Some(error) = synchronization_error {
            self.surface_native_config_lane_error(format!(
                "{saved_wording} could not be admitted: {error}"
            ));
        }
        if self.config_watch_admission_pending() || self.native_config_external_pending.is_some() {
            // A watcher candidate was observed while persistence owned the
            // lane. It may still be in raw parse/assets/font preparation, so
            // its relative order is ambiguous until a worker samples the path
            // after this point. Fence queued semantic writes immediately.
            self.native_config_service.mark_reconciliation_required();
            runtime_observation = None;
        }
        if let Some(prepared) = runtime_observation {
            self.reload_prepared_config_observation(prepared);
        }
        if let Err(error) = self.pump_native_config() {
            self.surface_native_config_lane_error(error);
        }
    }

    /// Single settlement for "a reconciliation worker reported back and the lane
    /// is still fenced". Two things have to happen together or the lane leaks:
    /// answer everything that was queued behind this exact sample, and tell the
    /// user once. The fence itself stays armed, because only an admitted disk
    /// generation is proof that the ordering authority is back.
    ///
    /// It deliberately does NOT re-pump. The fence is still armed and the fence
    /// check in `pump_native_config` precedes the emptiness check, so pumping
    /// here would dispatch another worker sample of the same unreadable path and
    /// loop. Retry is caller-driven instead: the next `settings set` or Settings
    /// save dispatches a fresh sample, and that is the seam through which a lane
    /// broken by a transient failure recovers without restarting aterm. Nothing
    /// remembers that this sample failed, deliberately: a remembered failure
    /// would only be used to pre-emptively fail the caller that dispatches the
    /// NEXT sample, i.e. to punish a caller for a diagnosis that its own probe
    /// is in the middle of disproving.
    ///
    /// "TELL THE USER ONCE" MEANS ABOUT WHAT WAS LOST, so it is said only when
    /// something was. The commonest way here is a hand-edited `aterm.toml` that
    /// does not parse: the watcher reports it ("Config observation was not valid
    /// TOML", on the config lane's one row), fences the lane, and the sample this
    /// settles re-reads the same broken file and fails the same way. With nothing
    /// queued, "queued changes were not written" was simply false — and because
    /// both messages share the lane's key, the false one REPLACED the true one on
    /// the band: the person who broke their file was told about writes they never
    /// asked for, instead of which line does not parse. With nothing queued there
    /// is nothing to account for and the fence costs nothing (the next save takes
    /// a fresh sample and reports against its own request), so the lane's row is
    /// left standing and the failure goes to the log.
    fn fail_native_config_reconciliation(&mut self, error: &str) {
        self.native_config_service.mark_reconciliation_required();
        let rejected =
            self.reject_pending_native_config(&format!("config reconciliation failed: {error}"));
        if rejected == 0 {
            aterm_log::warn!("native config: reconciliation failed with nothing queued: {error}");
            return;
        }
        let lost = if rejected == 1 {
            "1 queued change was".to_string()
        } else {
            format!("{rejected} queued changes were")
        };
        self.surface_native_config_lane_error(format!(
            "Config reconciliation failed; {lost} not written: {error}"
        ));
    }

    pub(crate) fn finish_native_config_reconciliation(
        &mut self,
        completion: NativeConfigReconciliationCompletion,
    ) {
        self.native_config_inflight = false;
        let prepared = match completion.observation {
            Ok(prepared) => self.rebase_prepared_config_themes(prepared),
            Err(error) => {
                // This sample was the ONE thing that could have cleared the
                // fence, and it came back broken. Every request behind it was
                // waiting for exactly this answer, so record the reason and hand
                // it to all of them now. The old code kept the fence, said
                // "queued changes remain pending", and returned -- and since
                // nothing else ever drains this queue and nothing but a
                // successful sample ever clears the fence, "pending" meant
                // "abandoned": a control caller's socket reply sat here until
                // the 30s wire timeout fired.
                self.fail_native_config_reconciliation(&error);
                return;
            }
        };

        if self.native_config_external_sequence > completion.pending_sequence {
            // A watcher completion arrived after this worker sampled the path.
            // Admit the sample as an exact intermediate generation, retain the
            // newer watcher payload, and sample once more before any write.
            if let Err(error) = self
                .native_config_service
                .synchronize_prepared_observation(prepared)
            {
                // Same abandonment as the observation arm above: the sample read
                // fine but the service refused to admit it, so the fence stays
                // armed with nothing scheduled to retry it.
                self.fail_native_config_reconciliation(&error);
                return;
            }
            self.native_config_service.mark_reconciliation_required();
            if let Err(error) = self.pump_native_config() {
                self.surface_native_config_lane_error(error);
            }
            return;
        }

        let pending_matches = self
            .native_config_external_pending
            .as_ref()
            .is_some_and(|pending| pending.baseline() == &prepared.observation.baseline);
        let pending_theme_is_current =
            self.native_config_external_pending
                .as_ref()
                .is_some_and(|pending| {
                    std::sync::Arc::ptr_eq(
                        pending.themes(),
                        &self.native_config_service.snapshot().assets.themes,
                    )
                });
        if pending_matches && pending_theme_is_current {
            self.drain_reconciled_deferred_config_generation();
            return;
        }

        if let Some(superseded) = self.native_config_external_pending.take() {
            aterm_log::debug!(
                "validated deferred config generation as superseded: {}",
                superseded.baseline().target.logical_path().display()
            );
        }
        let runtime = prepared.clone();
        let snapshot = match self
            .native_config_service
            .synchronize_prepared_observation(prepared)
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // Ditto: an unadmittable sample leaves no live retry, so answer
                // the queue instead of holding replies nobody will settle.
                self.fail_native_config_reconciliation(&error);
                return;
            }
        };
        self.publish_native_config_snapshot(&snapshot);
        self.reload_prepared_config_observation(runtime);
        if let Err(error) = self.pump_native_config() {
            self.surface_native_config_lane_error(error);
        }
    }

    fn rebase_prepared_config_themes(
        &self,
        mut prepared: crate::native_config_service::PreparedConfigObservation,
    ) -> crate::native_config_service::PreparedConfigObservation {
        let current = &self.native_config_service.snapshot().assets.themes;
        if !std::sync::Arc::ptr_eq(&prepared.assets.themes, current) {
            prepared.assets = std::sync::Arc::new(crate::app_config::ConfigAssetCatalog {
                trail_packs: std::sync::Arc::clone(&prepared.assets.trail_packs),
                kitty_sprite: prepared.assets.kitty_sprite.clone(),
                wallpaper: prepared.assets.wallpaper.clone(),
                themes: std::sync::Arc::clone(current),
                sparkle_spec_consumers: prepared.assets.sparkle_spec_consumers.clone(),
            });
        }
        prepared
    }

    pub(crate) fn defer_prepared_config_generation(
        &mut self,
        generation: crate::native_font_catalog::PreparedConfigGeneration,
    ) {
        self.native_config_external_sequence = self
            .native_config_external_sequence
            .saturating_add(1)
            .max(1);
        self.native_config_external_pending = Some(DeferredNativeConfigGeneration::Prepared(
            Box::new(generation),
        ));
    }

    /// Consume the exact deferred generation whose config baseline a
    /// reconciliation worker just observed. Prepared runtime generations must
    /// bypass the still-closed reconciliation gate exactly once; their own
    /// service admission clears it atomically with the matching bytes/assets.
    fn drain_reconciled_deferred_config_generation(&mut self) {
        if let Some(generation) = self.native_config_external_pending.take() {
            match generation {
                DeferredNativeConfigGeneration::Prepared(generation) => {
                    self.apply_reconciled_prepared_config_generation(*generation);
                }
                DeferredNativeConfigGeneration::Observation(prepared) => {
                    self.admit_manual_config_observation(*prepared);
                }
            }
        }
    }

    pub(crate) fn admit_manual_config_observation(
        &mut self,
        prepared: crate::native_config_service::PreparedConfigObservation,
    ) {
        if self.native_config_inflight {
            self.native_config_external_sequence = self
                .native_config_external_sequence
                .saturating_add(1)
                .max(1);
            self.native_config_external_pending = Some(
                DeferredNativeConfigGeneration::Observation(Box::new(prepared)),
            );
            return;
        }
        let prepared = self.rebase_prepared_config_themes(prepared);
        let runtime = prepared.clone();
        let admitted_baseline = prepared.observation.baseline.clone();
        match self
            .native_config_service
            .synchronize_prepared_observation(prepared)
        {
            Ok(snapshot) => {
                self.publish_native_config_snapshot(&snapshot);
                self.reload_prepared_config_observation(runtime);
                self.finish_native_config_external_admission(&admitted_baseline);
            }
            Err(error) => {
                // Same abandonment shape as the reconciliation arms, and on the
                // same path: this is reached from
                // `finish_native_config_reconciliation`'s deferred-generation
                // drain, so a caller parked behind that probe would be left with
                // a re-armed fence and NOTHING dispatched to clear it. Answer the
                // queue here too, or the probe-completion bound below is a
                // half-truth.
                self.native_config_service.mark_reconciliation_required();
                self.reject_pending_native_config(&format!(
                    "config generation could not be admitted: {error}"
                ));
                self.surface_native_config_lane_error(format!(
                    "Manual saved aterm.toml, but its exact generation could not be admitted: {error}"
                ));
            }
        }
    }

    pub(crate) fn finish_native_config_external_admission(
        &mut self,
        baseline: &crate::native_document_host::AtomicFileBaseline,
    ) {
        // Clear only the payload this admission actually proves. A slower
        // generation A must not discard a newer retained generation B.
        if self
            .native_config_external_pending
            .as_ref()
            .is_some_and(|pending| pending.baseline() == baseline)
        {
            self.native_config_external_pending = None;
        }
        self.acknowledge_config_watch_admission(baseline);
        if let Err(error) = self.pump_native_config() {
            self.surface_native_config_lane_error(error);
        }
    }

    pub(crate) fn prepare_native_config_external_observation(
        &mut self,
        observation: crate::native_config_service::ConfigDiskObservation,
    ) {
        let baseline = observation.baseline.clone();
        let Some(proxy) = self.proxy.clone() else {
            let result = crate::native_config_service::VersionedConfigService::prepare_observation(
                observation.clone(),
                std::sync::Arc::clone(&self.native_config_service.snapshot().assets.themes),
            );
            self.finish_native_config_external_preparation(
                NativeConfigExternalPreparationCompletion {
                    observation,
                    result,
                },
            );
            return;
        };
        let job = NativeConfigJob::PrepareExternal(NativeConfigExternalPreparationJob {
            observation,
            themes: std::sync::Arc::clone(&self.native_config_service.snapshot().assets.themes),
            proxy,
        });
        let result = native_config_queue().and_then(|queue| {
            queue
                .send(job)
                .map_err(|_| "native config worker stopped during external preparation".to_string())
        });
        if let Err(error) = result {
            self.native_config_service.mark_reconciliation_required();
            self.reject_config_watch_admission_for(
                &baseline,
                crate::config_watcher::WatchFailureKind::ConfigPreparationFailed,
            );
            // Reaching here means the worker queue itself is gone, so the fence
            // just armed above has no sample scheduled and never can have one:
            // every later pump fails at the same `native_config_queue()`. Queued
            // requests are therefore unanswerable, not merely delayed.
            self.reject_pending_native_config(&format!("config preparation failed: {error}"));
            self.surface_native_config_lane_error(error);
        }
    }

    pub(crate) fn finish_native_config_external_preparation(
        &mut self,
        completion: NativeConfigExternalPreparationCompletion,
    ) {
        match completion.result {
            Ok(prepared) => self.reload_prepared_config_observation(prepared),
            Err(error) => {
                // Invalid TOML is still valid UTF-8 editor content. Manual must
                // receive the exact watcher bytes so its LSP-style diagnostics
                // can help repair them, while live Config remains unchanged.
                if let Err(refresh_error) =
                    self.refresh_open_config_editor_observation(&completion.observation)
                {
                    aterm_log::warn!(
                        "config reload: Manual refresh needs attention ({refresh_error})"
                    );
                }
                self.native_config_service.mark_reconciliation_required();
                self.reject_config_watch_admission_for(
                    &completion.observation.baseline,
                    crate::config_watcher::WatchFailureKind::ConfigInvalidToml,
                );
                self.surface_native_config_lane_error(format!(
                    "Config observation was not valid TOML: {error}"
                ));
                if let Err(error) = self.pump_native_config() {
                    self.surface_native_config_lane_error(error);
                }
            }
        }
    }

    /// The native config lane's own error (an observation that was not TOML,
    /// a pump that failed, a request the lane dropped): logged, and posted as
    /// the `config.lane` row (design R13) — one row, the newest cause.
    pub(crate) fn surface_native_config_lane_error(&mut self, message: String) {
        aterm_log::warn!("native config: {message}");
        self.post_message(crate::message_reporters::config_lane_error(&message));
    }

    /// Answer EVERY still-queued semantic request with one bounded rejection and
    /// empty the lane.
    ///
    /// Call this from every arm that re-arms the write fence WITHOUT leaving a
    /// worker sample scheduled to clear it. Such an arm invalidates the disk
    /// baseline each queued request would have to be reduced against, so not one
    /// of them can be published, and nothing is coming that would change
    /// that. Leaving them parked "for the next retry" is precisely what
    /// abandoned control callers: their one-shot `Sender` stayed alive in the
    /// queue, the control thread blocked on it, and the wire reported a wedged
    /// event loop 30 seconds later -- from an event loop that was in fact
    /// turning normally and answering every read verb instantly. The retry is
    /// still available; it just starts from the caller's NEXT command instead of
    /// from a request nobody is watching any more.
    ///
    /// Nothing in the queue has been reduced yet (`pump_native_config` pops and
    /// reduces in the same step), so there is no optimistic in-memory state to
    /// roll back and no authoritative snapshot to republish here.
    ///
    /// Callers must NOT pump afterwards: the fence check in `pump_native_config`
    /// runs BEFORE the emptiness check, so pumping a freshly drained queue would
    /// dispatch yet another reconciliation and spin the worker against a path it
    /// already cannot read.
    ///
    /// Returns how many requests it rejected, so a caller that wants to tell the
    /// person something was LOST can tell whether anything was.
    pub(crate) fn reject_pending_native_config(&mut self, message: &str) -> usize {
        let abandoned = std::mem::take(&mut self.native_config_pending);
        if abandoned.is_empty() {
            return 0;
        }
        let rejected = abandoned.len();
        aterm_log::warn!(
            "native config: rejecting {} queued config request(s): {message}",
            abandoned.len()
        );
        for NativeConfigRequest { origin, .. } in abandoned {
            self.publish_native_config_origin(
                origin,
                ConfigPatchOutcome::Rejected {
                    message: message.to_string(),
                },
                None,
                false,
                None,
            );
        }
        self.refresh_serious_mode_queued_projection();
        rejected
    }

    fn publish_native_config_origin(
        &mut self,
        origin: NativeConfigOrigin,
        outcome: ConfigPatchOutcome,
        authoritative: Option<crate::native_config_service::ConfigSnapshot>,
        reconciled_changed: bool,
        synchronization_error: Option<String>,
    ) {
        if let Some(snapshot) = authoritative.as_ref() {
            self.publish_native_config_snapshot(snapshot);
        }
        match origin {
            NativeConfigOrigin::View {
                instance,
                view,
                reply,
            } => self.publish_native_config_completion(
                instance,
                view,
                reply,
                outcome,
                reconciled_changed,
            ),
            NativeConfigOrigin::SeriousMode { desired } => self.publish_serious_mode_completion(
                desired,
                outcome,
                authoritative,
                synchronization_error,
            ),
            NativeConfigOrigin::Presence { key, desired } => {
                self.publish_presence_completion(key, desired, &outcome, authoritative.as_ref());
            }
            NativeConfigOrigin::Control {
                key, value, reply, ..
            } => {
                let serious_mode = key == crate::prefs::EDIT_SERIOUS_MODE;
                if serious_mode && let Some(snapshot) = authoritative.as_ref() {
                    self.apply_serious_mode_config_snapshot(snapshot);
                }
                let response = control_settings_completion_reply(
                    &key,
                    value.as_deref(),
                    &outcome,
                    synchronization_error.as_deref(),
                );
                self.refresh_serious_mode_queued_projection();
                let _ = reply.send(response);
                if key == crate::prefs::EDIT_ROBI {
                    // The click-dismissal seam holds this reply's receiver;
                    // settle it NOW — a failure outcome requests no redraw of
                    // its own, and the banner (plus Robi's return) must not
                    // wait for the next natural frame.
                    self.poll_robi_dismissal();
                }
                if matches!(&outcome, ConfigPatchOutcome::Applied { undo: Some(_), .. })
                    || reconciled_changed
                    || (serious_mode && authoritative.is_some())
                {
                    self.request_redraw_all_windows();
                }
            }
        }
    }

    fn publish_native_config_completion(
        &mut self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
        outcome: ConfigPatchOutcome,
        reconciled_changed: bool,
    ) {
        let revision = match &outcome {
            ConfigPatchOutcome::Applied { revision, .. }
            | ConfigPatchOutcome::Conflict { revision } => Some(*revision),
            ConfigPatchOutcome::Indeterminate { .. } | ConfigPatchOutcome::Rejected { .. } => None,
        };
        if self.native_runtime.completion_is_current(&reply)
            && self.native_runtime.view_state(view).is_some()
            && self.native_runtime.app(instance).is_some()
            && let Some(wid) = self.windows.iter().find_map(|(wid, ws)| {
                ws.tab_set
                    .tabs()
                    .iter()
                    .any(|tab| tab.root.contains(view))
                    .then_some(*wid)
            })
        {
            let _ = self.dispatch_native_completion(
                wid,
                instance,
                view,
                AppEvent::ConfigPatchFinished {
                    operation: reply.operation,
                    outcome,
                },
            );
        }

        if revision.is_some() || reconciled_changed {
            self.request_redraw_all_windows();
        }
    }

    fn publish_serious_mode_completion(
        &mut self,
        desired: bool,
        outcome: ConfigPatchOutcome,
        authoritative: Option<crate::native_config_service::ConfigSnapshot>,
        synchronization_error: Option<String>,
    ) {
        let applied = authoritative
            .as_ref()
            .map(|snapshot| self.apply_serious_mode_config_snapshot(snapshot));

        let feedback = match (&outcome, &applied) {
            (ConfigPatchOutcome::Applied { .. }, Some(actual)) if *actual == desired => {
                synchronization_error.as_deref().map(|error| {
                    format!(
                        "Serious Mode was saved, but later config edits could not be verified: {error}"
                    )
                })
            }
            (ConfigPatchOutcome::Applied { .. }, Some(_)) => Some(
                "Serious Mode was saved, but a newer aterm.toml edit now controls it.".to_string(),
            ),
            (ConfigPatchOutcome::Applied { .. }, None) => Some(format!(
                "Serious Mode was saved but could not be applied: {}",
                synchronization_error
                    .as_deref()
                    .unwrap_or("no authoritative config snapshot")
            )),
            (ConfigPatchOutcome::Conflict { .. }, _) => Some(
                "Serious Mode was not changed because aterm.toml changed first; its current value was kept."
                    .to_string(),
            ),
            (ConfigPatchOutcome::Indeterminate { message }, _) => Some(format!(
                "Serious Mode may have been written but could not be verified; reload before retrying: {message}"
            )),
            (ConfigPatchOutcome::Rejected { message }, _) => Some(
                synchronization_error.as_deref().map_or_else(
                    || format!("Serious Mode was not changed: {message}"),
                    |error| {
                        format!(
                            "Serious Mode was not changed: {message}; current aterm.toml could not be admitted: {error}"
                        )
                    },
                ),
            ),
        };
        if let Some(message) = feedback {
            self.post_message(crate::message_reporters::serious_mode_feedback(&message));
        }

        // Settings already received the exact snapshot synchronously above;
        // the worker-prepared runtime generation is scheduled by the caller.
        self.refresh_serious_mode_queued_projection();
        self.request_redraw_all_windows();
    }

    /// Install only the process-global projection owned by this command from
    /// the worker-validated typed snapshot, without reparsing TOML on the event
    /// loop, then fan the same generation out to Settings immediately.
    fn apply_serious_mode_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) -> bool {
        self.config.serious_mode = snapshot.config.serious_mode;
        let enabled = self.apply_serious_mode(snapshot.config.serious_mode_or_default());
        self.publish_native_config_snapshot(snapshot);
        enabled
    }

    /// Feed one exact stable file observation through the serialized config
    /// lane. If a Settings commit is in flight, retain the complete bytes + disk
    /// generation; completion later rejects it if that generation is no longer
    /// current instead of rereading or replaying stale text.
    #[cfg(test)]
    pub(crate) fn sync_native_config_external_observation(
        &mut self,
        observation: crate::native_config_service::ConfigDiskObservation,
    ) -> Result<Option<crate::native_config_service::ConfigSnapshot>, String> {
        let prepared = crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            std::sync::Arc::clone(&self.native_config_service.snapshot().assets.themes),
        )?;
        if self.native_config_inflight {
            self.native_config_external_sequence = self
                .native_config_external_sequence
                .saturating_add(1)
                .max(1);
            self.native_config_external_pending = Some(
                DeferredNativeConfigGeneration::Observation(Box::new(prepared)),
            );
            return Ok(None);
        }
        let snapshot = self
            .native_config_service
            .synchronize_prepared_observation(prepared)?;
        Ok(Some(snapshot))
    }

    /// Publish one admitted config generation to every live Settings
    /// presentation. Full reload callers install `App.config` and assets first;
    /// a narrow process command may first install only the live projection it
    /// owns, then publish the complete durable Settings snapshot while the
    /// ordinary reload applies unrelated fields.
    pub(crate) fn publish_native_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) {
        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in views {
            let _ = self.native_runtime.dispatch(
                instance,
                view,
                AppEvent::ConfigChanged(snapshot.clone()),
            );
        }
        // Stable config observations also refresh host semantics for an open
        // Manual buffer. `analysis_generation` advances on byte-identical
        // observations, so referenced assets/fonts cannot leave diagnostics
        // stale merely because the TOML text did not change.
        if let Some(document) = self.native_runtime.config_editor_document() {
            self.request_config_host_diagnostics(document);
        }
    }

    fn dispatch_native_completion(
        &mut self,
        wid: WindowId,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        event: AppEvent,
    ) -> Result<(), String> {
        let outcome = self
            .native_runtime
            .dispatch(instance, view, event)
            .map_err(|error| format!("native completion failed: {error:?}"))?;
        for effect in outcome.effects {
            match effect {
                AppEffect::RepaintSelf(damage) => {
                    self.invalidate_native_view_cache(wid, view, damage);
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.last_present = None;
                    }
                }
                AppEffect::InvalidateOwnPresentation => {
                    if let Some(ws) = self.windows.get_mut(&wid) {
                        ws.last_present = None;
                    }
                }
                other => self.execute_native_effect(wid, instance, view, other)?,
            }
        }
        // Completion reducers remove pending work even when the host rejected
        // or blocked it. Refresh from the reducer's final presentation on every
        // accepted completion so the tab's busy/attention state cannot remain
        // one request behind its Settings view.
        self.refresh_native_presentation(wid, instance, view);
        if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            window.request_redraw();
        }
        Ok(())
    }

    fn execute_native_update(&mut self, request: UpdateRequest) -> UpdateOutcome {
        match request {
            UpdateRequest::Check | UpdateRequest::Retry => self.start_native_update_check(),
            UpdateRequest::InstallAndRelaunch => self.apply_native_update(ApplyMode::Immediate),
            UpdateRequest::InstallWhenSafe => {
                if self.native_updater_service.install_when_safe()
                    || self.native_updater_service.snapshot().install_on_clean_quit
                {
                    self.publish_native_update_state();
                    UpdateOutcome::Accepted
                } else {
                    UpdateOutcome::Blocked {
                        reasons: vec!["No newer verified update is staged".to_string()],
                    }
                }
            }
        }
    }

    /// Admit one Packages verb. Physical work (spawning the CO-LOCATED `atpkg`
    /// binary, reading its `status.toml`) always happens on a worker thread —
    /// this admission is memory-only plus one `current_exe`-sibling stat.
    pub(crate) fn execute_native_packages(&mut self, request: PackagesRequest) -> PackagesOutcome {
        let atpkg = crate::co_located_atpkg();
        if let Err(message) = packages_request_admissible(&request) {
            return PackagesOutcome::Failed { message };
        }
        let argv = packages_argv(&request);
        // `atpkg machine apply` is a LOCAL verb: no store, no index, no root key —
        // it works in a build whose manager is inert, so the manager gate below does
        // not apply to it. Its stdout is captured too: the
        // `machine-settings:` row and the verdict sentence ride it.
        let machine_apply = matches!(request, PackagesRequest::MachineApply);
        let check = matches!(request, PackagesRequest::CheckUpdate);
        let Some(atpkg) = atpkg else {
            return PackagesOutcome::Failed {
                message: "no co-located atpkg binary beside this executable".to_string(),
            };
        };
        if !machine_apply && !atpkg::manager_enabled() {
            // Same trust posture the binary itself enforces; refusing here is
            // honesty, not authority — atpkg would refuse loudly anyway.
            return PackagesOutcome::Blocked {
                message:
                    "the package manager is inert (no package root key is pinned in this build)"
                        .to_string(),
            };
        }
        if self.native_packages_service.busy().is_some() {
            return PackagesOutcome::Blocked {
                message: "a packages operation is already running".to_string(),
            };
        }
        let busy = packages_busy(&request);
        let Some(proxy) = self.proxy.clone() else {
            return PackagesOutcome::Failed {
                message: "packages verbs require the event-loop service".to_string(),
            };
        };
        let Some(sequence) = self.native_packages_service.begin(Some(busy)) else {
            // Backstop only: the page disables its buttons whenever ANY worker
            // (including the silent status refresh) is inflight, so reaching
            // this means a click raced the disable — say so in user voice.
            return PackagesOutcome::Blocked {
                message: "Still collecting package status — try again in a moment.".to_string(),
            };
        };
        // The busy flip is observable: publish before the worker starts.
        self.publish_native_packages_state();
        let spawn = std::thread::Builder::new()
            .name("aterm-packages-verb".into())
            .spawn(move || {
                // A package verb: the SAME lane, store and children as the
                // six-hourly pass, on a click. Nobody is blocked on it, and a
                // role does not reach a thread from its creator (see qos.rs).
                crate::qos::set_self(crate::qos::Role::Background);
                // atpkg records detailed durable status in status.toml. Keep
                // the process result separately: the old status may predate a
                // failed launch/non-zero exit and must never be presented as
                // the result of this attempt.
                // STDERR IS KEPT. atpkg explains its refusals there — "another
                // atpkg process holds the store lock at …" is the common one, since
                // the six-hourly loop can hold that lock for the length of a
                // multi-GB download. Discarding it turned an explainable wait into
                // "atpkg update exited with exit status: 1", repeated on every retry
                // for as long as the download ran (2026-08-20 round-8 audit).
                let mut command = PackagesCommandOutcome::Succeeded { operation: busy };
                let mut machine_verdict: Option<String> = None;
                let mut machine_state: Option<atpkg::machine::MachineState> = None;
                let layout = atpkg::store::resolve_configured();
                // THE CHECK'S ONE FAIL-FAST: a store held by a PERSON's typed verb (a
                // terminal's `aterm pkg …`, `claude update`) is answered at once — a click
                // must not queue silently behind work someone is watching. Behind any lane
                // (the window's own pass, a session's) it waits like the window's passes.
                if check && let Some(pid) = layout.as_ref().and_then(atpkg::lock::person_holder) {
                    command = PackagesCommandOutcome::Failed {
                        operation: busy,
                        message: check_held_by_person_message(pid),
                    };
                }
                // The one child, unless the fail-fast above already answered (`break` and
                // `continue` below end it early, with the outcome already set).
                let process =
                    (!matches!(command, PackagesCommandOutcome::Failed { .. })).then_some(argv);
                for verb in process.iter() {
                    // NO STDIN: a windowed child inherits whatever the app was launched
                    // with (a Terminal's tty when run from one), and a child can never
                    // sit on a prompt nobody sees.
                    //
                    // STDOUT IS NULL for every verb but the machine apply: the install
                    // passes stream their multi-GB marker contract there, and this
                    // worker is not their reader. The machine apply prints a few lines
                    // — the `machine-settings:` row (which the pull-down and the
                    // Security card both want) and one verdict sentence — so those are
                    // captured and fed through the same marker parser the launch pass
                    // uses (`spawn_machine_settings_once`).
                    // A PASS MAY APPLY THE HOST SETTINGS TOO, so its stdout is worth
                    // reading: `packages/check` (an update pass) and every install carry
                    // an EDIT to the `[machine]` table at their edge (Phase 3) — and with
                    // stdout nulled, the `machine-settings:` marker they print went
                    // nowhere: the card never learned about a change a Settings-initiated
                    // pass had just made, and the pull-down row never appeared.
                    let reads_stdout = machine_apply || busy.applies_machine_settings();
                    // Upstream's QoS-classed spawner, kept: a background pass must not
                    // compete with the window for the scheduler.
                    let mut child = crate::qos::command(crate::qos::Role::Background, &atpkg);
                    // THE CHECK IS THE WINDOW'S OWN PASS (Phase 3): the same argv
                    // (`--wait-lock` with the lanes' bound, `--progress-file`), the
                    // spawner's pid and the login shell's PATH — so a window that quits
                    // mid-check hands the child to the orphan watch instead of killing it
                    // at its next print — and it is run the lanes' way below, so its
                    // waiting row streams like any pass's.
                    if check {
                        child
                            .args(crate::pass_args(
                                crate::PassVerb::Update,
                                layout.as_ref(),
                                None,
                            ))
                            .env(atpkg::cli::SPAWNER_PID_ENV, std::process::id().to_string())
                            .env("PATH", crate::spawn::atpkg_child_path());
                    } else {
                        child.args(verb);
                    }
                    child
                        .stdin(std::process::Stdio::null())
                        .stdout(if reads_stdout {
                            std::process::Stdio::piped()
                        } else {
                            std::process::Stdio::null()
                        })
                        .stderr(std::process::Stdio::piped());
                    if machine_apply {
                        child
                            .env(atpkg::cli::SPAWNER_PID_ENV, std::process::id().to_string())
                            .env("PATH", crate::spawn::atpkg_child_path());
                    }
                    if machine_apply {
                        let result = machine_command_output_bounded(
                            &mut child,
                            "atpkg machine apply",
                            std::time::Duration::from_secs(60),
                            // A mutating child is never killed for a flood — see
                            // `Overflow`. Only the deadline can stop it.
                            Overflow::Truncate,
                        );
                        let read = machine_apply_completion(result, |event| {
                            let _ = proxy.send_event(event);
                        });
                        command = read.0;
                        machine_verdict = read.1;
                        machine_state = read.2;
                        if matches!(command, PackagesCommandOutcome::Failed { .. }) {
                            break;
                        }
                        continue;
                    }
                    // THE CHECK STREAMS, as the lanes' passes do ([`crate::run_pass_child`]):
                    // queued behind another pass, its waiting row shows at once instead of
                    // a busy page with no reason for up to its 30-minute bound. Not its
                    // `machine-state:` record, for the reason below.
                    if check {
                        let post_proxy = proxy.clone();
                        let post = move |event: Wake| {
                            if !matches!(event, Wake::PkgMachineState(_)) {
                                let _ = post_proxy.send_event(event);
                            }
                        };
                        let ran = crate::run_pass_child(&mut child, layout.as_ref(), false, &post);
                        let (said, result) = match ran {
                            Ok((run, status)) => (run.said.trim().to_string(), status),
                            Err(error) => (String::new(), Err(error)),
                        };
                        if let Some(message) =
                            packages_child_failure(result.as_ref().copied(), verb, &said)
                        {
                            command = PackagesCommandOutcome::Failed {
                                operation: busy,
                                message,
                            };
                            break;
                        }
                        continue;
                    }
                    let result = child.output();
                    if reads_stdout && let Ok(out) = result.as_ref() {
                        // The pass's own machine lines, through the same reader the
                        // launch lanes use, so one parser serves every lane. NOT the
                        // pass's `machine-state:` record: this lane collects stdout at
                        // exit, and the pass measured the machine at its top — minutes
                        // earlier for an update that waited on the lock or downloaded —
                        // so that record is not the newest state and must not be taken
                        // as one (review 2026-09-16). The completion re-reads instead,
                        // and its read closes the expectation the change line opens.
                        crate::read_seed_markers(
                            std::io::BufReader::new(out.stdout.as_slice()),
                            |event| {
                                if !matches!(event, Wake::PkgMachineState(_)) {
                                    let _ = proxy.send_event(event);
                                }
                            },
                        );
                    }
                    let said = result
                        .as_ref()
                        .ok()
                        .map(|out| String::from_utf8_lossy(&out.stderr).trim().to_string())
                        .unwrap_or_default();
                    let result = result.as_ref().map(|out| out.status);
                    if let Some(message) = packages_child_failure(result, verb, &said) {
                        command = PackagesCommandOutcome::Failed {
                            operation: busy,
                            message,
                        };
                        break;
                    }
                }
                let report = crate::packages_screen::collect_packages_status(true);
                let completion = PackagesWorkerCompletion::command(report, command)
                    .with_machine_verdict(machine_verdict)
                    .with_machine_state(machine_state);
                let _ = proxy.send_event(Wake::NativePackagesFinished {
                    sequence,
                    completion,
                });
            });
        match spawn {
            Ok(_) => PackagesOutcome::Accepted,
            Err(error) => {
                // Roll the reservation back so the surface does not stay busy
                // forever; the synchronous failure is the user feedback. abort
                // (not finish) keeps any previously-observed report's real
                // facts — no worker ran, so there is nothing new to store.
                let _ = self.native_packages_service.abort(sequence);
                self.publish_native_packages_state();
                PackagesOutcome::Failed {
                    message: format!("could not start the packages worker: {error}"),
                }
            }
        }
    }

    /// Start one status-collection worker (no verb). A running worker makes
    /// this a no-op join; headless hosts (no proxy) skip silently.
    pub(crate) fn start_native_packages_refresh(&mut self) {
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        let Some(sequence) = self.native_packages_service.begin(None) else {
            return;
        };
        // The stat is `current_exe`-sibling metadata (cheap); the status.toml
        // parse stays on the worker.
        let available = crate::co_located_atpkg().is_some();
        let spawn = std::thread::Builder::new()
            .name("aterm-packages-status".into())
            .spawn(move || {
                // QoS (port of 61a6c8b62): a package inventory probe is
                // cosmetic to the terminal; it lands via the proxy when done.
                crate::qos::set_self(crate::qos::Role::Background);
                let report = crate::packages_screen::collect_packages_status(available);
                let completion = PackagesWorkerCompletion::refresh(report);
                let _ = proxy.send_event(Wake::NativePackagesFinished {
                    sequence,
                    completion,
                });
            });
        if spawn.is_err() {
            // Release the reservation via abort (never a fabricated report):
            // a never-observed surface honestly stays on "Reading package
            // status…", and a previously-observed one keeps its real facts.
            let _ = self.native_packages_service.abort(sequence);
        }
        self.publish_native_packages_state();
    }

    /// Start one machine read — bare `atpkg machine`, no verb, no store lock — so
    /// the Security page's "This Mac" card CONFIRMS the `[machine]` settings from
    /// what the co-located atpkg measured, never from an in-process `defaults`
    /// read or a home walk (the one sanctioned seam for touching the machine is
    /// the atpkg child). A read asked for while one is running is QUEUED behind
    /// it (`PackagesService::request_machine_read`), never joined — the running
    /// read may predate an apply. Headless hosts (no proxy) and non-macOS builds
    /// skip silently, before anything is queued — the card is absent there.
    pub(crate) fn start_native_machine_refresh(&mut self) {
        if !cfg!(target_os = "macos") {
            return;
        }
        let Some(proxy) = self.proxy.clone() else {
            return;
        };
        dispatch_native_machine_read(
            &mut self.native_packages_service,
            MachineReadEvent::Request,
            || launch_native_machine_read(proxy),
        );
        self.publish_native_packages_state();
    }

    /// A queued rerun is already admitted by the completion reducer. Dispatch it
    /// directly; requesting admission again would mistake that reservation for
    /// a running worker and leave the card refreshing forever.
    pub(crate) fn finish_native_machine_refresh(
        &mut self,
        result: Result<atpkg::machine::MachineState, String>,
    ) {
        let proxy = self.proxy.clone();
        dispatch_native_machine_read(
            &mut self.native_packages_service,
            MachineReadEvent::Finished(result),
            || {
                let proxy =
                    proxy.ok_or_else(|| "machine read host is no longer available".to_string())?;
                launch_native_machine_read(proxy)
            },
        );
        self.publish_native_packages_state();
    }

    /// Main-thread half of the packages worker protocol (the packages analogue
    /// of [`Self::finish_native_update_check`]): stale sequences are inert. A
    /// finished verb that ran atpkg's machine pass ([`PackagesBusy::applies_machine_settings`])
    /// re-reads the machine so the card confirms rather than assumes — except
    /// `machine apply` itself when its completion carries the apply's own
    /// `machine-state:` record (2026-09-16: a short child whose record is printed
    /// last; the reducer took it as the newest state, so nothing is left to
    /// confirm). The collected lanes (`update`, every `install`) hand their stdout
    /// over at exit, minutes after the pass measured the machine at its top, so their
    /// record is never posted and the completion's read stays; that read — one read,
    /// which is why the apply lane's sorter posts no record-missing fallback of its
    /// own — is what closes the expectation their change line opened.
    pub(crate) fn finish_native_packages(
        &mut self,
        sequence: u64,
        completion: crate::packages_screen::PackagesWorkerCompletion,
    ) {
        let finished = self.native_packages_service.busy();
        let confirmed_by_its_record =
            finished == Some(PackagesBusy::MachineApply) && completion.machine_state.is_some();
        if !self.native_packages_service.finish(sequence, completion) {
            return;
        }
        self.publish_native_packages_state();
        if finished.is_some_and(PackagesBusy::applies_machine_settings) && !confirmed_by_its_record
        {
            let _ = self.native_packages_service.take_record_expectation();
            self.start_native_machine_refresh();
        }
    }

    /// Publish the Settings ▸ Messages projection to the Settings controller
    /// and fan the revision out to every Settings view (design §4.2, the
    /// [`Self::publish_native_packages_state`] shape): the projection is
    /// built ONCE from the center and the log (`App::messages_state`), and
    /// every view repaints from the controller's copy. Unconditional — the
    /// caller that opens Settings wants the page current now; the gated
    /// twin [`Self::publish_native_messages_state_if_due`] serves the sync
    /// and the park. A no-op with no Settings instance to hold it.
    pub(crate) fn publish_native_messages_state(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        let state = self.messages_state();
        let revision = state.revision;
        if !self
            .native_runtime
            .replace_settings_messages(state, revision)
        {
            return;
        }
        self.messages_last_publish = Some(std::time::Instant::now());
        self.messages_published_revision = revision;
        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in &views {
            let _ = self.native_runtime.dispatch(
                *instance,
                *view,
                AppEvent::MessagesChanged { revision },
            );
        }
        let setting_views = views
            .iter()
            .map(|(_, view)| *view)
            .collect::<std::collections::BTreeSet<_>>();
        for ws in self.windows.values_mut() {
            let shows_settings = ws.tab_set.tabs().iter().any(|tab| {
                tab.root
                    .leaves()
                    .iter()
                    .any(|view| setting_views.contains(view))
            });
            if shows_settings {
                ws.last_present = None;
                if let Some(window) = ws.os_window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    /// [`Self::publish_native_messages_state`] when it is DUE (design §4.2):
    /// the center moved since the last publish and at least
    /// [`MESSAGES_PUBLISH_MIN_GAP`] has passed — a downloading update
    /// restates at the tailer's 10 Hz and the page repaints at 2 at most —
    /// or [`MESSAGES_PUBLISH_TICK`] has passed while any Settings view is on
    /// the Messages route, so `3 min ago` becomes `4 min ago` with nothing
    /// posted. Run after every settle and on the way to every wait (the
    /// route-gated per-park pattern of `sync_settings_consent_posture`); a
    /// publish the gap holds back is re-armed through `messages_deadline`.
    /// Costs one enum compare per window off the route, nothing with no
    /// Settings instance.
    pub(crate) fn publish_native_messages_state_if_due(&mut self, now: std::time::Instant) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        let since = self
            .messages_last_publish
            .map(|at| now.saturating_duration_since(at));
        let moved = self.messages.revision() != self.messages_published_revision;
        let due = match since {
            None => true,
            Some(since) if moved => since >= MESSAGES_PUBLISH_MIN_GAP,
            Some(since) => since >= MESSAGES_PUBLISH_TICK && self.settings_view_on_messages(),
        };
        if due {
            self.publish_native_messages_state();
        }
    }

    /// The instant the next publish falls due, for the loop's deadline fold —
    /// the twin of [`Self::publish_native_messages_state_if_due`]'s predicate,
    /// so the loop is asked back for exactly what that park will do: the 2 Hz
    /// hold-back of a publish the center's move earned, or, with the center
    /// quiet, the [`MESSAGES_PUBLISH_TICK`] while any Settings view is on the
    /// Messages route (design §4.2: `3 min ago` becomes `4 min ago` in a
    /// window nobody touches — the loop is `Wait` when idle, so a tick nobody
    /// arms never fires). `None` with no Settings instance to publish into,
    /// or with the center quiet and no view on the route, so an idle band
    /// still arms no wake (FL-1).
    pub(crate) fn messages_publish_due_at(&self) -> Option<std::time::Instant> {
        self.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)?;
        let gap = if self.messages.revision() != self.messages_published_revision {
            MESSAGES_PUBLISH_MIN_GAP
        } else if self.settings_view_on_messages() {
            MESSAGES_PUBLISH_TICK
        } else {
            return None;
        };
        Some(
            self.messages_last_publish
                .map_or_else(std::time::Instant::now, |at| at + gap),
        )
    }

    /// Whether any window's Settings view is on the Messages route right
    /// now — the gate for the relative-time tick.
    fn settings_view_on_messages(&self) -> bool {
        self.view_store.iter().any(|(view, link)| {
            matches!(link, crate::tab_model::View::Native(_))
                && matches!(
                    self.native_runtime.view_state(view),
                    Some(crate::native_app::AppViewState::Settings(state))
                        if state.route == crate::native_settings::SettingsRoute::Messages
                )
        })
    }

    /// Publish the shared packages projection to the Settings controller and
    /// fan the revision out to every Settings view (the packages analogue of
    /// [`Self::publish_native_update_state`]).
    pub(crate) fn publish_native_packages_state(&mut self) {
        let revision = self.native_packages_service.revision();
        let state = self
            .native_packages_service
            .state(
                self.config.packages_enabled(),
                self.config.packages_auto_install(),
                self.package_update_loop_running,
            )
            .with_machine_config(
                self.config.machine_universal_control(),
                self.config.machine_spotlight_noindex(),
            )
            .with_retired_switch_note(self.config.packages_retired_switch_note());
        if !self
            .native_runtime
            .replace_settings_packages(state, revision)
        {
            return;
        }
        self.dispatch_to_settings_views(&AppEvent::PackagesChanged { revision });
    }

    /// Deliver `event` to every Settings view and repaint every window that shows one — the
    /// fan-out after a shared Settings projection (packages, the log) was replaced.
    fn dispatch_to_settings_views(&mut self, event: &AppEvent) {
        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in &views {
            let _ = self
                .native_runtime
                .dispatch(*instance, *view, event.clone());
        }
        let setting_views = views
            .iter()
            .map(|(_, view)| *view)
            .collect::<std::collections::BTreeSet<_>>();
        for ws in self.windows.values_mut() {
            let count = ws.tab_set.len();
            let mut shows_settings = false;
            for index in 0..count {
                let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                    continue;
                };
                if tab
                    .root
                    .leaves()
                    .iter()
                    .any(|view| setting_views.contains(view))
                {
                    shows_settings = true;
                }
            }
            if shows_settings {
                ws.last_present = None;
                if let Some(window) = ws.os_window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    pub(crate) fn execute_recovery_request(
        &mut self,
        wid: WindowId,
        request: crate::native_app::RecoveryRequest,
    ) -> crate::native_app::RecoveryOutcome {
        use crate::native_app::{RecoveryCapability, RecoveryOutcome, RecoveryRequest};

        let open_document =
            |app: &mut Self, kind: crate::native_app::AppKind, uri: String| -> RecoveryOutcome {
                let safe = uri.len() <= 4_096
                    && uri.starts_with("file:///")
                    && !uri
                        .chars()
                        .any(|character| matches!(character, '\0' | '\r' | '\n'))
                    && matches!(
                        kind,
                        crate::native_app::AppKind::Markdown | crate::native_app::AppKind::Editor
                    );
                if !safe {
                    return RecoveryOutcome::Denied {
                        message: "The retained document capability is invalid or unsafe"
                            .to_string(),
                    };
                }
                match app.open_document_tab_in_window(wid, kind, &uri) {
                    Ok(_) => RecoveryOutcome::Opened {
                        message: format!("Opened original in {}", kind.as_str()),
                    },
                    Err(message) => RecoveryOutcome::Failed { message },
                }
            };

        match request {
            RecoveryRequest::Retry(RecoveryCapability::Settings { route }) => {
                let Some(route) = crate::native_settings::SettingsRoute::from_path(&route) else {
                    return RecoveryOutcome::Denied {
                        message: "The retained Settings route is unavailable".to_string(),
                    };
                };
                if self.open_settings_tab(route) {
                    RecoveryOutcome::Opened {
                        message: format!("Reopened Settings · {}", route.label()),
                    }
                } else {
                    RecoveryOutcome::Failed {
                        message: "Settings could not be reopened".to_string(),
                    }
                }
            }
            RecoveryRequest::Retry(RecoveryCapability::Document {
                kind,
                uri,
                config_editor,
            }) => {
                if config_editor {
                    match self.ensure_and_open_config_editor_in_window(wid) {
                        Ok(_) => RecoveryOutcome::Opened {
                            message: "Reopened aterm.toml in Manual configuration mode".to_string(),
                        },
                        Err(message) => RecoveryOutcome::Failed { message },
                    }
                } else {
                    open_document(self, kind, uri)
                }
            }
            RecoveryRequest::OpenOriginal { uri } => {
                open_document(self, crate::native_app::AppKind::Editor, uri)
            }
        }
    }

    pub(crate) fn start_native_update_check(&mut self) -> UpdateOutcome {
        // Joining is pure with respect to physical work and does not require minting a
        // new event-loop capability. This fast path also makes the no-duplicate promise
        // directly testable in a headless host.
        if self.native_updater_service.snapshot().active.is_some() {
            return match self.native_updater_service.request_check() {
                CheckStart::Joined(_) => UpdateOutcome::Accepted,
                _ => UpdateOutcome::Failed {
                    message: "the active updater ticket could not be joined".to_string(),
                },
            };
        }
        let Some(proxy) = self.proxy.clone() else {
            return UpdateOutcome::Failed {
                message: "update checks require the event-loop service".to_string(),
            };
        };
        match self.native_updater_service.request_check() {
            CheckStart::Joined(_) => UpdateOutcome::Accepted,
            CheckStart::Rejected(block) => match block {
                CheckBlock::Disabled => UpdateOutcome::Failed {
                    message: "aterm updates itself on macOS only".to_string(),
                },
                CheckBlock::UpdateAlreadyStaged => UpdateOutcome::Blocked {
                    reasons: vec!["A verified update is already ready to install".to_string()],
                },
                CheckBlock::Applying => UpdateOutcome::Blocked {
                    reasons: vec!["An update is already being applied".to_string()],
                },
                CheckBlock::GenerationExhausted => UpdateOutcome::Failed {
                    message: "updater identity space is exhausted".to_string(),
                },
            },
            CheckStart::Start(ticket) => {
                self.publish_native_update_state();
                let provider = crate::update_control::check_settings_provider(proxy.clone());
                let snapshot = self.native_updater_service.snapshot();
                let build = snapshot.current_build;
                let spawn = std::thread::Builder::new()
                    .name("aterm-native-update-check".into())
                    .spawn(move || {
                        // QoS (port of 61a6c8b62): a network update probe;
                        // nobody at the keyboard is blocked on its answer.
                        crate::qos::set_self(crate::qos::Role::Background);
                        let status = aterm_update::check_now_with_settings(build, &provider);
                        let _ = proxy.send_event(Wake::NativeUpdateFinished {
                            ticket,
                            status: durable_update_status(status),
                        });
                    });
                match spawn {
                    Ok(_) => UpdateOutcome::Accepted,
                    Err(error) => {
                        let message = format!("could not start updater worker: {error}");
                        self.finish_native_update_check(
                            ticket,
                            failed_update_status(build, message.clone()),
                        );
                        UpdateOutcome::Failed { message }
                    }
                }
            }
        }
    }

    /// Main-thread half of the updater worker protocol. Service-owned completion
    /// remains valid after every Settings view closes and publishes one revision
    /// to all Settings subscribers.
    pub(crate) fn finish_native_update_check(
        &mut self,
        ticket: UpdaterWorkTicket,
        status: DurableUpdateStatus,
    ) {
        let completion = self.native_updater_service.finish_check(ticket, status);
        if completion != CheckCompletion::Reduced {
            // An old worker cannot redraw, notify, arm intent, or drain a newer
            // deferred observation. Reducer-inert means presentation-inert too.
            return;
        }
        if self.native_updater_service.snapshot().staged.is_some() {
            // The floor for disk observations: anything read before this instant
            // predates the stage this check imported (see
            // `reconcile_native_update_facts`).
            self.native_stage_imported_at = Some(std::time::Instant::now());
        }
        self.publish_native_update_state();

        // Facts parked while THIS check was active were observed BEFORE it staged
        // anything: replaying them now would compare the stage the check just
        // imported against a durable marker that did not yet exist and RETIRE it
        // ("stale in-memory stage retired after durable marker changed"), leaving a
        // verified stage on disk that nothing arms until the next background cycle
        // (2026-08-19 round-2 audit — a "Check for Updates…" click parks a Refresh
        // behind its own check). Keep the PURPOSE (a control apply must not be
        // lost) and re-observe the disk fresh, with the stage now present.
        if let Some((purpose, _stale)) = self.deferred_native_update_reconcile.take()
            && !self.request_native_update_reconcile(purpose)
        {
            if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                // A control apply never vanishes silently: the same refusal the control
                // entry path surfaces when facts cannot be collected.
                let reason = "Updater facts could not be collected safely";
                aterm_update::record_apply_refusal(
                    self.native_updater_service.snapshot().current_build,
                    reason,
                );
                self.surface_update_apply_outcome(
                    "control request",
                    UpdateOutcome::Blocked {
                        reasons: vec![reason.to_string()],
                    },
                    false,
                );
            } else {
                aterm_log::debug!(
                    "update check: could not re-request the reconcile a parked {purpose:?} asked for"
                );
            }
        }
        if self.native_updater_service.snapshot().phase == UpdaterPhase::Applying {
            return;
        }

        if let Some((build, digest)) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.dmg_sha256.clone()))
        {
            self.arm_native_auto_apply(build, &digest);
        }
        self.try_pending_native_auto_apply(true);
    }

    /// Retain automatic apply intent for one exact (or superseding) staged build.
    /// Returns true only when this call armed a new intent.
    /// Let a manual-only latch lapse once its deadline passes, restoring
    /// automatic apply for that artifact.
    ///
    /// Only the physical-failure schedule writes a latch: a retry inside its
    /// epoch, a stand-down between epochs ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`]),
    /// and `None` at convergence. A `retry_at: None` latch also survives as the
    /// fail-safe for a policy/outcome mismatch that no code path is supposed to
    /// reach; it never lapses, which is the correct answer for a state nobody
    /// understands.
    ///
    /// Returns true when a latch was actually released.
    pub(crate) fn lapse_expired_auto_apply_manual_only(&mut self) -> bool {
        let now = std::time::Instant::now();
        let lapsed = self
            .auto_apply_manual_only
            .is_some_and(|manual| manual.retry_at.is_some_and(|at| now >= at));
        if lapsed {
            aterm_log::info!(
                "update apply: the manual-only latch lapsed; automatic apply is \
                 eligible again"
            );
            self.auto_apply_manual_only = None;
            self.auto_overlap_retry = None;
            // `auto_apply_ladder` is deliberately NOT cleared either (2026-09-21).
            // It used to be, "so the retry after a genuine failure prefers a
            // quiet moment again" — which handed every physical failure a
            // fresh fifteen minutes on top of its 600 s latch, and on a
            // terminal that is never quiet meant a landing no earlier than
            // ~20 min after the FIRST arming for one unlucky dial. The bound
            // is per artifact, stated once, and counts from the first arming;
            // the latch's own spacing is all the quiet-moment preference a
            // physical retry gets. The anchor survives, the phase resumes
            // where the clock is, and a lapse at `Land` lands at the next poll.
            // `auto_apply_physical_retry` is deliberately NOT cleared. It is the
            // budget deciding how many physical retries remain, and since a
            // physical latch now lapses too, clearing it here would hand out fresh
            // budget on every lapse and turn a bounded retry into a permanent
            // ten-minute loop. Its own replenish window is what forgives an
            // artifact eventually.
        }
        lapsed
    }

    /// Authenticate the staged candidate OFF the GUI thread and OUTSIDE the
    /// parked window, caching the verdict for the next handoff attempt.
    ///
    /// Idempotent and cheap to call: a fresh verdict for the same
    /// `(build, commit)` short-circuits. A failure is cached too — a doomed
    /// candidate then fails preparation immediately rather than after parking
    /// every reader and spending a third of a second on `codesign`.
    fn spawn_staged_handoff_preverification(&mut self, build: u64) {
        let snapshot = self.native_updater_service.snapshot();
        let current_build = snapshot.current_build;
        let stage = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == build);
        // An ACTIVATION verifies the bundle under the executable, not a staged `.app`.
        let installed_activation = stage.is_some_and(|staged| staged.is_installed_activation());
        let commit = stage.and_then(|staged| staged.commit.clone());
        let artifact = stage.map(|staged| staged.dmg_sha256.clone());
        // Without a pinned commit the worker's own call would be a different
        // (weaker) query, so do not cache a verdict that would not match it.
        let (Some(commit), Some(artifact)) = (commit, artifact) else {
            return;
        };
        {
            let cached = self
                .handoff_preverified
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cached.as_ref().is_some_and(|entry| {
                entry.build == build
                    && entry.commit == commit
                    && entry.artifact == artifact
                    && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
            }) {
                return;
            }
        }
        let slot = std::sync::Arc::clone(&self.handoff_preverified);
        let spawned = std::thread::Builder::new()
            .name("aterm-update-preverify".to_string())
            .spawn(move || {
                // NO `qos::set_self` HERE, DELIBERATELY. This is the one worker of
                // the update lane that keeps the class it inherits. It publishes
                // into `handoff_preverified`, a mutex the UI thread takes with a
                // blocking `lock()` (`cached_handoff_preverification`), so qos.rs's
                // port-time floor rule forbids demoting it below `Responsive`: a
                // descheduled holder there is a priority inversion. Promoting it is
                // not right either — it is one ~0.3 s `codesign` per staged
                // candidate, hoisted out of the parked window precisely so nothing
                // waits on it, and a miss just re-verifies inline on the handoff
                // worker. DEFAULT sits between the two, which is what it wants.
                let passed = if installed_activation {
                    aterm_update::preverify_installed_for_handoff(current_build, build, &commit)
                } else {
                    aterm_update::preverify_staged_for_handoff(
                        current_build,
                        Some(crate::build_info::GIT_COMMIT),
                        Some(build),
                        Some(&commit),
                    )
                };
                let which = if installed_activation {
                    "installed bundle"
                } else {
                    "staged update"
                };
                if let Err(error) = passed.as_ref() {
                    aterm_log::warn!(
                        "update apply: {which} build {build} failed pre-park verification: {error}"
                    );
                }
                let reason = passed
                    .as_ref()
                    .err()
                    .map(|error| format!("{which} failed pre-park verification: {error}"));
                *slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(crate::HandoffPreverification {
                        build,
                        commit,
                        artifact,
                        at: std::time::Instant::now(),
                        passed: passed.is_ok(),
                        reason,
                    });
            });
        if spawned.is_err() {
            // No thread: the worker simply verifies in-line as it always did.
            aterm_log::warn!("update apply: pre-park verification thread unavailable");
        }
    }

    /// Re-arm automatic apply for whatever is still staged, after a latch
    /// lapsed. Reads the SERVICE snapshot (in-memory authority), performs no
    /// disk or network work, and is a no-op when nothing is staged.
    pub(crate) fn rearm_native_auto_apply_after_lapse(&mut self) {
        if self.native_updater_service.snapshot().phase == UpdaterPhase::Applying {
            return;
        }
        let Some((build, digest)) = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.dmg_sha256.clone()))
        else {
            return;
        };
        self.arm_native_auto_apply(build, &digest);
    }

    /// The close-preflight blocker for unsaved native-app (editor / Settings)
    /// work — the one blocker a person can clear, so it is also the update
    /// row's words ([`Self::update_blocker_for_person`]): what to do, point
    /// first. Never "before relaunching": the install is in place.
    pub(crate) const UNSAVED_NATIVE_WORK_BLOCKS_APPLY: &'static str =
        "Save or close the open editor to finish updating";
    /// The close-preflight blocker while session restore / adoption is in flight
    /// — it clears by itself.
    pub(crate) const RESTORE_IN_FLIGHT_BLOCKS_APPLY: &'static str =
        "Updating after session restore finishes";
    /// The `InstalledNeedsRelaunch` outcome's sentence: the bundle on disk is
    /// already the newer build. Switching to it has its own admission and
    /// automatic policy, so installation alone cannot promise when it will run.
    pub(crate) const INSTALLED_ACTIVATES_IN_PLACE: &'static str =
        "The update is installed; aterm switches to it in place";

    /// Whether a staged build is expected to land BY ITSELF — what the updater's
    /// overdue notice ([`aterm_update::set_automatic_apply`]) claims failed when
    /// a newer build has waited over an hour. `[update] auto_apply` on, and
    /// neither of the postures in which waiting is the design: an in-session
    /// handoff this process cannot run ([`crate::update_words::ApplyPosture::HandoffDisabled`],
    /// a record whose row promises nothing — nothing installs while a terminal
    /// is open), or unsaved work a person has to save
    /// ([`Self::update_blocker_for_person`], already its own row). Either used
    /// to raise "aterm can't install updates" and an OS banner after an hour
    /// over the row that names what to do.
    pub(crate) fn update_lands_by_itself(&self, auto_apply_on: bool) -> bool {
        self.update_lands_by_itself_with(auto_apply_on, self.seamless_handoff_unavailable())
    }

    /// [`Self::update_lands_by_itself`] with the handoff's availability
    /// supplied, as [`Self::apply_posture_with`] takes it.
    fn update_lands_by_itself_with(
        &self,
        auto_apply_on: bool,
        handoff_unavailable: Option<crate::app_update_handoff::HandoffUnavailable>,
    ) -> bool {
        auto_apply_on && handoff_unavailable.is_none() && self.update_waits_on_person.is_none()
    }

    /// The update row for a close-preflight refusal the PERSON can clear —
    /// unsaved editor or Settings work, a document checkpoint that failed —
    /// or `None` when every reason clears by itself (a checkpoint still
    /// running, a restore landing), which is the lane's to wait out without a
    /// word. The reasons are this file's own (`native_update_close_preflight`,
    /// the shutdown preflight's [`Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY`]).
    pub(crate) fn update_blocker_for_person(reasons: &[String]) -> Option<&'static str> {
        reasons
            .iter()
            .any(|reason| {
                reason == Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY
                    || reason.starts_with(SETTINGS_DRAFTS_BLOCK)
                    || reason.starts_with(DIRTY_DOCUMENTS_BLOCK)
                    || reason.starts_with(FAILED_CHECKPOINTS_BLOCK)
            })
            .then_some(Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
    }

    /// `true` while the `ATERM_DEBUG_RELAUNCH_NUDGE` screenshot seam is
    /// suppressing the automatic update lane — and it SAYS SO, once per process,
    /// the first time either the arm or the poll asks. A DEVELOPMENT SEAM
    /// (`aterm_types::dev_seam!`, 2026-09-23): a shipped binary never reads it.
    ///
    /// The seam seeds a fake `Wake::UpdateStaged` so the update-ready nudge can be
    /// captured with `ctl image` without waiting for a real background stage, and
    /// a fake nudge must not be able to install anything. That is the right
    /// intent. The wrong part was the silence: both `arm_native_auto_apply` and
    /// `poll_native_auto_apply` folded this `var_os` into `enabled` with no
    /// notice anywhere, so exporting a screenshot variable turned OFF automatic
    /// update application — a feature this project ships ON — invisibly, in a
    /// release build. The test at the bottom of this file has to assert the
    /// variable's ABSENCE or "every poll below answers Clear and this test proves
    /// nothing", which is the same hazard seen from the inside.
    pub(crate) fn relaunch_nudge_seam_suppresses_auto_apply() -> bool {
        static SEAM: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *SEAM.get_or_init(|| {
            let on = aterm_types::dev_seam!("ATERM_DEBUG_RELAUNCH_NUDGE").is_some();
            if on {
                aterm_log::warn!(
                    "$ATERM_DEBUG_RELAUNCH_NUDGE is set: this is the QA/screenshot \
                     seam for the update-ready nudge, and it DISABLES automatic update \
                     application for the whole process — no staged build will be \
                     armed or applied, whatever `[update] auto_apply` says. Unset it \
                     to restore the default."
                );
            }
            on
        })
    }

    pub(crate) fn arm_native_auto_apply(&mut self, build: u64, digest: &str) -> bool {
        use crate::native_update_auto_intent::{ArmDecision, ArmFacts};

        self.lapse_expired_auto_apply_manual_only();
        let enabled = crate::app_config::update_auto_apply(&self.config)
            && !Self::relaunch_nudge_seam_suppresses_auto_apply();
        let Some(dmg_sha256) = decode_dmg_sha256(digest) else {
            aterm_log::warn!(
                "refusing to arm automatic update build {build}: malformed DMG identity"
            );
            return false;
        };
        let armed = self.auto_apply_intent;
        let manual_only = self.auto_apply_manual_only;
        // WHICH SIDE OF THE SWAP the incoming artifact is on. Every caller arms
        // the reducer's own stage, so the stage says whether these bytes are the
        // installed bundle's activation.
        let incoming_activation = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .is_some_and(|staged| {
                staged.build == build
                    && decode_dmg_sha256(&staged.dmg_sha256) == Some(dmg_sha256)
                    && staged.is_installed_activation()
            });
        match crate::native_update_auto_intent::arm(ArmFacts {
            enabled,
            current_build: self.native_updater_service.snapshot().current_build,
            armed_build: armed.map(|intent| intent.build),
            armed_exact: armed
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256),
            // THE LATCH HOLDS THE UPDATE, NOT ONE SIDE OF ITS SWAP (plan P0-6):
            // the same build as a download or as its installed-bundle activation
            // is the same logical update, folded exactly as the budget behind the
            // latch folds it, so the confirming retry cannot be re-armed early by
            // the candidate's own bundle swap turning the download into an
            // activation.
            manual_only_exact: manual_only
                .is_some_and(|manual| manual.covers(build, dmg_sha256, incoming_activation)),
            manual_only_build: manual_only.map(|manual| manual.build),
            incoming_build: build,
        }) {
            ArmDecision::Clear => {
                self.auto_apply_intent = None;
                false
            }
            ArmDecision::SuppressManualOnly => {
                self.auto_apply_intent = None;
                // The status bar's Staged line was painted from policy alone; this
                // build's own latch is the finer fact — say so while the bar is up.
                self.restate_staged_bar_posture(build);
                false
            }
            ArmDecision::Keep => false,
            ArmDecision::Set(build) => {
                self.auto_apply_environment_block = None;
                // SEAM 1, HOISTED OUT OF THE PARKED WINDOW: authenticate the
                // staged bundle NOW, while every reader is still live and a
                // cancel costs nothing, instead of as the handoff worker's first
                // action with the whole terminal parked behind it.
                self.spawn_staged_handoff_preverification(build);
                // A different artifact at the same build, or a newer build, owns a
                // distinct budget. Older wakes were suppressed by `arm` above.
                self.auto_apply_manual_only = None;
                let now = std::time::Instant::now();
                // THE LADDER STARTS NOW — once per build. A re-arm of the same
                // build (a duplicate stage wake, a superseded intent for the same
                // bytes, a physical-failure latch lapsing) keeps the anchor, so
                // a busy terminal or an unlucky handoff can only delay the
                // landing, never restart the clock.
                let ladder = self
                    .auto_apply_ladder
                    .filter(|ladder| ladder.build == build);
                if ladder.is_none() {
                    self.auto_apply_ladder = Some(crate::AutoApplyLadder {
                        build,
                        armed_at: now,
                        announced: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
                    });
                }
                // Say so at the default log level. The automatic lane's waits and
                // its first blocked attempt used to be debug-only, so an operator
                // reading aterm.log could not tell an armed-and-waiting lane from a
                // dead one (2026-08-19: twenty minutes staring at a stage that never
                // applied, until a control apply exposed the reason). A re-arm on
                // a retained anchor says where the ladder already stands rather
                // than promising the bound a second time.
                match ladder {
                    Some(ladder) => aterm_log::info!(
                        "update auto-apply re-armed for build {build} ({}…): {} s since it \
                         was first armed, in the {} phase; the bound still counts from that \
                         first arming (it stops waiting {} s after it)",
                        &digest[..digest.len().min(12)],
                        now.saturating_duration_since(ladder.armed_at).as_secs(),
                        crate::native_update_auto_intent::apply_phase(
                            now.saturating_duration_since(ladder.armed_at)
                        )
                        .as_str(),
                        crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
                    ),
                    None => aterm_log::info!(
                        "update auto-apply armed for build {build} ({}…): lands at the first \
                         quiet moment, and stops waiting {} s after arming whatever the \
                         terminal is doing (idle preferred for {} s, then a gap in output \
                         until {} s, then a gap in typing, then unconditionally)",
                        &digest[..digest.len().min(12)],
                        crate::native_update_auto_intent::LANDS_WITHIN.as_secs(),
                        crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE.as_secs(),
                        (crate::native_update_auto_intent::PREFER_IDLE_WINDOW
                            + crate::native_update_auto_intent::PREFER_OUTPUT_GAP_WINDOW)
                            .as_secs()
                    ),
                }
                self.auto_apply_intent = Some(crate::AutoApplyIntent {
                    build,
                    dmg_sha256,
                    retry_at: now + crate::AUTOMATIC_UPDATE_QUIET_EPOCH,
                    attempts: 0,
                });
                // The status bar's "applies in place within ~2 min" was a POLICY line
                // when the Staged report painted it; now it is an armed fact (or, where
                // the handoff is unavailable, the line that says nothing applies while
                // a terminal is open) — re-state it while the bar is still up.
                self.restate_staged_bar_posture(build);
                true
            }
        }
    }

    /// Where the automatic lane stands on its ladder at `now`, for the build it
    /// is working on (`auto_apply_ladder`). Anchored nowhere — nothing armed
    /// through `arm_native_auto_apply` yet — the lane is at the first phase.
    pub(crate) fn automatic_apply_phase(
        &self,
        now: std::time::Instant,
    ) -> crate::native_update_auto_intent::ApplyPhase {
        self.auto_apply_ladder.map_or(
            crate::native_update_auto_intent::ApplyPhase::PreferIdle,
            |ladder| {
                crate::native_update_auto_intent::apply_phase(
                    now.saturating_duration_since(ladder.armed_at),
                )
            },
        )
    }

    /// The ladder phase an attempt under `mode` is gated by at `now`: the
    /// anchored phase, raised to the mode's own promise — `AutomaticPastGrace`
    /// never asks for a machine-wide idle moment, so an attempt authorized under
    /// it is at least in the output-gap phase even with no anchor (a direct
    /// caller, a test); `Automatic` is the anchored phase as it stands.
    pub(crate) fn automatic_phase_for(
        &self,
        mode: ApplyMode,
        now: std::time::Instant,
    ) -> crate::native_update_auto_intent::ApplyPhase {
        use crate::native_update_auto_intent::ApplyPhase;
        let floor = if mode == ApplyMode::AutomaticPastGrace {
            ApplyPhase::PreferOutputGap
        } else {
            ApplyPhase::PreferIdle
        };
        self.automatic_apply_phase(now).max(floor)
    }

    /// The facts about the terminal the ladder reads, sampled at `now`. The
    /// keystroke gate and the warm-up hold are consulted only on an automatic
    /// lane; an explicit apply never reads them and parks at once.
    pub(crate) fn automatic_activity_facts(
        &self,
        mode: ApplyMode,
        now: std::time::Instant,
    ) -> crate::native_update_auto_intent::ActivityFacts {
        crate::native_update_auto_intent::ActivityFacts {
            quiet: self.automatic_update_activity_quiet(now),
            hands_off_keys: mode.is_automatic() && self.update_apply_hands_off_keys(now),
            output_quiet: self.automatic_update_output_quiet(now),
            focused: self.any_os_window_focused(),
            consent_warmup: mode.is_automatic() && self.update_apply_warmup_holds(now),
        }
    }

    /// Say ONCE, at the default log level, that the lane moved to `phase` and
    /// what it now waits for. The per-poll deferral used to be the line
    /// (2026-09-20: 13,156 of them in one evening, two a second), which told an
    /// operator nothing a phase change does not.
    fn announce_apply_phase(&mut self, phase: crate::native_update_auto_intent::ApplyPhase) {
        let Some(ladder) = self.auto_apply_ladder.as_mut() else {
            return;
        };
        if ladder.announced >= phase {
            return;
        }
        ladder.announced = phase;
        aterm_log::info!(
            "update auto-apply for build {}: {} s since arming with no landing; now waiting \
             for {} (it stops waiting {} s after arming)",
            ladder.build,
            ladder.armed_at.elapsed().as_secs(),
            phase.waits_for(),
            crate::native_update_auto_intent::LANDS_WITHIN.as_secs()
        );
    }

    /// Attempt a retained automatic apply from an exact event or timer wake.
    /// Every caller honors the same retained deadline and activity gate, so a
    /// duplicate stage wake cannot bypass backoff or spend another attempt.
    pub(crate) fn try_pending_native_auto_apply(&mut self, announce: bool) {
        use crate::native_update_auto_intent::{
            ApplyPhase, AttemptDisposition, AttemptResult, PollDecision, PollFacts, WaitReason,
        };

        let Some(mut intent) = self.auto_apply_intent else {
            return;
        };
        let now = std::time::Instant::now();
        let deadline_ready = now >= intent.retry_at;
        let phase = self.automatic_apply_phase(now);
        self.announce_apply_phase(phase);
        // A SAME-IMAGE HANDOFF COUNTS AS WORK. Without this a real stage arriving while the
        // QA seam's handoff is in flight would reach `apply_staged_update_now` and be told
        // "an update handoff is already in flight" — a FAILURE that spends retry budget.
        // Counted as work instead, it answers `Wait(WorkActive)`: intent retained, no
        // budget spent, re-polled shortly. A no-op for real handoffs, which already set
        // `active`/`Applying` (2026-09-17).
        let work_active = self.native_updater_service.snapshot().active.is_some()
            || self.update_handoff_in_flight();
        let applying = self.native_updater_service.snapshot().phase == UpdaterPhase::Applying;
        // Durable facts are collected by workers and reduced by their exact wakes. A
        // timer callback is intentionally memory-only: it may wait for publication,
        // but it never reads/parses ledger files or launches PlistBuddy on the UI loop.
        if deadline_ready && !work_active && !applying {
            let Some(authoritative_intent) = self.auto_apply_intent else {
                // Asynchronous reconciliation retired or consumed the stage. Never
                // resurrect the copied intent into another wake loop.
                return;
            };
            intent = authoritative_intent;
        }
        // Durable reconciliation above is the one filesystem observation for this
        // retry wake. From here on, reduce the process-owned snapshot only; calling
        // `staged_update_ready()` would re-read the ledger on the latency-sensitive
        // event loop before immediately consulting the same reducer state.
        let updater = self.native_updater_service.snapshot();
        let current_build = updater.current_build;
        let staged_build = updater.staged.as_ref().map(|staged| staged.build);
        let staged_exact_target = updater.staged.as_ref().is_some_and(|staged| {
            staged.build == intent.build
                && decode_dmg_sha256(&staged.dmg_sha256) == Some(intent.dmg_sha256)
        });
        let staged_ready = updater.phase == UpdaterPhase::Staged
            && staged_build.is_some_and(|build| build > current_build);
        let decision = crate::native_update_auto_intent::poll(PollFacts {
            enabled: crate::app_config::update_auto_apply(&self.config)
                && !Self::relaunch_nudge_seam_suppresses_auto_apply(),
            deadline_ready,
            current_build,
            target_build: intent.build,
            work_active,
            applying,
            activity_quiet: self.automatic_update_activity_quiet(now),
            phase,
            staged_ready,
            staged_build,
            staged_exact_target,
        });
        let (attempt_build, attempt_phase) = match decision {
            PollDecision::Clear => {
                // The lane just dropped its promise: the staged words restate to
                // the posture that now holds (a ready row where a press is now how
                // it installs).
                if let Some(intent) = self.auto_apply_intent.take() {
                    self.restate_staged_bar_posture(intent.build);
                }
                return;
            }
            PollDecision::Wait(WaitReason::Deadline) => return,
            PollDecision::Wait(
                reason @ (WaitReason::WorkActive | WaitReason::Activity | WaitReason::StagePending),
            ) => {
                // Work/publication/activity are ordering facts, not physical
                // attempts. Retain exact intent through arbitrarily many bounded
                // active/drain transitions and consume zero retry budget.
                intent.retry_at = match reason {
                    // Re-poll on the quiet cadence. The ladder's phases bound
                    // how long this can go on; nothing here needs to.
                    WaitReason::Activity => {
                        crate::automatic_update_activity_retry_at(std::time::Instant::now())
                    }
                    WaitReason::WorkActive | WaitReason::StagePending => {
                        now + std::time::Duration::from_secs(2)
                    }
                    WaitReason::Deadline => unreachable!("matched above"),
                };
                self.auto_apply_intent = Some(intent);
                return;
            }
            PollDecision::Attempt {
                build,
                quiet: _,
                phase,
            } => (build, phase),
        };
        // A STANDING CAPTURE REFUSAL HOLDS THE ATTEMPT until the desk it refused
        // has changed (or the backstop passed): launching a successor into the
        // same refusal is the v0.91 loop (the 2026-09-22/23 update audit, plan
        // P0-3). Intent retained, nothing spent, re-probed on the refusal lane's
        // cadence.
        let activation = updater.staged.as_ref().is_some_and(|staged| {
            staged.build == attempt_build && staged.is_installed_activation()
        });
        if self.capture_refusal_holds(attempt_build, intent.dmg_sha256, activation, now) {
            intent.retry_at = now + CAPTURE_REFUSAL_PROBE;
            self.auto_apply_intent = Some(intent);
            return;
        }
        intent.build = attempt_build;
        self.auto_apply_intent = None;
        // The mode carries the phase to the handoff: `Automatic` is the first
        // phase, which asks for a quiet moment at the entry and again at the
        // park; `AutomaticPastGrace` is every later phase, whose entry and park
        // gates read the ladder. Both are revoked by activity mid-flight the
        // same way (`note_update_handoff_activity` is mode-blind by design, and
        // `exact_activity` is a mandatory conjunct of Commit in every lane).
        let outcome = self.apply_native_update(if attempt_phase == ApplyPhase::PreferIdle {
            ApplyMode::Automatic
        } else {
            ApplyMode::AutomaticPastGrace
        });
        if let UpdateOutcome::Deferred { reason } = outcome {
            // A deferral is the ladder saying "not this instant". It returned
            // above `begin_apply_preflight` — no park, no launch, no ticket, no
            // budget — so it is re-polled on the quiet cadence: the gap it waits
            // for is measured in seconds, and the ladder's phases bound how long
            // the waiting can go on. It is NOT logged per poll; the phase change
            // that ends the waiting is (`announce_apply_phase`). A path that
            // already re-armed its own intent (an activity-revoked completion's
            // spaced retry) keeps it. The one deferral that DID park — a fork
            // lane park that missed its budget — was surfaced and booked by
            // `fork_park_miss_outcome` before it got here, so returning without
            // a word is right for every deferral that reaches this line.
            if self.auto_apply_intent.is_none() {
                intent.retry_at = std::time::Instant::now() + crate::AUTOMATIC_UPDATE_QUIET_EPOCH;
                self.auto_apply_intent = Some(intent);
            }
            aterm_log::debug!(
                "automatic update retained exact intent after a deferral in {attempt_phase:?}: \
                 {reason}"
            );
            return;
        }
        intent.attempts = intent.attempts.saturating_add(1);
        // WHETHER A PERSON HOLDS THE LANE, as of this attempt: unsaved editor or
        // Settings work is the one blocker the lane cannot outwait, and while it
        // stands the build waits by design — the updater's overdue notice must
        // not call that "aterm can't install updates" (`App::update_lands_by_itself`).
        // Every attempt that got past the preflight answers it again.
        self.update_waits_on_person = match &outcome {
            UpdateOutcome::Blocked { reasons } => {
                Self::update_blocker_for_person(reasons).map(|_| intent.build)
            }
            _ => None,
        };
        let disposition = crate::native_update_auto_intent::finish(match &outcome {
            UpdateOutcome::Accepted => AttemptResult::Accepted,
            UpdateOutcome::InstalledNeedsRelaunch { .. } => AttemptResult::InstalledNeedsRelaunch,
            UpdateOutcome::Blocked { .. } => AttemptResult::Blocked,
            // The intent is RETAINED (the refusal lane re-arms it below), so to
            // the reducer this is the not-now family, never the latching one.
            UpdateOutcome::CaptureRefused { .. } => AttemptResult::Blocked,
            UpdateOutcome::Failed { .. } => AttemptResult::Failed,
            UpdateOutcome::Deferred { .. } => unreachable!("returned above"),
        });
        match (disposition, outcome) {
            (AttemptDisposition::Complete, UpdateOutcome::Accepted) => {
                // A successful replacement never returns. Accepted here means the
                // successor is LAUNCHING (or a joined in-flight request's owner now
                // owns completion/recovery) — a start, not a landing; the landing is
                // its own line at Commit (`finish_update_handoff`).
                aterm_log::info!(
                    "update auto-apply started for build {} (new process launching; \
                     attempt {})",
                    intent.build,
                    intent.attempts
                );
            }
            (
                AttemptDisposition::Complete,
                UpdateOutcome::InstalledNeedsRelaunch { build, message },
            ) => {
                self.auto_apply_intent = None;
                self.surface_update_apply_outcome(
                    "automatic",
                    UpdateOutcome::InstalledNeedsRelaunch { build, message },
                    false,
                );
            }
            (AttemptDisposition::Retry, UpdateOutcome::Blocked { reasons }) => {
                // Every actual blocked attempt leaves its explanation in durable
                // status, including timer retries with announce=false. UI silence
                // must not erase why a verified stage is still not running. Polls
                // that merely wait returned above and never write this ledger.
                let blocked = UpdateOutcome::Blocked {
                    reasons: reasons.clone(),
                };
                self.record_apply_outcome_in_ledger(&blocked);
                // A PREFLIGHT BLOCK IS A FACT ABOUT THIS MOMENT, NEVER EVIDENCE
                // AGAINST THE ARTIFACT — so a spent budget must slow the lane
                // down, not end it, AND must not become a recurring intrusion.
                //
                // History, because both halves were learned the hard way.
                //
                // (1) The original shape retired the intent and installed
                // `AutoApplyManualOnly { retry_at: None }`. `arm` then answers
                // `SuppressManualOnly` for that exact (build, artifact) FOREVER,
                // and `lapse_expired_auto_apply_manual_only` only releases
                // latches whose `retry_at` is `Some`. The budget above is three
                // attempts spaced 5 s / 15 s — roughly twenty seconds of being
                // ready-to-park — and every blocker `apply_native_update` can
                // report here is the user's own live state, so twenty busy
                // seconds permanently disabled automatic apply and printed
                // "Update paused — manual retry". That is the field report.
                //
                // (2) The obvious repair — give that latch a lapse deadline —
                // trades a permanent intrusion for a RECURRING one, which is
                // worse than it sounds. A lapse re-arms a FRESH intent at
                // `attempts: 0`, so every cooldown replays the whole budget:
                // three more attempts, each of which re-enters
                // `prepare_all_native_shutdown`. Calling those "three cheap
                // probes" was wrong on two independent counts, and BOTH are
                // fixed here rather than one:
                //   * the RATE (fixed by retaining the intent, below): a
                //     monotone `attempts` means one probe per cooldown, not
                //     three, and one status pill ever rather than one per
                //     exhaustion;
                //   * the COST OF A PROBE (fixed at its source in
                //     `apply_native_update`): a probe used to be a focus hijack.
                //     A native reducer answering `CloseReadiness::Blocked` made
                //     `surface_native_close_recovery` switch the active tab, move
                //     focus, re-front the window and replace the window overlay
                //     with a Close Recovery palette (`app_tabs.rs` →
                //     `app_palette.rs`). The automatic lane now probes with
                //     `ClosePreflightVisibility::Quiet`, so the verdict is
                //     unchanged and the screen is untouched. Rate alone was not
                //     enough: even ONE unrequested palette over a user's work is
                //     the intrusion, and it would still have recurred every
                //     cooldown for as long as the blocker lived.
                //
                // SO THE INTENT IS RETAINED INSTEAD OF LATCHED, and `attempts`
                // keeps counting for the life of that intent. Three consequences,
                // all of them the point:
                //   * the retry RATE past the budget is one attempt per
                //     `PREFLIGHT_BLOCK_COOLDOWN`, not three — the fast 5 s/15 s
                //     probes belong to the first twenty seconds only;
                //   * a monotone `attempts` is a per-(build, artifact) counter
                //     that no lapse can reset, so the status pill can fire on the
                //     FIRST exhaustion and never again for these bytes. `arm`
                //     answers `Keep` for a duplicate stage wake of the same
                //     artifact, so a wake cannot mint fresh budget either; only a
                //     genuinely different artifact re-arms at zero;
                //   * nothing is ever permanent: a user who fixes the blocker
                //     gets the update on the next cooldown without having to
                //     learn that a menu exists.
                //
                // THE COOLDOWN IS ONLY SAFE BECAUSE EVERY `Blocked` THAT CAN
                // REACH IT IS TRANSIENT. Audited producer by producer:
                //   * close-preflight blockers (unsaved Settings text, dirty
                //     or mid-checkpoint documents, a restore still landing) —
                //     the user's own live state. Most clear themselves; a
                //     FAILED checkpoint (`DocumentPhase::Blocked`) waits for
                //     an explicit retry, so it keeps costing ONE probe per
                //     cooldown until the user acts. That is the correct trade;
                //   * `NotStaged`/`NotDeferred` — ordering, a stage may
                //     publish on the next reduce;
                //   * a native-close REDUCER ERROR is NOT one of them any
                //     more: `apply_native_update` now reports it as
                //     `Failed`, so it takes the strict arm below;
                //   * `Disabled` and "missing sealed source provenance" are
                //     unreachable from THIS lane by construction — a disabled
                //     ledger clears `snapshot.staged` (so `poll` never
                //     attempts) and `staged_from_status` refuses to import a
                //     stage without a 40-hex commit (so provenance is always
                //     present on a reducer-imported artifact).
                // Genuine hard failures arrive as `UpdateOutcome::Failed` and
                // keep their strict, converging budget untouched.
                //
                // The retained intent cannot spin: `poll` answers
                // `Wait(Deadline)` without rescheduling until `retry_at`, and
                // `about_to_wait` folds that instant into winit's `WaitUntil`
                // (`fold_auto_apply_deadline`), so an IDLE terminal wakes exactly
                // once per cooldown. It also cannot outlive its artifact — a
                // superseded, retired or consumed stage clears the intent through
                // `arm`/`poll`/`reconcile_returned_native_apply_with_facts`.
                let cooling_down = match automatic_retry_delay(
                    intent.attempts,
                    AutomaticRetryKind::PreflightBlocked,
                ) {
                    Some(delay) => {
                        intent.retry_at = std::time::Instant::now() + delay;
                        false
                    }
                    None => {
                        intent.retry_at = std::time::Instant::now() + PREFLIGHT_BLOCK_COOLDOWN;
                        true
                    }
                };
                // Exactly the attempt that spent the last of the budget. Counting
                // from a monotone `attempts` (rather than from "is there a latch")
                // is what makes this fire ONCE per artifact instead of once per
                // cooldown: `attempts` is only reset by `arm` setting a new
                // intent, which requires different bytes or a newer build.
                let first_exhaustion =
                    cooling_down && intent.attempts == MAX_AUTOMATIC_UPDATE_CYCLES;
                self.auto_apply_intent = Some(intent);
                // THE WORDS MAY NOT OUTLIVE THE PROMISE — the physical arm's law,
                // owed here too now that admission refusals route through this
                // lane (2026-09-01): the refusal is synchronous, and the gate's
                // posture may now say the lane is off. Recompute the posture; for
                // an ordinary close-preflight block it is unchanged and this is a
                // no-op. Unsaved editor work holding it is the outcome row
                // `note_update_blockers` raises.
                self.restate_staged_bar_posture(intent.build);
                if intent.attempts == 1 {
                    // The FIRST block of an intent is always logged (the pill below
                    // stays gated on `announce`, which is the UI's business): a
                    // preflight blocker is the one thing an operator can act on.
                    aterm_log::info!(
                        "update auto-apply attempt 1 for build {} blocked by preflight: {}",
                        intent.build,
                        reasons.join(" · ")
                    );
                }
                if announce && intent.attempts == 1 {
                    // Already recorded above: announcement adds UI only, not a
                    // second durable write for this same refused attempt.
                    self.react_to_update_apply_outcome("automatic", blocked, false);
                }
                if !cooling_down {
                    aterm_log::info!(
                        "update auto-apply remains pending for build {}; bounded retry armed",
                        intent.build
                    );
                } else {
                    let message = format!(
                        "{} · automatic retries reached their safe cap; the automatic lane \
                         now re-probes once every {}s until it lands, and the Version menu \
                         can try immediately (it runs the same safety preflight, so the same \
                         blockers still apply)",
                        reasons.join(" · "),
                        PREFLIGHT_BLOCK_COOLDOWN.as_secs()
                    );
                    // The LOG says it at WARN once per artifact and at debug on
                    // every later cooldown — at one probe a minute, a warning each
                    // time would be sixty lines an hour for one unsaved Settings
                    // draft. The UI says it once, and only when it is the
                    // person's to clear (an unsaved editor; said again, it is the
                    // center's Duplicate of the announced block's row): a lane
                    // re-probing on its own posts no row.
                    if first_exhaustion {
                        aterm_log::warn!("{message}");
                        self.note_update_blockers(&reasons, true);
                    } else {
                        aterm_log::debug!("{message}");
                    }
                }
            }
            (AttemptDisposition::Retry, UpdateOutcome::CaptureRefused { message }) => {
                // THE FORK LANE'S CAPTURE REFUSED THE DESK (plan P0-3(d)): the
                // same lane the launched lane's completion takes — recorded as an
                // apply failure naming the session (so `failing_applies` moves
                // and `update status` shows it), retried once the desk changes,
                // never latched and never charged to the physical budget, where
                // this arm's `Failed` sibling used to put it (nine of them over
                // fourteen hours, then a latch with no deadline).
                self.arm_capture_refusal_retry(
                    intent.build,
                    intent.dmg_sha256,
                    activation,
                    &message,
                );
                self.restate_staged_bar_posture(intent.build);
                self.surface_update_apply_outcome(
                    "automatic · a session could not be carried",
                    UpdateOutcome::CaptureRefused { message },
                    false,
                );
            }
            (AttemptDisposition::ManualOnly, UpdateOutcome::Failed { message }) => {
                // A returned physical handoff can park/read/checkpoint sessions, so it
                // is retried rarely and only twice per epoch — but it IS retried, and
                // an epoch always ends. `retry_at: None` here used to be minted for a
                // SINGLE missed 15 s handoff deadline, which disabled automatic
                // in-session apply for that build outright; it is now reachable only
                // from a STRUCTURAL `PhysicalFailureSchedule::Converged`; the
                // transient lane converges into a quiet six-hourly re-sample instead
                // (the 2026-09-22/23 update audit, plan P1-1(d)).
                self.auto_apply_intent = None;
                // TRANSIENT, BECAUSE THIS LANE HAS NO TYPED OUTCOME TO READ. These
                // are SUBMISSION-time failures — the admission classifier refusing
                // ("Update kept N sessions running…"), a cold-lane `exec` that
                // returned, a reducer that went stale — and no worker verdict
                // exists yet, because no candidate has been spawned. The only
                // remaining discriminator is `message`, and deriving a convergence
                // schedule from a display string is exactly the string matching the
                // typed classification beside this replaced. The generous schedule
                // is also the right default for the set: every producer here is a
                // fact about this process's current state, which is what changes.
                // WHICH SIDE OF THE SWAP, derived from the live stage rather
                // than a ticket: this lane fails before any candidate exists, so
                // there is none. `poll` admitted this intent only against the
                // stage that exactly matches it, so the stage is this artifact.
                let activation = self
                    .native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .is_some_and(|staged| {
                        staged.build == intent.build && staged.is_installed_activation()
                    });
                // ONE TYPED FACT THIS LANE CAN READ (2026-09-14): the cached
                // pre-park verdict is the verifier's own, not a message match.
                // When the short-circuit in `start_unix_update_handoff` refused
                // this artifact, the same verdict reached through the worker
                // is `PreparationFailed`, which `of_outcome` calls STRUCTURAL —
                // two attempts, not nine over fourteen hours. And when the
                // refusal is the INSTALLED copy's (not the signed release: no
                // lane can install over it until a person changes it), there
                // is nothing to retry at all: a deadline-less latch, the remedy
                // said once, and `apply_retry_for` reads the ledger's reason to
                // show it. The latch clears with the reason — the checker's
                // installed-bundle probe re-runs every cycle, and a stage that
                // supersedes this artifact arms a fresh intent.
                #[cfg(unix)]
                let cached_refusal = self.cached_handoff_refusal_reason(intent.build);
                #[cfg(not(unix))]
                let cached_refusal: Option<String> = None;
                let needs_person = cached_refusal
                    .as_deref()
                    .is_some_and(crate::update_apply_trouble::ApplyTrouble::needs_person);
                if needs_person {
                    self.block_native_auto_apply_environment(intent.build, intent.dmg_sha256);
                    self.restate_staged_bar_posture(intent.build);
                    aterm_log::warn!(
                        "update auto-apply: build {} cannot be applied by any lane until \
                         a person changes the installed bundle — {}; automatic apply is \
                         suspended for this artifact (no retry is scheduled)",
                        intent.build,
                        cached_refusal.as_deref().unwrap_or("")
                    );
                    self.surface_update_apply_outcome(
                        "automatic · needs you",
                        UpdateOutcome::Failed { message },
                        false,
                    );
                    return;
                }
                let shape = if cached_refusal.is_some() {
                    PhysicalFailureShape::Structural
                } else {
                    PhysicalFailureShape::Transient
                };
                // ONE HELPER, EVERY LANE (plan P1-1(a)): the budget, the latch,
                // the schedule's log line, its standing note for `update status`
                // and — at convergence — the health notice are written by the
                // same code here and on both asynchronous completions.
                let schedule = self.latch_after_physical_failure(
                    intent.build,
                    intent.dmg_sha256,
                    activation,
                    shape,
                    &message,
                );
                // THE BAR MAY NOT OUTLIVE THE PROMISE. An admission refusal is
                // synchronous, so this arm runs inside the Staged bar's hold with
                // "applies in place within ~2 min" still on screen — while the
                // Version-menu row already says the attempt did not start. Same
                // fact, both surfaces, now.
                self.restate_staged_bar_posture(intent.build);
                self.surface_update_apply_outcome(
                    // The label used to say "manual retry required" even when
                    // `retry_at` had just scheduled another automatic attempt —
                    // it described the exhausted case for all of them. The label
                    // now names which of the three schedules answered; the glass
                    // says nothing while one is scheduled, and names the Version
                    // menu once they are spent.
                    match schedule {
                        PhysicalFailureSchedule::Retry(_) => "automatic · retrying later",
                        PhysicalFailureSchedule::StandDown(_) => {
                            "automatic · standing down, then retrying"
                        }
                        PhysicalFailureSchedule::Converged { resample_at: None } => {
                            "automatic · out of retries"
                        }
                        PhysicalFailureSchedule::Converged {
                            resample_at: Some(_),
                        } => "automatic · out of retries, re-sampling",
                    },
                    UpdateOutcome::Failed { message },
                    false,
                );
                // The standing schedule note the helper queued was written by
                // that surfacing, AFTER the failure it booked (which clears any
                // standing refusal) — see `App::apply_schedule_standing`.
            }
            (_, outcome) => {
                // A future policy/outcome mismatch must fail safe, never panic in the
                // input loop or accidentally arm an unbounded physical retry.
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: intent.build,
                    dmg_sha256: intent.dmg_sha256,
                    activation: false,
                    retry_at: None,
                });
                self.restate_staged_bar_posture(intent.build);
                aterm_log::warn!(
                    "automatic update policy mismatch; clearing timer and requiring manual retry"
                );
                self.surface_update_apply_outcome("automatic policy fallback", outcome, false);
            }
        }
    }

    pub(crate) fn publish_native_update_state(&mut self) {
        let snapshot = self.native_updater_service.snapshot();
        let revision = snapshot.revision;
        let checking = matches!(
            snapshot.phase,
            UpdaterPhase::Checking | UpdaterPhase::Available | UpdaterPhase::Downloading
        );
        let attention = snapshot.attention_pending();
        debug_assert!(
            !snapshot.has_determinate_progress(),
            "the current updater API supplies no progress denominator"
        );
        let staged = snapshot
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.version.clone()));
        let update = self.update_snapshot(checking);
        self.native_runtime
            .replace_settings_update(update, revision);

        let views =
            self.view_store
                .iter()
                .filter_map(|(view, link)| match link {
                    crate::tab_model::View::Native(native)
                        if self.native_runtime.app(native.instance).is_some_and(|app| {
                            app.kind() == crate::native_app::AppKind::Settings
                        }) =>
                    {
                        Some((native.instance, view))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
        for (instance, view) in &views {
            let _ = self.native_runtime.dispatch(
                *instance,
                *view,
                AppEvent::UpdateChanged { revision },
            );
        }

        let setting_views = views
            .iter()
            .map(|(_, view)| *view)
            .collect::<std::collections::BTreeSet<_>>();
        for ws in self.windows.values_mut() {
            let count = ws.tab_set.len();
            for index in 0..count {
                let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                    continue;
                };
                if tab
                    .root
                    .leaves()
                    .iter()
                    .any(|view| setting_views.contains(view))
                {
                    tab.presentation.indicators.busy = checking;
                    tab.presentation.indicators.attention = attention;
                }
            }
            ws.last_present = None;
        }

        self.relaunch = staged
            .map(|(build, version)| crate::relaunch_notice::RelaunchNotice { build, version });
        self.refresh_version_menu();
        self.palette_refresh_live();
        let windows = self.windows.keys().copied().collect::<Vec<_>>();
        for wid in windows {
            self.refresh_window_tabs(wid);
        }
        self.request_redraw_all_windows();
    }

    pub(crate) fn reconcile_native_update_facts(
        &mut self,
        facts: NativeUpdateReconcileFacts,
    ) -> NativeUpdateFactsResult {
        if facts.observation_sequence <= self.last_native_update_reconcile_sequence {
            return NativeUpdateFactsResult::IgnoredStale;
        }
        if self.native_updater_service.snapshot().active.is_some()
            || self.native_updater_service.snapshot().phase == UpdaterPhase::Applying
        {
            return NativeUpdateFactsResult::Deferred(Box::new(facts));
        }
        // A READ THAT BEGAN BEFORE THIS PROCESS'S OWN STAGE IMPORT describes a disk
        // without that stage (the read spans a codesign; the check's wake can land
        // first). Reducing it would RETIRE the stage the check just imported and
        // leave a verified update on disk armed by nothing until the next cycle
        // (2026-08-19 round-2 audit). It is stale by construction — ignored, not
        // reduced; the next observation sees the stage.
        if let Some(imported_at) = self.native_stage_imported_at
            && facts.observed_at < imported_at
            && facts
                .durable
                .as_ref()
                .is_none_or(|durable| durable.staged_build.is_none())
            && self.native_updater_service.snapshot().staged.is_some()
        {
            aterm_log::debug!(
                "update sync: ignoring facts observed before this process staged its update"
            );
            return NativeUpdateFactsResult::IgnoredStale;
        }
        self.last_native_update_reconcile_sequence = facts.observation_sequence;
        self.release_repaired_auto_apply_environment(&facts);
        let NativeUpdateReconcileFacts {
            _ticket: _,
            observation_sequence: _,
            observed_at: _,
            durable,
            installed,
        } = facts;
        // When the last check completed, whoever ran it (Settings says "Checked 12 min ago").
        if self
            .native_updater_service
            .note_checked_at(durable.as_ref().and_then(|durable| durable.checked_at))
        {
            self.publish_native_update_state();
        }
        let build = self.native_updater_service.snapshot().current_build;
        // THE INSTALLED BUNDLE OUTRANKS A STAGED DOWNLOAD once it is newer than this
        // process (the activation lane below): a downloaded stage still in memory is
        // retired first, so the activation can be imported through the ordinary
        // check/stage transitions (`request_check` refuses while a stage is held).
        if let Some(installed) = installed.as_ref()
            && installed.activation_stage(build, 0).is_some()
        {
            let retired = self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .filter(|staged| !staged.is_installed_activation())
                .map(|staged| staged.build);
            if let Some(retired_build) = retired
                && self.native_updater_service.retire_stage_for_activation()
            {
                self.auto_apply_intent = None;
                // THE LATCH IS NOT THE DOWNLOAD'S, IT IS THE UPDATE'S (the
                // 2026-09-22/23 update audit, plan P0-6). A latch on the build the
                // bundle now IS was minted by the very candidate that installed
                // it, so it is re-keyed to the activation with its deadline kept;
                // clearing it here is what fired the "ten minutes out" confirming
                // retry 0.5 s later and converged 0.90 and 0.91 for a day. An
                // OLDER latch retires with the download; a strictly NEWER one
                // survives — same rule as the `InstalledNeedsRelaunch` arms.
                self.carry_manual_only_latch_across_swap(
                    retired_build,
                    installed.build,
                    Some(installed),
                );
                aterm_log::info!(
                    "update sync: staged download {retired_build} retired — the bundle at this \
                     path is already build {}, activating it instead",
                    installed.build
                );
                self.publish_native_update_state();
            }
        }
        let current = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .cloned();
        let observed_stage_floor = current.as_ref().map_or(0, |stage| stage.build);
        let durable_enabled = durable.as_ref().is_some_and(|status| status.enabled);
        let durable_build = durable.as_ref().and_then(|status| status.staged_build);
        let durable_commit = durable
            .as_ref()
            .and_then(|status| status.staged_commit.as_deref());
        let durable_digest = durable
            .as_ref()
            .and_then(|status| status.staged_dmg_sha256.as_deref());
        if current.is_some() {
            let disposition = self.native_updater_service.reconcile_durable_stage(
                durable_enabled,
                durable_build,
                durable_commit,
                durable_digest,
                installed.as_ref(),
            );
            if disposition != DurableStageDisposition::Unchanged {
                self.auto_apply_intent = None;
                self.publish_native_update_state();
                match disposition {
                    DurableStageDisposition::InstalledNeedsRelaunch { build } => {
                        // A RETIRED BUILD MUST NOT KEEP ITS LATCH. The manual-only
                        // latch is a promise about ONE artifact: do not spend
                        // automatic apply on these bytes yet. Once those bytes are
                        // the installed bundle there is nothing left for the
                        // promise to refuse, and the whole class of bug this file
                        // has been repairing is a latch that outlived its reason.
                        // Harmless today only because `arm` independently refuses a
                        // build that is no longer newer — which is a second
                        // mechanism agreeing with this one, not a licence to keep
                        // state that says something false about the world.
                        //
                        // A latch naming a STRICTLY NEWER build is about a
                        // different artifact and survives, for exactly the reason
                        // the sibling `Retired` arm in
                        // `reconcile_returned_native_apply_with_facts` keeps its
                        // own: a newer attempt may already have completed out of
                        // order, and retiring old authority must not hand it back
                        // an automatic lane it just latched off.
                        //
                        // AND A LATCH ON THE INSTALLED BUILD ITSELF IS NOT RETIRED
                        // EITHER (plan P0-6): the bytes it refused are still the
                        // update on offer, now as the bundle's activation, so it
                        // is re-keyed with its deadline and cleared only by that
                        // deadline.
                        self.carry_manual_only_latch_across_swap(build, build, installed.as_ref());
                        // NOT "by another aterm process" ANY MORE, which is the
                        // likelier half of the truth and the one this line was
                        // written for. The other producer is THIS process: an
                        // overlap child swaps the bundle and re-execs BEFORE it can
                        // write a readiness proof, so a handoff rejected after
                        // ProofReady leaves the new bundle installed by our own
                        // candidate — and `send_warranted_handoff_failure` posts
                        // the disk facts that land here moments later. Name the
                        // state and the remedy, and do not attribute the installer
                        // to a process this reducer cannot identify.
                        let message = format!(
                            "Build {build} is already installed on disk; activation is pending \
                             — your shells keep running"
                        );
                        aterm_log::warn!("update sync: {message}");
                    }
                    DurableStageDisposition::Retired => {
                        // The latch belongs to the exact retired artifact. Keeping
                        // it here could suppress a replacement forever when its
                        // build is lower than the withdrawn stage but still newer
                        // than this process. A latch for other bytes survives.
                        if let Some(retired) = current.as_ref()
                            && self.auto_apply_manual_only.is_some_and(|manual| {
                                manual.build == retired.build
                                    && decode_dmg_sha256(&retired.dmg_sha256)
                                        == Some(manual.dmg_sha256)
                            })
                        {
                            self.auto_apply_manual_only = None;
                        }
                        aterm_log::warn!(
                            "update sync: stale in-memory stage retired after durable marker changed"
                        );
                    }
                    DurableStageDisposition::Unchanged => {}
                }
            }
        }

        // The ledger's failure verdict reaches the snapshot on EVERY reconcile that
        // holds a stage — exact or not (an activation never matches the durable
        // marker, and `request_check` refuses while a stage is held, so
        // `finish_check` cannot carry it): an apply-class streak builds while a
        // stage is held, and the headline must say so.
        if self.native_updater_service.snapshot().staged.is_some()
            && let Some(durable) = durable.as_ref()
            && self.native_updater_service.absorb_failure_state(durable)
        {
            self.publish_native_update_state();
        }
        let effective = self.native_updater_service.snapshot().staged.as_ref();
        let exact_effective = effective.is_some_and(|staged| {
            crate::native_updater_service::durable_artifact_identity_matches(
                durable_build,
                durable_commit,
                durable_digest,
                staged.build,
                staged.commit.as_deref(),
                &staged.dmg_sha256,
            )
        });
        if exact_effective {
            return NativeUpdateFactsResult::Reduced {
                effective_stage: effective.cloned(),
            };
        }

        // Newness is the STAGER's test and only the stager's: strictly newer than the
        // RUNNING image. observed_stage_floor is in-pass hysteresis against
        // re-importing a stage this pass just retired — not a second opinion on
        // newness. The installed BUNDLE gets no vote: its plist can be replaced under
        // a live process (a seamless update's surviving process, or a rebuild in
        // place), and folding it into the floor made a staged build the bundle
        // already carries compare as "not newer" — permanently, because a sealed
        // plist cannot change under a running process. That was a fixed point: every
        // later reconcile blanked the stage and reported no update was staged.
        let stage_floor = build.max(observed_stage_floor);
        // THE INSTALLED BUNDLE IS NEWER THAN THIS PROCESS — activate it. The bytes are
        // already at our own path (another producer put them there: the release
        // cutter writing into the bundle it was launched from, a user dragging a new
        // `.app` over the old one, a sibling aterm that swapped it, or our own
        // overlap child that swapped and then failed to prove readiness). Until
        // 2026-08-18 this state was reported as "relaunch once to activate it" and
        // then left alone — a verified, notarized, installed build sat inert until a
        // human read a log line. It is exactly a staged update whose swap has already
        // happened, so it is imported as an ACTIVATION stage: the sealed identity of
        // the installed bundle under `installed_activation_digest`, which then rides
        // the ordinary stage → automatic apply (quiet preference, bounded grace,
        // budget, manual-only latch) → seamless handoff path; the successor finds
        // nothing to swap and simply IS the newer build, adopting every window and
        // shell. Why not import the durable download as the stage instead: with the
        // bundle already replaced, a swap-apply reaches the rollback-source proof and
        // defers forever, since the installed bundle no longer matches the running
        // build. Activation outranks any durable stage while the bundle is newer;
        // the successor will observe that stage on its own terms.
        let activation = installed
            .as_ref()
            .and_then(|installed| installed.activation_stage(build, 0));
        if let Some(mut durable) = durable {
            if let Some(activation) = &activation {
                durable.staged_build = Some(activation.build);
                durable.staged_version = Some(activation.version.clone());
                durable.staged_commit = activation.commit.clone();
                durable.staged_dmg_sha256 = Some(activation.dmg_sha256.clone());
                durable.changelog = None;
                durable.outcome = format!(
                    "build {} is already installed on disk; activation is pending",
                    activation.build
                );
            } else {
                let eligible = durable.enabled
                    && durable
                        .staged_build
                        .is_some_and(|staged| staged > stage_floor);
                if !eligible {
                    durable.staged_build = None;
                    durable.staged_version = None;
                    durable.staged_commit = None;
                    durable.staged_dmg_sha256 = None;
                    durable.changelog = None;
                }
            }
            if let CheckStart::Start(ticket) = self.native_updater_service.request_check() {
                // Logged only when the import actually happens: while an activation is
                // HELD, `request_check` refuses and every ~75 s reconcile lands here
                // again with nothing new to say.
                if activation.is_some() {
                    aterm_log::info!("update sync: {}", durable.outcome);
                }
                let _ = self.native_updater_service.finish_check(ticket, durable);
                self.publish_native_update_state();
            }
        }
        NativeUpdateFactsResult::Reduced {
            effective_stage: self.native_updater_service.snapshot().staged.clone(),
        }
    }

    fn block_native_auto_apply_environment(&mut self, build: u64, dmg_sha256: [u8; 32]) {
        self.auto_apply_intent = None;
        self.auto_apply_environment_block = Some(AutoApplyEnvironmentBlock {
            build,
            dmg_sha256,
            blocked_at: std::time::Instant::now(),
        });
        self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256,
            // Exact bytes only: this latch is about the INSTALLED copy refusing,
            // and the bundle moving is precisely what releases it.
            activation: false,
            retry_at: None,
        });
    }

    pub(crate) fn auto_apply_environment_blocked(&self, build: u64) -> bool {
        self.auto_apply_environment_block.is_some_and(|blocked| {
            blocked.build == build
                && self.auto_apply_manual_only.is_some_and(|manual| {
                    manual.build == build
                        && manual.dmg_sha256 == blocked.dmg_sha256
                        && manual.retry_at.is_none()
                })
                && self
                    .native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .is_some_and(|stage| {
                        stage.build == build
                            && decode_dmg_sha256(&stage.dmg_sha256) == Some(blocked.dmg_sha256)
                    })
        })
    }

    /// This is a memory-only reduction of the facts worker's complete signing
    /// policy check. Unknown/older evidence cannot rehabilitate a refused source.
    fn release_repaired_auto_apply_environment(&mut self, facts: &NativeUpdateReconcileFacts) {
        self.release_repaired_auto_apply_environment_for_commit(
            facts,
            crate::build_info::GIT_COMMIT,
        );
    }

    fn release_repaired_auto_apply_environment_for_commit(
        &mut self,
        facts: &NativeUpdateReconcileFacts,
        running_commit: &str,
    ) {
        let Some(blocked) = self.auto_apply_environment_block else {
            return;
        };
        if !self.auto_apply_environment_blocked(blocked.build) {
            self.auto_apply_environment_block = None;
            return;
        }
        let snapshot = self.native_updater_service.snapshot();
        if facts.observed_at <= blocked.blocked_at
            || !facts
                .durable
                .as_ref()
                .is_some_and(|status| status.enabled && status.installable)
            || !facts.installed.as_ref().is_some_and(|installed| {
                installed.build == snapshot.current_build
                    && aterm_update::commit_matches(&installed.commit, running_commit)
            })
        {
            return;
        }
        // A cache publication newer than this read may have observed another
        // change. Never erase it with older evidence. Matching old denial is
        // invalidated, not turned into a fabricated full preverification pass.
        let mut cached = match self.handoff_preverified.try_lock() {
            Ok(cached) => cached,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        if cached.as_ref().is_some_and(|entry| {
            entry.build == blocked.build
                && decode_dmg_sha256(&entry.artifact) == Some(blocked.dmg_sha256)
                && entry.at >= facts.observed_at
        }) {
            return;
        }
        if cached.as_ref().is_some_and(|entry| {
            entry.build == blocked.build
                && decode_dmg_sha256(&entry.artifact) == Some(blocked.dmg_sha256)
        }) {
            *cached = None;
        }
        drop(cached);
        self.auto_apply_environment_block = None;
        self.auto_apply_manual_only = None;
        aterm_log::info!(
            "update apply: installed source now verifies against the running image; \
             automatic eligibility restored for build {}",
            blocked.build
        );
    }

    pub(crate) fn apply_native_update(&mut self, mode: ApplyMode) -> UpdateOutcome {
        // THE LADDER'S ENTRY GATE. An automatic apply asks the ladder whether
        // the terminal is in a state its current phase accepts — a quiet
        // moment, then a gap in output, then a gap in typing, then anything —
        // BEFORE a successor is launched or a ticket minted, so a refusal here
        // costs nothing and spends nothing. The same predicate is re-read at
        // the park (`prelaunch_park_admitted`), the instant the user feels. An
        // explicit "Update to Latest Now" is the person asking for the pause
        // and is never held.
        if mode.is_automatic() {
            let now = std::time::Instant::now();
            if let Some(reason) = crate::native_update_auto_intent::automatic_park_refusal(
                self.automatic_phase_for(mode, now),
                self.automatic_activity_facts(mode, now),
            ) {
                return UpdateOutcome::Deferred {
                    reason: reason.to_string(),
                };
            }
        }
        let start = self.native_updater_service.begin_apply_preflight(mode);
        let ticket = match start {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            ApplyPreflightStart::Joined(_) => return UpdateOutcome::Accepted,
            ApplyPreflightStart::Disabled => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["aterm updates itself on macOS only".to_string()],
                };
            }
            ApplyPreflightStart::NotStaged => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["No newer verified update is staged".to_string()],
                };
            }
            ApplyPreflightStart::NotDeferred => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["Install when safe has not been requested".to_string()],
                };
            }
            ApplyPreflightStart::Applying => return UpdateOutcome::Accepted,
            ApplyPreflightStart::GenerationExhausted => {
                return UpdateOutcome::Failed {
                    message: "updater preflight identity space is exhausted".to_string(),
                };
            }
        };
        // A native-close REDUCER ERROR is not "not now". `Ok(false)` means a
        // native app declined — the user's own live state, self-correcting. `Err`
        // means the close reducer itself broke (an unknown window, unhandled
        // effects): an invariant failure that no amount of waiting repairs. Both
        // used to flatten into the same `Blocked`, which was harmless only while
        // a `Blocked` latch was permanent anyway. Now that the automatic lane
        // gives a `Blocked` latch a cooldown and comes back, that flattening
        // would put a genuine hard failure into an endless retry loop — so keep
        // the distinction TYPED all the way to the outcome instead of asking a
        // later arm to guess it back out of a reason string.
        let mut shutdown_error = None;
        // A BACKGROUND PROBE MUST NOT TAKE OVER THE SCREEN. Under
        // `ClosePreflightVisibility::Interactive` a reducer that answers `Blocked`
        // makes `surface_native_close_recovery` switch the active tab, move
        // keyboard focus, re-front the window and replace the window overlay with
        // a Close Recovery palette. The automatic lane re-probes this call on a
        // `PREFLIGHT_BLOCK_COOLDOWN` schedule for as long as the blocker lives, so
        // Interactive here turned "an update is waiting" into a recurring focus
        // hijack aimed at a user whose only mistake was leaving a Settings draft
        // open. Owner instruction: a busy user must not have their tabs switched,
        // their focus stolen, or a recovery panel reopened on a schedule.
        //
        // Quiet changes NOTHING about the verdict: the same reducers run, an
        // `Ok(false)` still becomes `Blocked` and an `Err` still becomes the
        // strict `Failed` lane below. The person-initiated lanes — the Version
        // menu (`Immediate`) and `CleanQuit`, both of which happen because someone
        // asked — keep the recovery surface, and the exhaustion pill points at
        // exactly that menu, so the visible way to act is one deliberate click
        // away instead of arriving unannounced.
        let visibility = if mode.is_automatic() {
            crate::app_tabs::ClosePreflightVisibility::Quiet
        } else {
            crate::app_tabs::ClosePreflightVisibility::Interactive
        };
        let (readiness, safety_token) = match self
            .prepare_all_native_shutdown(crate::native_app::CloseScope::Relaunch, visibility)
        {
            Ok(true) => self.native_update_close_preflight(),
            Ok(false) => (
                ClosePreflight::Blocked(vec![Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()]),
                None,
            ),
            Err(message) => {
                shutdown_error = Some(message.clone());
                (ClosePreflight::Blocked(vec![message]), None)
            }
        };
        match self
            .native_updater_service
            .finish_apply_preflight(ticket, readiness)
        {
            ApplyDecision::Execute(command) => {
                let attempt = command.attempt();
                let Some(safety_token) = safety_token else {
                    return self.abort_unstarted_native_apply(
                        &attempt,
                        "native update preflight returned Ready without safety evidence"
                            .to_string(),
                    );
                };
                self.publish_native_update_state();
                let worker_attempt = attempt.clone();
                match command.execute(|| {
                    self.apply_staged_update_now(safety_token, mode, Some(worker_attempt), None)
                }) {
                    Ok(()) => UpdateOutcome::Accepted,
                    Err(error) if error.is_activity_deferred() => {
                        let message = error.into_message();
                        if self
                            .native_updater_service
                            .abort_apply(&attempt, message.clone())
                        {
                            self.publish_native_update_state();
                        }
                        UpdateOutcome::Deferred { reason: message }
                    }
                    // An admission REFUSAL: nothing was attempted, so nothing
                    // failed. The stage is re-armed exactly like a deferral,
                    // and the outcome routes to the Blocked lane — the ledger's
                    // non-streak Refused slot and the disposition's cooldown —
                    // instead of the failure streak + physical budget the
                    // `Failed` arm below spends (the field's failing_applies=23
                    // were all this shape; 2026-09-01 audit).
                    Err(error) if error.is_refused() => {
                        let message = error.into_message();
                        if self
                            .native_updater_service
                            .abort_apply(&attempt, message.clone())
                        {
                            self.publish_native_update_state();
                        }
                        UpdateOutcome::Blocked {
                            reasons: vec![message],
                        }
                    }
                    // THE FORK LANE'S PARK MISSED ITS BUDGET (the 2026-09-22/23
                    // update audit, plan P1-4): timing, exactly what the launched
                    // lane's stand-down after its last rung is — so the same
                    // spaced activity retry, never the physical budget that used
                    // to latch this lane after nine misses. A person's apply is
                    // told; it spends nothing either way.
                    Err(error) if error.is_park_missed() => {
                        let message = error.into_message();
                        if self
                            .native_updater_service
                            .abort_apply(&attempt, message.clone())
                        {
                            self.publish_native_update_state();
                        }
                        self.fork_park_miss_outcome(&attempt, mode, message)
                    }
                    // THE FORK LANE'S CAPTURE REFUSED THE DESK (plan P0-3(d)):
                    // the refusal lane, as on the launched lane — one fact, one
                    // verdict. The automatic caller arms the retry (it owns the
                    // intent); every caller records it as a failure. A PERSON'S
                    // press is the launched lane's `Manual` classification, whose
                    // answer is `Failed`: no desk-fingerprint retry is armed for
                    // it, so `CaptureRefused` ("retries when it changes") would
                    // promise that person a retry nothing schedules.
                    Err(error) if error.is_capture_refused() => {
                        let message = error.into_message();
                        if self
                            .native_updater_service
                            .abort_apply(&attempt, message.clone())
                        {
                            self.publish_native_update_state();
                        }
                        fork_capture_refusal_outcome(mode, message)
                    }
                    // Submission failed before the worker could touch disk. Re-arm the
                    // exact authority directly; physical failures always return through
                    // `finish_async_native_update_handoff` with worker-collected facts.
                    Err(error) => self.abort_unstarted_native_apply(&attempt, error.into_message()),
                }
            }
            ApplyDecision::Blocked(reasons) => {
                self.publish_native_update_state();
                // The service preflight ticket HAD to be consumed above — an
                // abandoned `pending_preflight` makes every later
                // `begin_apply_preflight` answer `Joined` for the rest of the
                // process — so the error takes the long way round and is
                // reported here rather than by an early return.
                if let Some(message) = shutdown_error {
                    return UpdateOutcome::Failed { message };
                }
                UpdateOutcome::Blocked { reasons }
            }
            ApplyDecision::Ignored => UpdateOutcome::Failed {
                message: "updater apply preflight became stale".to_string(),
            },
        }
    }

    /// Main-thread completion for any apply attempt that left this process alive,
    /// including the asynchronous overlap waiter. Disk is authoritative: the exact
    /// stage may be re-armed, a consumed/swapped stage becomes InstalledNeedsRelaunch,
    /// and a changed stage retires the old generation before importing the new one.
    ///
    /// `lane` is the completion path's TYPED classification (never a string
    /// match), derived from the [`crate::native_updater_service::ApplyMode`] the
    /// attempt was authorized under plus the worker's activity verdict. An
    /// activity-revoked AUTOMATIC attempt spends bounded
    /// [`AutomaticRetryKind::ActivityRevoked`] budget instead of latching
    /// manual-only; a person's attempt spends nothing at all (see
    /// [`HandoffFailureLane`]).
    pub(crate) fn finish_async_native_update_handoff(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        lane: HandoffFailureLane,
    ) -> Option<UpdateOutcome> {
        self.reconcile_returned_native_apply_with_facts(attempt, facts, message, lane)
    }

    /// Spend one PHYSICAL-failure attempt for this exact artifact and say what
    /// happens next.
    ///
    /// THE ONE PLACE THE PHYSICAL BUDGET IS KEPT. It used to exist only in the
    /// synchronous `(ManualOnly, Failed)` arm, which sees submission-time
    /// failures — while the failures the budget is NAMED for (the four worker
    /// outcomes `PhysicalFailureShape` classifies) return
    /// asynchronously through `abort_reaped_native_apply_before_reconcile` and
    /// `reconcile_returned_native_apply_with_facts`, which stamped a
    /// deadline-less latch and consulted no budget at all. Two lanes, one
    /// user-visible symptom, and the budgeted one was the lane that almost never
    /// fires. Both now come here.
    ///
    /// THE COUNTER IS A LIFETIME COUNT, NOT AN IN-EPOCH ONE, and that is the
    /// change this function exists to carry. It lives on
    /// `auto_apply_physical_retry` rather than inside the latch, because the latch
    /// is destroyed when it lapses and `AutoApplyIntent::attempts` resets when a
    /// fresh intent is armed — only a counter that outlives both can converge. The
    /// epoch and the position within it are DERIVED from it
    /// ([`PHYSICAL_FAILURES_PER_EPOCH`]), so the epoch count survives the
    /// stand-down between epochs; previously the stand-down deliberately outlasted
    /// [`PHYSICAL_RETRY_BUDGET_REPLENISH`], the counter reset on every lapse, and
    /// "converges to manual-only" was true of the doc comment only.
    ///
    /// Three answers, one per lane the caller has to drive:
    ///   * mid-epoch — retry in 600 s, then 1800 s;
    ///   * epoch spent — stand down [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] and start
    ///     the next epoch with the full in-epoch schedule;
    ///   * this SHAPE's [`PhysicalFailureShape::lifetime_attempts`] spent —
    ///     [`PhysicalFailureSchedule::Converged`]. For a STRUCTURAL failure that
    ///     is `retry_at: None`, the latch `arm` reads as `SuppressManualOnly` for
    ///     these exact bytes until a strictly newer build ships or the app
    ///     relaunches: nine failures across three independent epochs and ~14
    ///     hours is evidence about the artifact, not about the machine's
    ///     afternoon, and the user is told once, when the retries are spent
    ///     (`App::react_to_update_apply_outcome`'s decision row, and the
    ///     install-half health warning,
    ///     [`App::announce_automatic_apply_stranded`]), instead of every 40
    ///     minutes forever. For the two shapes that may be about the machine it
    ///     is a quiet re-sample every [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] (the
    ///     2026-09-22/23 update audit, plan P1-1(d)): nine failures across ~14
    ///     hours is enough to stop spending a pill on, never enough to keep a
    ///     healthy build off the machine forever. A shape that proved more
    ///     converges sooner; one that proved less —
    ///     [`PhysicalFailureShape::Unexplained`] — in between.
    ///
    /// Keyed by (build, dmg) so a different artifact starts clean, and gated by
    /// [`PHYSICAL_RETRY_BUDGET_REPLENISH`] so half a day with no physical failure
    /// at all for these bytes starts the whole schedule over.
    ///
    /// `shape` chooses WHICH of the answers above this failure has earned, and it
    /// does so ONLY through [`PhysicalFailureShape::lifetime_attempts`] and the
    /// structural early return. The counter itself is shared and shape-blind on
    /// purpose — it counts physical failures for these exact bytes, which is a fact
    /// no shape disputes — so evidence carries across the classification in the
    /// direction that matters: a structural failure arriving on an artifact that
    /// has already burned its transient budget converges immediately rather than
    /// buying a fresh pair of attempts, and six unexplained failures followed by a
    /// proven structural one do not buy two more.
    ///
    /// [`PhysicalFailureShape::Unexplained`] rides the epoch machinery below rather
    /// than the structural early return, and that is the point of it: an epoch is a
    /// re-sample of the MACHINE, and an unexplained candidate death is exactly the
    /// verdict that might be about the machine. It simply gets fewer epochs.
    fn spend_physical_failure_budget(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        shape: PhysicalFailureShape,
    ) -> PhysicalFailureSchedule {
        let now = std::time::Instant::now();
        let spent = self
            .auto_apply_physical_retry
            .filter(|retry| {
                retry.covers(build, dmg_sha256, activation)
                    && now.duration_since(retry.last_attempt) < PHYSICAL_RETRY_BUDGET_REPLENISH
            })
            .map_or(0, |retry| retry.cycles);
        self.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            activation,
            cycles: spent.saturating_add(1),
            last_attempt: now,
        });
        // `spent` is the number of physical failures these bytes had BEFORE this
        // one, so it is also this failure's 0-based lifetime index. Saturating
        // arithmetic keeps a `u8` that somehow ran away pinned at Converged rather
        // than wrapping back into a fresh budget.
        if spent >= shape.lifetime_attempts().saturating_sub(1) {
            return PhysicalFailureSchedule::converged(shape, now);
        }
        if shape == PhysicalFailureShape::Structural {
            // ONE CONFIRMING RETRY, THEN THE LANE IS DONE WITH THESE BYTES —
            // and by now the retry is the only thing left to decide, because the
            // shape's own ceiling was applied above.
            //
            // THE EPOCH MACHINERY BELOW IS SKIPPED, deliberately and only for this
            // shape: an epoch is a re-sample of the MACHINE, and there is nothing
            // about the machine left to re-sample once the candidate has told us
            // twice that it cannot become this process's successor. (An UNEXPLAINED
            // failure does ride it, precisely because the machine is still one of
            // the things it might have been.)
            //
            // The same first rung as the transient schedule: the difference between
            // the lanes is how many rungs there are, not how far apart the first two
            // sit. Fail-closed if that rung is ever legislated away — converging is
            // the safe answer for a structural failure with no schedule to ride.
            return automatic_retry_delay(0, AutomaticRetryKind::PhysicalFailure)
                .map_or(PhysicalFailureSchedule::converged(shape, now), |delay| {
                    PhysicalFailureSchedule::Retry(now + delay)
                });
        }
        let within_epoch = spent % PHYSICAL_FAILURES_PER_EPOCH;
        match automatic_retry_delay(within_epoch, AutomaticRetryKind::PhysicalFailure) {
            Some(delay) => PhysicalFailureSchedule::Retry(now + delay),
            // This epoch is spent but the lane is not. Stand down long enough that
            // the next epoch is an independent sample of the machine, and — unlike
            // the previous design — short enough that the counter carrying the
            // epoch tally survives it (the compile-time assert on
            // [`PHYSICAL_RETRY_BUDGET_REPLENISH`] is what guarantees that).
            None => PhysicalFailureSchedule::StandDown(now + PHYSICAL_FAILURE_EPOCH_COOLDOWN),
        }
    }

    /// SPEND, LATCH, AND SAY SO: the one sequence every physical handoff failure
    /// takes, on every lane (the 2026-09-22/23 update audit, plan P1-1(a)).
    ///
    /// The synchronous `(ManualOnly, Failed)` arm used to be the only place that
    /// logged which schedule answered and queued the standing note `update
    /// status` prints as `apply_refusal=`. The two ASYNCHRONOUS completions —
    /// `abort_reaped_native_apply_before_reconcile` and
    /// `reconcile_returned_native_apply_with_facts`, which every returned
    /// handoff takes and which minted the latches that stranded 0.90 and 0.91 —
    /// spent the same budget and wrote the same latch in silence: the incident
    /// log holds no line saying the lane had stood down, across 45 hours stuck,
    /// and the durable status carried neither a deadline nor a reason. All three
    /// lanes now call this, so a latch is never written without its line, its
    /// note and — at convergence — the health notice.
    fn latch_after_physical_failure(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        shape: PhysicalFailureShape,
        detail: &str,
    ) -> PhysicalFailureSchedule {
        let schedule = self.spend_physical_failure_budget(build, dmg_sha256, activation, shape);
        self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256,
            activation,
            retry_at: schedule.retry_at(),
        });
        self.say_physical_failure_schedule(build, shape, schedule, detail);
        schedule
    }

    /// The schedule's log line, its standing note, and — when the lane just
    /// converged — the loud notice. Split from [`Self::latch_after_physical_failure`]
    /// only so the words have one author; see that function for why.
    ///
    /// The note is QUEUED on [`crate::App::apply_schedule_standing`] rather than
    /// written here: the surfacing that follows books the failure in the ledger,
    /// booking a failure clears any standing refusal, and so the note has to
    /// land after it — which `record_apply_outcome_for_target_in_ledger` does,
    /// on every lane, in the same order the synchronous arm always used.
    fn say_physical_failure_schedule(
        &mut self,
        build: u64,
        shape: PhysicalFailureShape,
        schedule: PhysicalFailureSchedule,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        let wait_secs = |at: std::time::Instant| at.saturating_duration_since(now).as_secs();
        // The LIFETIME index of this failure against the artifact's budget, not
        // the per-intent counter (which restarts at 1 every time the latch lapses
        // and re-arms — every line of a 599 s → 1799 s → 21599 s escalation used
        // to read "(attempt 1)").
        let lifetime = self
            .auto_apply_physical_retry
            .filter(|retry| retry.build == build)
            .map_or(1, |retry| u32::from(retry.cycles));
        let ceiling = shape.lifetime_attempts();
        match schedule {
            PhysicalFailureSchedule::Retry(at) => aterm_log::info!(
                "update auto-apply: physical handoff failure on build {build} (failure \
                 {lifetime} of {ceiling}, {shape:?}: {detail}); automatic apply is latched off \
                 until the retry window in ~{}s, then eligible again",
                wait_secs(at)
            ),
            PhysicalFailureSchedule::StandDown(at) => aterm_log::warn!(
                "update auto-apply: physical handoff failure on build {build} (failure \
                 {lifetime} of {ceiling}, {shape:?}: {detail}) exhausted this epoch's retry \
                 budget; standing down for ~{}s, then a fresh epoch",
                wait_secs(at)
            ),
            PhysicalFailureSchedule::Converged { resample_at: None } => aterm_log::warn!(
                "update auto-apply: physical handoff failure on build {build} (failure \
                 {lifetime} of {ceiling}, {shape:?}: {detail}) spent this artifact's automatic \
                 attempts; automatic apply for it is done — the Version menu remains"
            ),
            PhysicalFailureSchedule::Converged {
                resample_at: Some(at),
            } => aterm_log::warn!(
                "update auto-apply: physical handoff failure on build {build} (failure \
                 {lifetime} of {ceiling}, {shape:?}: {detail}) spent this artifact's automatic \
                 attempts; it re-samples quietly in ~{}s, and the Version menu applies it sooner",
                wait_secs(at)
            ),
        }
        // THE SCHEDULE, DURABLY (2026-09-14, audit OBS-5): `status.toml` and
        // `update status` carry the pause, not only this process's memory.
        let horizon = |at: std::time::Instant| {
            aterm_types::rfc3339::format_rfc3339(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs())
                    .saturating_add(wait_secs(at)),
            )
        };
        self.apply_schedule_standing = Some(match schedule {
            PhysicalFailureSchedule::Retry(at) => format!(
                "automatic apply of build {build} retries at {} (failure {lifetime} of {ceiling})",
                horizon(at)
            ),
            PhysicalFailureSchedule::StandDown(at) => format!(
                "automatic apply of build {build} is standing down until {} after {lifetime} \
                 failures; the Version menu applies it sooner",
                horizon(at)
            ),
            PhysicalFailureSchedule::Converged { resample_at: None } => format!(
                "automatic apply of build {build} is out of retries after {lifetime} failed \
                 handoffs; the Version menu still applies it"
            ),
            PhysicalFailureSchedule::Converged {
                resample_at: Some(at),
            } => format!(
                "automatic apply of build {build} is out of retries after {lifetime} failed \
                 handoffs and re-samples at {}; the Version menu applies it sooner",
                horizon(at)
            ),
        });
        if let PhysicalFailureSchedule::Converged { resample_at } = schedule {
            self.announce_automatic_apply_stranded(build, shape, lifetime, resample_at, detail);
        }
    }

    /// THE AUTOMATIC LANE STOPPED, SAID AT ONCE (the 2026-09-22/23 update audit,
    /// plan P1-1(c)).
    ///
    /// The loud notice (the install half's R38 health warning,
    /// `aterm_update::health_failing_title("apply")`) used to come only from the
    /// updater's ledger, at [`aterm_update::PERSISTENT_AFTER`] (3) consecutive
    /// apply failures — and a STRUCTURAL failure converges after
    /// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] (2). So the one state in which
    /// the automatic lane had genuinely given up could never be announced: 0.90
    /// and 0.91 each sat staged for 20–25 hours with zero `update-health:` lines
    /// in the log. Convergence now raises that same health warning — through
    /// [`App::note_update_health`], main's one door for it: the band row held
    /// the warning's hold, the record, the heal on landing — and the OS banner
    /// the moment it happens, once per build and verdict.
    ///
    /// ONE DECISION PER STOPPED LANE (rulings 119 and 143): the `Install now`
    /// press is the outcome row's, which `react_to_update_apply_outcome` raises
    /// for a lane that stopped; the warning names the Version menu in words and
    /// carries main's `Software Update` capsule. A lane that re-samples by
    /// itself raises no decision row, and the warning is then the one row.
    ///
    /// NO RESTART PROMPT, deliberately, though the audit's draft asked for one:
    /// the owner's 2026-08-30 ruling is that nothing on the update lane asks for
    /// a restart (`update_words`' guards and `tools/grep_guard.sh` B12 hold it),
    /// and the Version menu's apply is the in-place remedy that ruling leaves.
    pub(crate) fn announce_automatic_apply_stranded(
        &mut self,
        build: u64,
        shape: PhysicalFailureShape,
        failures: u32,
        resample_at: Option<std::time::Instant>,
        detail: &str,
    ) {
        // Once per build and verdict: a re-sample that fails again is quiet, but
        // a lane that goes from re-sampling to stopped (a structural failure
        // arriving on a spent transient budget) says so again, since the old
        // words promised a retry that is no longer coming. Said AT MOST ONCE on
        // glass per launch all the same: `note_update_health` is the one latch
        // for this title, shared with the updater's streak and overdue notices,
        // so a warning already up (or folded and not healed) takes the new
        // words as a log line, and the stopped lane's own outcome row carries
        // the `Install now` press.
        let verdict = (build, resample_at.is_some());
        if self.auto_apply_stranded_announced == Some(verdict) {
            return;
        }
        self.auto_apply_stranded_announced = Some(verdict);
        let kind = match shape {
            PhysicalFailureShape::Structural => "the two builds could not hand off",
            PhysicalFailureShape::Transient => "each handoff missed its moment",
            PhysicalFailureShape::Unexplained => "the new build kept ending unexplained",
        };
        let next = if resample_at.is_some() {
            format!(
                "it retries on its own every {} h, or apply it from the Version menu",
                PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs() / 3600
            )
        } else {
            "apply it from the Version menu".to_string()
        };
        // The row carries the verdict and the remedy; the handoff's own words go
        // to the log line and the ledger, where `update status` reads them.
        let body = format!(
            "automatic apply of build {build} stopped after {failures} failed handoffs \
             ({kind}); {next}. Run `aterm ctl update status` for details."
        );
        aterm_log::warn!("update auto-apply: build {build} converged; last failure: {detail}");
        let title = aterm_update::health_failing_title("apply");
        let announced = self.note_update_health(title, &body);
        // The OS notification a health announcement owes, off the UI thread and
        // in the banner's words, exactly as `Wake::UpdateHealth` delivers it.
        // Never from a unit test: it would post to the desktop of whoever runs
        // the suite.
        #[cfg(all(target_os = "macos", not(test)))]
        if announced {
            let banner = crate::update_words::health_notification_body(&body);
            std::thread::spawn(move || {
                crate::notify::deliver(Some(title), &banner, false);
            });
        }
        #[cfg(not(all(target_os = "macos", not(test))))]
        let _ = announced;
    }

    /// Carry the manual-only latch across a retirement of the stage it was minted
    /// on, now that the bundle under our executable is `installed_build` (the
    /// 2026-09-22/23 update audit, plan P0-6).
    ///
    /// * A latch on `installed_build` ITSELF is about the same logical update —
    ///   our own candidate installed that bundle before it failed — so it is
    ///   RE-KEYED to the bundle's activation identity with `retry_at` UNCHANGED.
    ///   Clearing it here is what let the "confirming retry, ten minutes out"
    ///   fire 0.5 s after the failure and converge 0.90 and 0.91 for a day.
    ///   (The environment block is the one exception: it waits on the installed
    ///   copy changing, and that is exactly what happened, so it is released.)
    /// * A latch on a build no newer than the retired one retires with it.
    /// * A latch on a STRICTLY NEWER build is about a different artifact and
    ///   survives: a newer attempt may have completed out of order.
    fn carry_manual_only_latch_across_swap(
        &mut self,
        retired_build: u64,
        installed_build: u64,
        installed: Option<&InstalledUpdate>,
    ) {
        let Some(manual) = self.auto_apply_manual_only else {
            return;
        };
        if manual.build == installed_build {
            let environment_block = self.auto_apply_environment_block.is_some_and(|blocked| {
                blocked.build == manual.build && blocked.dmg_sha256 == manual.dmg_sha256
            });
            if environment_block {
                self.auto_apply_environment_block = None;
                self.auto_apply_manual_only = None;
                return;
            }
            let current_build = self.native_updater_service.snapshot().current_build;
            let activation = installed
                .filter(|installed| installed.build == installed_build)
                .and_then(|installed| installed.activation_stage(current_build, 0))
                .and_then(|stage| decode_dmg_sha256(&stage.dmg_sha256));
            self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                dmg_sha256: activation.unwrap_or(manual.dmg_sha256),
                activation: true,
                ..manual
            });
            aterm_log::info!(
                "update sync: the manual-only latch on build {installed_build} now names the \
                 installed bundle's activation of it; {}",
                manual.retry_at.map_or_else(
                    || "it holds until the Version menu or a newer build".to_string(),
                    |at| format!(
                        "it still lapses in ~{}s",
                        at.saturating_duration_since(std::time::Instant::now())
                            .as_secs()
                    ),
                )
            );
        } else if manual.build <= retired_build {
            self.auto_apply_manual_only = None;
        }
    }

    /// What a fork-lane park that missed its freeze budget comes back as: on the
    /// automatic lane a spaced activity retry (plan P1-4) that is RECORDED, and
    /// for a person's apply a failure they are told about.
    ///
    /// RECORDED, because this deferral is not the ladder's "not this instant":
    /// the readers parked and the screen froze before it missed. Handed back as
    /// a bare `Deferred`, it reached `try_pending_native_auto_apply`'s early
    /// return, which surfaces and books nothing — so a pool whose capture
    /// always overran the widest rung retried every fifteen minutes forever
    /// with no `apply_refusal=` in `update status`, the overdue notice saying
    /// "no apply attempt has been recorded", and only a WARN in the log. The
    /// launched lane's equivalent stand-down (`ActivityRevoked`) was always
    /// surfaced and booked as a refusal; the two lanes now agree (the
    /// 2026-09-24 review of the 2026-09-22/23 update audit fixes). A refusal,
    /// never a failure: a missed stopwatch is not evidence against the
    /// candidate, and `failing_applies` does not move.
    pub(crate) fn fork_park_miss_outcome(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        mode: crate::native_updater_service::ApplyMode,
        message: String,
    ) -> UpdateOutcome {
        if !mode.is_automatic() {
            return UpdateOutcome::Failed { message };
        }
        let Some(delay) = self.arm_activity_revoked_overlap_retry(attempt) else {
            return UpdateOutcome::Failed { message };
        };
        aterm_log::warn!(
            "update apply: the fork lane's park missed its budget ({message}); automatic retry \
             in {delay:?}"
        );
        self.surface_update_apply_outcome_for_target(
            "automatic",
            UpdateOutcome::Deferred {
                reason: message.clone(),
            },
            false,
            attempt.target_build(),
        );
        UpdateOutcome::Deferred { reason: message }
    }

    /// Consume one activity-revoked overlap retry cycle for this exact artifact
    /// and re-arm the automatic intent at its exponentially spaced deadline.
    /// `None` only when the artifact identity is malformed. The cycle counter
    /// lives on `auto_overlap_retry`, keyed by (build, dmg) — duplicate or
    /// reordered completions for the same artifact can never restart the
    /// spacing, and a different artifact starts at the first rung by
    /// construction. The ladder anchor is untouched: activity delays the
    /// landing, it does not restart the clock.
    fn arm_activity_revoked_overlap_retry(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<std::time::Duration> {
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        let build = attempt.target_build();
        // WHICH SIDE OF THE SWAP this attempt is on. A failed candidate leaves
        // the bundle installed, so the retry for the SAME logical update arrives
        // as an activation; `covers` folds the two sides into one ladder.
        let activation = attempt.is_installed_activation();
        let now = std::time::Instant::now();
        let cycles = self
            .auto_overlap_retry
            .filter(|retry| {
                // A busy stretch ends: once the terminal has gone long enough
                // without a revoked attempt, the spacing starts over.
                retry.covers(build, dmg_sha256, activation)
                    && now.duration_since(retry.last_attempt)
                        < crate::ACTIVITY_RETRY_BUDGET_REPLENISH
            })
            .map_or(0, |retry| retry.cycles);
        let delay = automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked)
            .expect("the activity-revoked spacing saturates and is never exhausted");
        self.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            activation,
            cycles: cycles.saturating_add(1),
            last_attempt: now,
        });
        self.auto_apply_manual_only = None;
        if self
            .auto_apply_ladder
            .is_none_or(|ladder| ladder.build != build)
        {
            // A revocation with no anchor for this build (the attempt was armed
            // before this process learned the ladder): the ladder starts here.
            self.auto_apply_ladder = Some(crate::AutoApplyLadder {
                build,
                armed_at: now,
                announced: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
            });
        }
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: now + delay,
            attempts: cycles,
        });
        Some(delay)
    }

    /// THE DESK, as a capture refusal sees it: the session set, and per session
    /// its rows, cols, alt screen, parser ground and a hash of its reported cwd
    /// — every fact a refusal the capture can still make is about (the session
    /// count and ids for `TooManySessions`/`DuplicateLocalId`; the rest for the
    /// content refusals the ladder lowers today and the next one nobody has found
    /// yet). `None` when an engine is busy this instant; nothing waits for it.
    ///
    /// The 2026-09-22/23 update audit (plan P0-3): the refusal lane is re-tried
    /// when THIS changes, not on a timer — a timer is what relaunched v0.91's
    /// successor into the same refusal ninety-six times a day.
    #[must_use]
    pub(crate) fn capture_desk_fingerprint(&self) -> Option<u64> {
        use std::hash::{Hash as _, Hasher as _};
        let mut sessions = Vec::new();
        for session in self.pool.iter() {
            let terminal = crate::term_try_lock(&session.term)?;
            let mut cwd = std::collections::hash_map::DefaultHasher::new();
            terminal.current_working_directory().hash(&mut cwd);
            sessions.push((
                session.id,
                terminal.rows(),
                terminal.cols(),
                terminal.is_alternate_screen(),
                terminal.parser_is_ground(),
                cwd.finish(),
            ));
        }
        sessions.sort_unstable_by_key(|session| session.0);
        let mut desk = std::collections::hash_map::DefaultHasher::new();
        sessions.hash(&mut desk);
        Some(desk.finish())
    }

    /// The park's capture REFUSED this exact artifact's handoff: record the desk
    /// it refused, and re-arm the automatic intent on the refusal lane's probe —
    /// never the activity spacing (a refusal is not the machine being busy), never
    /// the physical budget (it is not the candidate's bytes), and NEVER A LATCH
    /// (the 2026-09-22/23 update audit, plan P0-3). The failure itself is recorded
    /// by the caller's surfacing, which is what moves `failing_applies` and puts
    /// the session in `update status`'s `apply_failure=`.
    fn arm_capture_refusal_retry(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        self.auto_apply_capture_refusal = Some(crate::AutoApplyCaptureRefusal {
            build,
            dmg_sha256,
            activation,
            desk: self.capture_desk_fingerprint(),
            refused_at: now,
        });
        // The refusal lane owns the intent now; a lapsed physical latch for the
        // same bytes has nothing left to say about it.
        self.auto_apply_manual_only = None;
        if self
            .auto_apply_ladder
            .is_none_or(|ladder| ladder.build != build)
        {
            self.auto_apply_ladder = Some(crate::AutoApplyLadder {
                build,
                armed_at: now,
                announced: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
            });
        }
        let attempts = self
            .auto_apply_intent
            .filter(|intent| intent.build == build)
            .map_or(1, |intent| intent.attempts);
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: now + CAPTURE_REFUSAL_PROBE,
            attempts,
        });
        aterm_log::warn!(
            "update auto-apply: build {build} was refused by the park's capture ({detail}); \
             recorded as an apply failure, never latched — the lane retries once the desk \
             changes (probed every {}s) or after {} min",
            CAPTURE_REFUSAL_PROBE.as_secs(),
            CAPTURE_REFUSAL_BACKSTOP.as_secs() / 60
        );
    }

    /// Whether a standing capture refusal still HOLDS the attempt the poll just
    /// admitted: the same artifact, the desk unchanged, the backstop not yet
    /// reached. Releasing it clears the record, so the attempt that follows is an
    /// ordinary one. A desk that cannot be read this instant holds (the probe
    /// comes back); a refusal recorded while it could not be read takes the first
    /// readable desk as its baseline.
    pub(crate) fn capture_refusal_holds(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        now: std::time::Instant,
    ) -> bool {
        let Some(refusal) = self
            .auto_apply_capture_refusal
            .filter(|refusal| refusal.covers(build, dmg_sha256, activation))
        else {
            return false;
        };
        let release = |app: &mut Self, why: &str| {
            app.auto_apply_capture_refusal = None;
            aterm_log::info!(
                "update auto-apply: {why} since the park's capture refused build {build}; \
                 attempting again"
            );
            false
        };
        if now.saturating_duration_since(refusal.refused_at) >= CAPTURE_REFUSAL_BACKSTOP {
            return release(self, "the one-hour backstop elapsed");
        }
        match (refusal.desk, self.capture_desk_fingerprint()) {
            (_, None) => true,
            (None, Some(baseline)) => {
                self.auto_apply_capture_refusal = Some(crate::AutoApplyCaptureRefusal {
                    desk: Some(baseline),
                    ..refusal
                });
                true
            }
            (Some(refused), Some(desk)) if refused == desk => true,
            (Some(_), Some(_)) => release(self, "the desk changed"),
        }
    }

    fn abort_unstarted_native_apply(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
    ) -> UpdateOutcome {
        if self
            .native_updater_service
            .abort_apply(attempt, message.clone())
        {
            self.publish_native_update_state();
        }
        UpdateOutcome::Failed { message }
    }

    /// A handoff worker has already killed/reaped its child, but ordered disk
    /// reconciliation may still be queued. Re-arm the exact in-memory authority
    /// immediately so the UI never remains Applying while readers resume; the
    /// later generic reconcile wake retires or imports disk authority.
    pub(crate) fn abort_reaped_native_apply_before_reconcile(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
        lane: HandoffFailureLane,
    ) -> UpdateOutcome {
        if self
            .native_updater_service
            .abort_apply(attempt, message.clone())
        {
            // A PERSON'S FAILURE IS NOT THE BACKGROUND LANE'S BUSINESS. The apply
            // authority above is reduced either way (the artifact remains staged
            // and the UI leaves Applying), but nothing below this line may run:
            // spending the automatic budget here converged the background lane on
            // a human's retries, and the freshly-stamped record silenced the pill
            // for the very person who asked. See [`HandoffFailureLane`].
            if !lane.charges_the_automatic_lane() {
                self.publish_native_update_state();
                return UpdateOutcome::Failed { message };
            }
            // MIRRORS the `Rearmed` policy in
            // `reconcile_returned_native_apply_with_facts` (the lane every
            // completion actually takes: no `UpdateHandoffCompletion` carries
            // worker facts). ACTIVITY re-arms on the spaced ladder — never a
            // latch; a PHYSICAL failure spends its converging budget.
            if lane == HandoffFailureLane::ActivityRevoked
                && let Some(delay) = self.arm_activity_revoked_overlap_retry(attempt)
            {
                self.publish_native_update_state();
                aterm_log::info!(
                    "update apply: activity revoked the overlap; automatic retry in {delay:?}"
                );
                return UpdateOutcome::Deferred { reason: message };
            }
            // A CAPTURE REFUSAL: recorded (by the caller's surfacing), retried
            // when the desk changes, never latched (plan P0-3).
            if lane == HandoffFailureLane::Refused
                && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
            {
                self.arm_capture_refusal_retry(
                    attempt.target_build(),
                    dmg_sha256,
                    attempt.is_installed_activation(),
                    &message,
                );
                self.restate_staged_bar_posture(attempt.target_build());
                self.publish_native_update_state();
                return UpdateOutcome::CaptureRefused { message };
            }
            if let HandoffFailureLane::Physical(shape) = lane
                && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
            {
                // THE LANE EVERY RETURNED HANDOFF TAKES, and it used to write the
                // latch with no log line and no standing reason (plan P1-1(a)):
                // the 2026-09-22/23 incident log holds zero lines saying the lane
                // had stood down for good, across 45 hours stuck.
                let _ = self.latch_after_physical_failure(
                    attempt.target_build(),
                    dmg_sha256,
                    attempt.is_installed_activation(),
                    shape,
                    &message,
                );
            }
            self.auto_apply_intent = None;
            // The Staged bar, if it is still up for this build, stands down with
            // the lane — not after the next unrelated reconcile.
            self.restate_staged_bar_posture(attempt.target_build());
            self.publish_native_update_state();
        }
        UpdateOutcome::Failed { message }
    }

    fn reconcile_returned_native_apply_with_facts(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        lane: HandoffFailureLane,
    ) -> Option<UpdateOutcome> {
        let durable_enabled = facts
            .durable
            .as_ref()
            .is_some_and(|durable| durable.enabled);
        let durable_staged_build = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_build);
        let durable_staged_commit = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_commit.as_deref());
        let durable_staged_digest = facts
            .durable
            .as_ref()
            .and_then(|durable| durable.staged_dmg_sha256.as_deref());
        let disposition = self.native_updater_service.finish_returned_apply(
            &attempt,
            ReturnedApplyFacts::new(
                durable_enabled,
                durable_staged_build,
                durable_staged_commit,
                durable_staged_digest,
                facts.installed.as_ref(),
            ),
            message.clone(),
        );
        match disposition {
            ReturnedApplyDisposition::Rearmed => {
                // A PERSON'S FAILURE CHARGES NOTHING, exactly as in the sibling
                // reaped-abort lane: the stage is re-armed on disk, the facts are
                // reduced, and the automatic lane's budgets, latch and live intent
                // are left precisely as they were. See [`HandoffFailureLane`].
                if !lane.charges_the_automatic_lane() {
                    self.publish_native_update_state();
                    self.reduce_returned_apply_facts(facts);
                    return Some(UpdateOutcome::Failed { message });
                }
                // ACTIVITY-REVOKED: the exact stage was re-armed on disk and the
                // rollback was lossless, so schedule one spaced re-attempt —
                // never a latch. A genuine failure (the lane is `Physical`)
                // spends the converging budget and takes the manual latch.
                if lane == HandoffFailureLane::ActivityRevoked
                    && let Some(delay) = self.arm_activity_revoked_overlap_retry(&attempt)
                {
                    self.publish_native_update_state();
                    self.reduce_returned_apply_facts(facts);
                    aterm_log::info!(
                        "update apply: activity revoked the overlap; automatic retry in {:?}",
                        delay
                    );
                    return Some(UpdateOutcome::Deferred { reason: message });
                }
                // A CAPTURE REFUSAL: its own lane — recorded, retried when the
                // desk changes, never latched (plan P0-3).
                if lane == HandoffFailureLane::Refused
                    && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
                {
                    self.arm_capture_refusal_retry(
                        attempt.target_build(),
                        dmg_sha256,
                        attempt.is_installed_activation(),
                        &message,
                    );
                    self.restate_staged_bar_posture(attempt.target_build());
                    self.publish_native_update_state();
                    self.reduce_returned_apply_facts(facts);
                    return Some(UpdateOutcome::CaptureRefused { message });
                }
                if let HandoffFailureLane::Physical(shape) = lane
                    && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
                {
                    // Logged and recorded like every other lane (plan P1-1(a)).
                    let _ = self.latch_after_physical_failure(
                        attempt.target_build(),
                        dmg_sha256,
                        attempt.is_installed_activation(),
                        shape,
                        &message,
                    );
                }
                self.auto_apply_intent = None;
                self.restate_staged_bar_posture(attempt.target_build());
                self.publish_native_update_state();
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::Failed { message })
            }
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build } => {
                self.auto_apply_intent = None;
                // OUR OWN CANDIDATE SWAPPED THE BUNDLE AND THEN FAILED (the
                // 2026-09-22/23 update audit, plan P0-6). This arm used to clear
                // the latch and charge nothing, so the activation the reduction
                // below imports armed at +500 ms: a physical failure bought an
                // immediate retry instead of its schedule's. The failure is
                // charged like every other returned one — the budget folds the
                // download and the activation into one count — and the latch is
                // then carried to the activation it now guards.
                if let HandoffFailureLane::Physical(shape) = lane
                    && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
                {
                    let _ = self.latch_after_physical_failure(
                        attempt.target_build(),
                        dmg_sha256,
                        attempt.is_installed_activation(),
                        shape,
                        &message,
                    );
                }
                self.carry_manual_only_latch_across_swap(build, build, facts.installed.as_ref());
                self.publish_native_update_state();
                // A newer artifact is imported only when it exceeds the canonical
                // installed build; the fact reducer enforces that floor.
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: Self::INSTALLED_ACTIVATES_IN_PLACE.to_string(),
                })
            }
            ReturnedApplyDisposition::Retired => {
                self.auto_apply_intent = None;
                // Retiring old authority must not clear a sticky latch for a newer
                // artifact that may already have completed out of order.
                if self.auto_apply_manual_only.is_some_and(|manual| {
                    manual.build == attempt.target_build()
                        && decode_dmg_sha256(attempt.target_dmg_sha256()) == Some(manual.dmg_sha256)
                }) {
                    self.auto_apply_manual_only = None;
                }
                self.publish_native_update_state();
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::Failed {
                    message: format!(
                        "{message}; the durable stage changed and the old apply intent was retired"
                    ),
                })
            }
            // A delayed callback for attempt A must not perturb attempt B's UI. The
            // service is reducer-inert and this `None` forbids logs, alerts, redraws,
            // or misleading "remains ready" text at the caller.
            ReturnedApplyDisposition::Ignored => None,
        }
    }

    fn native_update_close_preflight(&self) -> (ClosePreflight, Option<NativeUpdateSafetyToken>) {
        let documents = self
            .view_store
            .iter()
            .filter_map(|(_, link)| match link {
                crate::tab_model::View::Native(native) => {
                    self.native_runtime.document_id(native.instance)
                }
                crate::tab_model::View::Terminal(_) => None,
            })
            .collect::<std::collections::BTreeSet<_>>();
        let dirty_documents = documents
            .iter()
            .filter(|document| self.document_store.dirty(**document) == Some(true))
            .count();
        let failed_checkpoints = documents
            .iter()
            .filter(|document| {
                matches!(
                    self.document_store.phase(**document),
                    Some(crate::document_store::DocumentPhase::Blocked { .. })
                )
            })
            .count();
        let pending_checkpoints = documents
            .iter()
            .filter(|document| {
                matches!(
                    self.document_store.phase(**document),
                    Some(crate::document_store::DocumentPhase::Closing { .. })
                )
            })
            .count();
        let settings_drafts = self
            .view_store
            .iter()
            .filter(|(view, link)| {
                let crate::tab_model::View::Native(native) = link else {
                    return false;
                };
                self.native_runtime
                    .app(native.instance)
                    .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
                    && self
                        .native_runtime
                        .presentation(native.instance, *view)
                        .is_ok_and(|presentation| {
                            presentation.indicators.dirty && !presentation.closable
                        })
            })
            .count();

        let mut blockers = Vec::new();
        if settings_drafts > 0 {
            blockers.push(format!(
                "{SETTINGS_DRAFTS_BLOCK} {settings_drafts} Settings view(s) have unsaved text"
            ));
        }
        if dirty_documents > 0 {
            blockers.push(format!(
                "{DIRTY_DOCUMENTS_BLOCK} {dirty_documents} document(s) have uncheckpointed edits"
            ));
        }
        if failed_checkpoints > 0 {
            blockers.push(format!(
                "{FAILED_CHECKPOINTS_BLOCK} {failed_checkpoints} document checkpoint(s) previously failed"
            ));
        }
        if pending_checkpoints > 0 {
            blockers.push(format!(
                "Wait: {pending_checkpoints} document checkpoint(s) are still running"
            ));
        }
        if self.pending_restore.is_some() || !self.seamless_adopt.is_empty() {
            blockers.push(Self::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string());
        }
        if blockers.is_empty() {
            (
                ClosePreflight::Ready,
                Some(NativeUpdateSafetyToken { _private: () }),
            )
        } else {
            (ClosePreflight::Blocked(blockers), None)
        }
    }

    /// Re-run native document/restore safety at asynchronous handoff completion.
    /// The original token authorizes preparation only; user edits can occur while the
    /// child boots, so the outgoing process must obtain fresh evidence before exit.
    pub(crate) fn revalidate_native_update_safety(
        &self,
    ) -> Result<NativeUpdateSafetyToken, Vec<String>> {
        match self.native_update_close_preflight() {
            (ClosePreflight::Ready, Some(token)) => Ok(token),
            (ClosePreflight::Blocked(reasons), None) => Err(reasons),
            _ => Err(vec![
                "Native update safety preflight returned inconsistent evidence".to_string(),
            ]),
        }
    }

    /// QA same-binary reexec through the exact production native-state preflight.
    /// This keeps `ATERM_DEBUG_SEAMLESS_REEXEC` useful without granting it a bypass
    /// around dirty documents, checkpoint work, or restore/adoption state.
    pub(crate) fn apply_debug_seamless_update(&mut self) -> UpdateOutcome {
        // A QA seam is driven by a person at a keyboard, so a blocker is the
        // answer to something they just did: surface the recovery commands.
        match self.prepare_all_native_shutdown(
            crate::native_app::CloseScope::Relaunch,
            crate::app_tabs::ClosePreflightVisibility::Interactive,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return UpdateOutcome::Blocked {
                    reasons: vec![Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()],
                };
            }
            Err(message) => return UpdateOutcome::Failed { message },
        }
        let (readiness, safety_token) = self.native_update_close_preflight();
        match readiness {
            ClosePreflight::Blocked(reasons) => UpdateOutcome::Blocked { reasons },
            ClosePreflight::Ready => {
                let Some(token) = safety_token else {
                    return UpdateOutcome::Failed {
                        message: "debug update preflight returned Ready without safety evidence"
                            .to_string(),
                    };
                };
                match self.apply_staged_update_now(token, ApplyMode::Immediate, None, None) {
                    Ok(()) => UpdateOutcome::Accepted,
                    Err(error) => UpdateOutcome::Failed {
                        message: error.into_message(),
                    },
                }
            }
        }
    }

    /// Clean-quit hook for an update previously deferred with Install When Safe.
    /// Returns true only when an asynchronous overlap child now owns the quit:
    /// callers must defer structural teardown/`el.exit()` until its completion
    /// wake succeeds or rolls back. A synchronous exec does not return on success;
    /// a returned failure leaves no pending handoff and lets quitting continue.
    pub(crate) fn apply_deferred_native_update_on_clean_quit(&mut self) -> bool {
        if !self.native_updater_service.snapshot().install_on_clean_quit {
            return false;
        }
        let accepted = matches!(
            self.apply_native_update(ApplyMode::CleanQuit),
            UpdateOutcome::Accepted
        );
        accepted && self.update_handoff_in_flight()
    }

    /// Viewing the exact published update revision quiets its one announcement
    /// without hiding the staged artifact or inventing a second notification state.
    pub(crate) fn acknowledge_native_update_attention(&mut self) {
        let Some(revision) = self.native_updater_service.snapshot().attention_revision else {
            return;
        };
        if self.native_updater_service.acknowledge_attention(revision) {
            self.publish_native_update_state();
        }
    }

    pub(crate) fn refresh_native_presentation(
        &mut self,
        wid: WindowId,
        _instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) {
        let tab = self.windows.get(&wid).and_then(|window| {
            window
                .tab_set
                .tabs()
                .iter()
                .find(|tab| tab.root.leaves().contains(&view))
                .map(|tab| tab.id)
        });
        if let Some(tab) = tab {
            self.refresh_aggregate_tab_presentation(wid, tab);
        } else {
            self.refresh_active_split_presentation(wid);
        }
        self.refresh_window_tabs(wid);
    }

    /// Fold presentation across every leaf of the active split. Focus supplies
    /// title/icon while dirty, busy and attention remain independently visible
    /// if any sibling owns them.
    pub(crate) fn refresh_active_split_presentation(&mut self, wid: WindowId) {
        let Some((tab_id, focused, leaves, stale_title)) =
            self.windows.get(&wid).and_then(|window| {
                let tab = window.tab_set.active()?;
                // The keep-stale rung below is this tab's OWN previous title —
                // captured here, before any leaf is re-read, because that is the
                // only value in reach that this same fold wrote.
                Some((
                    tab.id,
                    tab.focus,
                    tab.root.leaves(),
                    tab.presentation.title.clone(),
                ))
            })
        else {
            return;
        };
        let mut presentations = Vec::with_capacity(leaves.len());
        for view in leaves {
            let Some(linked) = self.view_store.get(view).copied() else {
                continue;
            };
            let presentation = match linked {
                crate::tab_model::View::Terminal(terminal) => {
                    // NONBLOCKING + KEEP-STALE: this runs on the winit thread from
                    // every tab switch/focus change (`sync_window`), and the reader
                    // thread holds this exact mutex for a whole ingest slice, so a
                    // blocking `lock()` parks the gesture behind the flooding pane's
                    // parser.
                    //
                    // SAME-RUNG STALENESS. What this fold writes is
                    // `tab.presentation.title`, deliberately stable model metadata
                    // (`app_control.rs`) read back as the FALLBACK rung by
                    // `tab_titles`, `refill_strip_titles`, `window_title_identity`
                    // and the `tabs` verb — and unlike `tab_title_cache` it is only
                    // corrected by the next structural sync, so a wrong value can
                    // linger. This function's rung is the RAW OSC title (or
                    // `"aterm"`) and nothing else, so on contention the only value
                    // we may reuse is this tab's own previous title: keeping it
                    // leaves the field exactly as it was, which is what a blocking
                    // read that returned the unchanged title would have produced.
                    // `tab_title_cache` must NOT be consulted here — `tab_titles`
                    // fills it from `resolved_terminal_title_rung`, so it can hold
                    // the operator's `meta set title` or the `~`-abbreviated cwd,
                    // and importing it would persist a foreign rung into the stable
                    // metadata. A tab with no prior title lands on the same
                    // `"aterm"` a titleless pane gets, and the next output wake
                    // re-publishes the true title either way.
                    let title = match self.pool.get(terminal.session) {
                        Some(session) => match session.term.try_lock() {
                            Ok(term) => Some(term.title().to_string()),
                            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                                Some(poisoned.into_inner().title().to_string())
                            }
                            Err(std::sync::TryLockError::WouldBlock) => Some(stale_title.clone()),
                        },
                        None => None,
                    };
                    let title = title
                        .filter(|title| !title.is_empty())
                        .unwrap_or_else(|| "aterm".to_string());
                    let mut presentation = crate::tab_model::TabPresentation::terminal(title);
                    // The same mapping the shared leaf builder applies: a window
                    // sync must not blank the indicators a status change just
                    // published for this pane.
                    presentation.indicators = self.session_status_indicators(terminal.session);
                    presentation
                }
                crate::tab_model::View::Native(native) => {
                    let Ok(presentation) = self.native_runtime.presentation(native.instance, view)
                    else {
                        continue;
                    };
                    crate::tab_model::TabPresentation {
                        title: presentation.title,
                        icon: Some(match presentation.icon {
                            crate::native_app::AppIcon::Settings => {
                                crate::tab_model::TabIconKind::Settings
                            }
                            crate::native_app::AppIcon::Markdown => {
                                crate::tab_model::TabIconKind::Markdown
                            }
                            crate::native_app::AppIcon::Editor => {
                                crate::tab_model::TabIconKind::Editor
                            }
                            crate::native_app::AppIcon::Recovery => {
                                crate::tab_model::TabIconKind::Recovery
                            }
                        }),
                        indicators: crate::tab_model::TabIndicators {
                            dirty: presentation.indicators.dirty,
                            busy: presentation.indicators.busy,
                            // A native leaf is the OUT-OF-BAND attention owner;
                            // it has no session and therefore no classified
                            // status.
                            attention: presentation.indicators.attention,
                            status_attention: false,
                        },
                        // No session, no edge table, no connection role.
                        conn: None,
                        closable: presentation.closable,
                        tooltip: presentation.tooltip,
                    }
                }
            };
            presentations.push((view, presentation));
        }
        let Some(presentation) = crate::tab_model::aggregate_presentations(focused, presentations)
        else {
            return;
        };
        if let Some(window) = self.windows.get_mut(&wid)
            && let Some(index) = window
                .tab_set
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)
            && let Some(tab) = window.tab_set.tab_at_mut(index)
        {
            tab.presentation = presentation;
        }
    }
}

/// The Check's fail-fast answer when a person's typed verb holds the store (pid `pid`).
fn check_held_by_person_message(pid: u32) -> String {
    format!(
        "A package command running in a terminal (pid {pid}) is using the package store; \
         check again when it finishes."
    )
}

/// The failure sentence for one finished `atpkg` child of the Packages worker, or
/// `None` when it succeeded — pure over the exit status, the argv and the child's
/// stderr, so the classification is testable without a child.
///
/// CONTENTION: the Check runs the window's own pass argv (`--wait-lock`, Phase 3), so its
/// 75 means a half-hour wait ran out; the worker answers it at once, before any spawn,
/// only when a PERSON's typed verb holds the store. Every other verb still fails fast —
/// a person clicked a button, and a wait they cannot see is worse than a sentence they
/// can act on. Either refusal is classified by CODE (atpkg reserves 75, `EX_TEMPFAIL`,
/// for contention, 2026-09-10), and its sentence names the other installer rather than
/// the lock file; the offline code (69) says offline. The headline stays the operation's
/// `failed_headline`: `PackagesCommandOutcome` is pinned to {none, success, failure} by
/// the aterm-spec model (`models_native.rs` / `native_packages_conformance.rs`), so a
/// distinct "deferred" outcome is a separate slice.
fn packages_child_failure(
    result: Result<std::process::ExitStatus, &std::io::Error>,
    verb: &[String],
    said: &str,
) -> Option<String> {
    match result {
        Ok(status) if status.success() => None,
        // EXIT 2 = "ran fine, installed nothing, and never will here"
        // (atpkg `cmd_install_default_set`: the signed index pins no
        // build for this machine's architecture). It is neither a
        // success nor a retryable failure, and reporting it as either
        // lies to the user — "install completed" over an empty store,
        // or a red error they will keep re-clicking. Give it its own
        // words.
        Ok(status) if status.code() == Some(2) && verb.iter().any(|v| v == "--default-set") => {
            Some(
                "Nothing was installed: the registry served no package \
                 this machine can run. This is not a temporary error — \
                 retrying will not change it."
                    .to_string(),
            )
        }
        // The offline code: the pass ran and nothing answered — the index listing and every
        // vendor channel. Not a fault of the store; said as what it is.
        Ok(status)
            if status.code()
                == Some(i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT)) =>
        {
            Some(
                "Could not reach the package channels \u{2014} this Mac looks offline. The \
                 installed packages are unchanged."
                    .to_string(),
            )
        }
        // EXIT 75 = another atpkg pass holds the store lock — the window's own
        // launch/loop pass mid-install (it can hold the lock for the length of a
        // multi-GB download), another window's, or a terminal `aterm pkg …`.
        // Transient by definition; the page's status refresh shows the store
        // that pass leaves behind.
        Ok(status) if status.code() == Some(i32::from(atpkg::lock::CONTENDED_EXIT)) => {
            // A FIXED sentence (2026-09-18): the child's own line is atpkg's
            // "another atpkg process holds the store lock at …", which names a
            // peer (after a self-update the holder is this app's own predecessor's
            // pass) and a lock (a word the owner read as a fault) — the exit code
            // already says all there is to say.
            Some(
                "A toolchain pass is already running on this store; it finishes by itself \
                 \u{2014} try again in a moment."
                    .to_string(),
            )
        }
        Ok(status) => Some(if said.is_empty() {
            format!("atpkg {} exited with {status}", verb.join(" "))
        } else {
            // atpkg's own sentence, which names the cause and often
            // the remedy — better than the exit code every time.
            said.lines().last().unwrap_or(said).to_string()
        }),
        Err(error) => Some(format!(
            "could not launch atpkg {}: {error}",
            verb.join(" ")
        )),
    }
}

/// Arrival at the machine worker's single dispatch seam.
enum MachineReadEvent {
    Request,
    Finished(Result<atpkg::machine::MachineState, String>),
}

/// Resolve admission once, then launch at most one worker. A completion's
/// `true` answer already owns the coalesced rerun; it must not re-enter Request.
/// The injected operation is thread creation, never the machine read itself.
fn dispatch_native_machine_read(
    service: &mut crate::packages_screen::PackagesService,
    event: MachineReadEvent,
    spawn: impl FnOnce() -> Result<(), String>,
) {
    let admitted = match event {
        MachineReadEvent::Request => service.request_machine_read(),
        MachineReadEvent::Finished(result) => service.replace_machine_state(result),
    };
    if admitted && let Err(error) = spawn() {
        // No event can interleave on this synchronous main-thread edge, so a
        // failed launch has no queued follower and must release its reservation.
        let rerun = service.replace_machine_state(Err(error));
        debug_assert!(!rerun);
    }
}

fn launch_native_machine_read(
    proxy: winit::event_loop::EventLoopProxy<Wake>,
) -> Result<(), String> {
    let atpkg = crate::co_located_atpkg()
        .ok_or_else(|| "no co-located atpkg binary beside this executable".to_string())?;
    spawn_native_machine_read(proxy, atpkg)
        .map_err(|error| format!("could not start the machine read: {error}"))
}

/// The detached machine read thread: one `atpkg machine` child, its parsed record
/// posted back as [`Wake::NativeMachineStateFinished`]. The launch helper serves
/// both an admitted initial request and an already reserved completion rerun;
/// the `Err` is the thread-spawn failure.
fn spawn_native_machine_read(
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    atpkg: std::path::PathBuf,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("aterm-machine-status".into())
        .spawn(move || {
            // A host-settings probe is cosmetic to the terminal; it lands via
            // the proxy when done.
            crate::qos::set_self(crate::qos::Role::Background);
            let result = read_machine_state(&atpkg);
            let _ = proxy.send_event(Wake::NativeMachineStateFinished { result });
        })
        .map(drop)
}

/// Run bare `atpkg machine` and parse its `machine-state:` line. The child gets the
/// same environment the launch-time apply gets (`spawn_machine_settings_once`): the
/// spawner pid and the atpkg child PATH. Both output streams are bounded; only
/// the stdout state line is the measured contract.
fn read_machine_state(atpkg: &std::path::Path) -> Result<atpkg::machine::MachineState, String> {
    let mut command = crate::qos::command(crate::qos::Role::Background, atpkg);
    command
        .arg("machine")
        .env(atpkg::cli::SPAWNER_PID_ENV, std::process::id().to_string())
        .env("PATH", crate::spawn::atpkg_child_path());
    let output = machine_command_output(
        &mut command,
        "atpkg machine",
        std::time::Duration::from_secs(15),
    )?;
    parse_machine_state_output(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
        if output.status.success() {
            // The line is printed after atpkg's own platform check, so its absence
            // means "no state was measured" — which the card must say, and must
            // never read as "nothing to apply".
            "atpkg machine printed no state".to_string()
        } else {
            format!("atpkg machine exited with {}", output.status)
        }
    })
}

/// A machine command has one deadline and bounded stdout/stderr. Nonblocking
/// pipe reads avoid a reader thread or a wait for a descendant's inherited stdout.
/// Cleanup spends only the reserved tail of the original budget and reports an
/// unconfirmed reap instead of waiting indefinitely after a failed kill.
///
/// NOT GATED, AND THE SPLIT IS ONE FUNCTION DEEP (2026-09-16). This wrapper and
/// [`Overflow`] used to be `#[cfg(unix)]` with a hand-written `#[cfg(not(unix))]`
/// twin of the wrapper alone, and the twin was one item short: a packages-screen
/// call site that reached for `machine_command_output_bounded(.., Overflow::Truncate)`
/// left the Windows build of THIS crate failing to compile with E0433 on the type
/// and E0425 on the function — invisible from a Unix box, because a mirror only
/// drifts on the side you cannot see. So there is now ONE `Overflow` and ONE
/// wrapper for every target, and the only thing a `cfg` still chooses is the BODY
/// of [`machine_command_output_bounded`], whose two arms carry the same signature.
fn machine_command_output(
    command: &mut std::process::Command,
    operation: &str,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    machine_command_output_bounded(command, operation, limit, Overflow::Fail)
}

/// What a flood of output means for this child.
///
/// A CHILD THAT IS CHANGING THE MACHINE IS NOT KILLED FOR TALKING TOO MUCH (2026-09-15).
/// The 64 KiB cap exists to bound THIS process's memory, and the only reason to stop
/// early is that nothing more can be learned. For a read that is true — the record is one
/// line, and a reader that floods is broken. For `atpkg machine apply` it is false and
/// dangerous: the apply narrates two lines per migrated directory, so a machine with
/// enough repositories crosses 64 KiB legitimately, and the old arm answered that by
/// SIGKILLing the child — possibly between the `rename` and the symlink that keeps the
/// build working. Truncation costs the tail of a narration nobody parses (the state is
/// re-read afterwards anyway); a kill costs the user a repository.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    /// Stop with an error, and stop the child with it.
    Fail,
    /// Keep the first 64 KiB, go on draining so the child never blocks on a full
    /// pipe, and let it finish its work.
    Truncate,
}

#[cfg(unix)]
fn machine_command_output_bounded(
    command: &mut std::process::Command,
    operation: &str,
    limit: std::time::Duration,
    overflow: Overflow,
) -> Result<std::process::Output, String> {
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    const MAX_OUTPUT: usize = 64 * 1024;
    const POLL: Duration = Duration::from_millis(5);
    let deadline = Instant::now() + limit;
    let reserve = Duration::from_millis(250).min(limit / 4);
    let read_deadline = deadline - reserve;
    if Instant::now() >= read_deadline {
        return Err(format!("{operation} timed out"));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not launch {operation}: {error}"))?;
    let result = (|| {
        let mut stdout_pipe = child
            .stdout
            .take()
            .ok_or("machine command stdout missing")?;
        let mut stderr_pipe = child
            .stderr
            .take()
            .ok_or("machine command stderr missing")?;
        for fd in [stdout_pipe.as_raw_fd(), stderr_pipe.as_raw_fd()] {
            aterm_pty::set_nonblocking(fd, true)
                .map_err(|error| format!("{operation} output: {error}"))?;
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut stdout_eof = false;
        let mut stderr_eof = false;
        let mut status = None;
        loop {
            if Instant::now() >= read_deadline {
                return Err(format!("{operation} timed out"));
            }
            // One bounded chunk per pipe per turn checks the deadline during a
            // flood and drains stderr while stdout is waiting for more data.
            for (pipe, bytes, eof) in [
                (
                    &mut stdout_pipe as &mut dyn std::io::Read,
                    &mut stdout,
                    &mut stdout_eof,
                ),
                (
                    &mut stderr_pipe as &mut dyn std::io::Read,
                    &mut stderr,
                    &mut stderr_eof,
                ),
            ] {
                if !*eof {
                    let mut chunk = [0_u8; 4096];
                    match pipe.read(&mut chunk) {
                        Ok(0) => *eof = true,
                        Ok(n) => {
                            if bytes.len() + n > MAX_OUTPUT {
                                if overflow == Overflow::Fail {
                                    return Err(format!(
                                        "{operation} output exceeded 64 KiB per stream"
                                    ));
                                }
                                // Keep the head, drop this chunk, keep reading: the
                                // point of the loop from here on is that the child's
                                // pipe never fills, so it runs to completion.
                                let room = MAX_OUTPUT.saturating_sub(bytes.len());
                                bytes.extend_from_slice(&chunk[..room.min(n)]);
                            } else {
                                bytes.extend_from_slice(&chunk[..n]);
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                ErrorKind::WouldBlock | ErrorKind::Interrupted
                            ) => {}
                        Err(error) => return Err(format!("{operation} output: {error}")),
                    }
                }
            }
            if status.is_none() {
                status = child
                    .try_wait()
                    .map_err(|error| format!("{operation} wait: {error}"))?;
            }
            if stdout_eof
                && stderr_eof
                && let Some(status) = status
            {
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            std::thread::sleep(POLL.min(read_deadline.saturating_duration_since(Instant::now())));
        }
    })();
    result.map_err(|mut error| {
        // On early errors, do not turn a short failure into the full command
        // budget. No blocking wait or reader join is permitted here.
        let cleanup_deadline = deadline.min(Instant::now() + reserve);
        if !stop_machine_read_child(&mut child, cleanup_deadline) {
            error.push_str("; child exit could not be confirmed within the cleanup budget");
        }
        error
    })
}

#[cfg(unix)]
fn stop_machine_read_child(child: &mut std::process::Child, deadline: std::time::Instant) -> bool {
    use std::time::{Duration, Instant};
    if matches!(child.try_wait(), Ok(Some(_))) {
        return true;
    }
    let _ = child.kill();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Err(_) => return false,
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(
            Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
        );
    }
}

/// The other arm of the ONE split, with the SAME signature as the Unix one above
/// — `Overflow` included, so every caller the Unix side accepts type-checks here
/// too and a new argument cannot land on one arm only. Nothing off Unix has the
/// nonblocking-pipe machinery the real body is built from, and no platform but
/// macOS has machine settings to apply, so this refuses before spawning anything.
#[cfg(not(unix))]
fn machine_command_output_bounded(
    _command: &mut std::process::Command,
    _operation: &str,
    _limit: std::time::Duration,
    _overflow: Overflow,
) -> Result<std::process::Output, String> {
    Err("machine settings commands require macOS".into())
}

/// The `machine-state:` record in one `atpkg machine` stdout, if any line carries a
/// parseable one (prose lines are skipped; a malformed record is no record).
pub(crate) fn parse_machine_state_output(stdout: &str) -> Option<atpkg::machine::MachineState> {
    stdout.lines().find_map(|line| {
        crate::r6_marker_body(line, atpkg::cli::MACHINE_STATE_MARKER)
            .and_then(atpkg::machine::parse_machine_state)
    })
}

/// Deliver measured changes before settling the explicit apply. A deadline or
/// failed write is not success even if earlier changes landed or the child exited 0.
fn machine_apply_completion(
    output: Result<std::process::Output, String>,
    mut emit: impl FnMut(Wake),
) -> (
    PackagesCommandOutcome,
    Option<String>,
    Option<atpkg::machine::MachineState>,
) {
    let operation = PackagesBusy::MachineApply;
    let output = match output {
        Ok(output) => output,
        Err(message) => {
            return (
                PackagesCommandOutcome::Failed {
                    operation,
                    message: format!(
                        "{message}; changes may be partial — refresh This Mac before retrying"
                    ),
                },
                None,
                None,
            );
        }
    };
    let read = machine_apply_stdout(&String::from_utf8_lossy(&output.stdout));
    for event in read.events {
        emit(event);
    }
    let failure = read.refusal.or_else(|| {
        packages_child_failure(
            Ok(output.status),
            &["machine".to_string(), "apply".to_string()],
            String::from_utf8_lossy(&output.stderr).trim(),
        )
    });
    let command = match failure {
        Some(message) => PackagesCommandOutcome::Failed { operation, message },
        None => PackagesCommandOutcome::Succeeded { operation },
    };
    (command, read.verdict, read.record)
}

/// What an `atpkg machine apply` child's stdout said, sorted for the worker.
pub(crate) struct MachineApplyStdout {
    /// The marker rows to raise (`machine-settings:` → the pull-down row, the
    /// appstatus ledger and the card's "Last change").
    pub(crate) events: Vec<Wake>,
    /// The verdict sentence after [`atpkg::cli::MACHINE_VERDICT_PREFIX`] —
    /// `applied — …`, `nothing changed — …` or `not applied — …` — the last such
    /// line.
    pub(crate) verdict: Option<String>,
    /// [`atpkg::cli::MACHINE_NOT_APPLIED_PREFIX`]` <reason>`: the pass refused (a
    /// home that is not the account's), or `MACHINE_APPLY_FAILED_PREFIX`: a write
    /// failed. Either is a failure even when the command exits successfully.
    pub(crate) refusal: Option<String>,
    /// The apply's own `machine-state:` record, parsed (2026-09-16) — carried on the
    /// completion rather than posted as an event, so it is this verb's and no other
    /// lane's that confirms the card.
    pub(crate) record: Option<atpkg::machine::MachineState>,
}

/// Sort an `atpkg machine apply` stdout into marker events, the verdict sentence and
/// any refusal or failed write. Pure, so these shapes are pinned by tests rather
/// than a child on the developer's machine. The prefixes are atpkg's own constants: a
/// re-spelling on either side is a compile error, never a verdict that silently
/// falls back to the generic headline — or a refusal reported as a success.
pub(crate) fn machine_apply_stdout(stdout: &str) -> MachineApplyStdout {
    use atpkg::cli::{
        MACHINE_APPLY_FAILED_PREFIX, MACHINE_NOT_APPLIED_PREFIX, MACHINE_VERDICT_PREFIX,
    };
    let mut read = MachineApplyStdout {
        events: Vec::new(),
        verdict: None,
        refusal: None,
        record: None,
    };
    for line in stdout.lines() {
        let line = line.trim_end();
        if let Some(reason) = line
            .strip_prefix(MACHINE_NOT_APPLIED_PREFIX)
            .or_else(|| line.strip_prefix(MACHINE_APPLY_FAILED_PREFIX))
        {
            read.refusal = Some(reason.trim().to_string());
        } else if let Some(sentence) = line.strip_prefix(MACHINE_VERDICT_PREFIX) {
            read.verdict = Some(sentence.trim().to_string());
        } else if let Some(event) = crate::parse_seed_line(line) {
            match event {
                // The record rides the completion (see `record`); the last one wins.
                Wake::PkgMachineState(body) => {
                    read.record = atpkg::machine::parse_machine_state(&body).or(read.record);
                }
                event => read.events.push(event),
            }
        }
    }
    // No record-missing fallback here: the completion this stdout belongs to re-reads
    // the machine whenever it carries no record (`finish_native_packages`), and that
    // read takes the expectation the change line opened — one read, not two.
    read
}

/// The exact argv of the `atpkg` process a [`PackagesRequest`] runs. THE table the
/// Settings verbs go through; pinned verbatim by `packages_argv_is_pinned_per_request`,
/// so a re-spelling of a flag is a red test rather than a verb that quietly stops working.
pub(crate) fn packages_argv(request: &PackagesRequest) -> Vec<String> {
    let parts: &[&str] = match request {
        PackagesRequest::CheckUpdate => &["update"],
        PackagesRequest::InstallDefaultSet => &["install", "--default-set"],
        PackagesRequest::UninstallAll => &["uninstall", "--all"],
        // The [machine] host settings, now — their own verb, which keeps them applied
        // (a pass carries only an edit to them). No `--wait-lock`: it takes no store lock.
        PackagesRequest::MachineApply => &["machine", "apply"],
    };
    parts.iter().map(ToString::to_string).collect()
}

/// The busy label a request reserves on the packages service.
pub(crate) fn packages_busy(request: &PackagesRequest) -> PackagesBusy {
    match request {
        PackagesRequest::CheckUpdate => PackagesBusy::Check,
        PackagesRequest::InstallDefaultSet => PackagesBusy::Install,
        PackagesRequest::UninstallAll => PackagesBusy::Uninstall,
        PackagesRequest::MachineApply => PackagesBusy::MachineApply,
    }
}

/// The structural admission a request must pass before the busy gate: the `[machine]`
/// host settings only where they exist.
pub(crate) fn packages_request_admissible(request: &PackagesRequest) -> Result<(), String> {
    match request {
        PackagesRequest::CheckUpdate
        | PackagesRequest::InstallDefaultSet
        | PackagesRequest::UninstallAll => Ok(()),
        PackagesRequest::MachineApply => {
            if cfg!(target_os = "macos") {
                Ok(())
            } else {
                Err("the [machine] settings are macOS host settings".to_string())
            }
        }
    }
}

#[cfg(test)]
mod packages_argv_tests {
    use super::*;

    /// The argv table, verbatim.
    #[test]
    fn packages_argv_is_pinned_per_request() {
        let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<String>>();
        assert_eq!(packages_argv(&PackagesRequest::CheckUpdate), s(&["update"]));
        assert_eq!(
            packages_argv(&PackagesRequest::InstallDefaultSet),
            s(&["install", "--default-set"])
        );
        assert_eq!(
            packages_argv(&PackagesRequest::UninstallAll),
            s(&["uninstall", "--all"])
        );
        assert_eq!(
            packages_argv(&PackagesRequest::MachineApply),
            s(&["machine", "apply"]),
            "no --wait-lock: the verb takes no store lock"
        );
    }

    /// THE CHECK IS THE WINDOW'S OWN PASS (Phase 3): the worker spawns the Check with the
    /// lanes' argv (`pass_args(PassVerb::Update, …)`: `--wait-lock` with their bound and
    /// `--progress-file`), the spawner's pid and the login shell's PATH — the orphan watch
    /// and the progress file are armed by exactly those — and answers at once only when a
    /// person's typed verb holds the store. Pinned by scrape: the spawn is inside the
    /// worker thread, which a unit test cannot reach without a real child.
    #[test]
    fn the_settings_check_runs_the_windows_pass_argv() {
        let src = include_str!("app_native.rs");
        let start = src
            .find("pub(crate) fn execute_native_packages(")
            .expect("the worker");
        let body = &src[start..start + src[start..].find("\n    }\n").unwrap()];
        let gate = body.find("if check {").expect("the Check's own spawn");
        let spawn = &body[gate..gate + 600];
        // Match the exact call after removing formatting whitespace: rustfmt
        // wraps this argument list across lines.
        let compact: String = spawn.chars().filter(|ch| !ch.is_whitespace()).collect();
        assert!(
            compact.contains("crate::pass_args(crate::PassVerb::Update,layout.as_ref(),None,)"),
            "Check uses the package lane's update argv: {spawn}"
        );
        for needle in [
            "atpkg::cli::SPAWNER_PID_ENV",
            "crate::spawn::atpkg_child_path()",
        ] {
            assert!(spawn.contains(needle), "{needle}: {spawn}");
        }
        let fail_fast = body
            .find("atpkg::lock::person_holder")
            .expect("the fail-fast rule");
        assert!(fail_fast < gate, "decided before anything is spawned");
        // …and RUN the lanes' way: its stdout streamed, not collected at exit.
        let runs = body[gate..]
            .find("crate::run_pass_child(&mut child, layout.as_ref(), false, &post)")
            .map(|i| gate + i)
            .expect("the Check runs through the lanes' child runner");
        let collected = body
            .find("let result = child.output();")
            .expect("the other verbs");
        assert!(
            runs < collected,
            "the Check leaves the loop before the collect-at-exit"
        );
        let args = crate::pass_args(crate::PassVerb::Update, None, None);
        assert_eq!(
            args,
            vec![
                std::ffi::OsString::from("update"),
                "--wait-lock".into(),
                crate::ATPKG_WAIT_LOCK_SECS.to_string().into()
            ],
            "the lanes' own bound"
        );
        assert!(check_held_by_person_message(4242).contains("pid 4242"));
    }

    /// The offline code reads as what it is, never as a bare exit status.
    #[cfg(unix)]
    #[test]
    fn an_offline_check_says_offline() {
        use std::os::unix::process::ExitStatusExt as _;
        let code = i32::from(aterm_update_core::pkg_check::PASS_OFFLINE_EXIT);
        let status = std::process::ExitStatus::from_raw(code << 8);
        let said =
            packages_child_failure(Ok(status), &["update".to_string()], "").expect("not a success");
        assert!(said.contains("offline"), "{said}");
    }

    /// The machine apply worker sorts the child's stdout: the `machine-settings:`
    /// row becomes the pull-down/card event, the LAST `atpkg machine: …` line is the
    /// verdict, and a `machine settings not applied — …` line is the refusal the
    /// worker reports as the failure (with its reason, not the prefix).
    #[test]
    fn machine_apply_stdout_is_sorted_into_events_verdict_and_refusal() {
        use atpkg::cli::{
            MACHINE_NOT_APPLIED_PREFIX, MACHINE_SETTINGS_MARKER, MACHINE_VERDICT_PREFIX,
        };
        // Today's verdict sentences, spelled from atpkg's own prefixes so a
        // re-spelling there is red here.
        let nothing_changed = format!(
            "{MACHINE_VERDICT_PREFIX}nothing changed — already applied, switched off in \
             [machine], or a change that did not land; `aterm pkg machine` lists what is \
             still open"
        );
        // The apply prints its own `machine-state:` record behind the change line
        // (2026-09-16); the worker hands it on as the event the card takes as the
        // newest state, so the completion has nothing to confirm by a read.
        let applied = machine_apply_stdout(&format!(
            "atpkg noindex: /Users//x/ay/target: renamed target.noindex\n\
             atpkg: {MACHINE_SETTINGS_MARKER}spotlight-noindex 1 dir(s) migrated; universal-control disabled\n\
             atpkg: {}universal-control=disabled; policy=off; noindex=true; \
             spotlight-exposed=0; spotlight-hidden=9; spotlight-migratable=0; \
             scan=complete; home=account\n\
             {MACHINE_VERDICT_PREFIX}applied — spotlight-noindex 1 dir(s) migrated; universal-control disabled\n",
            atpkg::cli::MACHINE_STATE_MARKER
        ));
        assert_eq!(applied.events.len(), 1, "{:?}", applied.events);
        assert!(matches!(
            &applied.events[0],
            Wake::PkgMachineSettings(body)
                if body == "spotlight-noindex 1 dir(s) migrated; universal-control disabled"
        ));
        assert!(
            applied
                .record
                .as_ref()
                .is_some_and(|s| s.hidden == 9 && s.exposed == 0),
            "the record rides the completion, not the event stream"
        );
        assert_eq!(
            applied.verdict.as_deref(),
            Some("applied — spotlight-noindex 1 dir(s) migrated; universal-control disabled")
        );
        assert!(applied.refusal.is_none());

        let nothing = machine_apply_stdout(&format!("{nothing_changed}\n"));
        assert!(nothing.events.is_empty());
        assert_eq!(
            nothing.verdict.as_deref(),
            Some(
                "nothing changed — already applied, switched off in [machine], or a change \
                 that did not land; `aterm pkg machine` lists what is still open"
            )
        );
        assert!(nothing.refusal.is_none());

        // A refused apply: the refusal line, then the verb's own `not applied` verdict
        // (never a "nothing changed" beside a refusal).
        let refused = machine_apply_stdout(&format!(
            "{MACHINE_NOT_APPLIED_PREFIX}HOME is /tmp/synthetic, the account home is /Users//x\n\
             {MACHINE_VERDICT_PREFIX}not applied — HOME is /tmp/synthetic, the account home is /Users//x\n"
        ));
        assert_eq!(
            refused.refusal.as_deref(),
            Some("HOME is /tmp/synthetic, the account home is /Users//x")
        );
        assert_eq!(
            refused.verdict.as_deref(),
            Some("not applied — HOME is /tmp/synthetic, the account home is /Users//x")
        );
        assert!(refused.events.is_empty());

        // Nothing said: no verdict, no refusal, no events — the worker then reports
        // the exit status alone.
        let silent = machine_apply_stdout("");
        assert!(silent.verdict.is_none() && silent.refusal.is_none() && silent.events.is_empty());
    }

    /// The machine READ parses the byte-stable `machine-state:` record out of the
    /// prose `atpkg machine` prints around it; prose alone, or a malformed record,
    /// is NO record — the worker then reports "printed no state", never a guess.
    #[test]
    fn machine_state_is_read_from_the_marker_line_and_never_guessed() {
        use atpkg::machine::{HomePosture, UcPosture};
        let stdout = "atpkg machine: warn — Universal Control is at the OS default (…)\n\
                      atpkg machine: 2 cargo target dir(s) under /Users//x open to Spotlight (1 a pass would hide), 8 hidden ([machine] spotlight_noindex = true)\n\
                      atpkg: machine-state: universal-control=default; policy=off; noindex=true; spotlight-exposed=2; spotlight-hidden=8; spotlight-migratable=1; scan=complete; home=account\n\
                      atpkg machine: next — aterm pkg machine apply (Universal Control off for this host; 1 target dir(s) hidden from Spotlight)\n";
        let state = parse_machine_state_output(stdout).expect("the marker line parses");
        assert_eq!(state.universal_control, UcPosture::Default);
        assert_eq!(state.policy, atpkg::config::UniversalControlPolicy::Off);
        assert!(state.spotlight_noindex);
        assert_eq!(
            (state.exposed, state.hidden, state.would_migrate),
            (2, 8, 1)
        );
        assert!(state.scan_complete);
        assert_eq!(state.home, HomePosture::Account);
        assert!(state.next().is_some());
        assert!(
            parse_machine_state_output(
                "atpkg machine: ok — Universal Control is disabled on this host\n\
                 atpkg machine: nothing to apply — Universal Control and Spotlight are where [machine] wants them\n"
            )
            .is_none(),
            "prose is not a record"
        );
        assert!(
            parse_machine_state_output(
                "atpkg: machine-state: universal-control=default; policy=off\n"
            )
            .is_none(),
            "a record missing fields is no record"
        );
        assert!(
            parse_machine_state_output(
                "atpkg machine: not applicable — these are macOS host settings\n"
            )
            .is_none()
        );
    }

    /// The worker's exit classification (2026-09-10): success is no sentence;
    /// `--default-set` exit 2 keeps its "nothing was installed" words; atpkg's
    /// contention code (75) names the OTHER installer by code, not by matching the
    /// sentence, and carries atpkg's own line; any other exit is the last stderr
    /// line, or the status when there is none.
    #[cfg(unix)]
    #[test]
    fn a_contended_verb_names_the_other_installer_by_exit_code() {
        use std::os::unix::process::ExitStatusExt as _;
        let status = |code: i32| std::process::ExitStatus::from_raw(code << 8);
        let s = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<String>>();
        let lock_line = "atpkg: another atpkg process holds the store lock at /p/store.lock \
                         \u{2014} refusing to mutate the store concurrently (retry when it exits)";
        assert_eq!(
            packages_child_failure(Ok(status(0)), &s(&["update"]), ""),
            None
        );
        assert!(
            packages_child_failure(Ok(status(2)), &s(&["install", "--default-set"]), "")
                .unwrap()
                .starts_with("Nothing was installed"),
        );
        let contended = packages_child_failure(
            Ok(status(i32::from(atpkg::lock::CONTENDED_EXIT))),
            &s(&["install", "--default-set"]),
            lock_line,
        )
        .unwrap();
        assert!(
            contended.starts_with("A toolchain pass is already running on this store"),
            "{contended}"
        );
        // Never a claim about ANOTHER aterm (2026-09-18): after a self-update the
        // holder is this app's own predecessor's pass.
        assert!(
            !contended.to_lowercase().contains("another aterm"),
            "{contended}"
        );
        assert!(!contended.to_lowercase().contains("lock"), "{contended}");
        assert!(contended.ends_with("try again in a moment."), "{contended}");
        // The child's own line is never quoted: it names a peer and a lock.
        assert!(!contended.contains("holds"), "{contended}");
        // With nothing on stderr the sentence is the same.
        let quiet = packages_child_failure(
            Ok(status(i32::from(atpkg::lock::CONTENDED_EXIT))),
            &s(&["update"]),
            "",
        )
        .unwrap();
        assert_eq!(quiet, contended);
        assert_eq!(
            packages_child_failure(Ok(status(1)), &s(&["update"]), "first\nlast line").as_deref(),
            Some("last line")
        );
        assert_eq!(
            packages_child_failure(Ok(status(1)), &s(&["update"]), "").as_deref(),
            Some("atpkg update exited with exit status: 1")
        );
        let error = std::io::Error::other("no such file");
        assert_eq!(
            packages_child_failure(Err(&error), &s(&["update"]), "").as_deref(),
            Some("could not launch atpkg update: no such file")
        );
    }

    #[test]
    fn every_request_reserves_its_own_busy_label() {
        assert_eq!(
            packages_busy(&PackagesRequest::CheckUpdate),
            PackagesBusy::Check
        );
        assert_eq!(
            packages_busy(&PackagesRequest::InstallDefaultSet),
            PackagesBusy::Install
        );
        assert_eq!(
            packages_busy(&PackagesRequest::UninstallAll),
            PackagesBusy::Uninstall
        );
        assert_eq!(
            packages_busy(&PackagesRequest::MachineApply),
            PackagesBusy::MachineApply
        );
    }

    /// The host re-reads the machine record after exactly the verbs whose atpkg
    /// pass runs `apply_machine_settings` first — derived from the pinned argv
    /// table, so the list mirrors atpkg's `verb_applies_machine_settings` (`update`,
    /// every `install`, `machine apply`; NOT `uninstall --all`).
    #[test]
    fn a_pass_that_applies_the_machine_settings_rereads_the_record() {
        let requests = [
            PackagesRequest::CheckUpdate,
            PackagesRequest::InstallDefaultSet,
            PackagesRequest::UninstallAll,
            PackagesRequest::MachineApply,
        ];
        let mut rereads = 0;
        for request in requests {
            let argv = packages_argv(&request);
            let verb: Vec<&str> = argv.iter().map(String::as_str).collect();
            let atpkg_applies = matches!(
                verb.as_slice(),
                ["update"] | ["install", ..] | ["machine", "apply"]
            );
            assert_eq!(
                packages_busy(&request).applies_machine_settings(),
                atpkg_applies,
                "{request:?} → {argv:?}"
            );
            rereads += usize::from(atpkg_applies);
        }
        assert_eq!(rereads, 3, "update, install --default-set, machine apply");
    }

    /// THE COLLECTED LANES NEVER POST A RECORD (2026-09-16). The worker hands a pass's
    /// stdout over at exit, so the `machine-state:` line it holds was measured at the
    /// pass's top; posting it would make a minutes-old measurement the card's newest
    /// state and supersede a fresher read. Pinned on the worker's own closure, and on
    /// the apply lane's sorter carrying the record on the completion instead.
    #[test]
    fn the_collected_lanes_never_post_the_passes_record() {
        let src = include_str!("app_native.rs");
        let start = src
            .find("let result = child.output();")
            .expect("the collected lane");
        let end = src[start..]
            .find("let said = result")
            .map_or(src.len(), |i| start + i);
        assert!(
            src[start..end].contains("!matches!(event, Wake::PkgMachineState(_))"),
            "the collected lane's closure must drop PkgMachineState"
        );
        let sorter = src.find("\npub(crate) fn machine_apply_stdout(").unwrap();
        let sorter_end = src[sorter..]
            .find("\n}\n")
            .map_or(src.len(), |i| sorter + i);
        assert!(
            src[sorter..sorter_end].contains("Wake::PkgMachineState(body) =>")
                && src[sorter..sorter_end].contains("read.record ="),
            "the apply lane's sorter carries the record on the completion"
        );
        assert!(
            !src[sorter..sorter_end].contains("PkgMachineRecordMissing"),
            "…and posts no fallback: the completion re-reads without a record"
        );
    }

    /// The structural admission: every verb but the `[machine]` host settings is
    /// admissible anywhere.
    #[test]
    fn packages_requests_are_admitted_by_platform() {
        for request in [
            PackagesRequest::CheckUpdate,
            PackagesRequest::InstallDefaultSet,
            PackagesRequest::UninstallAll,
        ] {
            assert!(packages_request_admissible(&request).is_ok(), "{request:?}");
        }
        // The [machine] host settings are macOS host settings: admissible there,
        // refused by name everywhere else — before any child is spawned.
        let machine = packages_request_admissible(&PackagesRequest::MachineApply);
        if cfg!(target_os = "macos") {
            assert!(machine.is_ok());
        } else {
            assert!(
                machine
                    .as_ref()
                    .is_err_and(|m| m.contains("macOS host settings")),
                "{machine:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte of `src` that is CODE. Comments, string literals (raw ones
    /// included) and character literals become spaces; newlines survive, so a
    /// byte offset into the answer still names the line it named in the file.
    /// A prose mention of a Unix-only name — and this file has several — must
    /// not read as a call to one.
    fn seam_code_only(src: &str) -> Vec<u8> {
        let b = src.as_bytes();
        let mut out: Vec<u8> = b
            .iter()
            .map(|&c| if c == b'\n' { b'\n' } else { b' ' })
            .collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                let mut depth = 1usize;
                i += 2;
                while i < b.len() && depth > 0 {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
                continue;
            }
            if b[i] == b'r' && (b.get(i + 1) == Some(&b'"') || b.get(i + 1) == Some(&b'#')) {
                let mut hash = i + 1;
                while b.get(hash) == Some(&b'#') {
                    hash += 1;
                }
                if b.get(hash) == Some(&b'"') {
                    let hashes = hash - i - 1;
                    let mut j = hash + 1;
                    while j < b.len() {
                        if b[j] == b'"'
                            && b.len() - (j + 1) >= hashes
                            && b[j + 1..j + 1 + hashes].iter().all(|&c| c == b'#')
                        {
                            j += 1 + hashes;
                            break;
                        }
                        j += 1;
                    }
                    i = j.min(b.len());
                    continue;
                }
            }
            if b[i] == b'"' {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'"' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            // A CHARACTER LITERAL, NEVER A LIFETIME. `'x'` and `'\n'` close;
            // the `'a` in `&'a str` does not, and swallowing it would blank the
            // rest of the file up to the next quote.
            if b[i] == b'\'' && (b.get(i + 1) == Some(&b'\\') || b.get(i + 2) == Some(&b'\'')) {
                i += 1;
                while i < b.len() {
                    if b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == b'\'' {
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                continue;
            }
            out[i] = b[i];
            i += 1;
        }
        out
    }

    fn seam_first_word(s: &str) -> &str {
        let end = s
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(s.len());
        &s[..end]
    }

    /// The name an item DECLARATION line binds, if it binds one. `impl` and
    /// `use` bind nothing new, so they answer `None` rather than handing back
    /// the first identifier that follows them — `impl Drop for X` must not
    /// enter the Unix-only set as `Drop`.
    fn seam_item_name(line: &str) -> Option<&str> {
        let mut rest = line.trim_start();
        loop {
            if let Some(after) = rest.strip_prefix("pub(") {
                rest = after[after.find(')')? + 1..].trim_start();
                continue;
            }
            let word = seam_first_word(rest);
            if word.is_empty() {
                return None;
            }
            let tail = rest[word.len()..].trim_start();
            match word {
                "pub" | "default" | "unsafe" | "async" => rest = tail,
                "extern" => {
                    rest = match tail.strip_prefix('"') {
                        Some(abi) => abi[abi.find('"')? + 1..].trim_start(),
                        None => tail,
                    };
                }
                // `const fn` is a function; `const NAME` is a constant.
                "const" if matches!(seam_first_word(tail), "fn" | "unsafe") => rest = tail,
                "fn" | "enum" | "struct" | "trait" | "type" | "const" | "static" | "union"
                | "mod" => {
                    let name = seam_first_word(tail);
                    return (!name.is_empty()).then_some(name);
                }
                _ => return None,
            }
        }
    }

    /// One `#[cfg(…unix…)]` and the whole construct it gates, as a byte range.
    struct SeamRegion {
        negated: bool,
        name: Option<String>,
        start: usize,
        end: usize,
    }

    fn seam_line_at(src: &str, idx: usize) -> &str {
        let start = src[..idx].rfind('\n').map_or(0, |n| n + 1);
        let end = src[idx..].find('\n').map_or(src.len(), |n| idx + n);
        &src[start..end]
    }

    /// Every `unix`-axis `cfg` region in one Rust source. The end is found by
    /// BRACE DEPTH from the construct's first byte: an item ends when its body
    /// closes or at a `;`, a field or match arm at the `,` that terminates it.
    /// That is what makes "outside every region" below mean what it says.
    fn seam_unix_regions(src: &str, code: &[u8]) -> Vec<SeamRegion> {
        let mut regions = Vec::new();
        let mut i = 0;
        while let Some(attr) = code
            .get(i..)
            .and_then(|tail| tail.windows(6).position(|w| w == b"#[cfg(").map(|p| p + i))
        {
            i = attr + 6;
            let mut depth = 1usize;
            let mut j = i;
            while j < code.len() && depth > 0 {
                match code[j] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    _ => {}
                }
                j += 1;
            }
            if code.get(j) != Some(&b']') {
                continue;
            }
            let predicate: String = String::from_utf8_lossy(&code[i..j - 1])
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            if !predicate
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|w| w == "unix")
            {
                continue;
            }
            // The construct itself: the next code byte past the item's other
            // attributes.
            let mut k = j + 1;
            loop {
                while code.get(k).is_some_and(u8::is_ascii_whitespace) {
                    k += 1;
                }
                if code.get(k) == Some(&b'#') {
                    let mut brackets = 0usize;
                    while k < code.len() {
                        match code[k] {
                            b'[' => brackets += 1,
                            b']' => {
                                brackets -= 1;
                                if brackets == 0 {
                                    k += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        k += 1;
                    }
                    continue;
                }
                break;
            }
            if k >= code.len() {
                continue;
            }
            let line = seam_line_at(src, k).trim_start();
            let name = seam_item_name(line).map(str::to_string);
            let item_like = name.is_some() || line.starts_with("impl") || line.starts_with("use ");
            let (mut depth, mut body, mut end) = (0usize, false, k);
            let mut q = k;
            while q < code.len() {
                match code[q] {
                    b'(' | b'[' => depth += 1,
                    b'{' => {
                        body |= depth == 0;
                        depth += 1;
                    }
                    b')' | b']' | b'}' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 && body {
                            end = q;
                            break;
                        }
                    }
                    b';' if depth == 0 => {
                        end = q;
                        break;
                    }
                    b',' if depth == 0 && !body && !item_like => {
                        end = q;
                        break;
                    }
                    _ => {}
                }
                q += 1;
                end = q.min(code.len().saturating_sub(1));
            }
            regions.push(SeamRegion {
                negated: predicate.contains("not(unix)"),
                name,
                start: attr,
                end,
            });
        }
        regions
    }

    /// The names a source defines ONLY behind `unix`, and every 1-based line on
    /// which ungated code names one of them.
    fn seam_unix_only(src: &str) -> (Vec<String>, Vec<(usize, String)>) {
        let code = seam_code_only(src);
        let regions = seam_unix_regions(src, &code);
        let gated = |idx: usize| regions.iter().any(|r| r.start <= idx && idx <= r.end);
        let mut only: std::collections::BTreeSet<String> = regions
            .iter()
            .filter(|r| !r.negated)
            .filter_map(|r| r.name.clone())
            .collect();
        for region in regions.iter().filter(|r| r.negated) {
            if let Some(name) = region.name.as_deref() {
                only.remove(name);
            }
        }
        // A name this file ALSO defines with no `cfg` at all is not Unix-only.
        // Read from the BLANKED bytes, not the source: this very module quotes a
        // `fn stop_machine_read_child` inside the plant below, and a declaration
        // inside a string literal declares nothing.
        let mut offset = 0;
        for line in code.split_inclusive(|&c| c == b'\n') {
            if !gated(offset) {
                let text = String::from_utf8_lossy(line);
                if let Some(name) = seam_item_name(&text) {
                    only.remove(name);
                }
            }
            offset += line.len();
        }
        let mut hits = Vec::new();
        let mut at = 0;
        while at < code.len() {
            if !(code[at].is_ascii_alphabetic() || code[at] == b'_') {
                at += 1;
                continue;
            }
            let mut end = at;
            while end < code.len() && (code[end].is_ascii_alphanumeric() || code[end] == b'_') {
                end += 1;
            }
            if !gated(at) {
                let word = String::from_utf8_lossy(&code[at..end]).into_owned();
                if only.contains(&word) {
                    hits.push((src[..at].matches('\n').count() + 1, word));
                }
            }
            at = end;
        }
        (only.into_iter().collect(), hits)
    }

    /// AN UNGATED CALL SITE MAY NOT NAME A `#[cfg(unix)]`-ONLY ITEM (2026-09-16).
    ///
    /// THE DEFECT THIS EXISTS FOR, and it shipped. `Overflow` and
    /// `machine_command_output_bounded` were `#[cfg(unix)]`; the `#[cfg(not(unix))]`
    /// block mirrored only the narrower `machine_command_output`; and the packages
    /// screen's `machine apply` branch — which carries no `cfg` — called the bounded
    /// form with `Overflow::Truncate`. On this box that is a clean build. For
    /// `x86_64-pc-windows-msvc` it is `E0433: cannot find type Overflow` and
    /// `E0425: cannot find function machine_command_output_bounded`, and aterm-gui —
    /// the shipped Windows binary's own crate — did not compile at all. Nobody
    /// working on a Unix machine could see it, and nothing in the crate's own test
    /// suite could either: a `#[cfg(not(unix))]` test compiles OUT here, which is
    /// the same blindness wearing a test's clothes.
    ///
    /// SO THE CHECK READS THE SOURCE, and runs on every host. It does not replace
    /// `xtask gate cells --cell win`, which puts a real compiler on the real triple
    /// and is the only thing that proves the crate builds; it is the cheap half that
    /// runs under `cargo test` on the machine somebody is actually typing on.
    ///
    /// SCOPED TO THIS FILE ON PURPOSE. The same scan over the rest of the crate is
    /// not sound without knowing which MODULES are themselves `#[cfg(unix)]` — much
    /// of aterm-gui's Unix code lives in files declared that way, where every name
    /// is legitimately Unix-only and every mention is legitimately inside the gate.
    /// `app_native.rs` is not one of those, so within it the law is exact.
    #[test]
    fn no_ungated_line_here_names_a_unix_only_item() {
        let (only, hits) = seam_unix_only(include_str!("app_native.rs"));
        assert!(
            hits.is_empty(),
            "ungated code names a `#[cfg(unix)]`-only item, so this file cannot compile off \
             Unix — app_native.rs {hits:?}"
        );
        // NON-VACUITY. A scan that found no Unix-only names at all would pass the
        // assertion above while proving nothing, so name one the file really gates.
        assert!(
            only.iter().any(|n| n == "stop_machine_read_child"),
            "the scan must find this file's Unix-only items; it found {only:?}"
        );
        // THE SHAPE OF THE FIX, pinned so a later edit cannot re-split them: ONE
        // `Overflow` and ONE `machine_command_output` for every target, and two arms
        // of `machine_command_output_bounded` that differ only in their body.
        for shared in [
            "Overflow",
            "machine_command_output",
            "machine_command_output_bounded",
        ] {
            assert!(
                !only.iter().any(|n| n == shared),
                "`{shared}` is Unix-only again; every target this crate ships for needs it — \
                 {only:?}"
            );
        }
    }

    /// The guard above can go RED. A checker nobody has seen fail is a checker
    /// nobody knows the shape of, so this plants the 2026-09-16 defect in
    /// miniature and demands the scan name its line — and demands the comment
    /// one line above it stay unnamed.
    #[test]
    fn the_unix_only_scan_goes_red_on_a_planted_ungated_call() {
        const PLANT: &str = r#"
#[cfg(unix)]
enum Overflow {
    Fail,
}

#[cfg(unix)]
fn bounded(_c: &mut Command, _o: Overflow) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn stop_machine_read_child() {}

fn apply(c: &mut Command) {
    // A prose mention of bounded() and Overflow must not count; the call does.
    let _ = bounded(c, Overflow::Fail);
}
"#;
        let (only, hits) = seam_unix_only(PLANT);
        assert_eq!(
            only,
            vec![
                "Overflow".to_string(),
                "bounded".to_string(),
                "stop_machine_read_child".to_string()
            ]
        );
        assert_eq!(
            hits,
            vec![(17, "bounded".to_string()), (17, "Overflow".to_string())],
            "the planted ungated call site, and only it"
        );
    }

    #[cfg(unix)]
    struct MachineChildFixture(std::path::PathBuf);

    #[cfg(unix)]
    impl MachineChildFixture {
        fn new(name: &str) -> Self {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "aterm-machine-{name}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    #[cfg(unix)]
    impl Drop for MachineChildFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn machine_command_timeout_reaps_owned_child_and_refresh_recovers() {
        use std::time::{Duration, Instant};
        let fixture = MachineChildFixture::new("timeout");
        let pid_path = fixture.0.join("pid");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", r#"printf '%s' "$$" > "$1"; exec /bin/sleep 30"#, "--"])
            .arg(&pid_path);
        let mut service = crate::packages_screen::PackagesService::new();
        dispatch_native_machine_read(&mut service, MachineReadEvent::Request, || Ok(()));
        let began = Instant::now();
        let error =
            machine_command_output(&mut command, "owned machine read", Duration::from_secs(2))
                .unwrap_err();
        assert!(began.elapsed() < Duration::from_secs(5), "{error}");
        assert!(error.contains("timed out"), "{error}");
        assert!(!error.contains("could not be confirmed"), "{error}");
        let pid: libc::pid_t = std::fs::read_to_string(pid_path).unwrap().parse().unwrap();
        let mut status = 0;
        // SAFETY: WNOHANG only observes the fixture's recorded direct child. The
        // helper must already have reaped it, so it is no longer our child.
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
        dispatch_native_machine_read(&mut service, MachineReadEvent::Finished(Err(error)), || {
            panic!("no queued rerun")
        });
        assert!(!service.machine().refreshing);
        dispatch_native_machine_read(&mut service, MachineReadEvent::Request, || Ok(()));
        let mut next = std::process::Command::new("/bin/sh");
        next.args(["-c", "printf '%s\n' 'atpkg: machine-state: universal-control=default; policy=off; noindex=true; spotlight-exposed=2; spotlight-hidden=8; spotlight-migratable=1; scan=complete; home=account'"]);
        let output =
            machine_command_output(&mut next, "owned successful read", Duration::from_secs(2))
                .unwrap();
        let record = parse_machine_state_output(&String::from_utf8_lossy(&output.stdout)).unwrap();
        dispatch_native_machine_read(&mut service, MachineReadEvent::Finished(Ok(record)), || {
            panic!("no queued rerun")
        });
        assert!(!service.machine().refreshing);
        assert!(service.machine().state.is_some());

        // Historical no-deadline waiting would still own this sleeping child;
        // verify that premise before exercising the same bounded cleanup seam.
        let mut old = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(old.try_wait().unwrap().is_none());
        assert!(stop_machine_read_child(
            &mut old,
            Instant::now() + Duration::from_secs(1)
        ));
    }

    /// An exited child is not stdout EOF, and a flood is capped: both bounded by
    /// CAUSE, never by a guess about load (2026-09-24).
    ///
    /// The grandchild that keeps the inherited stdout open sleeps `JOB`, far
    /// past any budget here, so a read that waits for EOF instead of the budget
    /// returns after `JOB` on any machine — the clock starts first. The fixture
    /// writes the grandchild's pid, and the test kills it the moment the call
    /// returns: a sleeper left to run would keep every unflagged descriptor this
    /// process had open at the fork, and a sibling test's closed peer would
    /// read as alive for that long. The pid is only written AFTER the fork, so
    /// a take whose shell is killed between the two would leave a sleeper no
    /// pid names; the shell therefore runs in its own process group, recorded
    /// BEFORE it forks, and such a take's group is killed instead.
    ///
    /// A take proves something only if the fixture RAN inside the budget:
    /// spawned, forked the holder and exited, which the pid file records. A
    /// loaded machine can starve a spawn past a 2 s budget, which says nothing
    /// about the product, so such a take (and only such a take) is repeated
    /// with a larger budget. The flood's take is repeated on the same terms: a
    /// `yes` that never ran cannot have overflowed.
    #[cfg(unix)]
    #[test]
    fn machine_command_bounds_inherited_pipe_eof_and_output_flood() {
        use std::os::unix::process::CommandExt as _;
        use std::time::{Duration, Instant};
        const JOB: Duration = Duration::from_secs(60);
        const BUDGETS: [Duration; 3] = [
            Duration::from_secs(2),
            Duration::from_secs(8),
            Duration::from_secs(30),
        ];
        let fixture = MachineChildFixture::new("pipe");
        let job = fixture.0.join("job");
        let group_file = fixture.0.join("job.group");
        let mut held = None;
        for budget in BUDGETS {
            let mut command = std::process::Command::new("/bin/sh");
            command
                .args([
                    "-c",
                    r#"printf '%s' "$$" > "$1.group"; /bin/sleep "$2" & printf '%s' "$!" > "$1"; exit 0"#,
                    "--",
                ])
                .arg(&job)
                .arg(JOB.as_secs().to_string())
                // Its own group, named BEFORE the holder forks: a take whose
                // shell is killed between the fork and the pid write still
                // leaves a group this test can end.
                .process_group(0);
            let began = Instant::now();
            let result = machine_command_output(&mut command, "owned inherited pipe", budget);
            let elapsed = began.elapsed();
            let pid = std::fs::read_to_string(&job)
                .ok()
                .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
                .filter(|&pid| pid > 0);
            if let Some(pid) = pid.filter(|_| elapsed < JOB) {
                // SAFETY: `kill` takes no pointers; `pid` is positive, so it
                // names one process, and before `JOB` it is still the sleeper.
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            if pid.is_none() {
                let group = std::fs::read_to_string(&group_file)
                    .ok()
                    .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
                    .filter(|&pgid| pgid > 1);
                if let Some(pgid) = group.filter(|_| elapsed < JOB) {
                    // SAFETY: `killpg` takes no pointers; `pgid` > 1 is this
                    // take's own group, alive while any holder it forked is.
                    unsafe { libc::killpg(pgid, libc::SIGKILL) };
                }
            }
            let _ = std::fs::remove_file(&group_file);
            if pid.is_some() {
                held = Some((result, elapsed));
                break;
            }
            let _ = std::fs::remove_file(&job);
        }
        let (result, elapsed) = held.expect("the fixture ran inside even the largest budget");
        assert!(
            result.as_ref().is_err_and(|e| e.contains("timed out")),
            "{result:?}"
        );
        assert!(
            elapsed < JOB,
            "an exited child is not stdout EOF: {elapsed:?}"
        );

        let mut flood = None;
        for budget in BUDGETS {
            let mut command = std::process::Command::new("/usr/bin/yes");
            command.arg("owned output flood");
            let result = machine_command_output(&mut command, "owned flood", budget);
            if !result.as_ref().is_err_and(|e| e.contains("timed out")) {
                flood = Some(result);
                break;
            }
        }
        let result = flood.expect("the flood ran inside even the largest budget");
        assert!(
            result.as_ref().is_err_and(|e| e.contains("64 KiB")),
            "{result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn machine_apply_timeout_and_failed_write_settle_as_failure() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::time::Duration;
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg("30");
        let result =
            machine_command_output(&mut command, "owned apply", Duration::from_millis(250));
        let (outcome, verdict, _) =
            machine_apply_completion(result, |_| panic!("no marker emitted"));
        assert!(
            matches!(&outcome, PackagesCommandOutcome::Failed { operation: PackagesBusy::MachineApply, message }
            if message.contains("timed out") && message.contains("may be partial"))
        );
        assert!(verdict.is_none());
        let mut app = App::headless_for_test();
        let sequence = app
            .native_packages_service
            .begin(Some(PackagesBusy::MachineApply))
            .unwrap();
        let report = crate::packages_screen::PackagesStatusReport::from_parts(
            true,
            true,
            "owned".into(),
            None,
            &[],
        );
        app.finish_native_packages(sequence, PackagesWorkerCompletion::command(report, outcome));
        assert!(app.native_packages_service.busy().is_none());
        // The real completion is admissible and releases the verb. Headless
        // fixtures have no proxy, so its follow-up read intentionally stays off.
        assert!(PackagesBusy::MachineApply.applies_machine_settings());
        assert!(!app.native_packages_service.machine().refreshing);

        let stdout = format!(
            "atpkg: {}spotlight-noindex 1 dir(s) migrated\n{}preference write did not land\n{}applied — spotlight-noindex 1 dir(s) migrated\n",
            atpkg::cli::MACHINE_SETTINGS_MARKER,
            atpkg::cli::MACHINE_APPLY_FAILED_PREFIX,
            atpkg::cli::MACHINE_VERDICT_PREFIX
        );
        let output = |bytes: Vec<u8>| std::process::Output {
            status: std::process::ExitStatus::from_raw(0),
            stdout: bytes,
            stderr: Vec::new(),
        };
        let mut events = Vec::new();
        let (outcome, verdict, record) =
            machine_apply_completion(Ok(output(stdout.as_bytes().to_vec())), |event| {
                events.push(event)
            });
        assert!(record.is_none(), "no record was printed");
        assert!(
            matches!(&outcome, PackagesCommandOutcome::Failed { message, .. } if message == "preference write did not land")
        );
        // The change alone: no `machine-state:` record rode this stdout, and the
        // completion carrying none is what makes the host re-read (2026-09-16) — no
        // record-missing event is posted here, so that read is the only one.
        assert_eq!(events.len(), 1, "{events:?}");
        assert!(
            matches!(&events[0], Wake::PkgMachineSettings(body) if body == "spotlight-noindex 1 dir(s) migrated")
        );
        assert_eq!(
            verdict.as_deref(),
            Some("applied — spotlight-noindex 1 dir(s) migrated")
        );
        let historical = stdout
            .lines()
            .filter(|line| !line.starts_with(atpkg::cli::MACHINE_APPLY_FAILED_PREFIX))
            .collect::<Vec<_>>()
            .join("\n");
        let (old, _, _) = machine_apply_completion(Ok(output(historical.into_bytes())), |_| {});
        assert!(
            matches!(old, PackagesCommandOutcome::Succeeded { .. }),
            "ignoring the failed-write marker would falsely report success"
        );
    }

    fn machine_read_dispatch_model() -> aterm_spec::derive::Model {
        aterm_spec::ty_model! {
            NativeMachineReadDispatch {
                const Buggy = 0;
                var requests = 0;
                var refreshing = 0;
                var rerun = 0;
                var workers = 0;
                action Request when (requests <= 2) {
                    requests = requests + 1;
                    rerun = if refreshing == 1 { 1 } else { 0 };
                    refreshing = 1;
                    workers = 1;
                }
                action Complete when (workers == 1) {
                    workers = if rerun == 1 && Buggy == 0 { 1 } else { 0 };
                    refreshing = rerun;
                    rerun = if rerun == 1 && Buggy == 1 { 1 } else { 0 };
                }
                action RerunSpawnFailure when (workers == 1 && rerun == 1) {
                    workers = 0;
                    refreshing = 0;
                    rerun = 0;
                }
                action InitialSpawnFailure when (refreshing == 0 && requests <= 2) {
                    requests = requests + 1;
                    workers = 0;
                    refreshing = 0;
                    rerun = 0;
                }
                invariant Bounded: requests <= 3 && workers <= 1 && rerun <= 1;
                invariant ReservedReadHasWorker: refreshing == workers;
                invariant QueuedReadHasWorker: rerun <= workers;
            }
        }
    }

    #[test]
    fn machine_read_dispatch_model_proves_and_catches_re_admission() {
        let model = machine_read_dispatch_model();
        aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
    }

    #[test]
    fn machine_read_dispatch_conforms_and_rejects_the_historical_orphan() {
        use crate::packages_screen::PackagesService;

        let model = machine_read_dispatch_model();
        for actions in [
            &["Request", "Request", "Request", "Complete", "Complete"][..],
            &[
                "Request",
                "Request",
                "RerunSpawnFailure",
                "Request",
                "Complete",
            ],
            &["InitialSpawnFailure", "Request", "Complete"],
            &[
                "Request", "Complete", "Request", "Request", "Complete", "Complete",
            ],
        ] {
            let mut service = PackagesService::new();
            let mut expected = model.init_state();
            let mut requests = 0;
            let mut workers = 0;
            let mut launches = 0;
            for action in actions {
                let is_request = matches!(*action, "Request" | "InitialSpawnFailure");
                let spawn_fails = matches!(*action, "InitialSpawnFailure" | "RerunSpawnFailure");
                let event = if is_request {
                    requests += 1;
                    MachineReadEvent::Request
                } else {
                    assert_eq!(workers, 1, "only a running worker can complete");
                    workers -= 1;
                    // A failed read is still a terminal completion. It must
                    // release or restart the read exactly like a parsed record.
                    MachineReadEvent::Finished(Err("owned read fixture completed".to_string()))
                };
                dispatch_native_machine_read(&mut service, event, || {
                    launches += 1;
                    assert_eq!(workers, 0, "dispatch must never overlap machine workers");
                    if spawn_fails {
                        Err("owned spawn refusal".to_string())
                    } else {
                        workers += 1;
                        Ok(())
                    }
                });
                assert!(model.fire(action, &mut expected), "{action}");
                let mut actual = expected.clone();
                actual.insert("requests", requests);
                actual.insert("workers", workers);
                actual.insert("refreshing", i64::from(service.machine().refreshing));
                actual.insert("rerun", i64::from(service.machine().rerun));
                assert_eq!(actual, expected, "{actions:?}: {action}");
            }
            assert!(
                launches >= 2,
                "every script reaches a real replacement attempt"
            );
            assert_eq!(workers, 0);
            assert!(!service.machine().refreshing);
        }

        // Replay the historical GUI completion sequence with the genuine
        // reducers: it requests admission a second time for the reserved rerun,
        // so the finished worker has no successor despite the refreshing flag.
        let mut historical = PackagesService::new();
        assert!(historical.request_machine_read());
        assert!(!historical.request_machine_read());
        let mut before = model.init_state();
        assert!(model.fire("Request", &mut before));
        assert!(model.fire("Request", &mut before));
        let expected = model.successors("Complete", &before);
        let reserved = historical.replace_machine_state(Err("old read completed".into()));
        assert!(reserved);
        let spawned = reserved && historical.request_machine_read();
        assert!(!spawned, "the historical dispatch loses the rerun");
        let mut broken = before;
        broken.insert("workers", i64::from(spawned));
        broken.insert("refreshing", i64::from(historical.machine().refreshing));
        broken.insert("rerun", i64::from(historical.machine().rerun));
        assert!(
            !expected.contains(&broken),
            "the conformance bind must reject a refreshing card with no worker"
        );
    }

    /// Rule 5 fence: a headless host (no event-loop proxy) never spawns the
    /// machine read and never queues one either — the proxy guard precedes the
    /// queue — so finishing an apply leaves the posture exactly as it was.
    #[test]
    fn a_headless_host_never_queues_or_spawns_a_machine_read() {
        let mut app = App::headless_for_test();
        app.native_packages_service.set_machine_refreshing(true);
        app.start_native_machine_refresh();
        assert!(app.native_packages_service.machine().refreshing);
        assert!(
            !app.native_packages_service.machine().rerun,
            "no proxy ⇒ nothing queued"
        );
        let sequence = app
            .native_packages_service
            .begin(Some(PackagesBusy::MachineApply))
            .unwrap();
        let report = crate::packages_screen::PackagesStatusReport::from_parts(
            true,
            true,
            "fp".to_string(),
            None,
            &[],
        );
        app.finish_native_packages(
            sequence,
            PackagesWorkerCompletion::command(
                report,
                PackagesCommandOutcome::Succeeded {
                    operation: PackagesBusy::MachineApply,
                },
            ),
        );
        assert!(app.native_packages_service.busy().is_none());
        assert!(app.native_packages_service.machine().refreshing);
        assert!(!app.native_packages_service.machine().rerun);
        assert!(app.native_packages_service.machine().state.is_none());
    }

    #[test]
    fn serious_mode_dequeue_builds_exact_authored_value_without_mutating_runtime() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = true\n".to_string(),
        )
        .unwrap();
        assert!(app.set_serious_mode(true));
        let revision = app.native_config_service.snapshot().revision;

        let patch = app.serious_mode_patch_request(false).unwrap();
        assert_eq!(patch.base_revision, revision);
        assert_eq!(patch.edits.len(), 1);
        assert_eq!(patch.edits[0].key, crate::prefs::EDIT_SERIOUS_MODE);
        assert_eq!(
            patch.edits[0].expected,
            ExpectedConfigValue::Exact(Some("true".to_string()))
        );
        assert_eq!(patch.edits[0].value.as_deref(), Some("false"));
        assert!(
            app.serious_mode_enabled(),
            "building/enqueuing intent cannot advance the live policy"
        );
    }

    #[test]
    fn rapid_serious_mode_intents_compose_against_the_queued_projection() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        assert!(!app.serious_mode_enabled());

        app.enqueue_serious_mode_intent().unwrap();
        app.enqueue_serious_mode_intent().unwrap();
        app.enqueue_serious_mode_intent().unwrap();

        let desired = app
            .native_config_pending
            .iter()
            .map(|request| match &request.origin {
                NativeConfigOrigin::SeriousMode { desired } => *desired,
                NativeConfigOrigin::View { .. }
                | NativeConfigOrigin::Control { .. }
                | NativeConfigOrigin::Presence { .. } => {
                    panic!("unexpected non-Serious-Mode request")
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(desired, vec![true, false, true]);
        assert!(
            app.native_config_pending
                .iter()
                .all(|request| { matches!(&request.work, NativeConfigWork::SeriousMode(_)) })
        );
        assert_eq!(app.serious_mode_queued_projection, Some(true));
        assert!(
            !app.serious_mode_enabled(),
            "queued intents cannot mutate the live policy before durability"
        );
    }

    #[test]
    fn legacy_serious_mode_set_and_native_toggle_share_one_semantic_projection() {
        let model = aterm_spec::derive::serious_mode_intent_queue_model();
        let mut model_state = model.init_state();
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        assert!(!app.serious_mode_enabled());

        let (reply, completion) = std::sync::mpsc::channel();
        app.enqueue_control_settings_field_intent(
            crate::prefs::EDIT_SERIOUS_MODE.to_string(),
            Some(" true ".to_string()),
            reply,
        );
        assert_eq!(app.serious_mode_queued_projection, Some(true));

        // The native command arrives before the control write completes. It
        // must toggle the queued true intent to false, not the still-live false
        // policy to true.
        app.enqueue_serious_mode_intent().unwrap();
        assert_eq!(app.serious_mode_queued_projection, Some(false));
        assert!(matches!(
            app.native_config_pending
                .get(1)
                .map(|request| &request.work),
            Some(NativeConfigWork::SeriousMode(false))
        ));
        for action in ["StartSetOn", "QueueToggle"] {
            let before = model_state.clone();
            assert!(model.fire(action, &mut model_state));
            assert_eq!(
                aterm_spec::interp::admits(&model, &before, &model_state),
                Some(action)
            );
        }

        let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
        let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
            ConfigPatchResult::Applied { snapshot, undo } => (
                ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                snapshot,
            ),
            other => panic!("control Serious Mode intent must apply: {other:?}"),
        };
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_SERIOUS_MODE)
                .unwrap()
                .as_deref(),
            Some("true")
        );
        assert_eq!(model_state["service"], 1);
        assert_eq!(model_state["live"], 0);
        assert_eq!(model_state["projection"], 0);
        assert_eq!(model_state["queue_count"], 1);
        app.publish_native_config_origin(origin, outcome, Some(snapshot), false, None);
        assert!(app.serious_mode_enabled());
        assert_eq!(app.serious_mode_queued_projection, Some(false));
        assert!(completion.recv().unwrap().unwrap().starts_with("saved:"));

        let NativeConfigRequest { origin, work } = app.native_config_pending.pop_front().unwrap();
        let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
            ConfigPatchResult::Applied { snapshot, undo } => (
                ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                snapshot,
            ),
            other => panic!("following native toggle must apply: {other:?}"),
        };
        let before = model_state.clone();
        assert!(model.fire("Complete", &mut model_state));
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &model_state),
            Some("Complete")
        );
        assert_eq!(model_state["live"], 1);
        assert_eq!(model_state["service"], 0);
        assert_eq!(model_state["inflight"], 1);
        assert!(app.serious_mode_enabled());
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_SERIOUS_MODE)
                .unwrap()
                .as_deref(),
            Some("false")
        );
        app.publish_native_config_origin(origin, outcome, Some(snapshot), false, None);
        let before = model_state.clone();
        assert!(model.fire("Complete", &mut model_state));
        assert_eq!(
            aterm_spec::interp::admits(&model, &before, &model_state),
            Some("Complete")
        );
        assert!(!app.serious_mode_enabled());
        assert_eq!(app.serious_mode_queued_projection, None);
        assert_eq!(model_state["live"], 0);
        assert_eq!(model_state["service"], 0);
        assert!(model.check_invariant("IdleIsAuthoritative", &model_state));
    }

    #[test]
    fn malformed_legacy_serious_mode_value_cannot_poison_toggle_projection() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        let (reply, _completion) = std::sync::mpsc::channel();
        app.enqueue_control_settings_field_intent(
            crate::prefs::EDIT_SERIOUS_MODE.to_string(),
            Some("TRUE".to_string()),
            reply,
        );
        assert_eq!(app.serious_mode_queued_projection, None);

        app.enqueue_serious_mode_intent().unwrap();
        assert_eq!(app.serious_mode_queued_projection, Some(true));
        assert!(matches!(
            app.native_config_pending
                .back()
                .map(|request| &request.work),
            Some(NativeConfigWork::SeriousMode(true))
        ));
    }

    /// Regression: a reconciliation worker that came back broken used to leave
    /// every queued request "pending" behind a fence that only a SUCCESSFUL
    /// sample can clear -- and nothing was scheduled to take another sample. A
    /// blocked `aterm-ctl settings set` therefore held a live one-shot until the
    /// 30-second wire deadline and then reported a wedged event loop, from a
    /// loop that was turning normally the whole time.
    #[test]
    fn failed_reconciliation_answers_queued_callers_instead_of_leaving_them_pending() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        let (reply, completion) = std::sync::mpsc::channel();
        app.enqueue_control_settings_field_intent(
            crate::prefs::EDIT_FONT_PX.to_string(),
            Some("14".to_string()),
            reply,
        );
        app.enqueue_serious_mode_intent().unwrap();
        assert_eq!(app.native_config_pending.len(), 2);

        app.fail_native_config_reconciliation("config path is unreadable");

        assert!(
            app.native_config_pending.is_empty(),
            "a fence with no live retry must not retain requests nobody will settle"
        );
        assert_eq!(app.serious_mode_queued_projection, None);
        let error = completion
            .try_recv()
            .expect("the control caller's one-shot must be settled, not abandoned")
            .unwrap_err();
        assert!(error.contains("config reconciliation failed"), "{error}");
        assert!(error.contains("config path is unreadable"), "{error}");
        assert!(
            app.native_config_service.reconciliation_required(),
            "only an admitted disk generation may reopen the lane"
        );
        // Two requests WERE lost, so the band says so — and says how many.
        assert!(
            app.has_live_message("2 queued changes were not written"),
            "a failure that discarded queued work must say it did"
        );
    }

    /// A hand-broken `aterm.toml` must be reported as what it is. The watcher
    /// posts "not valid TOML" on the config lane's row and fences the lane; the
    /// reconciliation sample then re-reads the same broken file and fails. With
    /// NOTHING queued it used to post "queued changes were not written" anyway —
    /// false — and, sharing the lane's key, that sentence REPLACED the true one on
    /// the band (measured on a live dev build 2026-09-23: the row read
    /// "Config reconciliation failed; queued changes were not written" with no
    /// Settings edit anywhere in the session).
    #[test]
    fn a_failed_reconciliation_with_nothing_queued_leaves_the_true_row_standing() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        app.surface_native_config_lane_error(
            "Config observation was not valid TOML: TOML parse error at line 2, column 19"
                .to_string(),
        );
        assert!(app.native_config_pending.is_empty(), "nothing is queued");

        app.fail_native_config_reconciliation(
            "existing aterm.toml is not valid TOML: TOML parse error at line 2, column 19",
        );

        assert!(
            !app.has_live_message("queued change"),
            "nothing was queued, so nothing may be reported as lost"
        );
        assert!(
            app.has_live_message("aterm.toml syntax error"),
            "the row that names the real problem must survive the settle"
        );
        assert!(
            app.native_config_service.reconciliation_required(),
            "the fence still stands; only its false report is gone"
        );
    }

    #[test]
    fn config_pump_preserves_queued_work_when_reconciliation_worker_is_unavailable() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-config-pump-reconcile-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "serious_mode = false\n").unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        app.native_config_service.mark_reconciliation_required();
        app.enqueue_serious_mode_intent().unwrap();
        let queued_revision = app.native_config_service.snapshot().revision;

        std::fs::write(&path, "serious_mode = [\n").unwrap();
        let error = app.pump_native_config().unwrap_err();
        assert!(error.contains("event-loop proxy"), "{error}");
        assert_eq!(app.native_config_pending.len(), 1);
        assert!(app.native_config_service.reconciliation_required());
        assert_eq!(
            app.native_config_service.snapshot().revision,
            queued_revision
        );

        std::fs::write(&path, "serious_mode = true\n").unwrap();
        let error = app.pump_native_config().unwrap_err();
        assert!(error.contains("event-loop proxy"), "{error}");
        assert!(
            app.native_config_service.reconciliation_required(),
            "the event loop must not reopen even a now-valid pathname without its worker"
        );
        assert_eq!(
            app.native_config_pending.len(),
            1,
            "proxy failure occurs before the reconciled request is popped"
        );
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_SERIOUS_MODE)
                .unwrap()
                .as_deref(),
            Some("false")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_control_completion_never_translates_conflict_or_unverified_publication_to_ok() {
        let conflict = control_settings_completion_reply(
            crate::prefs::EDIT_COPY_ON_SELECT,
            Some("true"),
            &ConfigPatchOutcome::Conflict { revision: 9 },
            None,
        )
        .unwrap_err();
        assert!(conflict.starts_with("save conflict"), "{conflict}");

        let unverified = control_settings_completion_reply(
            crate::prefs::EDIT_COPY_ON_SELECT,
            Some("true"),
            &ConfigPatchOutcome::Indeterminate {
                message: "post-rename observation failed".to_string(),
            },
            None,
        )
        .unwrap_err();
        assert!(
            unverified.starts_with("publication unverified"),
            "{unverified}"
        );
        assert!(unverified.contains("reload aterm.toml"), "{unverified}");

        assert_eq!(
            control_settings_completion_reply(
                crate::prefs::EDIT_COPY_ON_SELECT,
                Some("true"),
                &ConfigPatchOutcome::Applied {
                    revision: 2,
                    undo: Some(1),
                },
                None,
            )
            .unwrap(),
            format!("saved: {} = true", crate::prefs::EDIT_COPY_ON_SELECT),
        );
    }

    #[test]
    fn legacy_control_field_is_materialized_at_the_versioned_lane_head() {
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "copy_on_select = false\nfont_px = 14.0\n".to_string(),
        )
        .unwrap();

        // Advance the service before reducing the legacy absolute intent. The
        // control work must bind to this newest revision/value, not a standalone
        // file read or an enqueue-time baseline.
        let snapshot = app.native_config_service.snapshot();
        let base = snapshot.revision;
        let expected_font = snapshot
            .values()
            .unwrap()
            .remove(crate::prefs::EDIT_FONT_PX);
        assert!(matches!(
            app.native_config_service.patch(ConfigPatchRequest {
                base_revision: base,
                edits: vec![ConfigKeyEdit {
                    key: crate::prefs::EDIT_FONT_PX.to_string(),
                    expected: ExpectedValue::Exact(expected_font),
                    value: Some("16.0".to_string()),
                }],
            }),
            ConfigPatchResult::Applied { .. }
        ));

        let reduced = app
            .reduce_native_config_work(NativeConfigWork::ControlField {
                key: crate::prefs::EDIT_COPY_ON_SELECT.to_string(),
                value: Some("true".to_string()),
            })
            .unwrap();
        assert!(matches!(reduced, ConfigPatchResult::Applied { .. }));
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_COPY_ON_SELECT)
                .unwrap()
                .as_deref(),
            Some("true")
        );
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_FONT_PX)
                .unwrap()
                .as_deref(),
            Some("16"),
            "the serialized legacy edit preserves earlier lane work"
        );
    }

    #[test]
    fn legacy_unverified_completion_replies_err_and_keeps_reconciliation_gate_closed() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-config-control-unverified-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "copy_on_select = false\n").unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        app.native_config_inflight = true;
        let (reply, completion) = std::sync::mpsc::channel();

        // Make the bound logical path unobservable so the mandatory completion
        // reconciliation fails. A later write must remain fenced.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        app.finish_native_config_write(
            NativeConfigOrigin::Control {
                request_id: 7,
                key: crate::prefs::EDIT_COPY_ON_SELECT.to_string(),
                value: Some("true".to_string()),
                reply,
            },
            NativeConfigPersistenceCompletion {
                outcome: ConfigPatchOutcome::Indeterminate {
                    message: "post-publication proof failed".to_string(),
                },
                observation: Err("exact post-publication observation failed".to_string()),
            },
        );

        let response = completion.recv().expect("control completion delivered");
        let error = response.expect_err("unverified publication is never an OK reply");
        assert!(error.starts_with("publication unverified"), "{error}");
        assert!(error.contains("reconciliation required"), "{error}");
        assert!(
            app.native_config_service.reconciliation_required(),
            "a failed stable observation must keep the next-write gate closed"
        );
        assert!(!app.native_config_inflight);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_control_enqueue_reports_preflight_errors_without_stranding_a_reply() {
        let mut app = App::headless_for_test();
        let (unknown_reply, unknown_completion) = std::sync::mpsc::channel();
        app.queue_control_settings_field(
            "not_a_real_setting".to_string(),
            Some("true".to_string()),
            unknown_reply,
        );
        assert!(
            unknown_completion
                .recv()
                .unwrap()
                .unwrap_err()
                .contains("unknown key")
        );

        let (reply, completion) = std::sync::mpsc::channel();
        app.queue_control_settings_field(
            crate::prefs::EDIT_COPY_ON_SELECT.to_string(),
            Some("true".to_string()),
            reply,
        );
        let error = completion.recv().unwrap().unwrap_err();
        assert!(error.contains("event-loop proxy"), "{error}");
        assert!(app.native_config_pending.is_empty());
    }

    #[test]
    fn applied_completion_observation_failure_closes_gate_before_pumping_next_write() {
        let dir = std::env::temp_dir().join(format!(
            "aterm-native-config-applied-reconcile-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aterm.toml");
        std::fs::write(&path, "serious_mode = false\n").unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let base_revision = app.native_config_service.snapshot().revision;
        let ConfigPatchResult::Applied { snapshot, undo } =
            app.native_config_service.patch(ConfigPatchRequest {
                base_revision,
                edits: vec![ConfigKeyEdit {
                    key: crate::prefs::EDIT_SERIOUS_MODE.to_string(),
                    expected: ExpectedValue::Exact(Some("false".to_string())),
                    value: Some("true".to_string()),
                }],
            })
        else {
            panic!("test candidate must reduce");
        };
        std::fs::write(&path, snapshot.text.as_bytes()).unwrap();
        app.native_config_inflight = true;
        app.native_config_pending.push_back(NativeConfigRequest {
            origin: NativeConfigOrigin::SeriousMode { desired: false },
            work: NativeConfigWork::SeriousMode(false),
        });

        // The worker proved its candidate, then a non-cooperating writer made
        // the path unobservable before completion reconciliation.
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        app.finish_native_config_write(
            NativeConfigOrigin::SeriousMode { desired: true },
            NativeConfigPersistenceCompletion {
                outcome: ConfigPatchOutcome::Applied {
                    revision: snapshot.revision,
                    undo: Some(undo.get()),
                },
                observation: Err("exact committed observation was lost".to_string()),
            },
        );

        assert!(app.native_config_service.reconciliation_required());
        assert_eq!(
            app.native_config_pending.len(),
            1,
            "queued work cannot be consumed against the pre-write baseline"
        );
        assert!(!app.native_config_inflight);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn three_rapid_serious_mode_intents_rebase_through_each_completion() {
        struct PendingCompletion {
            desired: bool,
            outcome: ConfigPatchOutcome,
            snapshot: crate::native_config_service::ConfigSnapshot,
        }

        fn start_next(app: &mut App) -> PendingCompletion {
            let NativeConfigRequest { origin, work } = app
                .native_config_pending
                .pop_front()
                .expect("queued Serious Mode intent");
            let desired = match origin {
                NativeConfigOrigin::SeriousMode { desired } => desired,
                NativeConfigOrigin::View { .. }
                | NativeConfigOrigin::Control { .. }
                | NativeConfigOrigin::Presence { .. } => {
                    panic!("unexpected non-Serious-Mode request")
                }
            };
            assert!(matches!(
                &work,
                NativeConfigWork::SeriousMode(value) if *value == desired
            ));
            let (outcome, snapshot) = match app.reduce_native_config_work(work).unwrap() {
                ConfigPatchResult::Applied { snapshot, undo } => (
                    ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo: Some(undo.get()),
                    },
                    snapshot,
                ),
                ConfigPatchResult::Unchanged { snapshot } => (
                    ConfigPatchOutcome::Applied {
                        revision: snapshot.revision,
                        undo: None,
                    },
                    snapshot,
                ),
                result => panic!("serialized semantic intent must apply: {result:?}"),
            };
            PendingCompletion {
                desired,
                outcome,
                snapshot,
            }
        }

        fn complete(app: &mut App, completion: PendingCompletion) {
            app.publish_native_config_origin(
                NativeConfigOrigin::SeriousMode {
                    desired: completion.desired,
                },
                completion.outcome,
                Some(completion.snapshot),
                false,
                None,
            );
            assert_eq!(app.serious_mode_enabled(), completion.desired);
        }

        fn project(
            model: &aterm_spec::derive::Model,
            app: &App,
            current: Option<bool>,
            queued_expected: &std::collections::VecDeque<bool>,
            issued: i64,
            completed: i64,
        ) -> aterm_spec::interp::State {
            let mut state = model.init_state();
            let live = app.serious_mode_enabled();
            let service = app
                .native_config_service
                .value(crate::prefs::EDIT_SERIOUS_MODE)
                .unwrap()
                .as_deref()
                == Some("true");
            let queued = app
                .native_config_pending
                .iter()
                .map(|request| match request.origin {
                    NativeConfigOrigin::SeriousMode { desired } => desired,
                    NativeConfigOrigin::View { .. }
                    | NativeConfigOrigin::Control { .. }
                    | NativeConfigOrigin::Presence { .. } => {
                        panic!("unexpected non-Serious-Mode request")
                    }
                })
                .collect::<Vec<_>>();
            state.insert("live", i64::from(live));
            state.insert("service", i64::from(service));
            state.insert(
                "projection",
                i64::from(app.serious_mode_queued_projection.unwrap_or(live)),
            );
            state.insert("inflight", i64::from(current.is_some()));
            state.insert("current_desired", i64::from(current.unwrap_or(live)));
            state.insert("queue_count", queued.len() as i64);
            state.insert("q1", i64::from(queued.first().copied().unwrap_or(false)));
            state.insert("q2", i64::from(queued.get(1).copied().unwrap_or(false)));
            state.insert(
                "q1_expected",
                i64::from(queued_expected.front().copied().unwrap_or(false)),
            );
            state.insert(
                "q2_expected",
                i64::from(queued_expected.get(1).copied().unwrap_or(false)),
            );
            state.insert("issued", issued);
            state.insert("completed", completed);
            state.insert("conflict", 0);
            state.insert(
                "last_desired",
                i64::from(app.serious_mode_queued_projection.unwrap_or(live)),
            );
            state.insert("intent_kind", i64::from(issued > 0));
            state
        }

        fn assert_action(
            model: &aterm_spec::derive::Model,
            before: &aterm_spec::interp::State,
            after: &aterm_spec::interp::State,
            action: &'static str,
        ) {
            assert_eq!(
                model.successors(action, before).as_slice(),
                std::slice::from_ref(after),
                "shipping queue transition must refine {action}"
            );
            assert_eq!(
                aterm_spec::interp::admits(model, before, after),
                Some(action)
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, after),
                    "post-state violates {}::{}: {after:?}",
                    model.name,
                    invariant.name
                );
            }
        }

        let model = aterm_spec::derive::serious_mode_intent_queue_model();
        let mut app = App::headless_for_test();
        app.native_config_service = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap();
        assert!(!app.set_serious_mode(false));
        let baseline_revision = app.native_config_service.snapshot().revision;
        let mut issued = 0;
        let mut completed = 0;
        let mut queued_expected = std::collections::VecDeque::new();
        let mut state = project(&model, &app, None, &queued_expected, issued, completed);
        assert_eq!(state, model.init_state());

        app.enqueue_serious_mode_intent().unwrap();
        let mut current = start_next(&mut app);
        issued += 1;
        let after = project(
            &model,
            &app,
            Some(current.desired),
            &queued_expected,
            issued,
            completed,
        );
        assert_action(&model, &state, &after, "StartToggle");
        state = after;

        queued_expected.push_back(true);
        app.enqueue_serious_mode_intent().unwrap();
        issued += 1;
        let after = project(
            &model,
            &app,
            Some(current.desired),
            &queued_expected,
            issued,
            completed,
        );
        assert_action(&model, &state, &after, "QueueToggle");
        state = after;

        queued_expected.push_back(true);
        app.enqueue_serious_mode_intent().unwrap();
        issued += 1;
        let after = project(
            &model,
            &app,
            Some(current.desired),
            &queued_expected,
            issued,
            completed,
        );
        assert_action(&model, &state, &after, "QueueToggle");
        state = after;

        complete(&mut app, current);
        queued_expected.pop_front();
        current = start_next(&mut app);
        completed += 1;
        let after = project(
            &model,
            &app,
            Some(current.desired),
            &queued_expected,
            issued,
            completed,
        );
        assert_action(&model, &state, &after, "Complete");
        state = after;

        // Negative control: the old enqueue-time expected value conflicts the
        // third intent here and is not an admitted transition of the shipping
        // (Buggy=0) model.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let stale_third = buggy.successors("Complete", &state)[0].clone();
        assert_eq!(stale_third["conflict"], 1);
        assert_eq!(
            aterm_spec::interp::admits(&model, &state, &stale_third),
            None
        );
        assert!(!buggy.check_invariant("NoSerializedConflict", &stale_third));

        complete(&mut app, current);
        queued_expected.pop_front();
        current = start_next(&mut app);
        completed += 1;
        let after = project(
            &model,
            &app,
            Some(current.desired),
            &queued_expected,
            issued,
            completed,
        );
        assert_action(&model, &state, &after, "Complete");
        state = after;

        complete(&mut app, current);
        completed += 1;
        let after = project(&model, &app, None, &queued_expected, issued, completed);
        assert_action(&model, &state, &after, "Complete");
        assert!(app.native_config_pending.is_empty());
        assert_eq!(app.serious_mode_queued_projection, None);
        assert_eq!(
            app.native_config_service.snapshot().revision,
            baseline_revision + 3
        );
        assert_eq!(
            app.native_config_service
                .value(crate::prefs::EDIT_SERIOUS_MODE)
                .unwrap()
                .as_deref(),
            Some("true")
        );
    }

    #[test]
    fn durable_serious_mode_snapshot_updates_runtime_and_open_settings_immediately() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
        let (_, view) = app.active_native_view(wid).unwrap();
        let before_presentation = app
            .native_runtime
            .view_state(view)
            .unwrap()
            .common()
            .presentation_revision;
        let mut service =
            crate::native_config_service::VersionedConfigService::new(String::new()).unwrap();
        let snapshot = match service.patch(crate::native_config_service::ConfigPatchRequest {
            base_revision: 1,
            edits: vec![crate::native_config_service::ConfigKeyEdit {
                key: crate::prefs::EDIT_SERIOUS_MODE.to_string(),
                expected: crate::native_config_service::ExpectedValue::Exact(None),
                value: Some("true".to_string()),
            }],
        }) {
            crate::native_config_service::ConfigPatchResult::Applied { snapshot, .. } => snapshot,
            other => panic!("serious-mode fixture patch failed: {other:?}"),
        };

        assert!(app.apply_serious_mode_config_snapshot(&snapshot));

        assert!(app.serious_mode_enabled());
        assert_eq!(app.config.serious_mode, Some(true));
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert!(state.common.presentation_revision > before_presentation);
        let field = state
            .legacy
            .fields
            .iter()
            .find(|field| field.key == crate::prefs::EDIT_SERIOUS_MODE)
            .unwrap();
        assert_eq!(crate::settings::SettingsState::display_value(field), "true");
    }

    #[test]
    fn serious_mode_conflict_keeps_disk_authority_and_surfaces_feedback() {
        let mut app = App::headless_for_test();
        assert!(app.set_serious_mode(true));
        let authoritative = crate::native_config_service::VersionedConfigService::new(
            "serious_mode = false\n".to_string(),
        )
        .unwrap()
        .snapshot();

        app.publish_serious_mode_completion(
            true,
            ConfigPatchOutcome::Conflict {
                revision: authoritative.revision,
            },
            Some(authoritative),
            None,
        );

        assert!(!app.serious_mode_enabled());
        assert_eq!(app.config.serious_mode, Some(false));
        assert!(app.has_live_message("aterm.toml changed first"));
    }

    #[test]
    fn sole_worker_stamps_fifo_read_order_not_request_order() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let (drained_tx, drained_rx) = std::sync::mpsc::sync_channel(4);
        let worker = std::thread::spawn(move || {
            run_native_update_worker(
                receiver,
                |ticket, observation_sequence, _| NativeUpdateReconcileFacts {
                    _ticket: ticket,
                    observation_sequence,
                    observed_at: std::time::Instant::now(),
                    durable: None,
                    installed: None,
                },
                |_| {},
                || drained_tx.send(()).unwrap(),
            );
        });
        let enqueue = |request_sequence| {
            let (reply, result) = std::sync::mpsc::sync_channel(1);
            sender
                .send(NativeUpdateWorkerRequest::Reconcile(
                    NativeUpdateReconcileRequest {
                        ticket: NativeUpdateReconcileTicket { request_sequence },
                        current_build: 10,
                        destination: NativeUpdateFactDestination::Reply(reply),
                    },
                ))
                .unwrap();
            result
        };

        // A later-minted request can reach the shared FIFO first (handoff and UI
        // producers race). Observation identity follows the actual read order.
        let later_request = enqueue(2);
        let earlier_request = enqueue(1);
        let later_facts = later_request.recv().unwrap();
        let earlier_facts = earlier_request.recv().unwrap();
        assert_eq!(later_facts._ticket.request_sequence(), 2);
        assert_eq!(later_facts.observation_sequence, 1);
        assert_eq!(earlier_facts._ticket.request_sequence(), 1);
        assert_eq!(earlier_facts.observation_sequence, 2);
        drained_rx.recv().unwrap();
        drained_rx.recv().unwrap();
        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn durable_marker_is_observed_before_installed_bundle_identity() {
        let step = std::cell::Cell::new(0_u8);
        let facts = read_native_update_reconcile_facts_with(
            NativeUpdateReconcileTicket {
                request_sequence: 1,
            },
            1,
            || {
                assert_eq!(step.replace(1), 0);
                None
            },
            || {
                assert_eq!(step.replace(2), 1, "installed probe ran before ready read");
                None
            },
        );
        assert_eq!(step.get(), 2);
        assert!(facts.durable.is_none() && facts.installed.is_none());

        // Negative control: the seam assertion is not vacuous and catches reversal.
        let reversed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let step = std::cell::Cell::new(0_u8);
            let installed_first = || assert_eq!(step.replace(2), 1);
            installed_first();
        }));
        assert!(reversed.is_err());
    }

    #[test]
    fn updater_ui_snapshots_do_not_probe_ledgers_or_installed_bundle() {
        let app = App::headless_for_test();
        let before = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
        let _ = app.update_snapshot(false).projection();
        let after = UPDATE_FACT_PROBES_ON_THREAD.with(std::cell::Cell::get);
        assert_eq!(before, after, "UI projection invoked an updater disk probe");
    }

    #[test]
    fn saturated_boot_health_queue_retains_nonblocking_retry() {
        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);

        assert_eq!(
            app.request_native_boot_health_confirmation(),
            NativeUpdateDispatch::Saturated,
            "a full queue must retain the pending latch"
        );
        let _ = receiver.recv().unwrap();
        assert_eq!(
            app.request_native_boot_health_confirmation(),
            NativeUpdateDispatch::Queued
        );
        assert!(matches!(
            receiver.recv().unwrap(),
            NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 10 }
        ));
    }

    #[test]
    fn recovery_host_revalidates_capabilities_and_never_interprets_diagnostics() {
        let mut app = App::headless_for_test();
        let denied = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::OpenOriginal {
                uri: "file:///tmp/safe.md\nhttps://attacker.example".to_string(),
            },
        );
        assert!(matches!(
            denied,
            crate::native_app::RecoveryOutcome::Denied { .. }
        ));

        let invalid_route = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::Retry(
                crate::native_app::RecoveryCapability::Settings {
                    route: "../../diagnostics-from-metadata".to_string(),
                },
            ),
        );
        assert!(matches!(
            invalid_route,
            crate::native_app::RecoveryOutcome::Denied { .. }
        ));

        let settings = app.execute_recovery_request(
            WindowId(0),
            crate::native_app::RecoveryRequest::Retry(
                crate::native_app::RecoveryCapability::Settings {
                    route: "/about".to_string(),
                },
            ),
        );
        assert!(matches!(
            settings,
            crate::native_app::RecoveryOutcome::Opened { .. }
        ));
    }

    #[test]
    fn disconnected_reconcile_worker_drops_retry_latch_without_hot_loop() {
        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        drop(receiver);
        app.native_update_reconcile_worker = Some(sender);
        app.pending_native_update_reconcile_purpose =
            Some(NativeUpdateReconcilePurpose::StageAvailable);
        let destinations = std::cell::Cell::new(0_u32);
        for _ in 0..1_000 {
            let (reply, _result) = std::sync::mpsc::sync_channel(1);
            let _ = app.retry_pending_native_update_reconcile_with(|_| {
                destinations.set(destinations.get() + 1);
                NativeUpdateFactDestination::Reply(reply)
            });
        }
        assert_eq!(
            destinations.get(),
            1,
            "a disconnected worker gets one restart/failure attempt, not one poll per event turn"
        );
        assert!(app.pending_native_update_reconcile_purpose.is_none());
    }

    #[test]
    fn idle_reconcile_park_never_materializes_a_destination_or_warning_path() {
        let mut app = App::headless_for_test();
        assert!(!app.has_pending_native_update_reconcile());

        // This is the shipping wrapper with only proxy materialization injected:
        // a future move of the clone above the guard increments the counter and
        // fails the test (the previous `_with`-only test could not see that bug).
        let materializations = std::cell::Cell::new(0_u32);
        for _ in 0..10_000 {
            app.retry_pending_native_update_reconcile_via(|_| {
                materializations.set(materializations.get() + 1);
                None
            });
        }
        assert_eq!(
            materializations.get(),
            0,
            "idle park materialized/woke an EventLoopProxy"
        );

        // Tier-1 projection: the genuine idle guard and retry seam realize the
        // model's ParkIdle transition, whose observable proxy-wake and warning
        // counters remain exactly zero. The mutant makes both one, proving this
        // assertion catches the historical unconditional-clone regression.
        let model = aterm_spec::derive::native_update_worker_queue_model();
        let mut idle = model.init_state();
        assert!(model.fire("ParkIdle", &mut idle));
        assert_eq!(idle.get("idle_proxy_wakes"), Some(&0));
        assert_eq!(idle.get("idle_warnings"), Some(&0));
        assert!(model.check_invariant("IdleParkHasNoProxyWake", &idle));
        assert!(model.check_invariant("IdleParkHasNoWarning", &idle));

        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let polluted = buggy.successors("ParkIdle", &buggy.init_state())[0].clone();
        assert!(!buggy.check_invariant("IdleParkHasNoProxyWake", &polluted));
        assert!(!buggy.check_invariant("IdleParkHasNoWarning", &polluted));
    }

    /// Tier-1 binding for `NativeUpdateWorkerQueue`: consume the actual shipping
    /// capacity constant while driving the genuine `try_send` Full/drain/retry
    /// decisions, purpose coalescer, and disconnected restart path. The model
    /// specifies that externally-visible boundary, not worker scheduling timing.
    #[test]
    fn native_update_worker_queue_conforms_to_saturation_coalescing_and_disconnect() {
        let model = aterm_spec::derive::native_update_worker_queue_model();
        let mut state = model.init_state();

        let mut app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(NATIVE_UPDATE_WORKER_CAPACITY);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);
        assert!(model.fire("OccupyWorker", &mut state));

        let (stage_reply, _stage_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(stage_reply),
        ));
        assert!(model.fire("RequestStageFull", &mut state));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::StageAvailable)
        );
        assert_eq!(state.get("pending"), Some(&1));

        // A stronger intent arriving during the same saturation episode must
        // replace only the purpose, never allocate or lose a second request.
        let (apply_reply, _apply_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::ApplyControl,
            |_| NativeUpdateFactDestination::Reply(apply_reply),
        ));
        assert!(model.fire("UpgradePendingToApply", &mut state));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::ApplyControl)
        );
        assert_eq!(state.get("purpose"), Some(&2));

        let _ = receiver.recv().unwrap();
        assert!(model.fire("WorkerDrainsFiller", &mut state));
        let observed = std::cell::Cell::new(None);
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            app.retry_pending_native_update_reconcile_with(|purpose| {
                observed.set(Some(purpose));
                NativeUpdateFactDestination::Reply(reply)
            }),
            NativeUpdateDispatch::Queued
        );
        assert!(model.fire("RetryPendingOnDrain", &mut state));
        assert!(app.pending_native_update_reconcile_purpose.is_none());
        assert_eq!(
            observed.get(),
            Some(NativeUpdateReconcilePurpose::ApplyControl),
            "the real coalescer must preserve the strongest accepted purpose"
        );

        let NativeUpdateWorkerRequest::Reconcile(request) = receiver.recv().unwrap() else {
            panic!("the retained intent must become one real worker request");
        };
        assert!(model.fire("WorkerCompletesIntent", &mut state));
        let NativeUpdateFactDestination::Reply(reply) = request.destination else {
            panic!("the conformance request uses a reply destination");
        };
        reply
            .send(reconcile_facts(request.ticket.request_sequence(), 1, None))
            .unwrap();
        app.finish_native_update_reconcile(observed.get().unwrap(), result.recv().unwrap());
        assert!(model.fire("ReduceCompletion", &mut state));
        assert_eq!(state.get("delivered"), Some(&1));
        assert!(model.check_invariant("NoSilentlyLostAcceptedIntent", &state));

        // Dispatch-time disconnection gets exactly one restart attempt. A
        // headless App has no event-loop proxy with which to spawn that worker,
        // so the real path deterministically reports Unavailable and clears the
        // latch; repeated event turns cannot become a retry hot loop.
        let mut disconnected_app = App::headless_for_test();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        disconnected_app.native_update_reconcile_worker = Some(sender);
        let mut disconnected_state = model.init_state();
        assert!(model.fire("OccupyWorker", &mut disconnected_state));
        let (discard_reply, _discard_result) = std::sync::mpsc::sync_channel(1);
        assert!(disconnected_app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(discard_reply),
        ));
        assert!(model.fire("RequestStageFull", &mut disconnected_state));
        drop(receiver);
        assert!(model.fire("DisconnectWithPending", &mut disconnected_state));

        let destinations = std::cell::Cell::new(0_u32);
        let (unavailable_reply, _unavailable_result) = std::sync::mpsc::sync_channel(1);
        assert_eq!(
            disconnected_app.retry_pending_native_update_reconcile_with(|_| {
                destinations.set(destinations.get() + 1);
                NativeUpdateFactDestination::Reply(unavailable_reply)
            }),
            NativeUpdateDispatch::Unavailable
        );
        assert!(model.fire("RestartPendingUnavailable", &mut disconnected_state));
        for _ in 0..32 {
            let (reply, _result) = std::sync::mpsc::sync_channel(1);
            assert_eq!(
                disconnected_app.retry_pending_native_update_reconcile_with(|_| {
                    destinations.set(destinations.get() + 1);
                    NativeUpdateFactDestination::Reply(reply)
                }),
                NativeUpdateDispatch::Queued
            );
        }
        assert_eq!(destinations.get(), 1);
        assert!(
            disconnected_app
                .pending_native_update_reconcile_purpose
                .is_none()
        );
        assert_eq!(disconnected_state.get("failed_explicitly"), Some(&1));
        assert_eq!(disconnected_state.get("restarts"), Some(&1));

        // Negative control: the historical "Full means success but retain
        // nothing" projection is rejected by the same invariant.
        let buggy = aterm_spec::interp::with_buggy(&model, 1);
        let occupied = buggy.successors("OccupyWorker", &buggy.init_state())[0].clone();
        let lost = buggy.successors("RequestApplyFull", &occupied)[0].clone();
        assert!(!buggy.check_invariant("NoSilentlyLostAcceptedIntent", &lost));
    }

    #[test]
    fn boot_health_failure_rearms_bounded_retry_then_success_closes_latch() {
        let mut app = App::headless_for_test();
        let now = std::time::Instant::now();
        app.boot_health_confirmation_dispatched = true;
        app.finish_native_boot_health_confirmation(false, now);
        assert!(!app.boot_health_confirmation_dispatched);
        assert_eq!(
            app.boot_health_confirmation_retry_at,
            Some(now + std::time::Duration::from_secs(1))
        );

        app.finish_native_boot_health_confirmation(true, now);
        assert!(app.boot_health_confirmation_dispatched);
        assert!(app.boot_health_confirmation_retry_at.is_none());
    }

    #[test]
    fn saturated_stage_wake_drains_and_schedules_automatic_apply() {
        let mut app = App::headless_for_test();
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(true),
            ..crate::app_config::UpdateConfig::default()
        });
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .try_send(NativeUpdateWorkerRequest::ConfirmBootHealth { current_build: 9 })
            .unwrap();
        app.native_update_reconcile_worker = Some(sender);

        let (discard_reply, _discard_result) = std::sync::mpsc::sync_channel(1);
        assert!(app.request_native_update_reconcile_with(
            NativeUpdateReconcilePurpose::StageAvailable,
            |_| NativeUpdateFactDestination::Reply(discard_reply),
        ));
        assert_eq!(
            app.pending_native_update_reconcile_purpose,
            Some(NativeUpdateReconcilePurpose::StageAvailable),
            "a Full FIFO accepts the wake into the coalesced latch"
        );

        let _ = receiver.recv().unwrap();
        let (reply, result) = std::sync::mpsc::sync_channel(1);
        let observed = std::sync::Arc::new(std::sync::Mutex::new(None));
        let observed_for_destination = std::sync::Arc::clone(&observed);
        assert_eq!(
            app.retry_pending_native_update_reconcile_with(move |purpose| {
                *observed_for_destination.lock().unwrap() = Some(purpose);
                NativeUpdateFactDestination::Reply(reply)
            }),
            NativeUpdateDispatch::Queued
        );
        assert!(app.pending_native_update_reconcile_purpose.is_none());

        let NativeUpdateWorkerRequest::Reconcile(request) = receiver.recv().unwrap() else {
            panic!("pending stage retry must enqueue a facts read");
        };
        let facts = reconcile_facts(
            request.ticket.request_sequence(),
            1,
            Some(status(Some(11), 0)),
        );
        let NativeUpdateFactDestination::Reply(reply) = request.destination else {
            panic!("test retry uses reply destination");
        };
        reply.send(facts).unwrap();
        let purpose = observed.lock().unwrap().expect("effective purpose");
        app.finish_native_update_reconcile(purpose, result.recv().unwrap());

        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(11)
        );
        let intent = app
            .auto_apply_intent
            .expect("drained StageAvailable wake retains exact automatic intent");
        assert_eq!(intent.build, 11);
        assert_eq!(intent.attempts, 0);
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), None),
            Some(intent.retry_at),
            "the retained intent must arm the event-loop wake without spending budget early"
        );
    }

    #[test]
    fn regional_damage_rejects_theme_or_font_paint_revision_changes() {
        let previous = NativeUiCompileStamp {
            instance: crate::tab_model::AppInstanceId::from_stored(1),
            view: crate::tab_model::ViewId::from_stored(2),
            generation: 7,
            geometry: 11,
            config_revision: 13,
            update_revision: 17,
            document_seq: None,
            presentation_revision: 19,
            paint_revision: 23,
        };
        let mut current = previous;
        current.generation += 1;
        assert!(current.accepts_regional_damage_from(previous));
        current.paint_revision += 1;
        assert!(
            !current.accepts_regional_damage_from(previous),
            "paint input changes promote reducer-local damage to a full raster"
        );
    }

    #[test]
    fn native_appearance_inputs_have_distinct_cache_revisions() {
        let base = crate::native_appearance::AppearancePreferences::default();
        let revision = native_appearance_revision(base);
        for preferences in [
            crate::native_appearance::AppearancePreferences {
                high_contrast: true,
                ..base
            },
            crate::native_appearance::AppearancePreferences {
                reduced_transparency: true,
                ..base
            },
            crate::native_appearance::AppearancePreferences {
                text_scale: 1.25,
                ..base
            },
        ] {
            assert_ne!(native_appearance_revision(preferences), revision);
        }
    }

    #[test]
    fn native_motion_inputs_have_distinct_cache_revisions() {
        let base = crate::native_app::ViewMotionCx::default();
        let revision = native_motion_revision(base);
        for motion in [
            crate::native_app::ViewMotionCx {
                system_reduced: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                focused: false,
                ..base
            },
            crate::native_app::ViewMotionCx {
                performance_reduced: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                serious: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                system_dark: true,
                ..base
            },
            crate::native_app::ViewMotionCx {
                backend_gpu: true,
                ..base
            },
        ] {
            assert_ne!(native_motion_revision(motion), revision);
        }
    }

    #[test]
    fn live_motion_context_changes_the_native_compile_stamp() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
        app.windows.get_mut(&wid).unwrap().focused = true;

        let base = app.native_ui_compile_stamp(wid).unwrap();
        app.system_reduce_motion = true;
        let system_reduced = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(system_reduced.paint_revision, base.paint_revision);

        app.system_reduce_motion = false;
        app.windows.get_mut(&wid).unwrap().focused = false;
        let unfocused = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(unfocused.paint_revision, base.paint_revision);

        app.windows.get_mut(&wid).unwrap().focused = true;
        app.perf_reduced = true;
        let performance_reduced = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(performance_reduced.paint_revision, base.paint_revision);

        app.perf_reduced = false;
        app.set_serious_mode(true);
        let serious = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(serious.paint_revision, base.paint_revision);
    }

    #[test]
    fn plain_terminal_theme_os_flip_invalidates_auto_window_preview() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let theme = app.theme;
        app.os_appearance = aterm_types::Appearance::Light;
        let light = app.native_ui_compile_stamp(wid).unwrap();
        app.os_appearance = aterm_types::Appearance::Dark;
        assert_eq!(
            app.theme.fg, theme.fg,
            "plain terminal foreground stays unchanged"
        );
        assert_eq!(
            app.theme.bg, theme.bg,
            "plain terminal background stays unchanged"
        );
        let dark = app.native_ui_compile_stamp(wid).unwrap();
        assert_ne!(
            light.paint_revision, dark.paint_revision,
            "live OS appearance participates in retained native paint identity"
        );
    }

    #[test]
    fn native_compile_uses_each_window_font_for_stamp_and_preview() {
        let mut app = App::headless_for_test();
        let first = WindowId(0);
        app.windows.get_mut(&first).unwrap().metrics.font_px = 12.0;
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (first_instance, first_view) = app.active_native_view(first).unwrap();
        let first_viewport = app.native_ui_viewport(first).unwrap();

        let next_session = app.next_session_id;
        let (rows, cols) = (app.windows[&first].rows, app.windows[&first].cols);
        let second = app.insert_logical_window(crate::stub_session(next_session), rows, cols);
        {
            let window = app.windows.get_mut(&second).unwrap();
            window.metrics.font_px = 24.0;
            window.scale = 2.0;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::TextFonts));
        let (second_instance, second_view) = app.active_native_view(second).unwrap();
        let second_viewport = app.native_ui_viewport(second).unwrap();
        assert_eq!(
            second_instance, first_instance,
            "the Settings controller remains process-singleton"
        );

        let first_stamp = app
            .native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
            .unwrap();
        let second_stamp = app
            .native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
            .unwrap();
        assert_ne!(
            first_stamp.paint_revision, second_stamp.paint_revision,
            "window-local font size is a native paint input"
        );

        let preview_value = |compiled: &crate::native_ui::CompiledUi| {
            compiled
                .semantics
                .iter()
                .find(|node| node.label == "Typography preview")
                .and_then(|node| match &node.value {
                    crate::native_ui::SemanticValue::Text(value) => Some(value.clone()),
                    _ => None,
                })
                .expect("Typography renderer preview semantics")
        };
        let first_compiled = app
            .compiled_native_ui_for(first, first_instance, first_view, first_viewport)
            .unwrap();
        let second_compiled = app
            .compiled_native_ui_for(second, second_instance, second_view, second_viewport)
            .unwrap();
        assert!(preview_value(&first_compiled).contains("at 12 pixels"));
        assert!(preview_value(&second_compiled).contains("at 24 pixels"));

        // Moving the process-global renderer activation must not perturb either
        // window's native stamp; their MetricsView records remain authoritative.
        app.font_px = 31.0;
        assert_eq!(
            app.native_ui_compile_stamp_for(first, first_instance, first_view, first_viewport)
                .unwrap()
                .paint_revision,
            first_stamp.paint_revision
        );
        assert_eq!(
            app.native_ui_compile_stamp_for(second, second_instance, second_view, second_viewport)
                .unwrap()
                .paint_revision,
            second_stamp.paint_revision
        );
    }

    #[test]
    fn native_damage_union_is_bounded_and_all_dominates() {
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: 10,
                    y: 20,
                    width: 8,
                    height: 9,
                },
                DamageRegion::Rect {
                    x: 4,
                    y: 25,
                    width: 20,
                    height: 10,
                },
            ),
            DamageRegion::Rect {
                x: 4,
                y: 20,
                width: 20,
                height: 15,
            }
        );
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: u32::MAX - 2,
                    y: u32::MAX - 1,
                    width: 20,
                    height: 20,
                },
                DamageRegion::Rect {
                    x: u32::MAX - 4,
                    y: u32::MAX - 3,
                    width: 1,
                    height: 1,
                },
            ),
            DamageRegion::Rect {
                x: u32::MAX - 4,
                y: u32::MAX - 3,
                width: 4,
                height: 3,
            }
        );
        assert_eq!(
            union_native_damage(
                DamageRegion::Rect {
                    x: 1,
                    y: 2,
                    width: 3,
                    height: 4,
                },
                DamageRegion::All,
            ),
            DamageRegion::All
        );
    }

    fn status(staged_build: Option<u64>, failing_checks: u32) -> DurableUpdateStatus {
        DurableUpdateStatus {
            linux_host: false,
            linux: None,
            enabled: true,
            current_build: 10,
            staged_build,
            staged_version: staged_build.map(|build| format!("1.0.{build}")),
            staged_commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            staged_dmg_sha256: staged_build.map(|_| "ab".repeat(32)),
            changelog: Some("# Release notes".to_string()),
            outcome: if failing_checks == 0 {
                "up to date".to_string()
            } else {
                "network failed".to_string()
            },
            failing_checks,
            failing_persistent: false,
            failing_kind: String::new(),
            failing_applies: 0,
            apply_failure: String::new(),
            apply_failure_build: 0,
            apply_failures_for_target: 0,
            installable: true,
            channel_unreadable: false,
            checked_at: None,
        }
    }

    fn reconcile_facts(
        request_sequence: u64,
        observation_sequence: u64,
        durable: Option<DurableUpdateStatus>,
    ) -> NativeUpdateReconcileFacts {
        reconcile_facts_with_installed(request_sequence, observation_sequence, durable, None)
    }

    // The installed-bundle term was never exercised: every reconcile test passed
    // `installed: None`, which is exactly why a floor that folded it in shipped.
    fn reconcile_facts_with_installed(
        request_sequence: u64,
        observation_sequence: u64,
        durable: Option<DurableUpdateStatus>,
        installed: Option<InstalledUpdate>,
    ) -> NativeUpdateReconcileFacts {
        NativeUpdateReconcileFacts {
            _ticket: NativeUpdateReconcileTicket { request_sequence },
            observation_sequence,
            observed_at: std::time::Instant::now(),
            durable,
            installed,
        }
    }

    /// PRODUCTION SHAPE: the sealed `ATermGitCommit` an installed bundle carries is
    /// the 12-char SHORT sha (`248091d23ab0` on v0.27.0), not the manifest's full
    /// 40. The first activation-lane tests all used a full sha and passed while
    /// the field import rejected every real bundle; the fixture now says what the
    /// bundle says.
    fn installed_update(build: u64) -> InstalledUpdate {
        InstalledUpdate {
            build,
            commit: "248091d23ab0".to_string(),
            version: None,
            receipt_build: None,
            receipt_dmg_sha256: None,
        }
    }

    /// The stage the ACTIVATION lane imports for an installed bundle: the sealed
    /// identity under `installed_activation_digest`, and nothing else.
    fn assert_activation_stage(app: &App, build: u64, commit: &str) {
        let staged = app.native_updater_service.snapshot().staged.clone().expect(
            "an installed bundle newer than the process is imported as an activation stage",
        );
        assert_eq!(
            staged.build, build,
            "the activation names the installed build"
        );
        assert_eq!(
            staged.commit.as_deref(),
            Some(commit),
            "…and its sealed commit"
        );
        assert!(
            staged.is_installed_activation(),
            "…under the activation identity, not any DMG digest: {}",
            staged.dmg_sha256
        );
        assert_eq!(
            staged.dmg_sha256,
            crate::native_updater_service::installed_activation_digest(build, commit)
        );
    }

    /// THE ACTIVATION LANE (owner, 2026-08-18: "this cannot happen again"). The
    /// post-seamless-update survivor state — the on-disk bundle's sealed plist is
    /// ALREADY the staged build while the process still executes the older image —
    /// used to be reported as "relaunch once to activate it" and then left alone: a
    /// verified, installed build sat inert until a human read a log line (0.12.0 held
    /// 17.6 hours that way; the v0.25.0 roll-forward stayed un-activated in the very
    /// window it was cut from). Now that bundle is imported as an ACTIVATION stage —
    /// the ordinary stage → automatic apply → seamless handoff path, with the bundle
    /// under the executable as the artifact — and the answer is STABLE across
    /// reconciles (the original defect was a fixed point that blanked the stage on
    /// every pass; the activation is Unchanged while the bundle still backs it).
    /// The "Update ready" toast and the level-up fire on a REAL stage import through
    /// the production door (`finish_native_update_reconcile`). They never did: the
    /// reconcile published the stage into `self.relaunch` before `newly_announced`
    /// was computed, so every import compared equal to itself and stayed silent.
    /// The RETURNED-apply arms can carry disk facts (`finish_async_native_update_handoff`
    /// with `reconcile: Some` — a lane every shipping completion site today leaves
    /// `None`, posting facts as a separate Startup wake instead; this pins the arms
    /// for whoever wires it). Our own child swapped the bundle to the target and
    /// then failed to commit: the facts say the installed bundle is newer, the
    /// stage is consumed (InstalledNeedsRelaunch), and what those arms did next was
    /// `let _ = reconcile(...)` — an activation imported and never ARMED, with the
    /// outcome promising "activates at the next quiet moment". Every arm now reduces
    /// through the arming door.
    #[test]
    fn a_returned_apply_whose_child_swapped_the_bundle_arms_the_activation() {
        let mut app = App::headless_for_test();
        let (_, _settings) = park_a_settings_draft_in_a_background_tab(&mut app);
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        let ApplyPreflightStart::Inspect(preflight) = app
            .native_updater_service
            .begin_apply_preflight(ApplyMode::AutomaticPastGrace)
        else {
            panic!("the stage must admit an apply preflight");
        };
        let ApplyDecision::Execute(command) = app
            .native_updater_service
            .finish_apply_preflight(preflight, ClosePreflight::Ready)
        else {
            panic!("a ready close preflight must authorize the replacement");
        };
        let attempt = command.attempt();
        command.execute(|| ());
        app.auto_apply_intent = None;
        // The child swapped the bundle (installed == target, receipt names the
        // download) and then failed to prove readiness.
        let facts = reconcile_facts_with_installed(
            7,
            7,
            Some(DurableUpdateStatus {
                linux_host: false,
                linux: None,
                enabled: true,
                current_build: app.native_updater_service.snapshot().current_build,
                staged_build: Some(build),
                staged_version: Some(format!("1.0.{build}")),
                staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
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
            }),
            Some(InstalledUpdate {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                version: None,
                receipt_build: Some(build),
                receipt_dmg_sha256: Some("ab".repeat(32)),
            }),
        );
        let outcome = app.finish_async_native_update_handoff(
            attempt,
            facts,
            "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );
        assert!(
            matches!(outcome, Some(UpdateOutcome::InstalledNeedsRelaunch { .. })),
            "the disposition names the installed build, got {outcome:?}"
        );
        assert_activation_stage(&app, build, PREFLIGHT_TEST_COMMIT);
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .unwrap();
        let activation = decode_dmg_sha256(&staged.dmg_sha256).unwrap();
        // THE FAILURE IS CHARGED, AND ITS LATCH GUARDS THE ACTIVATION (the
        // 2026-09-22/23 update audit, plan P0-6). This arm used to clear the latch
        // and charge nothing, so the activation armed at once: a physical failure
        // bought an immediate retry instead of its schedule's 600 s.
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "the returned physical failure spends the shared budget"
        );
        let latch = app
            .auto_apply_manual_only
            .expect("the failure latches the automatic lane");
        assert_eq!(
            (latch.build, latch.dmg_sha256, latch.activation),
            (build, activation, true),
            "…keyed to the activation the reduction just imported"
        );
        assert_eq!(latch_rung(latch.retry_at), "retry-600");
        assert!(
            app.auto_apply_intent.is_none(),
            "the activation does not arm inside the failure's own latch"
        );
        // …and it is ARMED, not merely described, the moment that latch lapses.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..latch
        });
        app.rearm_native_auto_apply_after_lapse();
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == activation),
            "the activation the returned facts imported is ARMED once the latch lapses"
        );
    }

    #[test]
    fn an_unknown_probe_after_returned_activation_keeps_the_bounded_retry() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let installed = installed_update(build);
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            1,
            1,
            Some(status(None, 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, build, &installed.commit);
        let ApplyPreflightStart::Inspect(preflight) = app
            .native_updater_service
            .begin_apply_preflight(ApplyMode::AutomaticPastGrace)
        else {
            panic!("activation must admit preflight");
        };
        let ApplyDecision::Execute(command) = app
            .native_updater_service
            .finish_apply_preflight(preflight, ClosePreflight::Ready)
        else {
            panic!("ready activation must issue an apply ticket");
        };
        let attempt = command.attempt();
        command.execute(|| ());
        app.auto_apply_intent = None;
        let outcome = app.finish_async_native_update_handoff(
            attempt.clone(),
            reconcile_facts(2, 2, Some(status(None, 0))),
            "candidate timed out and installed-bundle verification was inconclusive".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );
        assert!(matches!(outcome, Some(UpdateOutcome::Failed { .. })));
        assert_activation_stage(&app, build, &installed.commit);
        let retry = app
            .auto_apply_manual_only
            .expect("failure schedules its retry");
        assert_eq!(retry.build, build);
        assert!(
            retry
                .retry_at
                .is_some_and(|at| at > std::time::Instant::now())
        );
        assert!(app.automatic_apply_retry_scheduled(build));
        assert!(
            app.auto_apply_intent.is_none(),
            "the cooldown must not be bypassed"
        );

        let budget = app.auto_apply_physical_retry;
        assert!(
            budget.is_some(),
            "the real completion must charge the physical budget"
        );
        assert!(
            app.finish_async_native_update_handoff(
                attempt,
                reconcile_facts(3, 3, Some(status(None, 0))),
                "duplicate completion".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
            )
            .is_none()
        );
        assert_eq!(app.auto_apply_physical_retry, budget);
        assert_eq!(app.auto_apply_manual_only, Some(retry));
    }

    #[test]
    fn a_freshly_imported_stage_raises_no_card_and_no_glow_and_a_repeat_import_is_quiet() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let build = running + 1;
        assert!(app.level_up.is_none() && app.relaunch.is_none());
        let rows_before = app.messages.live_rows().count();
        let facts = || {
            reconcile_facts_with_installed(
                1,
                1,
                Some(DurableUpdateStatus {
                    linux_host: false,
                    linux: None,
                    enabled: true,
                    current_build: running,
                    staged_build: Some(build),
                    staged_version: Some("9.9.0".to_string()),
                    staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
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
                }),
                Some(installed_update(running)),
            )
        };
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, facts());
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|s| s.build),
            Some(build),
            "PRECONDITION: the stage imported"
        );
        // THE ANNOUNCEMENT IS THE BAND'S (2026-09-07): the stage is the band's
        // staged row, and nothing floats — the retired card is gone and no
        // border glow is raised; the surge is the upgrade's, and fires when the
        // apply actually runs.
        let rows = |app: &App| {
            app.messages
                .live_rows()
                .map(|l| (l.id, l.msg.title.clone()))
                .collect::<Vec<_>>()
        };
        let announced = rows(&app);
        assert!(
            announced.len() <= rows_before + 1,
            "one staged row at most: {announced:?}"
        );
        assert!(app.level_up.is_none(), "…and no stage-time glow");
        // The SAME stage again is not news.
        app.level_up = None;
        let mut again = facts();
        again.observation_sequence = 2;
        again._ticket = NativeUpdateReconcileTicket {
            request_sequence: 2,
        };
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, again);
        assert!(
            rows(&app) == announced && app.level_up.is_none(),
            "a repeat import is quiet"
        );
    }

    #[test]
    fn a_bundle_newer_than_the_process_becomes_an_activation_stage_and_stays_one() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        assert!(running < 12, "fixture must model a newer installed build");
        let installed = installed_update(12);
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            1,
            1,
            Some(status(Some(12), 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, 12, &installed.commit);
        let outcome = app.native_updater_service.snapshot().outcome.clone();
        assert!(
            outcome.contains("already installed") && outcome.contains("activation is pending"),
            "the durable outcome says the bytes are on disk with activation pending, got {outcome:?}"
        );

        // STABLE: the same facts again keep the same activation stage (no retire,
        // no re-import churn — the automatic intent would be reset every pass).
        let before = app.native_updater_service.snapshot().staged.clone();
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            2,
            2,
            Some(status(Some(12), 0)),
            Some(installed.clone()),
        ));
        assert_eq!(
            app.native_updater_service.snapshot().staged,
            before,
            "repeating the reconcile must not disturb the activation stage"
        );

        // AND IT RETIRES WHEN THE BUNDLE MOVES ON: a bundle that is no longer newer
        // (rolled back under us) drops the activation instead of activating stale
        // bytes; the next observation imports whatever is really there.
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            3,
            3,
            Some(status(None, 0)),
            Some(installed_update(running)),
        ));
        assert!(
            app.native_updater_service.snapshot().staged.is_none(),
            "an activation whose bundle is no longer newer must retire"
        );
    }

    /// A newer INSTALLED bundle outranks a newer staged DOWNLOAD: once the bundle
    /// under the executable is not the running build, a swap-apply of any download
    /// can no longer proceed (its rollback-source proof needs the running build on
    /// disk), so the download stage retires and the activation takes its place —
    /// the successor observes the download on its own terms. (Before the activation
    /// lane this test asserted the opposite: that the download stayed staged while
    /// the installed bundle "got no vote" — which left the machine wedged: a stage
    /// that could never apply and a bundle that never activated.)
    #[test]
    fn a_newer_installed_bundle_outranks_a_newer_staged_download() {
        let mut app = App::headless_for_test();
        let installed = installed_update(11);
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            1,
            1,
            Some(status(Some(12), 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, 11, &installed.commit);
    }

    /// The drag-a-new-app-over-the-old-one case: NOTHING is staged or downloaded,
    /// the bundle under the executable is simply newer. That is an activation too —
    /// the lane keys off the bare installed fact, not off any durable stage marker.
    #[test]
    fn a_newer_installed_bundle_with_nothing_staged_is_still_an_activation() {
        let mut app = App::headless_for_test();
        let installed = installed_update(12);
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            1,
            1,
            Some(status(None, 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, 12, &installed.commit);
    }

    #[test]
    fn observation_order_prevents_old_or_missing_read_from_downgrading_stage() {
        let mut missing_then_new = App::headless_for_test();
        let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(2, 1, None));
        let _ = missing_then_new.reconcile_native_update_facts(reconcile_facts(
            1,
            2,
            Some(status(Some(12), 0)),
        ));
        assert_eq!(
            missing_then_new
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(12)
        );

        let mut completion_reordered = App::headless_for_test();
        let _ = completion_reordered.reconcile_native_update_facts(reconcile_facts(
            1,
            2,
            Some(status(Some(12), 0)),
        ));
        assert!(matches!(
            completion_reordered.reconcile_native_update_facts(reconcile_facts(2, 1, None)),
            NativeUpdateFactsResult::IgnoredStale
        ));
        assert_eq!(
            completion_reordered
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.build),
            Some(12),
            "older observation completion cannot retire the newer imported stage"
        );
    }

    #[test]
    fn same_build_different_digest_uses_latest_observation_only() {
        let mut app = App::headless_for_test();
        let mut newest = status(Some(11), 0);
        newest.staged_dmg_sha256 = Some("cd".repeat(32));
        let _ = app.reconcile_native_update_facts(reconcile_facts(1, 4, Some(newest)));
        assert!(matches!(
            app.reconcile_native_update_facts(reconcile_facts(2, 3, Some(status(Some(11), 0)),)),
            NativeUpdateFactsResult::IgnoredStale
        ));
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|stage| stage.dmg_sha256.as_str()),
            Some("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd")
        );
    }

    #[test]
    fn installed_environment_repair_conforms_and_preserves_unrelated_budgets() {
        use crate::update_apply_trouble::ApplyRetry;
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let model = aterm_spec::derive::native_update_environment_repair_model();
        // Good, stale, unknown, wrong build, wrong commit, unrelated latch,
        // newer cache, auto opt-out, an ordinary exhausted budget, and a
        // verified but noninstallable (for example dev-marked) source.
        for case in 0..10 {
            let mut app = App::headless_for_test();
            app.native_updater_service = NativeUpdaterService::new(10, "test", true);
            stage_one_build_for_test(&mut app, 11);
            app.block_native_auto_apply_environment(11, [0xab; 32]);
            let blocked_at = app.auto_apply_environment_block.unwrap().blocked_at;
            let reason = "the installed bundle cannot be the rollback source";
            app.native_updater_service
                .note_apply_failure(&aterm_update::RecordedApplyFailure {
                    apply_failures: 1,
                    failures_for_target: 1,
                    target_build: 11,
                    reason: reason.to_string(),
                    persistent: false,
                });
            assert_eq!(app.apply_retry_for(Some(11)), ApplyRetry::NeedsPerson);
            let blocked_detail = app.update_snapshot(false).projection().detail.unwrap();
            assert!(
                blocked_detail
                    .contains("Install the signed release from the release DMG, then retry."),
                "the real App must pass its current environment block to the page: {blocked_detail}"
            );
            assert!(!blocked_detail.contains("try again by itself"));
            let cache = std::sync::Arc::clone(&app.handoff_preverified);
            *cache.lock().unwrap() = Some(crate::HandoffPreverification {
                build: 11,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: if case == 6 {
                    blocked_at + std::time::Duration::from_secs(2)
                } else {
                    blocked_at
                },
                passed: false,
                reason: Some(reason.to_string()),
            });
            if case == 5 {
                app.auto_apply_manual_only.as_mut().unwrap().dmg_sha256 = [0xcd; 32];
            }
            if case == 8 {
                app.auto_apply_environment_block = None;
            }
            if case == 7 {
                app.config.update = Some(crate::app_config::UpdateConfig {
                    auto_apply: Some(false),
                    ..Default::default()
                });
            }
            app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
                build: 12,
                dmg_sha256: [0xcd; 32],
                activation: false,
                cycles: PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
                last_attempt: blocked_at,
            });
            let budget = app.auto_apply_physical_retry;
            let owned = !matches!(case, 5 | 8);
            let fresh = !matches!(case, 1 | 6);
            let verified = !matches!(case, 2..=4 | 9);
            let mut durable = status(Some(11), 0);
            durable.failing_applies = 1;
            durable.apply_failure_build = 11;
            durable.apply_failures_for_target = 1;
            durable.apply_failure = reason.to_string();
            durable.installable = case != 9;
            let mut facts = reconcile_facts_with_installed(
                1,
                1,
                Some(durable),
                (case != 2).then(|| InstalledUpdate {
                    build: if case == 3 { 9 } else { 10 },
                    commit: if case == 4 {
                        "f".repeat(40)
                    } else {
                        PREFLIGHT_TEST_COMMIT.to_string()
                    },
                    version: None,
                    receipt_build: None,
                    receipt_dmg_sha256: None,
                }),
            );
            facts.observed_at = if case == 1 {
                blocked_at - std::time::Duration::from_millis(1)
            } else {
                blocked_at + std::time::Duration::from_secs(1)
            };
            let mut before = model.init_state();
            for (guard, action) in [(owned, "Own"), (fresh, "Fresh"), (verified, "Verify")] {
                if guard {
                    before = model.successors(action, &before).remove(0);
                }
            }
            if case == 0 {
                facts.durable.as_mut().unwrap().enabled = false;
                app.release_repaired_auto_apply_environment_for_commit(
                    &facts,
                    PREFLIGHT_TEST_COMMIT,
                );
                assert!(
                    app.auto_apply_manual_only.is_some(),
                    "disabled updater cannot release a repair latch"
                );
                facts.durable.as_mut().unwrap().enabled = true;
                app.release_repaired_auto_apply_environment_for_commit(
                    &facts,
                    &format!("{PREFLIGHT_TEST_COMMIT}-dirty"),
                );
                assert!(
                    app.auto_apply_manual_only.is_some(),
                    "a dirty running identity cannot prove a signed-source repair"
                );
                let held = cache.lock().unwrap();
                app.release_repaired_auto_apply_environment_for_commit(
                    &facts,
                    PREFLIGHT_TEST_COMMIT,
                );
                assert!(
                    app.auto_apply_manual_only.is_some(),
                    "cache contention must not wait or release"
                );
                drop(held);
            }
            // A dirty test binary has no verifiable committed identity. Supply a
            // clean running identity to the same production reduction; shipping
            // calls pass GIT_COMMIT and retain their dirty-identity refusal.
            app.release_repaired_auto_apply_environment_for_commit(&facts, PREFLIGHT_TEST_COMMIT);
            let mut after = before.clone();
            after.insert("decided", 1);
            after.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
            assert_eq!(
                model.successors("Reduce", &before),
                vec![after.clone()],
                "case {case}"
            );
            assert_eq!(app.auto_apply_physical_retry, budget);
            if owned && fresh && verified {
                assert!(
                    cache.lock().unwrap().is_none(),
                    "old denial is invalidated, never forged into success"
                );
                // Substitute the next full preverification result, avoiding any
                // OS verifier in this policy fixture, then drive real arming.
                *cache.lock().unwrap() = Some(crate::HandoffPreverification {
                    build: 11,
                    commit: PREFLIGHT_TEST_COMMIT.to_string(),
                    artifact: "ab".repeat(32),
                    at: std::time::Instant::now(),
                    passed: true,
                    reason: None,
                });
                assert_eq!(app.arm_native_auto_apply(11, &"ab".repeat(32)), case != 7);
                assert_eq!(
                    app.apply_retry_for(Some(11)),
                    if case == 7 {
                        ApplyRetry::ManualOnly
                    } else {
                        ApplyRetry::Scheduled
                    }
                );
                assert_eq!(app.native_updater_service.snapshot().apply_failure, reason);
                assert!(
                    !app.update_snapshot(false)
                        .projection()
                        .detail
                        .unwrap()
                        .contains("Install the signed release"),
                    "a repaired source no longer asks for repair, including with auto-apply off"
                );
                assert_eq!(
                    app.apply_trouble_for(11)
                        .unwrap()
                        .sentence()
                        .contains("try again by itself"),
                    case != 7,
                    "historical cause cannot override current repaired scheduling"
                );
                // Historical permanent-latch behavior fails this recovery law.
                after.insert("latched", 1);
                assert!(!model.check_invariant("RepairedSourceRecovers", &after));
            }
        }
    }

    #[test]
    fn retiring_a_withdrawn_stage_releases_only_its_exact_latch_and_rearms_a_replacement() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let model = aterm_spec::derive::native_update_retired_intent_model();
        for unrelated in [0, 1, 2] {
            let mut app = App::headless_for_test();
            let withdrawn = app.native_updater_service.snapshot().current_build + 2;
            let replacement = withdrawn - 1;
            let _ = app.reconcile_native_update_facts(reconcile_facts(
                1,
                1,
                Some(status(Some(withdrawn), 0)),
            ));
            let latch = crate::AutoApplyManualOnly {
                build: withdrawn + u64::from(unrelated == 1),
                dmg_sha256: if unrelated == 2 {
                    [0xcd; 32]
                } else {
                    [0xab; 32]
                },
                activation: false,
                retry_at: None,
            };
            app.auto_apply_manual_only = Some(latch);
            // An unchanged exact stage and an older observation must keep the
            // latch: a periodic reconcile is not permission to reset a budget.
            let _ = app.reconcile_native_update_facts(reconcile_facts(
                2,
                2,
                Some(status(Some(withdrawn), 0)),
            ));
            assert_eq!(app.auto_apply_manual_only, Some(latch));
            assert!(matches!(
                app.reconcile_native_update_facts(reconcile_facts(1, 1, Some(status(None, 0)))),
                NativeUpdateFactsResult::IgnoredStale
            ));
            assert_eq!(app.auto_apply_manual_only, Some(latch));

            let mut before = model.init_state();
            if unrelated != 0 {
                before = model.successors("OtherArtifact", &before).remove(0);
            }
            let _ = app.reconcile_native_update_facts(reconcile_facts(3, 3, Some(status(None, 0))));
            let mut after = before.clone();
            after.insert(
                "retired",
                i64::from(app.native_updater_service.snapshot().staged.is_none()),
            );
            after.insert("latched", i64::from(app.auto_apply_manual_only.is_some()));
            assert_eq!(model.successors("Retire", &before), vec![after.clone()]);
            if unrelated != 0 {
                assert_eq!(app.auto_apply_manual_only, Some(latch));
                continue;
            }
            assert!(app.auto_apply_manual_only.is_none());
            app.finish_native_update_reconcile(
                NativeUpdateReconcilePurpose::StageAvailable,
                reconcile_facts(4, 4, Some(status(Some(replacement), 0))),
            );
            assert!(
                app.auto_apply_intent
                    .is_some_and(|intent| intent.build == replacement)
            );
            assert!(app.automatic_apply_retry_scheduled(replacement));

            // Restore only the historical leftover state, then drive the real
            // arming reducer. It demonstrates why the removed latch stranded a
            // valid replacement that is newer than the running process.
            app.auto_apply_manual_only = Some(latch);
            app.auto_apply_intent = None;
            assert!(!app.arm_native_auto_apply(replacement, &"ab".repeat(32)));
            assert!(app.auto_apply_intent.is_none());
            let mut historical = after;
            historical.insert("latched", 1);
            assert!(!model.successors("Retire", &before).contains(&historical));
            assert!(!model.check_invariant("NoObsoleteLatch", &historical));
        }
    }

    #[test]
    fn a_refresh_never_outranks_a_purpose_that_announces_or_applies() {
        use NativeUpdateReconcilePurpose::{ApplyControl, Refresh, StageAvailable, Startup};
        for other in [Startup, StageAvailable, ApplyControl] {
            assert_eq!(merge_reconcile_purpose(Refresh, other), other);
            assert_eq!(merge_reconcile_purpose(other, Refresh), other);
        }
        assert_eq!(merge_reconcile_purpose(Refresh, Refresh), Refresh);
        // And a startup fact import coalesced with a refresh still announces.
        assert_eq!(merge_reconcile_purpose(Startup, Refresh), Startup);
    }

    #[test]
    fn deferred_reconcile_merges_strongest_purpose_independently_of_fact_order() {
        for (first_purpose, first_observation, second_purpose, second_observation) in [
            (
                NativeUpdateReconcilePurpose::StageAvailable,
                9,
                NativeUpdateReconcilePurpose::ApplyControl,
                8,
            ),
            (
                NativeUpdateReconcilePurpose::ApplyControl,
                8,
                NativeUpdateReconcilePurpose::Startup,
                9,
            ),
            (
                NativeUpdateReconcilePurpose::Startup,
                7,
                NativeUpdateReconcilePurpose::ApplyControl,
                10,
            ),
        ] {
            let mut app = App::headless_for_test();
            let _active = start(&mut app.native_updater_service);
            app.finish_native_update_reconcile(
                first_purpose,
                reconcile_facts(1, first_observation, Some(status(Some(11), 0))),
            );
            app.finish_native_update_reconcile(
                second_purpose,
                reconcile_facts(2, second_observation, Some(status(Some(12), 0))),
            );
            let (purpose, facts) = app
                .deferred_native_update_reconcile
                .as_ref()
                .expect("active service defers facts");
            assert_eq!(*purpose, NativeUpdateReconcilePurpose::ApplyControl);
            assert_eq!(
                facts.observation_sequence,
                first_observation.max(second_observation),
                "purpose merge and newest-facts selection are independent"
            );
        }
    }

    /// "Check for Updates…" parks a Refresh behind its own check (the route opens,
    /// queues a reconcile, and the check starts in the same turn). When the check
    /// then STAGES, replaying those pre-stage facts retired the fresh stage and
    /// left a verified update on disk armed by nothing. The parked facts are now
    /// dropped in favour of a fresh observation, and the stage survives.
    #[test]
    fn facts_parked_behind_a_check_cannot_retire_the_stage_that_check_imports() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let build = running + 1;
        let ticket = start(&mut app.native_updater_service);
        // Observed BEFORE the check staged anything: nothing staged, bundle == running.
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::Refresh,
            reconcile_facts_with_installed(
                1,
                3,
                Some(status(None, 0)),
                Some(installed_update(running)),
            ),
        );
        assert!(
            app.deferred_native_update_reconcile.is_some(),
            "PRECONDITION: parked"
        );
        // The check completes WITH a stage.
        app.finish_native_update_check(ticket, status(Some(build), 0));
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|s| s.build),
            Some(build),
            "the stage the check imported survives the parked pre-stage facts"
        );
        assert!(
            app.deferred_native_update_reconcile.is_none(),
            "the stale parked facts are gone (re-observed fresh, not replayed)"
        );
    }

    /// The IN-FLIGHT variant of the same defect: a read that BEGAN before the check
    /// staged (its wake lands after, so it is not parked — the reducer is free) must
    /// not retire the stage either. `observed_at` is the floor.
    #[test]
    fn facts_read_before_the_stage_import_cannot_retire_it_however_late_they_land() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let build = running + 1;
        // Read began BEFORE the import…
        let mut early = reconcile_facts_with_installed(
            1,
            3,
            Some(status(None, 0)),
            Some(installed_update(running)),
        );
        early.observed_at = std::time::Instant::now() - std::time::Duration::from_secs(1);
        let ticket = start(&mut app.native_updater_service);
        app.finish_native_update_check(ticket, status(Some(build), 0));
        assert!(
            app.native_stage_imported_at.is_some(),
            "PRECONDITION: the import is floored"
        );
        // …and its wake lands after the check completed (reducer free, not parked).
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh, early);
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|s| s.build),
            Some(build),
            "a pre-import observation is stale by construction and retires nothing"
        );
        // A read that began AFTER the import and sees the stage on disk is reduced
        // normally and keeps it.
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::Refresh,
            reconcile_facts_with_installed(
                2,
                4,
                Some(status(Some(build), 0)),
                Some(installed_update(running)),
            ),
        );
        assert_eq!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|s| s.build),
            Some(build)
        );
    }

    /// The SHIPPING returned-apply lane: a control apply parked while the reducer
    /// was busy, then the failed attempt's own (newer) Startup facts arrive first.
    /// The parked ApplyControl used to wait for the idle backstop's replay, which
    /// found its facts stale and dropped the request with nothing surfaced. It now
    /// rides the newer facts: the reduction that lands them acts on ApplyControl.
    #[test]
    fn a_parked_control_apply_rides_the_next_newer_facts_instead_of_going_stale() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let build = running + 1;
        // Park an ApplyControl behind an active check (the reducer defers facts
        // while work is active).
        let active = start(&mut app.native_updater_service);
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::ApplyControl,
            reconcile_facts(1, 5, Some(status(Some(build), 0))),
        );
        assert!(
            app.deferred_native_update_reconcile
                .as_ref()
                .is_some_and(|(p, f)| *p == NativeUpdateReconcilePurpose::ApplyControl
                    && f.observation_sequence == 5),
            "PRECONDITION: the control apply is parked"
        );
        // The check completes with nothing (the reducer is free again)…
        let _ = app
            .native_updater_service
            .finish_check(active, status(None, 0));
        // …and NEWER facts arrive under a plain Startup purpose, exactly like the
        // failed attempt's own facts wake. Before the fix these reduced on their own,
        // the parked pair replayed later, went IgnoredStale, and the apply was lost.
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::Startup,
            reconcile_facts(2, 9, Some(status(Some(build), 0))),
        );
        assert!(
            app.deferred_native_update_reconcile.is_none(),
            "the parked purpose merged into the newer facts instead of waiting to go stale"
        );
        // ApplyControl acted: an Immediate apply was attempted on the imported stage,
        // which in a headless host is refused by preflight (no event-loop service) —
        // observable as a surfaced control-request outcome rather than silence.
        let outcome = app.native_updater_service.snapshot().outcome.clone();
        assert!(
            app.native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .map(|s| s.build)
                == Some(build),
            "the newer facts imported the stage, got outcome {outcome:?}"
        );
        assert!(
            app.update_row_text().is_some()
                || app.native_updater_service.snapshot().phase != UpdaterPhase::Staged,
            "the control apply was acted on (surfaced or moved the phase), not dropped"
        );
    }

    fn start(service: &mut NativeUpdaterService) -> UpdaterWorkTicket {
        match service.request_check() {
            CheckStart::Start(ticket) => ticket,
            other => panic!("expected updater work, got {other:?}"),
        }
    }

    #[test]
    fn same_auto_apply_build_keeps_existing_backoff_and_attempt_count() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let retry_at = std::time::Instant::now() + std::time::Duration::from_secs(23);
        let retained = crate::AutoApplyIntent {
            build,
            dmg_sha256: [0xab; 32],
            retry_at,
            attempts: 3,
        };
        app.auto_apply_intent = Some(retained);

        assert!(!app.arm_native_auto_apply(build, &"ab".repeat(32)));
        assert_eq!(app.auto_apply_intent, Some(retained));
    }

    #[test]
    fn automatic_retry_budget_is_bounded_and_physical_failures_have_no_timer_plan() {
        assert_eq!(
            automatic_retry_delay(2, AutomaticRetryKind::PreflightBlocked),
            Some(std::time::Duration::from_secs(15))
        );
        assert_eq!(
            automatic_retry_delay(u8::MAX, AutomaticRetryKind::PreflightBlocked),
            None
        );
        // Physical failures get a SHORT budget on a LONG leash — not the zero they
        // used to get. `TimedOut` is classified physical while the handoff deadline
        // must cover a whole cold boot + swap + re-exec + repaint, so the commonest
        // physical failure is a missed deadline, and permanently retiring automatic
        // apply for it stranded the staged build until the next relaunch.
        assert_eq!(
            automatic_retry_delay(0, AutomaticRetryKind::PhysicalFailure),
            Some(std::time::Duration::from_secs(600)),
            "the first physical failure earns one retry, ten minutes out"
        );
        assert_eq!(
            automatic_retry_delay(1, AutomaticRetryKind::PhysicalFailure),
            Some(std::time::Duration::from_secs(1800)),
            "the second waits half an hour"
        );
        // ...and then this schedule really does stop. `None` here ends an EPOCH,
        // not the lane: `spend_physical_failure_budget` turns it into a long
        // stand-down, and only `MAX_PHYSICAL_FAILURE_EPOCHS` of them converge to
        // manual-only (see `the_physical_failure_budget_converges_under_its_own_schedule`).
        assert_eq!(
            automatic_retry_delay(
                MAX_PHYSICAL_FAILURE_CYCLES,
                AutomaticRetryKind::PhysicalFailure
            ),
            None,
            "the epoch's budget is spent"
        );
        assert_eq!(
            automatic_retry_delay(u8::MAX, AutomaticRetryKind::PhysicalFailure),
            None
        );
    }

    #[test]
    fn superseding_auto_apply_build_replaces_intent_without_ui_churn() {
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: current + 1,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now() + std::time::Duration::from_secs(30),
            attempts: 4,
        });

        let before = std::time::Instant::now();
        assert!(app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
        let after = std::time::Instant::now();
        let intent = app.auto_apply_intent.expect("newer build stays armed");
        assert_eq!(intent.build, current + 2);
        assert_eq!(intent.dmg_sha256, [0xcd; 32]);
        assert_eq!(intent.attempts, 0);
        assert!(intent.retry_at >= before + crate::AUTOMATIC_UPDATE_QUIET_EPOCH);
        assert!(intent.retry_at <= after + crate::AUTOMATIC_UPDATE_QUIET_EPOCH);
    }

    /// Regression trace for the presentation-ack liveness bug:
    /// HiddenOutput -> WakeHandledNoPresent -> quiet epoch -> Attempt.
    ///
    /// The hidden session deliberately retains its first-edge presentation stamp
    /// forever. With cursor/blink/effects disabled there is no incidental redraw to
    /// clear it, yet latest actual output ages to quiet and the compiled auto-intent
    /// reducer admits the exact staged build.
    #[test]
    fn old_hidden_output_without_present_ages_to_automatic_attempt() {
        let mut app = App::headless_for_test();
        app.config.cursor_blink = Some(false);
        app.serious_mode = true;
        // Serious Mode suppresses word decorations without an invisible
        // runtime override that could contradict the saved Top Settings toys.
        // Matrix rain needs no kill here: the config default is OFF and no
        // session override exists, so no engine (hence no effect redraw) can
        // arise (the old app-global `rain_force_off` latch is retired).
        if let Some(window) = app.windows.get_mut(&crate::WindowId(0)) {
            window.focused = false;
            window.next_blink = None;
        }

        // Appending session 1 makes session 0 a genuine hidden background tab.
        app.push_stub_tab(crate::WindowId(0), crate::stub_session(app.next_session_id));
        assert!(!app.is_visible_session(0));
        assert!(app.headless);

        let output_at = std::time::Instant::now();
        let output_ns = u64::try_from(
            output_at
                .saturating_duration_since(app.lat_epoch)
                .as_nanos(),
        )
        .unwrap_or(u64::MAX)
        .max(1);
        let (hidden_present_stamp, hidden_activity_stamp) = {
            let hidden = app.pool.get(0).expect("hidden session remains pooled");
            (
                hidden.last_output_ns.clone(),
                hidden.latest_output_activity_ns.clone(),
            )
        };
        hidden_present_stamp.store(output_ns, std::sync::atomic::Ordering::Relaxed);
        hidden_activity_stamp.store(output_ns, std::sync::atomic::Ordering::Release);

        // Before the output wake is handled, the latest-output clock alone closes
        // the race even though the preceding handled activity is already quiet.
        app.last_update_activity_at = output_at
            .checked_sub(crate::AUTOMATIC_UPDATE_QUIET_EPOCH + std::time::Duration::from_nanos(1))
            .expect("monotonic clock has at least one quiet epoch of history");
        assert!(!app.automatic_update_activity_quiet_with_pending_input(output_at, false));

        // Handle the wake without presenting the hidden tab. Both recent-output
        // clocks reject immediately after the wake.
        app.note_update_handoff_activity();
        let wake_handled_at = app.last_update_activity_at;
        assert!(!app.automatic_update_activity_quiet_with_pending_input(wake_handled_at, false));

        let quiet_at = wake_handled_at
            + crate::AUTOMATIC_UPDATE_QUIET_EPOCH
            + std::time::Duration::from_nanos(1);
        assert!(app.automatic_update_activity_quiet_with_pending_input(quiet_at, false));
        assert_ne!(
            hidden_present_stamp.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "no present acknowledged the hidden latency sample"
        );

        let target_build = app.native_updater_service.snapshot().current_build + 1;
        assert_eq!(
            crate::native_update_auto_intent::poll(crate::native_update_auto_intent::PollFacts {
                enabled: true,
                deadline_ready: true,
                current_build: target_build - 1,
                target_build,
                work_active: false,
                applying: false,
                activity_quiet: true,
                phase: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
                staged_ready: true,
                staged_build: Some(target_build),
                staged_exact_target: true,
            }),
            crate::native_update_auto_intent::PollDecision::Attempt {
                build: target_build,
                quiet: true,
                phase: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
            }
        );

        let retry_now = std::time::Instant::now();
        assert!(crate::automatic_update_activity_retry_at(retry_now) > retry_now);
    }

    /// Seamless seam 4 (retry spacing): an activity-revoked overlap schedules
    /// spaced automatic re-attempts that SATURATE at 30 s and never exhaust —
    /// activity is a delay, not a verdict, and the spacing stays inside the
    /// ladder's one-minute bound. Genuine physical failures keep their small,
    /// exhausting budget, and a preflight block keeps its own much smaller one.
    #[test]
    fn activity_revoked_retry_spacing_saturates_and_never_exhausts() {
        use std::time::Duration;
        let schedule = [2, 5, 10, 20, 30];
        assert_eq!(
            schedule.len(),
            usize::from(ACTIVITY_REVOKED_LADDER_RUNGS),
            "the schedule must cover exactly the ladder's rungs"
        );
        for (cycles, seconds) in schedule.into_iter().enumerate() {
            assert_eq!(
                automatic_retry_delay(
                    u8::try_from(cycles).expect("small"),
                    AutomaticRetryKind::ActivityRevoked
                ),
                Some(Duration::from_secs(seconds)),
                "cycle {cycles}"
            );
        }
        for cycles in [ACTIVITY_REVOKED_LADDER_RUNGS, 50, u8::MAX] {
            assert_eq!(
                automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked),
                Some(Duration::from_secs(30)),
                "cycle {cycles}: the last rung, forever — never `None`"
            );
        }
        assert!(
            Duration::from_secs(30) < crate::native_update_auto_intent::LANDS_WITHIN,
            "the saturated spacing fits inside the bound it serves"
        );
        // The preflight budget is deliberately NOT widened: a blocked preflight
        // is a real ordering fault, not "the terminal was busy".
        assert_eq!(
            automatic_retry_delay(
                MAX_AUTOMATIC_UPDATE_CYCLES,
                AutomaticRetryKind::PreflightBlocked
            ),
            None
        );
        // A physical failure is not free to repeat: its epoch budget is small
        // and it IS spent.
        for cycles in 0..MAX_PHYSICAL_FAILURE_CYCLES {
            assert!(
                automatic_retry_delay(cycles, AutomaticRetryKind::PhysicalFailure).is_some(),
                "physical cycle {cycles} is inside the budget"
            );
        }
        for cycles in MAX_PHYSICAL_FAILURE_CYCLES..=ACTIVITY_REVOKED_LADDER_RUNGS {
            assert_eq!(
                automatic_retry_delay(cycles, AutomaticRetryKind::PhysicalFailure),
                None,
                "a spent physical budget must never mint another timer retry"
            );
        }
    }

    /// A BUDGET WHOSE OWN SCHEDULE RESETS IT IS NOT A BUDGET.
    ///
    /// The physical lane's cycle counter is only retained while the gap since the
    /// last attempt is shorter than its replenish window. It used to borrow the
    /// ACTIVITY window (30 min) — and its own second retry waits exactly 30 min,
    /// so a failure that arrived on the schedule the budget itself armed always
    /// landed at or past the threshold, reset `cycles` to zero, and handed out
    /// the schedule again. A structurally broken pair of builds alternated
    /// 10-minute and 30-minute park/spawn/paint round trips forever — roughly 48
    /// of them a day — and the stand-down that ends them was unreachable through
    /// the timed lane.
    ///
    /// …AND THE FIX FOR THAT MADE THE LANE UNBOUNDED, WHICH IS THIS TEST'S REAL
    /// SUBJECT. Widening the window to 4 h made the in-epoch cap reachable, and a
    /// spent cap ended an EPOCH: stand down 6 h, then start over. But the
    /// stand-down was deliberately LONGER than the replenish window, so the
    /// counter reset during it and every epoch began with a full budget — forever.
    /// Measured cost: three park/spawn/paint round trips per ~6.7 h, about ten a
    /// day, each with an "Update delayed" pill, on an artifact that was never
    /// going to hand off — while the constant's own prose claimed it "converges to
    /// manual-only quickly". This walks the real chronology and pins the number of
    /// round trips at a FINITE one.
    ///
    /// SCOPE OF THE CLAIM, stated honestly: the counter is still REPLENISHING by
    /// design, so a gap longer than [`PHYSICAL_RETRY_BUDGET_REPLENISH`] (12 h with
    /// no physical failure at all for these exact bytes) still forgives the
    /// artifact and starts the schedule over. What must never happen — and is what
    /// the last block rules out — is the schedule producing such a gap ITSELF.
    #[test]
    fn the_physical_failure_budget_converges_under_its_own_schedule() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let dmg = [0xab_u8; 32];

        // THE REAL METHOD, in order. Consecutive calls here are microseconds
        // apart, which models production faithfully precisely because of the last
        // block below: the longest gap the schedule can produce (one stand-down)
        // is well inside the replenish window, so no real chronology resets the
        // counter either.
        let mut verdicts = Vec::new();
        for _ in 0..64 {
            let verdict = app.spend_physical_failure_budget(
                build,
                dmg,
                false,
                PhysicalFailureShape::Transient,
            );
            verdicts.push(verdict);
            if matches!(verdict, PhysicalFailureSchedule::Converged { .. }) {
                break;
            }
        }
        let now = std::time::Instant::now();
        let shape = verdicts
            .iter()
            .map(|verdict| match verdict {
                PhysicalFailureSchedule::Retry(at) => {
                    let secs = at.saturating_duration_since(now).as_secs();
                    // Real `Instant`s, so name the SCHEDULE each delay came from
                    // rather than asserting to the second.
                    if secs >= 1000 {
                        "retry-1800"
                    } else {
                        "retry-600"
                    }
                }
                PhysicalFailureSchedule::StandDown(_) => "stand-down",
                PhysicalFailureSchedule::Converged { .. } => "converged",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            vec![
                "retry-600",
                "retry-1800",
                "stand-down",
                "retry-600",
                "retry-1800",
                "stand-down",
                "retry-600",
                "retry-1800",
                "converged",
            ],
            "three whole epochs, then the lane is done — no fourth epoch, ever"
        );
        assert_eq!(
            verdicts.len(),
            usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS),
            "the artifact costs exactly {PHYSICAL_FAILURE_LIFETIME_ATTEMPTS} \
             park/spawn/paint round trips in total"
        );
        // …and it STAYS converged. A duplicate or reordered completion must not
        // mint a fourth epoch out of the saturating counter: every later failure
        // is convergence again, never a fresh in-epoch rung.
        //
        // CONVERGED IS NOT FOREVER FOR THIS SHAPE, THOUGH (the 2026-09-22/23
        // update audit, plan P1-1(d)). Nine timed-out handoffs are a fact about
        // this machine's day, not its bytes, so the transient lane re-samples
        // quietly one epoch cooldown out — every time, at most once per 6 h —
        // instead of a latch that never lapses. Before this, the deadline was
        // `None` and a busy day kept a healthy build off the machine until a
        // relaunch.
        for extra in 0..4 {
            let verdict = app.spend_physical_failure_budget(
                build,
                dmg,
                false,
                PhysicalFailureShape::Transient,
            );
            let PhysicalFailureSchedule::Converged {
                resample_at: Some(resample_at),
            } = verdict
            else {
                panic!(
                    "late completion {extra} must stay converged and re-sample, got {verdict:?}"
                );
            };
            let wait = resample_at.saturating_duration_since(std::time::Instant::now());
            assert!(
                wait > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60)
                    && wait <= PHYSICAL_FAILURE_EPOCH_COOLDOWN,
                "the re-sample is one epoch cooldown out, never sooner: {wait:?}"
            );
        }
        // A DIFFERENT artifact is a different question and starts clean.
        assert!(matches!(
            app.spend_physical_failure_budget(
                build,
                [0xcd_u8; 32],
                false,
                PhysicalFailureShape::Transient
            ),
            PhysicalFailureSchedule::Retry(_)
        ));

        // THE CHRONOLOGY THE VERDICTS PRESCRIBE, as wall time. Two numbers a
        // future tweak has to look at: how long the whole lane lasts, and the
        // longest gap it contains.
        let epoch = 600 + 1800 + PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs();
        let lifetime = u64::from(MAX_PHYSICAL_FAILURE_EPOCHS) * epoch
            - PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs();
        assert!(
            (12 * 60 * 60..=24 * 60 * 60).contains(&lifetime),
            "the lane must span most of a day — long enough that its last epoch \
             samples a genuinely different machine, short enough that a user whose \
             morning was bad still gets the update that evening ({lifetime}s)"
        );
        // THE PROPERTY THAT MAKES THE EPOCH TALLY REAL, and the one the previous
        // design had backwards: the counter must outlive the longest gap the
        // schedule itself produces, or the epochs cannot be counted and the cap is
        // unreachable. Decided at compile time so a future tweak to either number
        // has to face it.
        const {
            assert!(
                PHYSICAL_RETRY_BUDGET_REPLENISH.as_secs()
                    > PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs(),
                "the replenish window must outlast the stand-down between epochs, \
                 or the counter forgives itself between them and the lane never \
                 converges"
            )
        };
    }

    /// Name the SCHEDULE a latch deadline came from rather than asserting to the
    /// second: these are real `Instant`s, and the rungs (600 s, 1800 s, a 6 h
    /// stand-down, or no deadline at all) are orders of magnitude apart.
    fn latch_rung(retry_at: Option<std::time::Instant>) -> &'static str {
        match retry_at.map(|at| at.saturating_duration_since(std::time::Instant::now())) {
            None => "no-retry",
            Some(wait) if wait <= std::time::Duration::from_secs(600) => "retry-600",
            Some(wait) if wait <= std::time::Duration::from_secs(1800) => "retry-1800",
            Some(_) => "stand-down",
        }
    }

    /// A FORK-LANE PARK MISS IS BOOKED WHERE `update status` READS IT (the
    /// 2026-09-24 review). The miss froze the screen before it came back, yet it
    /// returned as a bare `Deferred`, which `try_pending_native_auto_apply`
    /// passes over without surfacing or booking anything: a pool whose capture
    /// always overran the widest rung retried every fifteen minutes forever with
    /// no `apply_refusal=`, and the overdue notice said no attempt had been
    /// recorded. It is now booked as a REFUSAL (the launched lane's
    /// `ActivityRevoked` always was) — never a failure: `failing_applies` does
    /// not move, and the spaced activity retry is armed as before. A person's
    /// apply is told it failed and arms nothing.
    ///
    /// RED on the code before the fix: the automatic arm recorded nothing, so
    /// `last_refusal` still held the marker.
    #[test]
    fn a_fork_lane_park_miss_is_booked_as_a_refusal_not_a_failure() {
        use crate::native_updater_service::ApplyMode;
        // The ledger is one file per test process: hold it, so a sibling
        // test's failure cannot move `failing_applies` under the comparison.
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        let build = current + 6_127;
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"e7".repeat(32),
        );
        const BEFORE: &str = "test marker before the fork-lane park miss";
        aterm_update::record_apply_refusal(current, BEFORE);
        let before = aterm_update::status(current).unwrap();
        let miss = format!(
            "a PTY reader missed the 250 ms handoff park deadline (fork-lane test {build})"
        );

        let outcome =
            app.fork_park_miss_outcome(&ticket, ApplyMode::AutomaticPastGrace, miss.clone());
        assert!(
            matches!(&outcome, UpdateOutcome::Deferred { reason } if *reason == miss),
            "the automatic lane defers: {outcome:?}"
        );
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "the spaced activity retry is armed"
        );
        let report = aterm_update::apply_lane_report(current).unwrap();
        assert!(
            report.last_refusal.contains(&miss),
            "the miss is booked where `update status` reads it: {:?}",
            report.last_refusal
        );
        assert_eq!(
            aterm_update::status(current).unwrap().failing_applies,
            before.failing_applies,
            "a missed stopwatch is not a failed apply"
        );
        assert!(
            !report.last_failure.contains(&miss),
            "and it is never booked in the failure slot: {:?}",
            report.last_failure
        );

        // A person's apply: a failure they are told about, nothing armed.
        let mut person = App::headless_for_test();
        let outcome = person.fork_park_miss_outcome(&ticket, ApplyMode::Immediate, miss.clone());
        assert!(
            matches!(outcome, UpdateOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert!(person.auto_apply_intent.is_none());
    }

    /// A CAPTURE REFUSAL GETS ONE VERDICT PER ASKER, WHICHEVER LANE CARRIED
    /// IT. The launched lane classifies a person's press `Manual` and answers
    /// `Failed`; the fork lane used to answer every caller `CaptureRefused`, so
    /// Settings told the person "retries when it changes" over a press nothing
    /// re-arms, and with `[update] auto_apply = false` the stopped-lane row
    /// contradicted it beside that line. Table-driven from the launched lane's
    /// own classifier, so the two cannot drift apart again.
    #[test]
    fn a_capture_refusal_answers_by_who_asked_on_both_lanes() {
        use crate::native_updater_service::ApplyMode;
        for mode in [
            ApplyMode::Automatic,
            ApplyMode::AutomaticPastGrace,
            ApplyMode::Immediate,
            ApplyMode::CleanQuit,
        ] {
            let fork = fork_capture_refusal_outcome(mode, "session 3".to_string());
            match HandoffFailureLane::classify(
                mode,
                crate::UpdateHandoffOutcome::CaptureRefused,
                crate::ChildDeathEvidence::Unobserved,
                false,
            ) {
                HandoffFailureLane::Refused => assert!(
                    matches!(fork, UpdateOutcome::CaptureRefused { .. }),
                    "{mode:?}: {fork:?}"
                ),
                HandoffFailureLane::Manual => assert!(
                    matches!(fork, UpdateOutcome::Failed { .. }),
                    "{mode:?}: {fork:?}"
                ),
                other => panic!("{mode:?}: a capture refusal classified {other:?}"),
            }
        }
    }

    /// A STRUCTURAL LATCH, SIDE BY SIDE, AS ITS DOC NOW STATES IT (the
    /// 2026-09-24 review corrected "a strictly newer build moves it", which is
    /// only true of one side). On a DOWNLOAD the latch names those bytes and a
    /// newer build arms on its own. On the installed ACTIVATION — where every
    /// launched-lane failure lands — the activation outranks a newer download
    /// while the bundle is newer than this process, so the latched build is the
    /// only stage there is and the newer release is never offered: only the
    /// Version menu or a relaunch moves it. A pin of the stated behaviour, so
    /// the doc and the code cannot drift apart again.
    #[test]
    fn a_structural_latch_yields_to_a_newer_download_but_not_on_the_activation() {
        // The download side.
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: current + 1,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: None,
        });
        assert!(
            !app.arm_native_auto_apply(current + 1, &"ab".repeat(32)),
            "the latched bytes stay latched"
        );
        assert!(
            app.arm_native_auto_apply(current + 2, &"cd".repeat(32)),
            "a strictly newer download arms past a download latch"
        );

        // The activation side: bundle 11 installed under a running 10, a newer
        // download 12 staged, and the activation of 11 latched.
        let mut app = App::headless_for_test();
        let installed = installed_update(11);
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            1,
            1,
            Some(status(Some(12), 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, 11, &installed.commit);
        let activation =
            crate::native_updater_service::installed_activation_digest(11, &installed.commit);
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: 11,
            dmg_sha256: decode_dmg_sha256(&activation).expect("an activation digest"),
            activation: true,
            retry_at: None,
        });
        assert!(
            !app.arm_native_auto_apply(11, &activation),
            "the activation stays latched"
        );
        let _ = app.reconcile_native_update_facts(reconcile_facts_with_installed(
            2,
            2,
            Some(status(Some(12), 0)),
            Some(installed.clone()),
        ));
        assert_activation_stage(&app, 11, &installed.commit);
        assert!(
            app.auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_none()),
            "the newer download neither replaces the stage nor releases the latch"
        );
    }

    /// A TRANSIENT AND A STRUCTURAL FAILURE ARE NOT THE SAME EVENT, AND THE BUDGET
    /// USED TO CHARGE THEM ALIKE.
    ///
    /// The worker classifies its four physical outcomes precisely and the
    /// completion path then flattened them into one lane, so `AdoptionMismatch` —
    /// a parent and a candidate that cannot agree on an adoption proof, which is a
    /// property of the two IMAGES — rode the schedule written for a missed 15 s
    /// deadline: nine park/spawn/paint round trips across ~14 hours, eight of them
    /// re-learning what the first one had already established, and a promise on
    /// screen that the update "retries on its own" for most of a day.
    ///
    /// Both lanes are driven here through the REAL completion path
    /// (`abort_reaped_native_apply_before_reconcile`, which every returned overlap
    /// failure takes) in the SAME `App`, and the CONTRAST is the assertion: either
    /// half alone passes with the shape discarded.
    #[test]
    fn a_structural_handoff_failure_converges_where_a_transient_one_keeps_its_epochs() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let structural = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        assert!(
            app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "PRECONDITION: automatic apply is enabled and armable for these bytes, \
             so the `arm` REFUSAL asserted after convergence is the latch talking \
             and not a disabled lane"
        );

        // THE STRUCTURAL LANE: one confirming retry — because this seam cannot
        // separate a candidate that fails `codesign` from a screen-carry digest
        // that lost a race with a resize — and then the lane is done with these
        // bytes.
        let mut structural_rungs = Vec::new();
        for _ in 0..3 {
            structural.make_current_apply_for_test(&mut app.native_updater_service);
            app.abort_reaped_native_apply_before_reconcile(
                &structural,
                "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            );
            structural_rungs.push(latch_rung(
                app.auto_apply_manual_only
                    .expect("a returned physical failure always latches manual-only")
                    .retry_at,
            ));
        }
        assert_eq!(
            structural_rungs,
            vec!["retry-600", "no-retry", "no-retry"],
            "a structural failure gets ONE confirmation and then converges; a \
             stand-down here would mean the lane is still re-sampling the machine \
             over a disagreement between two builds"
        );
        // AND CONVERGED MEANS CONVERGED, at the gate that decides whether the lane
        // ever attempts again: a deadline-less latch is what `arm` reads as
        // `SuppressManualOnly` until a strictly newer build ships or the app
        // relaunches — which is also why no LATER transient failure can hand these
        // bytes a fresh schedule. The automatic lane will not attempt them again,
        // and a person's attempt charges nothing.
        assert!(
            !app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "the converged artifact must not re-arm automatic apply"
        );
        assert!(app.auto_apply_intent.is_none());

        // THE TRANSIENT LANE, SAME `App`, DIFFERENT BYTES: three failures in and it
        // is still going, on the schedule whose generosity is bought by the claim
        // that the machine's next moment may differ — which is true of `TimedOut`
        // and is exactly what a structural failure cannot claim.
        let transient = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"cd".repeat(32),
        );
        let mut transient_rungs = Vec::new();
        for _ in 0..3 {
            transient.make_current_apply_for_test(&mut app.native_updater_service);
            app.abort_reaped_native_apply_before_reconcile(
                &transient,
                "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
            );
            transient_rungs.push(latch_rung(
                app.auto_apply_manual_only
                    .expect("a returned physical failure always latches manual-only")
                    .retry_at,
            ));
        }
        assert_eq!(
            transient_rungs,
            vec!["retry-600", "retry-1800", "stand-down"],
            "the transient lane must keep its full epoch schedule — collapsing it \
             onto the structural budget would strand a staged build on one cold \
             page cache, which is the regression the epochs exist for"
        );
    }

    /// The shared counter is deliberately shape-BLIND: it counts physical failures
    /// for these exact bytes, which neither lane disputes. So evidence carries
    /// across the classification in the direction that matters — an artifact that
    /// has already cost the lane round trips does not buy a fresh pair of them by
    /// failing in a new way.
    #[test]
    fn a_structural_failure_inherits_the_round_trips_the_artifact_already_cost() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let dmg = [0xab_u8; 32];

        assert!(matches!(
            app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Transient),
            PhysicalFailureSchedule::Retry(_)
        ));
        assert!(matches!(
            app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Transient),
            PhysicalFailureSchedule::Retry(_)
        ));
        assert_eq!(
            app.spend_physical_failure_budget(build, dmg, false, PhysicalFailureShape::Structural),
            PhysicalFailureSchedule::Converged { resample_at: None },
            "two round trips are already spent on these bytes and the structural \
             budget is {STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS}; a `Retry` here would \
             mean the structural verdict RESET the artifact's history"
        );
    }

    /// The physical-failure budget must survive a latch LAPSE, or "two tries" is
    /// an unbounded ten-minute loop.
    ///
    /// This is the trap the fix walked into: the obvious place to keep the count
    /// is `auto_apply_manual_only`, but that struct is cleared the moment the
    /// latch lapses, and `AutoApplyIntent::attempts` resets to 0 when a fresh
    /// intent is armed. Only a counter that outlives BOTH converges. Guard it
    /// directly: lapsing must not restore budget.
    #[test]
    fn a_lapsing_physical_latch_does_not_replenish_its_own_budget() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        app.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles: MAX_PHYSICAL_FAILURE_CYCLES,
            last_attempt: std::time::Instant::now(),
        });
        // An already-expired latch, i.e. one that lapses on this very call.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        });

        assert!(app.lapse_expired_auto_apply_manual_only(), "latch lapsed");
        assert!(app.auto_apply_manual_only.is_none());
        let retained = app
            .auto_apply_physical_retry
            .expect("the physical budget must OUTLIVE the latch it gated");
        assert_eq!(
            retained.cycles, MAX_PHYSICAL_FAILURE_CYCLES,
            "lapsing must not hand back spent physical budget"
        );
        assert_eq!(
            automatic_retry_delay(retained.cycles, AutomaticRetryKind::PhysicalFailure),
            None,
            "a spent budget stays spent across a lapse"
        );
    }

    /// REGRESSION: an activity-revoked overlap must never retire automatic
    /// apply for the life of the process.
    ///
    /// This drives the REAL completion lane —
    /// `abort_reaped_native_apply_before_reconcile`, the one every automatic
    /// overlap failure actually takes — rather than the policy helpers beneath
    /// it. That distinction is the whole point. The bounded retry schedule sat
    /// behind a sibling branch that requires the handoff completion to carry
    /// worker disk facts, and no completion has ever carried them (every
    /// construction site sets `reconcile: None`), so the schedule was dead code
    /// whose only callers were unit tests. Meanwhile this lane stamped a
    /// manual-only latch with `retry_at: None`, and
    /// `lapse_expired_auto_apply_manual_only` requires a deadline — so one
    /// unlucky moment switched automatic updates off until the app restarted,
    /// with a fully green suite.
    #[test]
    fn an_activity_revoked_completion_never_latches_automatic_apply_forever() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );
        let armed_at = std::time::Instant::now() - std::time::Duration::from_secs(300);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build: 77,
            armed_at,
            announced: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
        });

        // Well past the ladder's rungs: EVERY completion re-arms a live intent
        // on the spaced schedule, no cycle latches manual-only, and the ladder's
        // anchor is untouched — activity delays the landing, it never restarts
        // the clock or ends the lane.
        for cycle in 0..(usize::from(ACTIVITY_REVOKED_LADDER_RUNGS) + 4) {
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended ActivityRevoked".to_string(),
                HandoffFailureLane::ActivityRevoked,
            );
            assert!(
                app.auto_apply_intent.is_some(),
                "cycle {cycle}: an activity-revoked completion must leave a live retry intent"
            );
            assert!(
                app.auto_apply_manual_only.is_none(),
                "cycle {cycle}: activity never latches manual-only"
            );
            assert_eq!(
                app.auto_apply_ladder.map(|ladder| ladder.armed_at),
                Some(armed_at),
                "cycle {cycle}: the ladder's anchor survives a revocation"
            );
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(ACTIVITY_REVOKED_LADDER_RUNGS + 4),
            "the spacing keeps counting past the last rung"
        );
    }

    /// THE SECOND FINDING OF THE 2026-09-21 LADDER AUDIT, as a dead mutant: a
    /// physical-failure latch lapsing used to clear the ladder's anchor, so one
    /// transient dial timeout cost its 600 s latch PLUS a fresh ladder — on a
    /// never-quiet terminal (then with a fifteen-minute ladder), admitted at
    /// KeysOnly (~300 s), failed at ~330 s, latched to ~930 s, then refused
    /// again until a NEW KeysOnly at ~1230 s. The bound is per artifact and
    /// counts from the first arming; the lapse re-arms the intent and resumes
    /// the ladder where the clock is.
    ///
    /// Driven through the real completion lane and the real re-arm (the Refresh
    /// reconcile's `arm_native_auto_apply`, which lapses the latch first).
    #[test]
    fn a_physical_latch_lapse_resumes_the_ladder_where_the_clock_left_it() {
        use crate::native_update_auto_intent::{ApplyPhase, LANDS_WITHIN};
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let digest = "ab".repeat(32);
        assert!(
            app.arm_native_auto_apply(build, &digest),
            "PRECONDITION: automatic apply is enabled and armable for these bytes"
        );
        // The incident's machine: admitted at KeysOnly.
        let now = std::time::Instant::now();
        let armed_at = now
            - crate::native_update_auto_intent::PREFER_IDLE_WINDOW
            - crate::native_update_auto_intent::PREFER_OUTPUT_GAP_WINDOW
            - std::time::Duration::from_secs(1);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build,
            armed_at,
            announced: ApplyPhase::KeysOnly,
        });
        assert_eq!(app.automatic_apply_phase(now), ApplyPhase::KeysOnly);

        // One transient physical failure (the rendezvous dial never arrived).
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &digest,
        );
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );
        let latch = app
            .auto_apply_manual_only
            .expect("a returned physical failure latches manual-only");
        let wait = latch
            .retry_at
            .expect("a first transient failure schedules a retry")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            wait > std::time::Duration::from_secs(590)
                && wait <= std::time::Duration::from_secs(600),
            "the physical schedule's first rung is 600 s, got {wait:?}"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "the intent is dropped under the latch"
        );
        assert_eq!(
            app.auto_apply_ladder.map(|ladder| ladder.armed_at),
            Some(armed_at),
            "a physical failure does not touch the anchor"
        );

        // The latch lapses. The wall clock is now past the bound measured from
        // the FIRST arming (the 600 s latch alone outlasts the ladder): age the
        // anchor with it and expire the deadline, then take the re-arm the
        // Refresh reconcile takes.
        let now = std::time::Instant::now();
        let armed_at = now - LANDS_WITHIN - std::time::Duration::from_secs(30);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build,
            armed_at,
            announced: ApplyPhase::KeysOnly,
        });
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(now - std::time::Duration::from_secs(1)),
            ..latch
        });
        assert!(
            app.arm_native_auto_apply(build, &digest),
            "the lapsed latch re-arms the intent for the same bytes"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "the latch is released"
        );
        assert!(app.auto_apply_intent.is_some(), "a live intent again");
        assert_eq!(
            app.auto_apply_ladder.map(|ladder| ladder.armed_at),
            Some(armed_at),
            "THE MUTANT: a lapse that clears the anchor starts a second ladder for \
             the same artifact"
        );
        assert_eq!(
            app.automatic_apply_phase(now),
            ApplyPhase::Land,
            "past the bound since the first arming the lane lands: the next poll \
             attempts whatever the terminal is doing, not a fresh PreferIdle"
        );
        assert_eq!(
            app.auto_apply_ladder.map(|ladder| ladder.announced),
            Some(ApplyPhase::KeysOnly),
            "the announced phase rides with the anchor, so the log carries one line \
             per phase change and never re-announces an earlier phase"
        );
    }

    /// The counterpart: a GENUINE failure (not activity) is latched manual-only
    /// and takes the STRICT budget rather than the activity one — but it is still
    /// scheduled to come back, because "the handoff did not land" is not the same
    /// claim as "these bytes are broken". That holds for BOTH physical shapes: the
    /// structural one converges after its confirmation, not on the failure that
    /// first revealed it, so the first rung is the same ten minutes either way.
    #[test]
    fn a_genuine_failure_completion_latches_manual_only_on_the_strict_budget() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            78,
            "0123456789abcdef0123456789abcdef01234567",
            &"cd".repeat(32),
        );
        ticket.make_current_apply_for_test(&mut app.native_updater_service);

        app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );

        let manual = app
            .auto_apply_manual_only
            .expect("a genuine failure latches manual-only");
        let wait = manual
            .retry_at
            .expect(
                "the FIRST physical failure must always schedule a comeback; \
                 `retry_at: None` belongs to convergence, nine failures away",
            )
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            wait > std::time::Duration::from_secs(500),
            "a genuine failure waits its own SLOW schedule, never the activity \
             lane's fast one, got {wait:?}"
        );
        assert!(
            app.auto_overlap_retry.is_none(),
            "a genuine failure must not touch the ACTIVITY budget"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "a genuine failure must not leave an automatic intent armed"
        );
    }

    /// The App-side budget consumer: each activity-revoked completion for one
    /// exact artifact re-arms `auto_apply_intent` (clearing any manual-only
    /// latch) and consumes exactly one cycle; the fourth revocation returns
    /// `None` so the completion path falls back to the sticky manual latch.
    /// A different artifact owns a fresh budget by construction.
    /// THE FLIP THE LADDER USED TO FORGET. A candidate that fails after booting
    /// has already swapped the installed bundle — the boot apply swaps and
    /// re-execs BEFORE it can prove readiness — so the retry for the SAME
    /// logical update arrives as an installed-bundle ACTIVATION, whose digest is
    /// a different 32 bytes. Keyed on the bytes, the ladder read `cycles == 0`
    /// and started over: the reported "retry in 2s", twice.
    ///
    /// The budget must count attempts at the LOGICAL update, so the schedule
    /// continues across the flip and still converges in
    /// `MAX_ACTIVITY_REVOKED_CYCLES` attempts total.
    #[test]
    fn the_activation_of_a_failed_download_continues_its_ladder_instead_of_refilling_it() {
        const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
        let mut app = App::headless_for_test();
        let download = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            COMMIT,
            &"ab".repeat(32),
        );
        let activation = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            COMMIT,
            &crate::native_updater_service::installed_activation_digest(77, COMMIT),
        );
        assert!(
            activation.is_installed_activation() && !download.is_installed_activation(),
            "the fixture must actually straddle the swap"
        );

        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&download),
            Some(std::time::Duration::from_secs(2)),
            "rung 1, as a download"
        );
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&download),
            Some(std::time::Duration::from_secs(5)),
            "rung 2, still a download"
        );

        // The candidate swapped the bundle before it failed. Same update, new
        // artifact identity — the ladder CONTINUES.
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&activation),
            Some(std::time::Duration::from_secs(10)),
            "rung 3 across the flip — not a refilled `2s`"
        );

        for (cycle, want) in [
            Some(std::time::Duration::from_secs(20)),
            Some(std::time::Duration::from_secs(30)),
            Some(std::time::Duration::from_secs(30)),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                app.arm_activity_revoked_overlap_retry(&activation),
                want,
                "post-flip cycle {cycle}"
            );
        }
        // What is pinned here is that the rungs are counted ACROSS the flip —
        // six attempts total, not five per artifact identity — and that the
        // last rung repeats rather than ending anything.
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(ACTIVITY_REVOKED_LADDER_RUNGS + 1),
            "six attempts total, not five per artifact identity"
        );
        assert!(
            app.auto_overlap_retry.is_some_and(|retry| retry.activation),
            "and the record now names the side of the swap it was last spent on"
        );
    }

    /// The physical ladder carries the same key and the same flip.
    #[test]
    fn a_physical_failure_ladder_survives_the_download_to_activation_flip() {
        let mut app = App::headless_for_test();
        let build = 77;
        let dmg = [0xab_u8; 32];
        let activation_dmg = [0xcd_u8; 32];

        for cycle in 0..PHYSICAL_FAILURES_PER_EPOCH {
            let _ = app.spend_physical_failure_budget(
                build,
                dmg,
                false,
                PhysicalFailureShape::Transient,
            );
            assert_eq!(
                app.auto_apply_physical_retry.map(|r| r.cycles),
                Some(cycle + 1),
                "download cycle {cycle}"
            );
        }

        // Same logical update, now an activation: the count CONTINUES.
        let _ = app.spend_physical_failure_budget(
            build,
            activation_dmg,
            true,
            PhysicalFailureShape::Transient,
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|r| r.cycles),
            Some(PHYSICAL_FAILURES_PER_EPOCH + 1),
            "the flip must not refill the lifetime counter"
        );
    }

    #[test]
    fn overlap_retry_spacing_rearms_intent_per_artifact_and_saturates() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );

        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: 77,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: None,
        });
        let expected = [
            Some(std::time::Duration::from_secs(2)),
            Some(std::time::Duration::from_secs(5)),
            Some(std::time::Duration::from_secs(10)),
            Some(std::time::Duration::from_secs(20)),
            Some(std::time::Duration::from_secs(30)),
            Some(std::time::Duration::from_secs(30)),
        ];
        for (cycle, want) in expected.into_iter().enumerate() {
            let got = app.arm_activity_revoked_overlap_retry(&ticket);
            assert_eq!(got, want, "cycle {cycle}");
            let intent = app.auto_apply_intent.expect("intent re-armed");
            assert_eq!(intent.build, 77);
            assert_eq!(intent.dmg_sha256, [0xab; 32]);
            assert!(
                app.auto_apply_manual_only.is_none(),
                "a live retry clears the manual-only latch"
            );
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(ACTIVITY_REVOKED_LADDER_RUNGS + 1),
            "duplicate completions cannot restart the spacing"
        );

        // A different artifact (same build, new bytes) starts a fresh budget.
        let other = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"cd".repeat(32),
        );
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&other),
            Some(std::time::Duration::from_secs(2))
        );
    }

    /// Seamless seam 5 (recoverable degradation). A latch with a deadline must
    /// LAPSE, restoring both automatic apply and the artifact's activity budget;
    /// a deadline-less one must not.
    ///
    /// The deadline-less case has exactly two producers now, and neither is "one
    /// unlucky moment": the policy/outcome mismatch fail-safe, which no path is
    /// supposed to reach, and physical-lane CONVERGENCE
    /// ([`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] failures across
    /// [`MAX_PHYSICAL_FAILURE_EPOCHS`] epochs and ~14 hours). Every other cause — a
    /// busy terminal, any single physical handoff failure — carries a deadline,
    /// because three unlucky moments used to retire automatic apply until the next
    /// relaunch, which is precisely the "staged, applies on next launch" state seen
    /// in the field. The mechanism is asserted here regardless of who mints it: the
    /// lapse reads `retry_at` and nothing else.
    #[test]
    fn a_deadlined_manual_only_latch_lapses_but_the_fail_safe_one_does_not() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;

        // FAIL-SAFE latch: no deadline, never lapses, budget untouched.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: None,
        });
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(app.auto_apply_manual_only.is_some());

        // ACTIVITY-shaped, still within its window: holds.
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
            last_attempt: std::time::Instant::now(),
        });
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
        });
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(app.auto_apply_manual_only.is_some());

        // ACTIVITY-shaped, deadline passed: lapses, AND the artifact's retry
        // budget starts over so the next attempt is not instantly exhausted.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
        });
        assert!(app.lapse_expired_auto_apply_manual_only());
        assert!(app.auto_apply_manual_only.is_none());
        assert!(
            app.auto_overlap_retry.is_none(),
            "a lapsed latch replenishes the activity retry budget"
        );
        // …and automatic apply is armable again for the same artifact.
        assert!(app.arm_native_auto_apply(build, &"ab".repeat(32)));
    }

    const PREFLIGHT_TEST_COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    /// Drive the REAL check reducer to a Staged phase for `build`, exactly as a
    /// completed updater worker does, so the automatic intent under test is the
    /// one production arms — not a hand-built struct.
    ///
    /// The pre-park verification verdict is pre-seeded so `arm_native_auto_apply`
    /// short-circuits its worker thread: these tests are about retry policy, not
    /// about running `codesign` from a unit test.
    ///
    /// AND THE SEEDED VERDICT IS `passed: true`, WHICH IS NOT COSMETIC. A cached
    /// REFUSAL is a short-circuit: `start_unix_update_handoff` turns it into
    /// "the staged update failed verification; the terminal was left untouched"
    /// before a single reader parks. Every retry-policy test built on a
    /// `passed: false` fixture was therefore describing an artifact production
    /// declines outright, while claiming to describe the schedule that carries a
    /// HEALTHY artifact to the physical gate. The two are only indistinguishable
    /// because a headless `App` is blocked one gate earlier
    /// (`native_update_admission` has no seamless lane without a proxy), which is
    /// precisely why the premise has to be stated in the fixture rather than
    /// inferred from a green suite.
    fn stage_one_build_for_test(app: &mut App, build: u64) {
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: std::time::Instant::now(),
                passed: true,
                reason: None,
            });
        let current_build = app.native_updater_service.snapshot().current_build;
        assert!(
            build > current_build,
            "the staged build must supersede the running one or nothing is armable"
        );
        let CheckStart::Start(ticket) = app.native_updater_service.request_check() else {
            panic!("a fresh service must start exactly one check");
        };
        // The REDUCER is driven for real, and so is `arm_native_auto_apply`
        // below; only the presentation fan-out that
        // `App::finish_native_update_check` wraps around them is skipped
        // (window repaints, tab-strip rebuilds, palette refresh). None of that
        // is the retry policy under test, and everything the policy reads —
        // phase, staged identity, generation — comes out of the same reducer
        // either way.
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
                    staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
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
            "PRECONDITION: the check must actually reduce, or nothing is staged"
        );
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .expect("PRECONDITION: the reduced check staged the build");
        assert_eq!(staged.build, build);
        assert!(
            app.arm_native_auto_apply(staged.build, &staged.dmg_sha256),
            "PRECONDITION: a strictly newer staged build arms automatic intent"
        );
    }

    /// The durable marker a download stage of `build` leaves on disk, as the
    /// reconcile tests below feed it back.
    fn durable_download_status(app: &App, build: u64) -> DurableUpdateStatus {
        DurableUpdateStatus {
            linux_host: false,
            linux: None,
            enabled: true,
            current_build: app.native_updater_service.snapshot().current_build,
            staged_build: Some(build),
            staged_version: Some(format!("1.0.{build}")),
            staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
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
        }
    }

    /// Take the staged download of `build` through a real automatic preflight
    /// to an `Applying` ticket, the state a returned handoff completes from.
    fn begin_automatic_apply_for_test(
        app: &mut App,
    ) -> crate::native_updater_service::ApplyAttemptTicket {
        let ApplyPreflightStart::Inspect(preflight) = app
            .native_updater_service
            .begin_apply_preflight(ApplyMode::AutomaticPastGrace)
        else {
            panic!("the stage must admit an apply preflight");
        };
        let ApplyDecision::Execute(command) = app
            .native_updater_service
            .finish_apply_preflight(preflight, ClosePreflight::Ready)
        else {
            panic!("a ready close preflight must authorize the replacement");
        };
        let attempt = command.attempt();
        command.execute(|| ());
        attempt
    }

    /// THE CONFIRMING RETRY REALLY WAITS TEN MINUTES (the 2026-09-22/23 update
    /// audit, plan P0-6).
    ///
    /// A STRUCTURAL failure earns ONE confirming retry, "ten minutes out"
    /// ([`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`]). On the launched lane that
    /// retry fired half a second later instead: the failed candidate had
    /// already boot-applied the bundle, so the next reconcile retired the
    /// download for the installed-bundle ACTIVATION of the same build — and the
    /// retire arm cleared the latch (`manual.build <= retired_build`), and the
    /// activation armed at +500 ms. The budget folds download and activation
    /// into one counter (`AutoOverlapRetry::covers`), so that second attempt
    /// converged the lane to a deadline-less manual-only latch 1.4 s after the
    /// first failure (0.91: failure 1790122251.440, re-arm .757, failure 2 at
    /// 1790122252.840), and the machine sat on the old build for 25 h. 0.87,
    /// 0.88 and 0.89 each LANDED once their 600 s latch lapsed — the retry this
    /// test guarantees.
    ///
    /// Driven through the shipping completion (`abort_reaped_native_apply_\
    /// before_reconcile`, the lane every returned handoff takes) and the
    /// shipping reconcile door, with the installed bundle already the target.
    #[test]
    fn a_structural_latch_survives_its_own_candidates_bundle_swap() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        let attempt = begin_automatic_apply_for_test(&mut app);
        app.auto_apply_intent = None;

        // Failure 1, STRUCTURAL: the confirming retry is ten minutes out.
        let _ = app.abort_reaped_native_apply_before_reconcile(
            &attempt,
            "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );
        let latched = app
            .auto_apply_manual_only
            .expect("a structural failure latches the automatic lane");
        assert_eq!(latched.build, build);
        assert_eq!(
            latch_rung(latched.retry_at),
            "retry-600",
            "the first structural failure buys one retry, ten minutes out"
        );

        // The candidate had already swapped the bundle: the next reconcile
        // sees the installed bundle IS the target build and retires the
        // download for the activation of the same logical update.
        app.finish_native_update_reconcile(
            NativeUpdateReconcilePurpose::Startup,
            reconcile_facts_with_installed(
                21,
                21,
                Some(durable_download_status(&app, build)),
                Some(InstalledUpdate {
                    build,
                    commit: PREFLIGHT_TEST_COMMIT.to_string(),
                    version: None,
                    receipt_build: Some(build),
                    receipt_dmg_sha256: Some("ab".repeat(32)),
                }),
            ),
        );
        assert_activation_stage(&app, build, PREFLIGHT_TEST_COMMIT);

        // THE LATCH SURVIVED, with the SAME deadline, re-keyed to the stage it
        // now guards — and it is what the arming door reads.
        let carried = app
            .auto_apply_manual_only
            .expect("the latch must survive its own candidate's bundle swap");
        assert_eq!(carried.build, build);
        assert_eq!(
            carried.retry_at, latched.retry_at,
            "the confirming retry keeps its ten minutes"
        );
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .expect("the activation is the stage on record");
        assert_eq!(
            Some(carried.dmg_sha256),
            decode_dmg_sha256(&staged.dmg_sha256),
            "re-keyed to the activation identity the surfaces compare against"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "the activation must NOT arm half a second after the failure"
        );
        assert!(
            !app.arm_native_auto_apply(build, &staged.dmg_sha256),
            "arm() answers SuppressManualOnly for the activation of the latched update"
        );
        assert!(app.auto_apply_intent.is_none());
    }

    /// A RETIRED DOWNLOAD TAKES ITS OLDER LATCHES WITH IT, CARRIES ITS OWN TO THE
    /// ACTIVATION, AND LEAVES A NEWER ONE ALONE.
    ///
    /// Three latches against the same retirement (the installed bundle is now the
    /// staged build, so the download stage retires for its activation):
    ///   * OLDER than the retired build — about bytes nothing offers any more:
    ///     cleared, "a latch that outlived its reason" being the shape of every bug
    ///     this lane has had;
    ///   * ON the retired build — the SAME logical update, whose own failed
    ///     candidate installed the bundle: re-keyed to the activation with its
    ///     deadline UNCHANGED (the 2026-09-22/23 update audit, plan P0-6). This
    ///     case used to be cleared too, and that is exactly what fired a
    ///     structural failure's "confirming retry, ten minutes out" half a second
    ///     later and converged 0.90 and 0.91 for a day;
    ///   * NEWER — a completion can land out of order, and a latch minted for a
    ///     newer artifact must not be handed back an automatic lane it just
    ///     latched off (the rule the sibling `Retired` arm in
    ///     `reconcile_returned_native_apply_with_facts` already follows).
    #[test]
    fn retiring_a_download_for_its_activation_carries_only_its_own_latch() {
        for offset in [-1_i64, 0, 1] {
            let mut app = App::headless_for_test();
            let build = app.native_updater_service.snapshot().current_build + 1;
            stage_one_build_for_test(&mut app, build);
            let latched_build = build.saturating_add_signed(offset);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
            app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                build: latched_build,
                dmg_sha256: [0xab; 32],
                activation: false,
                retry_at: Some(deadline),
            });

            // The survivor state a swapped-but-uncommitted handoff leaves behind:
            // the canonical bundle carries this build and its receipt names this
            // exact artifact, so the reducer retires the stage as installed.
            app.finish_native_update_reconcile(
                NativeUpdateReconcilePurpose::Startup,
                reconcile_facts_with_installed(
                    11,
                    11,
                    Some(durable_download_status(&app, build)),
                    Some(InstalledUpdate {
                        build,
                        commit: PREFLIGHT_TEST_COMMIT.to_string(),
                        version: None,
                        receipt_build: Some(build),
                        receipt_dmg_sha256: Some("ab".repeat(32)),
                    }),
                ),
            );

            // THE DOWNLOAD STAGE RETIRES AND THE ACTIVATION TAKES ITS PLACE (the
            // installed bundle is newer than the process): the stage on record is
            // now the activation of `build`, under the activation identity — not
            // the retired download's DMG digest.
            let staged = app
                .native_updater_service
                .snapshot()
                .staged
                .clone()
                .expect("PRECONDITION: the newer installed bundle became an activation stage");
            assert!(
                staged.build == build && staged.is_installed_activation(),
                "the stage on record is the activation, got {staged:?}"
            );
            let activation = decode_dmg_sha256(&staged.dmg_sha256).unwrap();
            match offset {
                -1 => assert!(
                    app.auto_apply_manual_only.is_none(),
                    "a latch on an older build retires with the download"
                ),
                0 => assert_eq!(
                    app.auto_apply_manual_only,
                    Some(crate::AutoApplyManualOnly {
                        build,
                        dmg_sha256: activation,
                        activation: true,
                        retry_at: Some(deadline),
                    }),
                    "the latch on the update itself is carried to its activation, \
                     deadline and all"
                ),
                _ => assert_eq!(
                    app.auto_apply_manual_only,
                    Some(crate::AutoApplyManualOnly {
                        build: latched_build,
                        dmg_sha256: [0xab; 32],
                        activation: false,
                        retry_at: Some(deadline),
                    }),
                    "a latch about a newer artifact is untouched"
                ),
            }
            assert_eq!(
                app.auto_apply_intent.is_some(),
                offset == -1,
                "the activation arms only where no latch holds it (offset {offset})"
            );
        }
    }

    /// Put the app in the exact state the field report describes: one Settings
    /// view holding an unsaved draft, parked in a BACKGROUND tab while the user
    /// works in the terminal tab. Returns the settings instance/view.
    ///
    /// THIS BLOCKER IS CHOSEN OVER `pending_restore` ON PURPOSE, and the choice is
    /// the whole point of the test that uses it. Both stop an update apply, but
    /// they are discovered in different places:
    ///   * `pending_restore` is found by `native_update_close_preflight`, a pure
    ///     counting function with NO user interface whatsoever;
    ///   * an unsaved Settings draft is found EARLIER, by
    ///     `prepare_all_native_shutdown` → `CloseReadiness::Blocked` →
    ///     `surface_native_close_recovery`, which switches the active tab, moves
    ///     keyboard focus, re-fronts the window and replaces the window overlay
    ///     with a Close Recovery palette.
    ///
    /// A test written against `pending_restore` therefore cannot observe a single
    /// one of those disturbances even when they are happening on every probe —
    /// which is exactly how the recurring focus hijack survived two reviews.
    fn park_a_settings_draft_in_a_background_tab(
        app: &mut App,
    ) -> (crate::tab_model::AppInstanceId, crate::tab_model::ViewId) {
        let wid = WindowId(0);
        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::Home),
            "PRECONDITION: the Settings tab opens"
        );
        let (instance, view) = app
            .active_native_view(wid)
            .expect("PRECONDITION: the new Settings tab is the active native view");
        for event in [
            crate::native_app::AppEvent::FocusChanged(Some(crate::native_ui::UiKey::new(format!(
                "settings/control/{}",
                crate::prefs::EDIT_FONT_FAMILY
            )))),
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::SelectAll),
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "Update Probe Mono".to_string(),
            )),
        ] {
            app.dispatch_native_view_event(wid, view, event)
                .expect("PRECONDITION: the Settings draft edit dispatches");
        }
        // The reducer's own verdict, read directly. `closable == false` is what
        // `prepare_close` turns into `CloseReadiness::Blocked { recovery }`, so
        // this is the precondition that the probe below really does reach the
        // recovery-surfacing branch rather than some UI-less blocker.
        assert!(
            !app.native_runtime
                .presentation(instance, view)
                .expect("PRECONDITION: the Settings view still presents")
                .closable,
            "PRECONDITION: the draft makes the Settings view refuse a close, which is \
             the only verdict that surfaces Close Recovery"
        );
        // Back to the terminal tab: the user is working somewhere else. Every
        // later assertion that this stayed true is an assertion that no probe
        // dragged them into Settings.
        app.switch_tab_in(wid, 0);
        assert!(
            app.active_native_view(wid).is_none(),
            "PRECONDITION: the user is on the terminal tab, not on Settings"
        );
        (instance, view)
    }

    /// Clear the draft through the app's own recovery command, the way a real
    /// user would: walk over to the Settings tab, discard, walk back. The trip
    /// back matters — every later "nothing moved" assertion is only meaningful if
    /// the user really is somewhere else again.
    fn discard_settings_drafts(app: &mut App, view: crate::tab_model::ViewId) {
        let wid = WindowId(0);
        app.switch_tab_in(wid, 1);
        assert!(
            app.active_native_view(wid)
                .is_some_and(|(_, active)| active == view),
            "the Settings tab is where the draft lives"
        );
        for _ in 0..2 {
            app.dispatch_native_view_event(
                WindowId(0),
                view,
                crate::native_app::AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new("settings/drafts/discard-all"),
                    value: None,
                }),
            )
            .expect("the discard-all recovery command dispatches");
        }
        assert!(
            app.native_runtime
                .presentation(
                    app.active_native_view(wid).expect("still on Settings").0,
                    view
                )
                .expect("the Settings view still presents")
                .closable,
            "the discard really cleared the blocker — otherwise the 'way back' \
             below would be testing the blocked path all over again"
        );
        app.switch_tab_in(wid, 0);
        assert!(
            app.active_native_view(wid).is_none(),
            "the user went back to their terminal tab"
        );
    }

    /// Nothing on screen moved. Called after every automatic probe.
    fn assert_no_probe_disturbance(app: &App, wid: WindowId, active_tab: crate::tab_model::TabId) {
        assert!(
            app.windows[&wid].palette().is_none(),
            "a background update probe must never open a palette over the user's work"
        );
        assert!(
            app.windows[&wid].overlay.is_none(),
            "a background update probe must never install a window overlay"
        );
        assert_eq!(
            app.windows[&wid].tab_set.active_id(),
            Some(active_tab),
            "a background update probe must never switch the active tab"
        );
        assert!(
            app.active_native_view(wid).is_none(),
            "a background update probe must never move focus onto the blocking \
             native view"
        );
        assert_eq!(
            app.frontmost_window,
            Some(wid),
            "a background update probe must never change the frontmost window"
        );
    }

    /// Make the retained intent eligible RIGHT NOW and put its ladder at
    /// `Land`, so the attempt takes the `AutomaticPastGrace` lane and no
    /// activity gate holds it. Whether the machine running the suite happens to
    /// be quiet is then irrelevant.
    fn force_auto_apply_attempt_now(app: &mut App) {
        let mut intent = app
            .auto_apply_intent
            .expect("an automatic intent must be armed to force an attempt");
        let now = std::time::Instant::now();
        intent.retry_at = now - std::time::Duration::from_secs(1);
        app.auto_apply_intent = Some(intent);
        app.auto_apply_ladder = Some(crate::AutoApplyLadder {
            build: intent.build,
            armed_at: now - crate::native_update_auto_intent::LANDS_WITHIN,
            announced: crate::native_update_auto_intent::ApplyPhase::Land,
        });
    }

    /// Drive the automatic lane through one whole preflight-block budget against
    /// whatever blocker the caller has already installed.
    fn spend_one_preflight_block_budget(app: &mut App) {
        for attempt in 1..=u32::from(MAX_AUTOMATIC_UPDATE_CYCLES) {
            force_auto_apply_attempt_now(app);
            app.try_pending_native_auto_apply(false);
            // PRECONDITION, not decoration: a `Wait` would leave `attempts`
            // untouched, so this is what proves the preflight really RAN and
            // really came back `Blocked` rather than the poll short-circuiting.
            assert_eq!(
                app.auto_apply_intent.map(|intent| intent.attempts),
                Some(u8::try_from(attempt).expect("small")),
                "attempt {attempt} must have consumed exactly one retry budget cycle"
            );
        }
    }

    /// THE OVERDUE NOTICE IS A CLAIM ABOUT A LANE THAT LANDS BY ITSELF (the
    /// review of the update audit's merge): the updater is told `false` with
    /// `[update] auto_apply` off, while the in-session handoff cannot run here
    /// (main's `HandoffDisabled`, a record that promises nothing), and while a
    /// person's unsaved work holds the lane (main's editor row) — so an hour of
    /// any of them is never "aterm can't install updates" plus a banner.
    #[test]
    fn a_lane_that_waits_by_design_is_not_reported_as_landing_by_itself() {
        use crate::app_update_handoff::HandoffUnavailable;
        let mut app = App::headless_for_test();
        assert!(app.update_lands_by_itself_with(true, None));
        assert!(
            !app.update_lands_by_itself_with(false, None),
            "the switch off"
        );
        for why in HandoffUnavailable::ALL {
            assert!(!app.update_lands_by_itself_with(true, Some(why)), "{why:?}");
        }
        app.update_waits_on_person = Some(7);
        assert!(
            !app.update_lands_by_itself_with(true, None),
            "a person holds it"
        );
        assert!(
            !app.update_lands_by_itself(true),
            "and the live reading agrees (a headless App has no handoff either)"
        );
    }

    /// AN EDITOR HOLDING THE INSTALL IS SAID ONCE THROUGH THE BUDGET (2026-09-24
    /// review of §5.3(d)). With no automatic staged row to carry it, the blocker is
    /// the `Update waits for you` outcome row: the announced first attempt raises
    /// it, and the budget's exhaustion a few seconds later says the same words
    /// again. That second saying is the center's Duplicate — one row, one record —
    /// never the first row resolved into its Fault echo and the same row posted back.
    #[test]
    fn an_announced_editor_block_stays_one_row_through_the_budget() {
        let mut app = App::headless_for_test();
        let _ = park_a_settings_draft_in_a_background_tab(&mut app);
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        app.clear_messages_for_test();
        let waits = |app: &App| {
            app.messages
                .log()
                .records()
                .filter(|r| r.title == crate::app_update_screen::UPDATE_WAITS_FOR_YOU)
                .count()
        };
        let outcome_row = |app: &App| {
            app.messages
                .live_by_key(crate::update_words::KEY_OUTCOME)
                .map(|l| l.id)
        };

        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(true);
        assert_eq!(app.auto_apply_intent.map(|i| i.attempts), Some(1));
        let row = outcome_row(&app).expect("the announced first block is the row");
        assert_eq!(waits(&app), 1);

        for attempt in 2..=MAX_AUTOMATIC_UPDATE_CYCLES {
            force_auto_apply_attempt_now(&mut app);
            app.try_pending_native_auto_apply(false);
            assert_eq!(app.auto_apply_intent.map(|i| i.attempts), Some(attempt));
        }
        // PRECONDITION: the budget is spent, so the exhaustion said it again.
        assert!(
            app.auto_apply_intent.is_some_and(
                |i| i.retry_at > std::time::Instant::now() + PREFLIGHT_BLOCK_COOLDOWN / 2
            ),
            "the budget must be exhausted, or the second saying never ran"
        );
        assert_eq!(outcome_row(&app), Some(row), "the same row stands");
        assert_eq!(waits(&app), 1, "one record for one blocker");
    }

    /// Drive the real nonannounced timer path into a dirty-Settings refusal and
    /// read its durable answer. A child process isolates the shared scratch ledger
    /// from unrelated updater tests that write different standing explanations.
    #[cfg(target_os = "macos")]
    #[test]
    fn nonannounced_automatic_blocks_are_durable_only_after_real_attempts() {
        const CHILD: &str = "ATERM_TEST_AUTOMATIC_REFUSAL_LEDGER_CHILD";
        const DONE: &str = "automatic-refusal-ledger assertions completed";
        if std::env::var_os(CHILD).is_none() {
            let name = std::thread::current()
                .name()
                .expect("the harness names its current test")
                .to_string();
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .env("RUST_TEST_THREADS", "1")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("launch isolated automatic-refusal test");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while child.try_wait().unwrap().is_none() {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("isolated automatic-refusal test exceeded its deadline");
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let output = child.wait_with_output().unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success(),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
            assert!(
                stdout.contains(DONE),
                "the child must execute its assertions"
            );
            return;
        }

        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        assert!(crate::app_config::update_auto_apply(&app.config));
        assert!(!App::relaunch_nudge_seam_suppresses_auto_apply());
        let wid = WindowId(0);
        let _ = park_a_settings_draft_in_a_background_tab(&mut app);
        let working_tab = app.windows[&wid].tab_set.active_id().unwrap();
        let current = app.native_updater_service.snapshot().current_build;
        stage_one_build_for_test(&mut app, current + 4_913);
        let before = aterm_update::status(current).unwrap();
        app.clear_messages_for_test();

        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert_eq!(app.auto_apply_intent.unwrap().attempts, 1);
        // The build now waits on a person BY DESIGN: the updater is told it does
        // not land by itself, so its overdue notice never calls this a failure.
        assert_eq!(app.update_waits_on_person, Some(current + 4_913));
        assert!(!app.update_lands_by_itself_with(true, None));
        let report = aterm_update::apply_lane_report(current).unwrap();
        assert_eq!(report.last_refusal, App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY);
        assert!(!report.last_refusal_at.is_empty());
        let after = aterm_update::status(current).unwrap();
        assert!(
            after
                .outcome
                .contains(App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY)
        );
        assert_eq!(after.failing_applies, before.failing_applies);
        assert_eq!(after.failing_checks, before.failing_checks);
        assert!(
            app.update_row_text().is_none(),
            "announce=false remains quiet"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // A different durable marker makes an accidental rewrite observable even
        // if timestamps have only second precision. Merely polling a future retry
        // must neither consume an attempt nor replace this standing explanation.
        const BETWEEN: &str = "test marker between actual automatic attempts";
        aterm_update::record_apply_refusal(current, BETWEEN);
        for _ in 0..16 {
            app.try_pending_native_auto_apply(false);
        }
        assert_eq!(app.auto_apply_intent.unwrap().attempts, 1);
        assert_eq!(
            aterm_update::apply_lane_report(current)
                .unwrap()
                .last_refusal,
            BETWEEN
        );

        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert_eq!(app.auto_apply_intent.unwrap().attempts, 2);
        assert_eq!(
            aterm_update::apply_lane_report(current)
                .unwrap()
                .last_refusal,
            App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY,
        );
        assert_eq!(
            aterm_update::status(current).unwrap().failing_applies,
            before.failing_applies
        );
        assert!(app.update_row_text().is_none());
        assert_no_probe_disturbance(&app, wid, working_tab);
        println!("{DONE}");
    }

    /// THE "UPDATE PAUSED" REGRESSION, AND THE RECURRING NAG IT WAS ALMOST TRADED
    /// FOR. A close-preflight BLOCK is a fact about the moment, not about the
    /// artifact — so exhausting its bounded budget must neither retire automatic
    /// apply for the life of the process NOR turn into a scheduled disturbance.
    ///
    /// The budget is three attempts spaced 5 s / 15 s: about twenty seconds of
    /// being ready to park. Two failure modes bracket this test.
    ///   * PERMANENCE (the original bug): twenty busy seconds installed a
    ///     `retry_at: None` latch, `arm` then answered `SuppressManualOnly` for
    ///     that exact (build, artifact) forever, and only a newer build or a
    ///     relaunch escaped. The user was told "Update paused — manual retry"
    ///     permanently for having been busy.
    ///   * RECURRENCE (the first attempt at a fix): giving that latch a lapse
    ///     deadline re-armed a FRESH intent at `attempts: 0` every cooldown, so
    ///     the whole budget replayed — three more `prepare_all_native_shutdown`
    ///     passes, each of which can hijack focus into a Close Recovery palette,
    ///     plus a new status pill — every two hours, forever.
    ///
    /// So this walks TWO full cooldown rounds of the REAL lane
    /// (`try_pending_native_auto_apply` → `apply_native_update` →
    /// `prepare_all_native_shutdown` → the service's own preflight reducer) and
    /// pins the shape that is neither: one attempt per cooldown, one pill ever,
    /// NOTHING on screen ever, and a way back the moment the blocker clears.
    ///
    /// THE BLOCKER IS A REAL RECOVERY-UI BLOCKER. An earlier version of this test
    /// used `pending_restore`, which is discovered by the UI-less counting pass in
    /// `native_update_close_preflight` and therefore cannot disturb anything even
    /// in principle — so the test passed while every probe was switching the
    /// user's tab and throwing a Close Recovery palette over their work. An
    /// unsaved Settings draft is discovered by `prepare_all_native_shutdown`,
    /// which is the code that does the disturbing.
    #[test]
    fn an_exhausted_preflight_block_budget_neither_latches_forever_nor_nags_on_a_schedule() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(
            crate::app_config::update_auto_apply(&app.config)
                && !App::relaunch_nudge_seam_suppresses_auto_apply(),
            "PRECONDITION: the automatic lane must be enabled or every poll below \
             answers Clear and this test proves nothing"
        );
        let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
        let working_tab = app.windows[&wid]
            .tab_set
            .active_id()
            .expect("PRECONDITION: the user is on a tab");
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "PRECONDITION: the staged build armed automatic intent"
        );

        app.clear_messages_for_test();
        spend_one_preflight_block_budget(&mut app);
        // THE FIRST BUDGET IS ALREADY THREE PROBES. Even before the cooldown
        // schedule is reached, none of them may have taken the screen.
        assert_no_probe_disturbance(&app, wid, working_tab);

        // NOT PERMANENT: the intent is retained on a long cooldown, and it is a
        // real deadline in the future that `about_to_wait` folds into winit's
        // `WaitUntil` (`fold_auto_apply_deadline`), so it fires on a fully IDLE
        // terminal instead of waiting for an event that never comes.
        let cooling = app
            .auto_apply_intent
            .expect("a spent preflight budget must NOT retire the automatic lane");
        assert_eq!(cooling.build, build);
        assert!(
            app.auto_apply_manual_only.is_none(),
            "a transient block must not install a manual-only latch at all — that \
             latch is what `arm` reads as SuppressManualOnly"
        );
        let cooldown = cooling
            .retry_at
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            cooldown > PREFLIGHT_BLOCK_COOLDOWN / 2 && cooldown <= PREFLIGHT_BLOCK_COOLDOWN,
            "the retry must be spaced by the cooldown, not by the 5s/15s probe \
             cadence (got {cooldown:?})"
        );

        // The ONE row, and it names the one thing the person can do (ruling
        // 143): the lane comes back by itself, so it says nothing about itself.
        let pill = app
            .update_row_text()
            .expect("the first exhaustion tells the user once");
        assert_eq!(
            pill,
            format!(
                "{} \u{2014} {}",
                crate::app_update_screen::UPDATE_WAITS_FOR_YOU,
                App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY
            ),
            "the row must name the blocker the person can clear"
        );

        // SECOND ROUND. Each further attempt costs exactly ONE probe (not three)
        // and must add nothing to the screen.
        for round in 0..4 {
            app.clear_messages_for_test();
            force_auto_apply_attempt_now(&mut app);
            app.try_pending_native_auto_apply(false);
            let again = app
                .auto_apply_intent
                .expect("the lane keeps trying while the blocker persists");
            assert_eq!(
                again.attempts,
                MAX_AUTOMATIC_UPDATE_CYCLES + 1 + u8::try_from(round).expect("small"),
                "round {round}: a cooldown probe must consume exactly one attempt, \
                 and `attempts` must keep counting — resetting it is what would \
                 re-arm the whole budget and re-fire the pill"
            );
            assert!(
                again
                    .retry_at
                    .saturating_duration_since(std::time::Instant::now())
                    > PREFLIGHT_BLOCK_COOLDOWN / 2,
                "round {round}: still spaced by the cooldown"
            );
            assert!(
                app.update_row_text().is_none() && app.update_record_text().is_none(),
                "round {round}: THE NAG. The user was already told once; telling \
                 them again on a two-hour schedule is the regression this test exists \
                 for (got {:?} / {:?})",
                app.update_row_text(),
                app.update_record_text()
            );
            // THE OTHER HALF OF THE NAG, and the half no `notice` assertion can
            // ever see: the pill is one-shot, but the tab switch / focus theft /
            // recovery palette were not. This is the assertion the finding is
            // about.
            assert_no_probe_disturbance(&app, wid, working_tab);
        }

        // AND THE WAY BACK: the moment the blocker clears, the next cooldown probe
        // must leave the Blocked lane and be handed to physical replacement — not
        // merely bump a counter while the update sits there staged forever.
        //
        // THE SCOPE OF WHAT THIS BLOCK PROVES, because the previous version of it
        // claimed more. It shows the AUTHORIZATION, by two witnesses that a
        // `Blocked` outcome cannot fake:
        //   * the receipt. The refusal the attempt records is the ADMISSION
        //     gate's ("Update kept …"), which only an attempt that got past the
        //     close preflight can reach — a `Blocked` preflight records its own
        //     blocker, and the close preflight cannot produce `Failed` for a
        //     declining reducer (only a reducer ERROR is `Failed`, and there is
        //     none here — the reducer answered cleanly for four rounds above);
        //   * the SCHEDULING SHAPE. `Blocked` retains the intent and installs no
        //     latch; the physical lane does the exact opposite. This is the pair
        //     the sibling test `a_genuine_failure_takes_the_physical_budget_…`
        //     pins as mutually exclusive.
        // It does NOT show the update landing — in this process the physical lane
        // cannot land (headless: no event-loop proxy, so
        // `native_update_admission::classify` never reaches `Apply(Seamless)`).
        // `a_busy_user_eventually_gets_the_update` carries the arc the rest of the
        // way and ends on the INSTALLED artifact; this test's subject is the
        // cooldown, the nag and the screen.
        assert!(
            app.pool.iter().count() > 0,
            "PRECONDITION AND A SAFETY RAIL: this app must own at least one live \
             session. With an empty pool `native_update_admission::classify` admits \
             the COLD lane, and the authorized apply below would `exec()` the test \
             binary instead of returning"
        );
        discard_settings_drafts(&mut app, settings);
        app.clear_messages_for_test();
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);

        // The cleared blocker lets the attempt past the close preflight to the
        // ADMISSION gate, which this process can never pass (no seamless lane,
        // a live session vetoes cold). That refusal is a refusal, not a failed
        // attempt (2026-09-01): past the spent budget it re-probes silently on
        // the standing cooldown — no new pill (the one exhaustion pill already
        // fired), no manual-only latch, no physical failure booked, the intent
        // retained.
        assert!(
            app.update_row_text().is_none(),
            "a refused re-probe past the exhaustion pill paints nothing: {:?}",
            app.update_row_text()
        );
        let refusal = app
            .native_updater_service
            .snapshot()
            .error
            .clone()
            .expect("the refused attempt records why it stopped");
        assert!(
            refusal.starts_with("Update kept"),
            "the receipt that the attempt got PAST the close preflight to the \
             admission gate, got {refusal:?}"
        );
        assert!(
            app.auto_apply_intent.is_some_and(
                |intent| intent.build == build && intent.retry_at > std::time::Instant::now()
            ),
            "the admission refusal keeps the intent armed on its cooldown"
        );
        assert!(
            app.auto_apply_manual_only.is_none() && app.auto_apply_physical_retry.is_none(),
            "an admission refusal neither latches manual-only nor books a \
             physical failure"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);
    }

    /// THE OTHER HALF OF THE FIX, stated as an outcome rather than as a pill: with
    /// the SAME Settings draft in place, a background probe and a person's click
    /// get the same VERDICT and completely different SCREENS.
    ///
    /// Deleting the recovery surface outright would satisfy every "nothing moved"
    /// assertion in the lane test above, so this pins that the surface is intact
    /// where it belongs — on the lane a person actually asked for (the Version
    /// menu's "Install aterm vX now").
    #[test]
    fn an_automatic_probe_is_silent_while_a_person_s_apply_still_surfaces_recovery() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
        let working_tab = app.windows[&wid]
            .tab_set
            .active_id()
            .expect("PRECONDITION: the user is on a tab");
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);

        // BACKGROUND: the verdict is real and it is `Blocked` by the native-app
        // barrier (not by the UI-less `native_update_close_preflight` counters —
        // that string is different, and getting it would mean the reducer barrier
        // never ran).
        let UpdateOutcome::Blocked { reasons } =
            app.apply_native_update(ApplyMode::AutomaticPastGrace)
        else {
            panic!("an unsaved Settings draft blocks an update apply");
        };
        assert_eq!(
            reasons,
            vec![App::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string()],
            "the block came from the native close barrier — the one that surfaces \
             recovery — and not from a later, UI-less blocker"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // A PERSON: same state, same verdict, but now the blocking leaf is focused
        // and its reducer-supplied recovery commands are on screen.
        let UpdateOutcome::Blocked { reasons: manual } =
            app.apply_native_update(ApplyMode::Immediate)
        else {
            panic!("the same draft blocks a manual apply");
        };
        assert_eq!(manual, reasons, "identical verdict, by construction");
        assert_eq!(
            app.active_native_view(wid).map(|(_, view)| view),
            Some(settings),
            "a person's apply focuses the leaf that refused"
        );
        let lines = app.windows[&wid]
            .palette()
            .expect("a person's apply opens Close Recovery")
            .controls_lines();
        for action in ["settings/drafts/review", "settings/drafts/discard-all"] {
            assert!(
                lines.iter().any(|line| {
                    line.contains("target=native") && line.contains(&format!("action={action}"))
                }),
                "recovery exposes {action}: {lines:?}"
            );
        }
    }

    /// THE OUTCOME THE WHOLE FIX IS FOR, END TO END: a user who was merely BUSY
    /// eventually GETS THE UPDATE — and the update is INSTALLED at the end of it.
    ///
    /// Two earlier versions of this test each stopped one step short:
    ///   * the first proved the cleared blocker let the attempt reach the physical
    ///     ADMISSION gate and then asserted the gate's REFUSAL. A refusal is not an
    ///     update: every assertion in it was equally satisfied by a world where the
    ///     lane reaches the last gate forever and the build is never applied;
    ///   * the second reached an installed VERDICT, but by handing worker-collected
    ///     disk facts straight to `finish_async_native_update_handoff`. That arm is
    ///     gated on `UpdateHandoffCompletion::reconcile` being `Some`, and every
    ///     construction site in this crate sets it to `None` — so it proved that a
    ///     RECEIPT REDUCES, down a path no worker has ever taken, and it never
    ///     re-attempted the apply the cooldown exists to re-arm.
    ///
    /// SO THIS DRIVES THE REAL SEQUENCE, in production's own order, and the only
    /// substituted step is marked at the call site:
    ///   1. the blocked probes spend the cheap preflight budget (act one);
    ///   2. the blocker clears, an AUTHORIZED attempt reaches physical replacement
    ///      and books a physical failure with a comeback (act two);
    ///   3. the comeback deadline lapses and the lane RE-ATTEMPTS FOR REAL, through
    ///      the same three calls `about_to_wait` makes — lapse, re-arm, poll — and
    ///      the second failure lands on the SECOND rung of the physical schedule,
    ///      which is what makes it a fresh attempt rather than a replay (act three);
    ///   4. the third attempt's child gets far enough to swap the bundle before it
    ///      dies, and the two events a real worker emits are replayed in order: the
    ///      handoff completion (`reconcile: None`, like every real one) and then the
    ///      separately-collected disk facts (act four).
    ///
    /// WHAT A UNIT TEST CANNOT DO, STATED PLAINLY, so the substitution is not
    /// mistaken for the thing itself:
    ///   * `apply_staged_update_now` cannot run. The seamless lane needs a live
    ///     winit event-loop proxy and spawns a real successor process; the cold lane
    ///     calls `Command::exec`. Either would fork or destroy the test binary, and
    ///     a headless `App` (`proxy: None`, `headless: true`) cannot reach
    ///     `Apply(Seamless)` in `native_update_admission::classify` at all. Acts two
    ///     and three therefore fail AT that gate — which is exactly the physical
    ///     failure the retry schedule is written for — and act four consumes the
    ///     one-shot apply authority with an empty closure in its place. The full
    ///     park → spawn → adopt → repaint path is covered by the QA seam
    ///     (`ATERM_DEBUG_SEAMLESS_REEXEC`) against a real binary, not by this suite;
    ///   * the INSTALLED facts are supplied rather than read. `installed_update_facts`
    ///     runs `verify_bundle_policy` (a real `codesign --deep` against a real
    ///     signed bundle) plus PlistBuddy, so no unit test can produce a true
    ///     reading. What is NOT faked is the judgement, and it is worth being exact
    ///     about how much of it the receipt carries: `reconcile_durable_stage` will
    ///     only say `InstalledNeedsRelaunch` when the receipt re-proves the
    ///     in-memory stage's build, commit AND DMG digest, and that is what retires
    ///     the stage here — but a receipt that failed to prove them would still
    ///     retire it (as `Retired`) and the durable outcome would still name the
    ///     installed build, because the bundle and the ledger agree on the number.
    ///     So the assertion that discriminates is the CONSUMED STAGE, not the
    ///     string; both are produced by production code on production's own
    ///     reconcile wake.
    ///
    /// WHAT IS THEREFORE PINNED: the strongest APPLIED state a surviving process can
    /// observe — the stage is CONSUMED and the durable outcome names the installed
    /// build — reached only after a re-armed automatic apply really attempted again.
    /// The pre-park verdict is about ONE artifact. A downloaded `.app` and the same
    /// build already installed under our executable share (build, commit) but are
    /// different bytes checked by different probes; the download's `passed` must
    /// not answer for the activation (that would skip the last codesign check
    /// before an exec that swaps nothing), and a corrupt download's `false` must
    /// not refuse a good installed bundle.
    #[cfg(unix)]
    #[test]
    fn a_pre_park_verdict_answers_only_for_its_own_artifact() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        let download = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        let activation = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &crate::native_updater_service::installed_activation_digest(
                build,
                PREFLIGHT_TEST_COMMIT,
            ),
        );
        assert_eq!(app.cached_handoff_preverification(&download), Some(true));
        assert_eq!(
            app.cached_handoff_preverification(&activation),
            None,
            "the download's verdict must not be reused for the installed bundle"
        );
        *app.handoff_preverified
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some(crate::HandoffPreverification {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                artifact: "ab".repeat(32),
                at: std::time::Instant::now(),
                passed: false,
                reason: None,
            });
        assert_eq!(app.cached_handoff_preverification(&download), Some(false));
        assert_eq!(
            app.cached_handoff_preverification(&activation),
            None,
            "a corrupt download must not refuse a good installed bundle"
        );
    }

    #[test]
    fn a_busy_user_eventually_gets_the_update() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let (_, settings) = park_a_settings_draft_in_a_background_tab(&mut app);
        let working_tab = app.windows[&wid]
            .tab_set
            .active_id()
            .expect("PRECONDITION: the user is on a tab");
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        assert!(
            app.pool.iter().count() > 0,
            "PRECONDITION AND A SAFETY RAIL: with an empty session pool the \
             admission classifier admits the destructive COLD lane and the \
             authorized apply below would `exec()` the test binary"
        );
        // PRECONDITION, READ BACK THE WAY PRODUCTION READS IT: the staged candidate
        // is one `start_unix_update_handoff` would carry to the physical gate. A
        // cached REFUSAL is a short-circuit before anything parks, so a fixture
        // carrying one would make every act below a story about an artifact
        // production declines outright — and a headless `App`, blocked one gate
        // earlier, cannot tell the difference from its outcomes alone.
        #[cfg(unix)]
        assert_eq!(
            app.cached_handoff_preverification(
                &crate::native_updater_service::ApplyAttemptTicket::for_test(
                    build,
                    PREFLIGHT_TEST_COMMIT,
                    &"ab".repeat(32),
                )
            ),
            Some(true),
            "the fixture must model a candidate that PASSED pre-park verification"
        );

        // ACT ONE — BUSY. The user's unsaved Settings draft blocks every probe
        // until the cheap budget is spent and the lane drops to its cooldown.
        // Surviving this at all is the first half of the fix: the old code retired
        // automatic apply permanently right here.
        spend_one_preflight_block_budget(&mut app);
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "a spent preflight budget must retain the intent, not retire the lane"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "a transient block must not install the latch `arm` reads as \
             SuppressManualOnly"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // ACT TWO — THE USER STOPS BEING BUSY, and the next cooldown probe is
        // AUTHORIZED: it leaves the close-preflight lane and reaches the
        // ADMISSION gate, which this process can never pass (no seamless lane;
        // a live session vetoes cold). That is a REFUSAL, not a failed attempt
        // (2026-09-01): the lane retains the intent on its standing cooldown —
        // no manual-only latch, no physical failure booked, no budget spent on
        // an attempt that never started. (Booking these as physical failures
        // is what marched the field machine to failing_applies=23 and, at nine,
        // would have retired the automatic lane outright.)
        discard_settings_drafts(&mut app, settings);
        let row_before = app.update_row_text();
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert_eq!(
            app.update_row_text(),
            row_before,
            "a refused re-probe past the exhaustion row paints nothing new"
        );
        // WHICH GATE REFUSED IS PART OF THE PREMISE, not decoration. The fixture's
        // candidate carries a PASSED pre-park verification, so a real attempt is
        // carried all the way to the admission gate; a `passed: false` fixture
        // would be short-circuited by `start_unix_update_handoff` before anything
        // parked, and every act below it would be describing an artifact production
        // refuses outright. The refusal recorded here must therefore be the
        // admission gate this process genuinely cannot pass — never the verification
        // one, which would mean the whole arc was measured on a doomed candidate.
        let refusal = app
            .native_updater_service
            .snapshot()
            .error
            .clone()
            .expect("a refused attempt records why it stopped");
        // Both `native_update_admission` refusals open this way ("…could not be
        // prepared" for the seamless lane a headless `App` cannot offer, "…could not
        // be proven safe" for the cold lane's foreground probe). Which of the two
        // answers is an accident of the test process's PTY state and is not the
        // claim; that the attempt got as far as that gate is.
        assert!(
            refusal.starts_with("Update kept"),
            "the attempt must reach the physical admission gate, got {refusal:?}"
        );
        assert!(
            !refusal.contains("failed verification"),
            "the staged candidate must be one production would carry to the gate, \
             not one the pre-park verification cache already refused: {refusal:?}"
        );
        let after_gate = app
            .auto_apply_intent
            .expect("the admission refusal RETAINS the intent");
        assert_eq!(after_gate.build, build);
        assert!(
            after_gate.retry_at > std::time::Instant::now(),
            "the lane is scheduled to come back on its cooldown, not finished"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "an admission refusal must not latch manual-only"
        );
        assert!(
            app.auto_apply_physical_retry.is_none(),
            "no physical failure may be booked for an attempt that never started"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // ACT THREE — THE COMEBACK IS REAL, NOT A PROMISE ON A STRUCT. Only the
        // clock is forced: the retained intent's own deadline is moved into the
        // past and the production poll runs verbatim. The receipt that the lane
        // genuinely re-ATTEMPTED — rather than merely re-reading its struct —
        // is the monotone `attempts` counter advancing across the gate.
        let attempts_before = after_gate.attempts;
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        let re_probed = app
            .auto_apply_intent
            .expect("the second refusal retains the intent too");
        assert_eq!(
            re_probed.attempts,
            attempts_before + 1,
            "the re-armed lane must actually ATTEMPT again — the monotone \
             attempts counter is the receipt for a second admission round trip"
        );
        assert!(
            re_probed.retry_at > std::time::Instant::now(),
            "…and the cooldown is re-armed, not exhausted"
        );
        assert!(
            app.auto_apply_manual_only.is_none() && app.auto_apply_physical_retry.is_none(),
            "refusals stay refusals on every repeat — no latch, no physical booking"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // ACT FOUR — THE ATTEMPT THAT LANDS.
        //
        // The comeback re-authorizes through exactly this reducer path, which is
        // the only producer of an `ApplyAttemptTicket`: `begin_apply_preflight`
        // binds the artifact generation, and `finish_apply_preflight` mints the
        // one-shot command ONLY for `ClosePreflight::Ready`. So holding this
        // ticket is itself proof that the close preflight said yes.
        let ApplyPreflightStart::Inspect(preflight) = app
            .native_updater_service
            .begin_apply_preflight(ApplyMode::AutomaticPastGrace)
        else {
            panic!("the re-armed stage must admit a fresh apply preflight");
        };
        let ApplyDecision::Execute(command) = app
            .native_updater_service
            .finish_apply_preflight(preflight, ClosePreflight::Ready)
        else {
            panic!("a ready close preflight must authorize the replacement");
        };
        let attempt = command.attempt();
        assert_eq!(attempt.target_build(), build);
        // THE ONE STEP A UNIT TEST MAY NOT RUN. In production this closure is
        // `apply_staged_update_now`, which parks every reader and either `exec`s or
        // spawns the successor. Consuming the one-shot authority without it is the
        // whole of the substitution; everything after this line is the real
        // completion path with the real ticket.
        command.execute(|| ());

        // EVENT ONE: the handoff completion. This child swapped the bundle and then
        // failed to commit, so the parent survives — and the parent learns nothing
        // about the swap here, because a real completion carries `reconcile: None`.
        // This call IS the `(Some(attempt), None)` arm of
        // `reduce_returned_handoff_completion`; the mode-to-lane classification that
        // wraps it is proven in `app_update_handoff.rs`.
        let returned = app.abort_reaped_native_apply_before_reconcile(
            &attempt,
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );
        assert!(
            matches!(returned, UpdateOutcome::Failed { .. }),
            "the parent's view of a non-committed handoff is a failure, whatever \
             the child managed to do to the bundle first, got {returned:?}"
        );
        app.surface_update_apply_outcome("automatic handoff", returned, false);
        assert!(
            app.auto_apply_manual_only
                .is_some_and(|manual| manual.build == build),
            "PRECONDITION FOR THE LATCH ASSERTION BELOW: the returned failure \
             latched THESE bytes manual-only, which is correct — at this instant \
             the parent has no idea the child got as far as swapping the bundle"
        );

        // EVENT TWO: the disk facts the worker collected right after it published
        // that completion (`send_warranted_handoff_failure` posts them as a separate
        // `NativeUpdateReconcileFinished` wake, which is why the completion above
        // could not carry them). The canonical bundle now carries this build and its
        // INSTALLED RECEIPT names this exact artifact — build, commit AND digest,
        // all three re-proved against the in-memory stage before the reducer will
        // call anything installed.
        let facts = reconcile_facts_with_installed(
            9,
            9,
            Some(DurableUpdateStatus {
                linux_host: false,
                linux: None,
                enabled: true,
                current_build: app.native_updater_service.snapshot().current_build,
                staged_build: Some(build),
                staged_version: Some(format!("1.0.{build}")),
                staged_commit: Some(PREFLIGHT_TEST_COMMIT.to_string()),
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
            }),
            Some(InstalledUpdate {
                build,
                commit: PREFLIGHT_TEST_COMMIT.to_string(),
                version: None,
                receipt_build: Some(build),
                receipt_dmg_sha256: Some("ab".repeat(32)),
            }),
        );
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::Startup, facts);

        // THE ASSERTION THE OLD TESTS DID NOT MAKE, ON THE PATH A WORKER TAKES: the
        // update is INSTALLED — the child swapped the bundle before it failed to
        // commit — and the surviving parent does not just SAY "relaunch": the
        // installed bundle is newer than this process, so it becomes an ACTIVATION
        // stage under its own identity, and the automatic lane comes back for it on
        // the failure's OWN schedule (the 2026-09-22/23 update audit, plan P0-6:
        // this arc used to arm the activation at once, which is how a structural
        // failure's "confirming retry, ten minutes out" ran half a second later).
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .expect("the swapped-in bundle becomes an activation stage");
        assert!(
            staged.build == build && staged.is_installed_activation(),
            "the stage on record is the activation of the installed build, got {staged:?}"
        );
        let outcome = app.native_updater_service.snapshot().outcome.clone();
        assert!(
            outcome.contains(&format!("build {build} is already installed")),
            "the durable outcome names the installed build, got {outcome:?}"
        );
        assert!(
            outcome.contains("activation is pending"),
            "…and names the outstanding activation, got {outcome:?}"
        );
        assert!(
            !outcome.to_lowercase().contains("relaunch"),
            "…and never asks for one: {outcome:?}"
        );
        // THE LATCH IS THE UPDATE'S, NOT THE DOWNLOAD'S. The retired DMG and the
        // installed bundle are the same logical update — this very candidate put
        // the bundle there — so the latch is carried to the activation's identity
        // with its deadline unchanged, and the activation does not arm under it.
        let activation_digest = decode_dmg_sha256(&staged.dmg_sha256).unwrap();
        let carried = app
            .auto_apply_manual_only
            .expect("the latch survives its own candidate's bundle swap");
        assert_eq!(
            (carried.build, carried.dmg_sha256),
            (build, activation_digest),
            "re-keyed to the activation it now guards"
        );
        assert_eq!(
            latch_rung(carried.retry_at),
            "retry-600",
            "the structural failure's one confirming retry is still ten minutes out"
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "the activation must not arm inside the latch the failure just bought"
        );
        // …AND THE BUSY USER STILL GETS THE UPDATE: once that deadline passes, the
        // lapse releases the latch and the automatic lane arms for the ACTIVATION
        // (its own identity, its budget folded with the download's).
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..carried
        });
        app.rearm_native_auto_apply_after_lapse();
        assert!(
            app.auto_apply_manual_only.is_none(),
            "the deadline passed: the latch lapses"
        );
        assert!(
            app.auto_apply_intent.is_some_and(
                |intent| intent.build == build && intent.dmg_sha256 == activation_digest
            ),
            "the automatic lane arms for the ACTIVATION once the latch lapses"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);
    }

    /// The counterpart the fix must NOT loosen: `UpdateOutcome::Failed` is the
    /// evidence-against-the-artifact channel and takes the strict, converging
    /// PHYSICAL budget — never the cheap preflight cooldown — on the very same
    /// artifact, in the very same `App`.
    ///
    /// The contrast is the assertion. A `Blocked` attempt leaves a live intent
    /// re-probing in ~2 h; a `Failed` one retires the intent, latches manual-only,
    /// and schedules its comeback off the physical schedule (10 min, then 30 min,
    /// then a stand-down epoch). Confusing the two in either direction is a bug:
    /// one way spams park/spawn round trips, the other never applies.
    #[test]
    fn a_genuine_failure_takes_the_physical_budget_while_a_preflight_block_only_cools_down() {
        // The pure policy split the two lanes ride on, asserted directly so a
        // future reclassification cannot silently swap them.
        use crate::native_update_auto_intent::{AttemptDisposition, AttemptResult, finish};
        assert_eq!(finish(AttemptResult::Blocked), AttemptDisposition::Retry);
        assert_eq!(
            finish(AttemptResult::Failed),
            AttemptDisposition::ManualOnly
        );

        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        app.pending_restore = Some(crate::restore::RestoreManifest::new(Vec::new()));
        spend_one_preflight_block_budget(&mut app);
        assert!(
            app.auto_apply_intent.is_some() && app.auto_apply_manual_only.is_none(),
            "PRECONDITION: the transient lane keeps a live intent and no latch"
        );

        // Same artifact, same process — but this time the physical handoff
        // genuinely failed (the child died), arriving asynchronously through the
        // completion lane every real overlap failure takes.
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
        );
        assert!(
            app.auto_apply_intent.is_none(),
            "a genuine failure retires the intent; it does not inherit the \
             preflight lane's live re-probe"
        );
        let genuine = app
            .auto_apply_manual_only
            .expect("a genuine failure latches manual-only");
        let wait = genuine
            .retry_at
            .expect(
                "even a genuine failure gets a comeback on its first attempt; \
                 `None` here would be the one-unlucky-moment permanence",
            )
            .saturating_duration_since(std::time::Instant::now());
        // The FIRST physical cycle: ten minutes, i.e. the physical schedule and
        // not the two-hour preflight cooldown. Getting the cheap lane's number
        // here would mean the classification collapsed.
        assert!(
            wait > std::time::Duration::from_secs(500)
                && wait <= std::time::Duration::from_secs(600),
            "a physical failure must wait its own first cycle (~600s), got {wait:?}"
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "the ASYNC completion lane must spend the shared physical budget — it \
             used to consult no budget at all and stamp `retry_at: None`"
        );
    }

    /// EVERY PHYSICAL LANE SHARES ONE BUDGET, THE BUDGET RUNS OUT, AND THE USER IS
    /// TOLD ONCE — WHEN IT HAS — NOT NINE TIMES AND NOT FOREVER.
    ///
    /// Three independent things are pinned here because each of them shipped
    /// broken in a different round:
    ///   * the failures the budget is NAMED for return through
    ///     `abort_reaped_native_apply_before_reconcile`, which once consulted no
    ///     budget and stamped a deadline-less latch. One missed 15 s handoff
    ///     deadline — the commonest physical failure there is, and an
    ///     environmental one — disabled automatic apply for that build outright;
    ///   * the repair for that made the lane UNBOUNDED: a spent epoch stood down
    ///     6 h, the stand-down outlasted the 4 h replenish window by design, the
    ///     counter reset, and the artifact got a fresh full budget every epoch
    ///     forever (~10 round trips a day) while the constant's prose claimed it
    ///     converged. The whole lifetime is walked here so "converges" is a fact
    ///     about the code and not about a comment;
    ///   * and the user-visible half: every one of those failures painted
    ///     "Update delayed — retries on its own". That is a notification on a
    ///     SCHEDULE for a condition the user cannot act on. Owner instruction: the
    ///     transient lane retries quietly (2026-09-23: automatic retries post no
    ///     row at all). Exactly one row,
    ///     when the lane is out of retries, and it names a control.
    ///
    /// The pill is surfaced here the way the production caller does it
    /// (`app_update_handoff.rs`: `surface_update_apply_outcome("automatic handoff",
    /// surfaced, false)`), because the completion lane returns the outcome and the
    /// event-loop caller paints it.
    #[test]
    fn the_async_physical_lane_converges_and_stops_nagging() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        // Relative to the running build: `arm` answers `Clear` for anything not
        // strictly newer, which would make the two `arm_native_auto_apply`
        // assertions at the end pass for the wrong reason.
        let build = app.native_updater_service.snapshot().current_build + 1;
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );

        // THE WHOLE LIFETIME, driven through the REAL completion lane, one entry
        // per failure: what the latch said, and what the user saw.
        let mut latched = Vec::new();
        let mut pills = Vec::new();
        for _ in 0..usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS) {
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            app.clear_messages_for_test();
            let outcome = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
            );
            app.surface_update_apply_outcome("automatic handoff", outcome, false);
            latched.push(
                app.auto_apply_manual_only
                    .expect("a returned physical failure always latches manual-only")
                    .retry_at
                    .map(|at| at.saturating_duration_since(std::time::Instant::now())),
            );
            // Ruling 143: a retry the lane scheduled is silent on the glass; the
            // "pill" this test counts is the outcome ROW, which only a lane
            // that stopped raises (`Install now`, the Version menu's press).
            pills.push(app.update_row_text());
        }

        // 600 s, 1800 s, stand-down — three times over, and the last stand-down is
        // the CONVERGED lane's quiet re-sample, one epoch cooldown out (the
        // 2026-09-22/23 update audit, plan P1-1(d); it used to be `None`, a latch
        // that never lapsed). Names the SCHEDULE each deadline came from rather
        // than asserting to the second, because these are real `Instant`s.
        let schedule = latched
            .iter()
            .map(|wait| match wait {
                None => "no-retry",
                Some(wait) if *wait <= std::time::Duration::from_secs(600) => "retry-600",
                Some(wait) if *wait <= std::time::Duration::from_secs(1800) => "retry-1800",
                Some(_) => "stand-down",
            })
            .collect::<Vec<_>>();
        assert_eq!(
            schedule,
            vec![
                "retry-600",
                "retry-1800",
                "stand-down",
                "retry-600",
                "retry-1800",
                "stand-down",
                "retry-600",
                "retry-1800",
                "stand-down",
            ],
            "the async lane must ride the shared physical schedule and then STOP \
             spending a schedule; a fourth epoch here is the unbounded-retry \
             regression"
        );
        let resample = latched
            .last()
            .copied()
            .flatten()
            .expect("the converged transient lane re-samples");
        assert!(
            resample > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60),
            "…one epoch cooldown out, never sooner: {resample:?}"
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS),
            "each returned failure spends exactly one attempt"
        );

        // THE NAG, counted: silence while the lane is coming back, and one row
        // at the end naming the control.
        assert_eq!(
            pills.iter().filter(|pill| pill.is_some()).count(),
            1,
            "nine failures may cost at most one row, got {pills:?}"
        );
        assert!(
            pills[..usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS) - 1]
                .iter()
                .all(Option::is_none),
            "every failure before the last must pass in silence — the user has \
             nothing to do and the lane is already coming back, got {pills:?}"
        );
        // The ONE actionable moment — the lane is out of retries — is the
        // install half's health warning (plan P1-1(c), on main's R38 health
        // surface), naming the control and the quiet re-sample, never a repeat
        // of "retries on its own". The re-sampling lane is still scheduled, so
        // no decision row stands beside it (ruling 143).
        let last = pills.last().and_then(Clone::clone).unwrap_or_default();
        assert!(
            last.starts_with(&format!(
                "{} — automatic apply of build {build} stopped after {} failed handoffs",
                aterm_update::health_failing_title("apply"),
                PHYSICAL_FAILURE_LIFETIME_ATTEMPTS
            )) && last.contains("Version menu"),
            "the converged lane must say so once, naming a control: {last:?}"
        );

        // AND CONVERGED MEANS CONVERGED — until the re-sample. `arm` refuses the
        // artifact, and the latch lapses only at its one-cooldown deadline.
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(
            app.auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_some()),
            "the converged latch stays, with its re-sample deadline"
        );
        assert!(
            !app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "a duplicate stage wake for the SAME artifact must not restart the lane"
        );
        // …but a strictly newer build is a different artifact and is not punished
        // for this one's history.
        assert!(
            app.arm_native_auto_apply(build + 1, &"cd".repeat(32)),
            "convergence is per-artifact; a newer build still arms automatically"
        );
    }

    /// The standing health row, when one is live: its title, its detail joined,
    /// and its capsules.
    fn health_row(app: &App) -> Option<(String, String, Vec<aterm_messages::Intent>)> {
        app.messages
            .live_by_key(crate::update_words::KEY_HEALTH)
            .map(|live| {
                (
                    live.msg.title.clone(),
                    live.msg.detail.join("; "),
                    live.msg.actions.clone(),
                )
            })
    }

    /// A STRUCTURAL CONVERGENCE IS SAID AT ONCE (the 2026-09-22/23 update audit,
    /// plan P1-1(c)).
    ///
    /// The loud notice lived only behind the updater's streak gate,
    /// `PERSISTENT_AFTER` = 3 consecutive failures, while a structural lane
    /// converges after `STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS` = 2 — so the one
    /// state in which automatic apply had genuinely given up could never be
    /// announced, and 0.90 and 0.91 sat staged for a day with zero
    /// `update-health:` lines. Driven through the shipping completion lane: the
    /// second failure posts the install half's health warning at once, beside
    /// the stopped lane's `Install now` decision row; a late third completion
    /// for the same build says nothing new.
    #[test]
    fn a_structural_convergence_posts_the_health_notice_at_once() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        let fail = |app: &mut App| {
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            let outcome = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended AdoptionMismatch".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Structural),
            );
            app.surface_update_apply_outcome("automatic handoff", outcome, false);
        };

        fail(&mut app);
        assert!(
            health_row(&app).is_none(),
            "one structural failure buys a confirming retry, not an alarm"
        );
        fail(&mut app);
        let (title, body, actions) =
            health_row(&app).expect("the converged lane is announced the moment it converges");
        assert_eq!(title, aterm_update::health_failing_title("apply"));
        assert!(
            body.starts_with(&format!(
                "automatic apply of build {build} stopped after {STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS} \
                 failed handoffs (the two builds could not hand off); apply it from the Version menu"
            )),
            "{body}"
        );
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, aterm_messages::Intent::ApplyUpdate { .. })),
            "one decision per stopped lane (rulings 119/143): the press is the \
             outcome row's, not the warning's: {actions:?}"
        );
        assert!(
            app.messages
                .live_by_key(crate::update_words::KEY_OUTCOME)
                .is_some_and(|l| l
                    .msg
                    .actions
                    .contains(&aterm_messages::Intent::ApplyUpdate { build })),
            "the explicit lane is what moves a converged artifact, and the stopped \
             lane's decision row carries its press"
        );
        assert!(
            app.auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_none()),
            "a structural convergence stays converged — loud, not retried"
        );
        // Once per build: a late or duplicate completion re-announces nothing.
        app.clear_messages_for_test();
        fail(&mut app);
        assert!(
            health_row(&app).is_none(),
            "the row was said once; a later failure of the same build is quiet"
        );
    }

    /// A SPENT UNEXPLAINED BUDGET RE-SAMPLES AFTER THE COOLDOWN, AND IS ANNOUNCED
    /// ONCE (the 2026-09-22/23 update audit, plan P1-1(d)).
    ///
    /// Six unexplained candidate deaths — the shape a desk at load average 140-160
    /// produces — used to end in `retry_at: None`: a latch that never lapses, a
    /// healthy build kept off the machine until someone relaunched. The lane now
    /// converges into a quiet re-sample one epoch cooldown out: the health row is
    /// posted once at convergence, the latch LAPSES at its deadline and the
    /// automatic intent re-arms, and a re-sample that fails again converges
    /// again, quietly, onto the next cooldown.
    #[test]
    fn a_spent_unexplained_budget_resamples_after_the_cooldown_and_announces_once() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        let fail = |app: &mut App| {
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            app.clear_messages_for_test();
            let outcome = app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
                HandoffFailureLane::Physical(PhysicalFailureShape::Unexplained),
            );
            app.surface_update_apply_outcome("automatic handoff", outcome, false);
            health_row(app)
        };
        let mut announced = Vec::new();
        for _ in 0..UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS {
            announced.push(fail(&mut app).is_some());
        }
        let mut expected = vec![false; usize::from(UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS)];
        *expected.last_mut().unwrap() = true;
        assert_eq!(
            announced, expected,
            "silent while the schedule runs, loud exactly once at convergence"
        );
        let latch = app
            .auto_apply_manual_only
            .expect("the converged lane is latched");
        let wait = latch
            .retry_at
            .expect("an unexplained convergence re-samples; it never latches forever")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            wait > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60)
                && wait <= PHYSICAL_FAILURE_EPOCH_COOLDOWN,
            "one epoch cooldown out: {wait:?}"
        );
        assert!(
            !app.lapse_expired_auto_apply_manual_only(),
            "not before its deadline"
        );

        // The cooldown passes: the latch lapses and automatic apply is armable
        // for the same bytes again.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
            ..latch
        });
        assert!(
            app.arm_native_auto_apply(build, &"ab".repeat(32)),
            "the lapsed re-sample re-arms the automatic lane"
        );

        // The re-sample fails too: converged again, onto the next cooldown, and
        // quiet — the row was already said for this build.
        assert!(
            fail(&mut app).is_none(),
            "a failed re-sample does not announce again"
        );
        assert!(
            app.auto_apply_manual_only
                .and_then(|manual| manual.retry_at)
                .is_some_and(|at| {
                    at.saturating_duration_since(std::time::Instant::now())
                        > PHYSICAL_FAILURE_EPOCH_COOLDOWN - std::time::Duration::from_secs(60)
                }),
            "and it re-samples again one cooldown later"
        );
    }

    /// EVERY LATCH WRITES ITS STANDING NOTE (the 2026-09-22/23 update audit, plan
    /// P1-1(a)). The asynchronous completion lane — the one every returned
    /// handoff takes, which minted the latches that stranded 0.90 and 0.91 —
    /// latched with no note, so `update status` showed neither the pause nor its
    /// deadline. Now its schedule's note is queued with the latch and written by
    /// the surfacing AFTER the failure it explains (booking a failure clears any
    /// standing refusal, so the order is the whole point). Read back from the
    /// real ledger in a child process, isolated from sibling tests that record
    /// their own refusals.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_async_physical_lane_records_its_schedule_after_the_failure() {
        const CHILD: &str = "ATERM_TEST_ASYNC_SCHEDULE_NOTE_CHILD";
        const DONE: &str = "async-schedule-note assertions completed";
        if std::env::var_os(CHILD).is_none() {
            let name = std::thread::current()
                .name()
                .expect("the harness names its current test")
                .to_string();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &name, "--nocapture"])
                .env(CHILD, "1")
                .env("RUST_TEST_THREADS", "1")
                .output()
                .expect("launch the isolated schedule-note test");
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains(DONE),
                "{stdout}\n{}",
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        let build = current + 7_211;
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        let outcome = app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended TimedOut".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );
        let queued = app
            .apply_schedule_standing
            .clone()
            .expect("the async lane queues its schedule's standing note");
        assert!(
            queued.starts_with(&format!("automatic apply of build {build} retries at ")),
            "{queued}"
        );
        // The production caller surfaces the returned outcome, which books the
        // failure and then writes the note.
        app.surface_update_apply_outcome_for_target("automatic handoff", outcome, false, build);
        assert!(
            app.apply_schedule_standing.is_none(),
            "the note is written once"
        );
        let report = aterm_update::apply_lane_report(current).expect("the ledger is readable");
        assert!(
            report.last_failure.contains("TimedOut"),
            "the failure was booked: {report:?}"
        );
        assert_eq!(
            report.last_refusal, queued,
            "…and the schedule stands beside it, not erased by it"
        );
        println!("{DONE}");
    }

    /// A NATIVE-CLOSE REDUCER `Err` IS NOT "NOT NOW".
    ///
    /// `prepare_all_native_shutdown` answers `Ok(false)` when a native app
    /// deliberately retained its view — the user's own live state, self-correcting
    /// — and `Err` when the close reducer itself broke (an unknown instance,
    /// unhandled effects). Both used to flatten into `UpdateOutcome::Blocked`,
    /// which was harmless only while a `Blocked` latch was permanent anyway. Now
    /// that `Blocked` keeps a live intent re-probing every cooldown forever, that
    /// flattening would put a genuine invariant failure into an endless retry
    /// loop, so the distinction is kept TYPED all the way to the outcome.
    ///
    /// THE BAR MAY NOT OUTLIVE THE PROMISE.
    ///
    /// The `Staged` report paints the posture from policy, BEFORE the stage is
    /// reconciled and armed; an admission refusal on the first automatic attempt
    /// is synchronous and lands well inside that bar's 8 s hold. Until
    /// 2026-08-30 the `Failed` arm latched manual-only and restated nothing, so a
    /// bar that promised "applies in place within ~2 min" kept promising it while
    /// the Version-menu row already said "tried once, didn't start" — two
    /// surfaces, two answers — and when the bar folded, the ledger kept the false
    /// sentence. Driven through the real lane: a headless `App` has no seamless
    /// lane, so the physical gate refuses ("Update kept N … session(s)
    /// running…") and the poll takes exactly the arm that was silent.
    ///
    /// AND THE BAR AGREES WITH THE GATE BEFORE THE ATTEMPT. The gate's
    /// `seamless_capable` and the bar's posture read ONE predicate
    /// (`App::seamless_handoff_unavailable`), so the line the `Staged` report
    /// paints here is already the handoff-off warning — until 2026-08-30 the
    /// posture folded only the `$ATERM_NO_SEAMLESS_UPDATE` opt-out, and a
    /// `--control-sock` or `--headless` process painted the automatic promise
    /// over an apply the gate refused with any terminal open. The promise a
    /// WINDOWED app paints is modelled onto the bar by hand so the restatement is
    /// observable as a change of words.
    #[test]
    fn a_stand_down_restates_the_staged_bar_while_it_is_still_up() {
        use crate::app_update_handoff::HandoffUnavailable;
        use crate::update_apply_trouble::ApplyRetry;
        use crate::update_words::{ApplyPosture, staged_detail};
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        assert!(
            app.pool.iter().count() > 0,
            "PRECONDITION AND A SAFETY RAIL: with an empty session pool the \
             admission classifier admits the destructive COLD lane and the \
             authorized apply below would `exec()` the test binary"
        );
        assert!(
            std::env::var_os("ATERM_CONTROL_SOCK").is_none(),
            "PRECONDITION: the reason the lane is unavailable must be the App's own shape"
        );
        let build = app.native_updater_service.snapshot().current_build + 1;
        let staged = aterm_update::Progress::Staged {
            version: format!("1.0.{build}"),
            build,
        };
        // Design §10.5 H2: a staged build nothing can press is a RECORD — the
        // log keeps its posture's words, and no row promises anything.
        let recorded = |app: &App| {
            app.messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
                .map(|r| r.detail.join("; "))
                .expect("the staged report is recorded")
        };
        let no_row = |app: &App| {
            app.messages
                .live_by_key(crate::update_words::KEY_PROGRESS)
                .is_none()
        };
        // The gate and the bar read one predicate: headless, so no seamless lane.
        let why = app
            .seamless_handoff_unavailable()
            .expect("a headless App has no seamless lane");
        assert_eq!(why, HandoffUnavailable::Headless);
        let unavailable = ApplyPosture::HandoffDisabled { why, veto: None };
        // The report lands first, from the check thread, and paints the posture —
        // already the warning, because the lane is not there…
        app.note_update_progress(&staged);
        assert_eq!(recorded(&app), staged_detail(unavailable));
        assert!(no_row(&app), "the headless posture is a record, not a row");
        assert!(
            !recorded(&app).contains("~2 min") && !recorded(&app).contains("Version menu"),
            "{}",
            recorded(&app)
        );
        // …then the stage is reconciled and armed (the arm reads policy, not the
        // lane: the cold lane is a real automatic path once every PTY is gone).
        stage_one_build_for_test(&mut app, build);
        assert!(
            no_row(&app),
            "arming raises no row over a lane that cannot run"
        );
        // Model the row a WINDOWED app with automatic install off paints — the
        // ready row, the lane's flow state (what the restatement below looks the
        // row up by); an automatic posture paints none (§5.3(d), 2026-09-24).
        app.post_update_row(
            crate::update_words::progress(&staged, Some(ApplyPosture::ManualByConfig), ""),
            &format!("1.0.{build}"),
            crate::messages_host::FlowPhase::Staged { build },
        );
        assert_eq!(
            app.update_row_detail().as_deref(),
            Some(staged_detail(ApplyPosture::ManualByConfig).as_str())
        );

        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);

        // An admission refusal is a REFUSAL, not a failed attempt (2026-09-01):
        // the intent stays armed on its own cooldown, no manual-only latch is
        // minted, and no physical failure is booked — the lane keeps retrying
        // by itself while the bar tells the truth below.
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "the refused attempt RETAINS the intent on its cooldown"
        );
        assert!(
            app.auto_apply_manual_only.is_none(),
            "an admission refusal must not latch manual-only"
        );
        assert!(
            app.auto_apply_physical_retry.is_none(),
            "an admission refusal books no physical failure"
        );
        let refusal = app
            .native_updater_service
            .snapshot()
            .error
            .clone()
            .expect("a refused attempt records why it stopped");
        assert!(
            refusal.starts_with("Update kept"),
            "the attempt must reach the physical admission gate, got {refusal:?}"
        );
        // The refusal is the gate acting on the predicate the bar already named;
        // this build's latch does not outrank a lane that cannot run.
        let posture = app.apply_posture_for(build);
        assert_eq!(posture, unavailable);
        assert!(
            no_row(&app),
            "the stand-down folds the ready row: its posture is a record now"
        );
        assert_eq!(
            recorded(&app),
            staged_detail(posture),
            "the record carries the posture the App computes now"
        );
        let detail = staged_detail(posture);
        assert!(
            detail.contains("installs once every terminal is closed")
                && !detail.contains("minute")
                && !detail.contains("Version menu"),
            "{detail}"
        );
        // …and the Version-menu row states the lane's own fact: the retained
        // intent IS a scheduled retry.
        assert_eq!(app.apply_retry_for(Some(build)), ApplyRetry::Scheduled);
    }

    /// The same law at the reaped-abort completion: a handoff worker that killed
    /// and reaped its child latches manual-only on the physical budget, and the
    /// Staged bar — if it is still up for that build — says so at once rather
    /// than after the next unrelated reconcile.
    #[test]
    fn a_reaped_abort_restates_the_staged_bar_it_stands_down() {
        use crate::update_words::{ApplyPosture, staged_detail};
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        let staged = aterm_update::Progress::Staged {
            version: format!("1.0.{build}"),
            build,
        };
        app.note_update_progress(&staged);
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            build,
            PREFLIGHT_TEST_COMMIT,
            &"ab".repeat(32),
        );
        ticket.make_current_apply_for_test(&mut app.native_updater_service);
        // Design §10.5 H2: the staged build is a RECORD under every posture
        // but a stood-down one where a press applies it.
        let recorded = |app: &App| {
            app.messages
                .log()
                .records()
                .rev()
                .find(|r| r.key.as_deref() == Some(crate::update_words::KEY_PROGRESS))
                .map(|r| r.detail.join("; "))
                .expect("the staged report is recorded")
        };
        // A headless App has no seamless lane, and the report painted that fact
        // (the posture reads the gate's own predicate). Model the promise a
        // WINDOWED app paints so the restatement below is a change of words.
        let unavailable = ApplyPosture::HandoffDisabled {
            why: app
                .seamless_handoff_unavailable()
                .expect("a headless App has no seamless lane"),
            veto: None,
        };
        assert_eq!(recorded(&app), staged_detail(unavailable));
        // Model the row a WINDOWED app with automatic install off paints — the
        // ready row, the lane's flow state (what the restatement below looks the
        // row up by); an automatic posture paints none (§5.3(d), 2026-09-24).
        app.post_update_row(
            crate::update_words::progress(&staged, Some(ApplyPosture::ManualByConfig), ""),
            &format!("1.0.{build}"),
            crate::messages_host::FlowPhase::Staged { build },
        );
        assert_eq!(
            app.update_row_detail().as_deref(),
            Some(staged_detail(ApplyPosture::ManualByConfig).as_str())
        );

        app.abort_reaped_native_apply_before_reconcile(
            &ticket,
            "overlap handoff failed safely: handoff proof ended ChildDied".to_string(),
            HandoffFailureLane::Physical(PhysicalFailureShape::Transient),
        );

        let latched = app
            .auto_apply_manual_only
            .expect("a returned physical failure latches manual-only");
        assert_eq!(latched.build, build);
        let posture = app.apply_posture_for(build);
        assert_eq!(
            posture, unavailable,
            "this build's latch does not outrank a lane that cannot run"
        );
        // The posture is a record now: the ready row folded and its words
        // are on record; nothing is raised over a lane that cannot run.
        assert!(
            app.staged_update_row().is_none(),
            "a record posture raises no decision"
        );
        let detail = recorded(&app);
        assert_eq!(detail, staged_detail(posture));
        assert!(
            !detail.contains("~2 min") && !detail.contains("Version menu"),
            "{detail}"
        );
    }

    /// Driven through the real lane with a real broken reducer: a Settings view
    /// whose instance has been removed from the runtime under it, which is exactly
    /// the `RuntimeError::UnknownInstance` shape `prepare_close` reports.
    #[test]
    fn a_broken_close_reducer_is_a_failure_not_a_block() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let mut app = App::headless_for_test();
        let wid = crate::WindowId(0);
        assert!(
            app.open_settings_tab(crate::native_settings::SettingsRoute::Home),
            "PRECONDITION: a native view must exist for a close reducer to break"
        );
        let (instance, _view) = app
            .active_native_view(wid)
            .expect("PRECONDITION: the Settings tab is the active native view");

        let build = app.native_updater_service.snapshot().current_build + 1;
        stage_one_build_for_test(&mut app, build);
        // An ordinary transient blocker, held constant across BOTH phases below,
        // so the only variable is whether the close reducer works. It also keeps
        // the preflight from ever answering Ready, which in a unit test would mean
        // a real park/spawn/exec.
        app.pending_restore = Some(crate::restore::RestoreManifest::new(Vec::new()));

        // PRECONDITION / ANTI-VACUITY: with the runtime intact this same apply is
        // NOT a failure, so anything the assertions below catch is the reducer
        // error and not the fixture.
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.attempts >= 1),
            "PRECONDITION: an intact runtime leaves the attempt on the cheap \
             Blocked lane with a live intent"
        );
        assert!(
            app.auto_apply_physical_retry.is_none(),
            "PRECONDITION: an ordinary block spends no physical budget"
        );

        // Break it: the view is still in the tab tree, its app instance is gone.
        assert!(
            app.native_runtime.remove_instance(instance).is_some(),
            "PRECONDITION: the instance existed to be removed"
        );

        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);

        assert!(
            app.auto_apply_intent.is_none(),
            "a broken close reducer must NOT be treated as a transient block — a \
             live intent would re-probe it every cooldown for the life of the process"
        );
        let latched = app
            .auto_apply_manual_only
            .expect("a reducer error takes the strict Failed lane and latches manual-only");
        assert_eq!(latched.build, build);
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "and it spends the strict, converging budget rather than the cheap \
             preflight cooldown"
        );
    }

    /// The retry spacing starts over after a long idle gap: a busy hour widens
    /// the spacing to its last rung, a quiet half hour brings it back to the
    /// first.
    #[test]
    fn an_idle_gap_restarts_the_activity_revoked_retry_spacing() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build: 77,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
            last_attempt: std::time::Instant::now(),
        });
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&ticket),
            Some(std::time::Duration::from_secs(30)),
            "past the last rung the spacing stays at the last rung"
        );
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build: 77,
            dmg_sha256: [0xab; 32],
            activation: false,
            cycles: ACTIVITY_REVOKED_LADDER_RUNGS,
            last_attempt: std::time::Instant::now()
                - crate::ACTIVITY_RETRY_BUDGET_REPLENISH
                - std::time::Duration::from_secs(1),
        });
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&ticket),
            Some(std::time::Duration::from_secs(2)),
            "after a long idle gap the spacing starts over"
        );
    }

    #[test]
    fn manual_only_latch_survives_duplicate_wakes_and_exactly_new_bytes_rearm() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            activation: false,
            retry_at: None,
        });

        for _ in 0..3 {
            assert!(!app.arm_native_auto_apply(build, &"ab".repeat(32)));
            assert!(app.auto_apply_intent.is_none());
            assert_eq!(
                app.auto_apply_manual_only,
                Some(crate::AutoApplyManualOnly {
                    build,
                    dmg_sha256: [0xab; 32],
                    activation: false,
                    retry_at: None,
                })
            );
        }

        assert!(app.arm_native_auto_apply(build, &"cd".repeat(32)));
        assert_eq!(
            app.auto_apply_intent.map(|intent| intent.dmg_sha256),
            Some([0xcd; 32])
        );
        assert!(app.auto_apply_manual_only.is_none());
    }

    #[test]
    fn disabling_auto_apply_clears_retained_intent() {
        let mut app = App::headless_for_test();
        let current = app.native_updater_service.snapshot().current_build;
        app.auto_apply_intent = Some(crate::AutoApplyIntent {
            build: current + 1,
            dmg_sha256: [0xab; 32],
            retry_at: std::time::Instant::now(),
            attempts: 1,
        });
        app.config.update = Some(crate::app_config::UpdateConfig {
            auto_apply: Some(false),
            ..crate::app_config::UpdateConfig::default()
        });

        assert!(!app.arm_native_auto_apply(current + 2, &"cd".repeat(32)));
        assert!(app.auto_apply_intent.is_none());
    }

    #[test]
    fn absent_auto_apply_intent_contributes_no_event_loop_deadline() {
        let other = std::time::Instant::now() + std::time::Duration::from_secs(7);
        assert_eq!(crate::fold_auto_apply_deadline(None, None), None);
        assert_eq!(
            crate::fold_auto_apply_deadline(None, Some(other)),
            Some(other)
        );

        let retry = other + std::time::Duration::from_secs(5);
        let intent = crate::AutoApplyIntent {
            build: 11,
            dmg_sha256: [0xab; 32],
            retry_at: retry,
            attempts: 1,
        };
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), None),
            Some(retry)
        );
        assert_eq!(
            crate::fold_auto_apply_deadline(Some(intent), Some(other)),
            Some(other),
            "the retry joins the existing minimum-deadline fold"
        );
    }

    #[test]
    fn debug_reexec_and_menu_feedback_cannot_bypass_dirty_native_state() {
        let _ledger = crate::app_update_screen::hold_update_ledger_for_test();
        let dir = std::env::temp_dir().join(format!(
            "aterm-update-dirty-preflight-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.md");
        std::fs::write(&path, "draft\n").unwrap();
        // The shipping encoder, not a hand-rolled `format!` — the latter is
        // malformed on Windows (drive letter + backslashes after the authority
        // slot), so this test could not even open its document there.
        let uri = crate::native_document_host::path_to_file_uri(&path).unwrap();

        let mut app = App::headless_for_test();
        app.open_document_tab(crate::native_app::AppKind::Editor, &uri)
            .unwrap();
        let wid = WindowId(0);
        app.dispatch_native_event(
            wid,
            crate::native_app::AppEvent::TextInput(crate::native_app::TextInputEvent::Commit(
                "unsaved ".to_string(),
            )),
        )
        .unwrap();

        let outcome = app.apply_debug_seamless_update();
        assert!(matches!(
            &outcome,
            UpdateOutcome::Blocked { reasons }
                if reasons.iter().any(|reason| reason.contains("Checkpoint Drafts"))
        ));
        // This is the exact outcome sink used by the enabled ApplyUpdate menu row:
        // the click becomes visible Software Update details instead of a dead item.
        app.surface_update_apply_outcome("menu test", outcome, true);
        let (_, view) = app.active_native_view(wid).expect("Software Update tab");
        assert!(matches!(
            app.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.route == crate::native_settings::SettingsRoute::SoftwareUpdate
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn app_reduces_up_to_date_completion_and_broadcasts_revision() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::SoftwareUpdate));
        let (_, view) = app
            .active_native_view(WindowId(0))
            .expect("active Settings view");
        let before_presentation = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                state.common.presentation_revision
            }
            _ => panic!("Settings view state"),
        };

        let ticket = start(&mut app.native_updater_service);
        app.finish_native_update_check(ticket, status(None, 0));

        assert_eq!(
            app.native_updater_service.snapshot().phase,
            UpdaterPhase::Idle
        );
        assert_eq!(
            app.native_updater_service.last_transitions()[0]
                .action
                .model_action(),
            Some("CheckUpToDate")
        );
        let after_presentation = match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => {
                state.common.presentation_revision
            }
            _ => panic!("Settings view state after completion"),
        };
        assert!(after_presentation > before_presentation);
    }

    #[test]
    fn rejected_native_completion_clears_the_settings_tab_busy_indicator() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let wid = WindowId(0);
        let (instance, view) = app.active_native_view(wid).expect("active Settings view");
        let pending = app
            .native_runtime
            .dispatch(
                instance,
                view,
                AppEvent::Action(crate::native_app::ActionInvocation {
                    id: crate::native_ui::ActionId::new(format!(
                        "settings/set/{}",
                        crate::prefs::EDIT_CURSOR_BLINK
                    )),
                    value: Some(crate::native_app::SemanticInput::Bool(false)),
                }),
            )
            .unwrap();
        let operation = pending
            .effects
            .iter()
            .find_map(|effect| match effect {
                AppEffect::ConfigPatch { reply, .. } => Some(reply.operation),
                _ => None,
            })
            .expect("Settings reducer owns pending config work");
        // Drive the same presentation refresh the normal initiating dispatch
        // performs, while deliberately retaining the effect for this test.
        app.refresh_native_presentation(wid, instance, view);
        assert!(
            app.windows
                .get(&wid)
                .and_then(|window| window.tab_set.active())
                .unwrap()
                .presentation
                .indicators
                .busy
        );

        app.dispatch_native_completion(
            wid,
            instance,
            view,
            AppEvent::ConfigPatchFinished {
                operation,
                outcome: crate::native_app::ConfigPatchOutcome::Rejected {
                    message: "read-only test config".to_string(),
                },
            },
        )
        .unwrap();
        assert!(
            !app.windows
                .get(&wid)
                .and_then(|window| window.tab_set.active())
                .unwrap()
                .presentation
                .indicators
                .busy,
            "completion-time presentation refresh removes stale busy state"
        );
    }

    #[test]
    fn app_drops_stale_worker_completion_while_new_generation_runs() {
        let mut app = App::headless_for_test();
        let first = start(&mut app.native_updater_service);
        app.finish_native_update_check(first, status(None, 1));
        let second = start(&mut app.native_updater_service);
        let before_revision = app.native_updater_service.snapshot().revision;

        app.finish_native_update_check(first, status(Some(99), 0));

        let snapshot = app.native_updater_service.snapshot();
        assert_eq!(snapshot.phase, UpdaterPhase::Checking);
        assert_eq!(snapshot.active, Some(second));
        assert!(snapshot.staged.is_none());
        assert_eq!(snapshot.ignored_completions, 1);
        assert!(snapshot.revision > before_revision);
        assert!(app.native_updater_service.last_transitions().is_empty());
    }

    #[test]
    fn tab_focus_walks_the_compiled_semantic_order() {
        let mut app = App::headless_for_test();
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        let wid = WindowId(0);
        let (_, view) = app.active_native_view(wid).expect("active Settings view");

        app.move_native_focus(wid, false).unwrap();
        let first = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("first focusable semantic node");
        app.move_native_focus(wid, false).unwrap();
        let second = app
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone())
            .expect("second focusable semantic node");
        assert_ne!(first, second);

        app.move_native_focus(wid, true).unwrap();
        assert_eq!(
            app.native_runtime
                .view_state(view)
                .and_then(|state| state.common().last_focus.as_ref()),
            Some(&first)
        );
    }

    #[test]
    fn focused_native_text_field_receives_space_and_submit_before_activation() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();

        let press = |key| crate::input::InputEvent::Key {
            key: Key::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        assert!(app.native_input_event(wid, &press(NamedKey::Space)));
        let (_, view) = app.active_native_view(wid).unwrap();
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), " ");

        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Preedit("候補".to_string())),
        )
        .unwrap();
        assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert_eq!(state.search_input.value(), " ");
        assert!(
            state.search_input.preedit().is_none(),
            "Return is Submit for a focused text field, not generic activation"
        );
    }

    /// THE KEYPAD ON A NATIVE PAGE, through the real key path (`App::on_key`
    /// -> `on_key_native_mode` -> `keymap::build_key_input` -> `App::input`
    /// -> `native_input_event`): a physical KP_5 reaches the reducer as
    /// `Numpad5` — the identity the PTY encoders need — and the reducer folds
    /// it onto `5`, so a focused Settings search field types the digit. A
    /// NumLock-off KP_Decimal (`Delete` at the keypad location, `NumpadDelete`
    /// to the engine) is a forward delete. Before the fold both fell to the
    /// lowering's `_ => None` arm: nothing typed, nothing deleted.
    #[test]
    fn keypad_digit_and_delete_reach_a_focused_settings_field() {
        use winit::event::{ElementState, KeyEvent};
        use winit::keyboard::{
            Key as WinitKey, KeyCode, KeyLocation, NamedKey as WinitNamed, PhysicalKey, SmolStr,
        };

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        let (_, view) = app.active_native_view(wid).unwrap();
        let search_value = |app: &App| {
            let Some(crate::native_app::AppViewState::Settings(state)) =
                app.native_runtime.view_state(view)
            else {
                panic!("Settings view");
            };
            state.search_input.value().to_string()
        };
        // A keypad press exactly as the desktop backends deliver it: the
        // keypad in `location`, the layout's glyph (or NumLock-off name) in
        // `logical_key`, the glyph's text alongside a `Character`.
        let keypad = |code: KeyCode, logical: WinitKey| {
            let text = match &logical {
                WinitKey::Character(s) => Some(s.clone()),
                _ => None,
            };
            KeyEvent::synthetic_for_test(
                PhysicalKey::Code(code),
                logical,
                text,
                KeyLocation::Numpad,
                ElementState::Pressed,
                false,
            )
        };

        app.on_key(
            wid,
            keypad(KeyCode::Numpad5, WinitKey::Character(SmolStr::new("5"))),
        );
        assert_eq!(
            search_value(&app),
            "5",
            "KP_5 types its glyph into the focused field"
        );
        app.on_key(
            wid,
            keypad(KeyCode::Numpad1, WinitKey::Character(SmolStr::new("1"))),
        );
        assert_eq!(search_value(&app), "51");

        // Caret to the start, then the NumLock-off decimal key: a forward
        // delete of the `5`.
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Home { extend: false }),
        )
        .unwrap();
        app.on_key(
            wid,
            keypad(KeyCode::NumpadDecimal, WinitKey::Named(WinitNamed::Delete)),
        );
        assert_eq!(
            search_value(&app),
            "1",
            "NumLock-off KP_Decimal is a forward delete"
        );
    }

    #[test]
    fn bare_return_activates_the_pages_primary_default_button() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        // About route: `navigate()` anchors keyboard focus on the page
        // CONTAINER (a non-actionable Group, its scroll/a11y anchor), so a
        // fresh page has no activatable focus and "Copy Build Information" is
        // its Primary. A bare Return must fire it (the native default-button
        // convention); before, the key fell through to a text Submit that
        // no-ops outside an edit. Space must NOT fall back — on macOS Space
        // only ever activates the focused control.
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        // A REGULAR-width viewport: the default headless grid classifies as
        // Compact, where the paginated About page does not author its action
        // row (and so genuinely has no default to fire).
        if let Some(ws) = app.windows.get_mut(&wid) {
            ws.cols = 220;
            ws.rows = 60;
        }
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
        let (_, view) = app.active_native_view(wid).unwrap();
        assert!(
            !app.activate_native_focus(wid).unwrap_or(false),
            "fresh page must hold no ACTIVATABLE focus for this test to exercise the fallback"
        );

        let press = |key| crate::input::InputEvent::Key {
            key: Key::Named(key),
            mods: Modifiers::empty(),
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        assert!(app.native_input_event(wid, &press(NamedKey::Space)));
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        assert_eq!(
            state.feedback, None,
            "unfocused Space must not trigger the default button"
        );

        assert!(app.native_input_event(wid, &press(NamedKey::Enter)));
        let Some(crate::native_app::AppViewState::Settings(state)) =
            app.native_runtime.view_state(view)
        else {
            panic!("Settings view");
        };
        // The headless clipboard executor completes inline, so the feedback has
        // already advanced past "Copying build information…" to the done state.
        assert_eq!(
            state.feedback.as_deref(),
            Some("Build information copied"),
            "unfocused Return must fire the page's Primary (default) button"
        );
    }

    #[test]
    fn settings_search_field_honors_readline_control_keys() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
        )
        .unwrap();

        let ctrl = |c| crate::input::InputEvent::Key {
            key: Key::Character(c),
            mods: Modifiers::CTRL,
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        let (_, view) = app.active_native_view(wid).unwrap();
        let search = |app: &App| match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => (
                state.search_input.value().to_string(),
                state.search_input.selection().range(),
            ),
            _ => panic!("Settings view"),
        };

        // Ctrl-A: caret to the start (NOT select-all, NOT swallowed).
        assert!(app.native_input_event(wid, &ctrl('a')));
        assert_eq!(search(&app), ("cursor trail".to_string(), 0..0));
        // Ctrl-F / Ctrl-B: caret right then left.
        assert!(app.native_input_event(wid, &ctrl('f')));
        assert_eq!(search(&app).1, 1..1);
        assert!(app.native_input_event(wid, &ctrl('b')));
        assert_eq!(search(&app).1, 0..0);
        // Ctrl-D: forward-delete at the caret.
        assert!(app.native_input_event(wid, &ctrl('d')));
        assert_eq!(search(&app), ("ursor trail".to_string(), 0..0));
        // Ctrl-E: caret to the end; Ctrl-W: previous word dies.
        assert!(app.native_input_event(wid, &ctrl('e')));
        assert_eq!(search(&app).1, 11..11);
        assert!(app.native_input_event(wid, &ctrl('w')));
        assert_eq!(search(&app).0, "ursor ");
        // Ctrl-K after Ctrl-A kills the whole line; Ctrl-U from the end does too.
        assert!(app.native_input_event(wid, &ctrl('a')));
        assert!(app.native_input_event(wid, &ctrl('k')));
        assert_eq!(search(&app).0, "");
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("beam".to_string())),
        )
        .unwrap();
        assert!(app.native_input_event(wid, &ctrl('u')));
        assert_eq!(search(&app).0, "");
    }

    /// Off macOS, Ctrl+←/→ and Ctrl+⌫ are WORD operations in the Settings text
    /// fields (the cfg'd `alt_word || readline` term): Ctrl+arrow lands exactly
    /// where Alt+arrow does, Ctrl+⌫ kills exactly what Ctrl+W does, and the
    /// ctrl-LETTER readline arms stay by-character because they match first.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn settings_search_field_ctrl_arrows_walk_words_off_macos() {
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Appearance));
        app.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: crate::native_ui::ActionId::new("settings/search"),
                value: None,
            }),
        )
        .unwrap();
        app.dispatch_native_event(
            wid,
            AppEvent::TextInput(TextInputEvent::Commit("cursor trail".to_string())),
        )
        .unwrap();

        let chord = |key, mods| crate::input::InputEvent::Key {
            key,
            mods,
            base_layout: None,
            event_type: KeyEventType::Press,
        };
        let left = |mods| chord(Key::Named(NamedKey::ArrowLeft), mods);
        let right = |mods| chord(Key::Named(NamedKey::ArrowRight), mods);
        let (_, view) = app.active_native_view(wid).unwrap();
        let search = |app: &App| match app.native_runtime.view_state(view) {
            Some(crate::native_app::AppViewState::Settings(state)) => (
                state.search_input.value().to_string(),
                state.search_input.selection().range(),
            ),
            _ => panic!("Settings view"),
        };

        // Alt+← is the existing word motion; note where it lands from the end.
        assert!(app.native_input_event(wid, &left(Modifiers::ALT)));
        let alt_word_stop = search(&app).1;
        // Back to the end, then Ctrl+←: the SAME stop.
        assert!(app.native_input_event(wid, &chord(Key::Character('e'), Modifiers::CTRL)));
        assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
        assert_eq!(search(&app).1, alt_word_stop, "Ctrl+← = Alt+← (word left)");
        assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
        assert_eq!(search(&app).1, 0..0, "second Ctrl+← reaches the start");
        // Forward: Ctrl+→ lands exactly where Alt+→ does (emacs-style: the END
        // of the word, not the start of the next — hence not `alt_word_stop`).
        assert!(app.native_input_event(wid, &right(Modifiers::CTRL)));
        let ctrl_right_stop = search(&app).1;
        assert!(app.native_input_event(wid, &left(Modifiers::CTRL)));
        assert_eq!(search(&app).1, 0..0, "Ctrl+← undoes the word step");
        assert!(app.native_input_event(wid, &right(Modifiers::ALT)));
        assert_eq!(
            search(&app).1,
            ctrl_right_stop,
            "Ctrl+→ = Alt+→ (word right)"
        );
        // Ctrl+⌫ from the end kills the last word — exactly Ctrl+W's cut.
        assert!(app.native_input_event(wid, &chord(Key::Character('e'), Modifiers::CTRL)));
        assert!(app.native_input_event(
            wid,
            &chord(Key::Named(NamedKey::Backspace), Modifiers::CTRL)
        ));
        assert_eq!(search(&app).0, "cursor ", "Ctrl+⌫ = backward-kill-word");
        // The ctrl-LETTER readline arms match first: ^B is still ONE character.
        assert!(app.native_input_event(wid, &chord(Key::Character('b'), Modifiers::CTRL)));
        assert_eq!(search(&app).1, 6..6, "^B stays by-character");
    }

    #[test]
    fn explicit_font_metrics_seed_initial_and_additional_headless_windows() {
        let mut app = App::headless_for_test();
        app.backend.activate_px(16.0);
        app.font_px = 16.0;
        app.backend.set_pad(crate::pad_for_scale(1.0));
        app.backend.set_head(0);
        let expected = app.unattached_window_metrics();
        // The initial production call has to provide MetricsView explicitly. Verify
        // the constructor stores the supplied renderer truth without deriving a
        // separate automatic-font value of its own.
        let initial = crate::WindowState::new_native(
            None,
            24,
            80,
            expected,
            crate::tab_model::TabSet::default(),
        );
        assert_eq!(initial.metrics, expected);

        // Every post-startup creation seam uses the same renderer authority. Drive
        // the real testable logical-window installation path to guard that wiring.
        let sid = app.next_session_id;
        let additional = app.insert_logical_window(crate::stub_session(sid), 24, 80);
        assert_eq!(app.windows[&additional].metrics, expected);
        assert_eq!(app.win_cell_size(additional), app.cell_size());
        assert_eq!(app.win_pad(additional), app.backend.pad());
    }

    /// Tier-1 conformance for the shipping exact-observation handoff. A failed
    /// reconciliation must retain both the deferred external bytes and a queued
    /// semantic write; retry admits that exact generation before the write can
    /// leave the queue. The negative control proves the model rejects the lost
    /// candidate state this test is intended to exclude.
    #[test]
    fn exact_observation_handoff_conforms_and_preserves_failed_reconciliation() {
        fn project(
            model: &aterm_spec::derive::Model,
            app: &App,
            phase: i64,
            sampled: i64,
            admitted: i64,
            reconciliation_failed: bool,
        ) -> aterm_spec::interp::State {
            let pending = i64::from(app.native_config_external_pending.is_some());
            let mut state = model.init_state();
            state.insert("phase", phase);
            state.insert("pending", pending);
            state.insert("sampled", sampled);
            state.insert(
                "gate",
                i64::from(app.native_config_service.reconciliation_required() || pending == 1),
            );
            state.insert("queued", i64::from(!app.native_config_pending.is_empty()));
            state.insert("admitted", admitted);
            state.insert("reconciliation_failed", i64::from(reconciliation_failed));
            state
        }

        fn assert_step(
            model: &aterm_spec::derive::Model,
            before: &aterm_spec::interp::State,
            after: &aterm_spec::interp::State,
            action: &str,
        ) {
            assert_eq!(
                model.successors(action, before).as_slice(),
                std::slice::from_ref(after),
                "shipping transition must refine {action}"
            );
            for invariant in &model.invariants {
                assert!(
                    model.check_invariant(invariant.name, after),
                    "post-state violates {}::{}: {after:?}",
                    model.name,
                    invariant.name
                );
            }
        }

        let root = std::env::temp_dir().join(format!(
            "aterm-config-observation-handoff-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("aterm.toml");
        std::fs::write(&path, "serious_mode = false\n").unwrap();

        let model = aterm_spec::derive::native_config_observation_handoff_model();
        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        let mut state = project(&model, &app, 0, 0, 0, false);
        assert_eq!(state, model.init_state());

        app.native_config_inflight = true;
        let after = project(&model, &app, 1, 0, 0, false);
        assert_step(&model, &state, &after, "BeginWrite");
        state = after;

        app.enqueue_serious_mode_intent().unwrap();
        let after = project(&model, &app, 1, 0, 0, false);
        assert_step(&model, &state, &after, "QueueWrite");
        state = after;

        std::fs::write(&path, "serious_mode = true\n").unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        assert!(
            app.sync_native_config_external_observation(observation.clone())
                .unwrap()
                .is_none()
        );
        let after = project(&model, &app, 1, 0, 0, false);
        assert_step(&model, &state, &after, "ObserveFirst");
        state = after;

        app.finish_native_config_write(
            NativeConfigOrigin::SeriousMode { desired: false },
            NativeConfigPersistenceCompletion {
                outcome: ConfigPatchOutcome::Conflict {
                    revision: app.native_config_service.snapshot().revision,
                },
                observation: Err("publication was overtaken".to_string()),
            },
        );
        let after = project(&model, &app, 0, 0, 0, false);
        assert_step(&model, &state, &after, "FinishWrite");
        state = after;
        assert_eq!(app.native_config_pending.len(), 1);
        assert!(app.native_config_external_pending.is_some());

        app.native_config_inflight = true;
        let after = project(&model, &app, 2, 1, 0, false);
        assert_step(&model, &state, &after, "StartReconcile");
        state = after;
        app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
            pending_sequence: app.native_config_external_sequence,
            observation: Err("transient stable-read failure".to_string()),
        });
        let after = project(&model, &app, 0, 0, 0, true);
        assert_step(&model, &state, &after, "FailReconcile");
        state = after;
        // The failed reconciliation ANSWERS its queued caller (with the error)
        // rather than leaving it pending — 92a43f0d, pinned by the dedicated
        // `failed_reconciliation_answers_queued_callers_…` test. The queue is
        // therefore EMPTY here; the external observation is its own pending
        // state and survives for the retry below.
        assert_eq!(app.native_config_pending.len(), 0);
        assert!(app.native_config_external_pending.is_some());

        let themes = std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes);
        let reconciled = crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            themes,
        )
        .unwrap();
        app.native_config_inflight = true;
        let after = project(&model, &app, 2, 1, 0, false);
        assert_step(&model, &state, &after, "RetryReconcile");
        state = after;
        app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
            pending_sequence: app.native_config_external_sequence,
            observation: Ok(reconciled),
        });
        let after = project(&model, &app, 0, 0, 1, false);
        assert_step(&model, &state, &after, "AdmitExact");
        assert_eq!(
            app.native_config_service.snapshot().text.as_ref(),
            "serious_mode = true\n"
        );
        assert!(app.native_config_external_pending.is_none());
        // Still empty: the one queued caller was answered at FailReconcile
        // above, and nothing requeued since.
        assert_eq!(app.native_config_pending.len(), 0);

        let mut lost = after;
        lost.insert("dropped_candidate", 1);
        assert!(
            !model.check_invariant("DeferredGenerationNeverLost", &lost),
            "negative control: dropping the failed observation must be rejected"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn matching_prepared_generation_reconciliation_admits_without_requeue() {
        let root = std::env::temp_dir().join(format!(
            "aterm-config-prepared-reconcile-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("aterm.toml");
        std::fs::write(&path, "serious_mode = false\n").unwrap();

        let mut app = App::headless_for_test();
        app.native_config_service =
            crate::native_config_service::VersionedConfigService::load_path(&path).unwrap();
        std::fs::write(&path, "serious_mode = true\n").unwrap();
        let observation =
            crate::native_config_service::VersionedConfigService::observe_path(&path, false)
                .unwrap();
        let themes = std::sync::Arc::clone(&app.native_config_service.snapshot().assets.themes);
        let prepared = crate::native_config_service::VersionedConfigService::prepare_observation(
            observation,
            themes,
        )
        .unwrap();
        let expected_assets = std::sync::Arc::clone(&prepared.assets);
        let generation = crate::native_font_catalog::PreparedConfigGeneration {
            observation: prepared.observation.clone(),
            config: prepared.config.clone(),
            values: prepared.values.clone(),
            assets: std::sync::Arc::clone(&prepared.assets),
            path_feed_fps: prepared.config.path_feed_fingerprints(),
            sparkle: prepared.config.prepare_sparkle_runtime(),
            fonts: None,
            warnings: Vec::new(),
        };

        app.native_config_service.mark_reconciliation_required();
        app.defer_prepared_config_generation(generation);
        let deferred_sequence = app.native_config_external_sequence;
        assert!(app.native_config_external_pending.is_some());
        app.native_config_inflight = true;

        app.finish_native_config_reconciliation(NativeConfigReconciliationCompletion {
            pending_sequence: deferred_sequence,
            observation: Ok(prepared),
        });

        assert!(!app.native_config_inflight);
        assert!(!app.native_config_service.reconciliation_required());
        assert!(app.native_config_external_pending.is_none());
        assert_eq!(app.native_config_external_sequence, deferred_sequence);
        let snapshot = app.native_config_service.snapshot();
        assert_eq!(snapshot.text.as_ref(), "serious_mode = true\n");
        assert!(std::sync::Arc::ptr_eq(&snapshot.assets, &expected_assets));
        assert!(app.config.serious_mode_or_default());
        let _ = std::fs::remove_dir_all(root);
    }

    /// KEEP-STALE MUST NOT SWAP RUNGS. `refresh_active_split_presentation` owns
    /// exactly one title rung — the raw OSC title, or `"aterm"` — and writes it
    /// into `tab.presentation.title`, the deliberately stable model metadata that
    /// `tab_titles`, `refill_strip_titles`, `window_title_identity` and the `tabs`
    /// verb all read back as their FALLBACK rung, and which only a later
    /// structural sync corrects. The window's `tab_title_cache` is a DIFFERENT
    /// rung: `tab_titles` fills it from `resolved_terminal_title_rung`, so it can
    /// hold the operator's `meta set title` or the `~`-abbreviated cwd. This pins
    /// that a contended terminal lock (a flooding pane holds it for a whole ingest
    /// slice, which is why the read is nonblocking at all) keeps THIS tab's own
    /// previous title and never imports the tab-label cache.
    #[test]
    fn contended_split_presentation_keeps_its_own_title_rung() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        crate::term_lock(&term).process(b"\x1b]0;osc-live\x07");

        let active_title = |app: &App| {
            app.windows[&wid]
                .tab_set
                .active()
                .expect("active tab")
                .presentation
                .title
                .clone()
        };

        // Uncontended: the fold publishes its own rung, the live OSC title.
        app.refresh_active_split_presentation(wid);
        assert_eq!(active_title(&app), "osc-live");

        // Seed the tab-label cache with a foreign rung — precisely what
        // `tab_titles` caches for a pane whose shell reported only a cwd.
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .tab_title_cache
            .insert(0, "~/repo".to_string());

        // Contended: the terminal mutex is held across the whole call, so the
        // leaf read must take the WouldBlock arm.
        let flood = crate::term_lock(&term);
        app.refresh_active_split_presentation(wid);
        drop(flood);

        assert_eq!(
            active_title(&app),
            "osc-live",
            "contention must keep this tab's own OSC-title rung, never the \
             tab-label cache's cwd/operator rung"
        );
    }

    /// The same contention, on a tab that has no title yet: the pre-audit blocking
    /// read produced `"aterm"` for a titleless pane, and keep-stale must land on
    /// the same string rather than resurrecting some other tab's label.
    #[test]
    fn contended_split_presentation_without_a_prior_title_falls_back_to_aterm() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        let term = app.pool.get(0).expect("session 0").term.clone();
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .tab_set
            .active_mut()
            .expect("active tab")
            .presentation
            .title
            .clear();
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .tab_title_cache
            .insert(0, "~/repo".to_string());

        let flood = crate::term_lock(&term);
        app.refresh_active_split_presentation(wid);
        drop(flood);

        assert_eq!(
            app.windows[&wid]
                .tab_set
                .active()
                .expect("active tab")
                .presentation
                .title,
            "aterm",
            "an empty stale title keeps the titleless pane's `aterm`"
        );
    }
}
