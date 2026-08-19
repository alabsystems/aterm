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
pub(crate) enum NativeUpdateFactsResult {
    IgnoredStale,
    Deferred(NativeUpdateReconcileFacts),
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

/// Activity revocation gets a much larger budget than a preflight block, and a
/// long-tailed schedule. Rationale from the field: on the machine this feature
/// exists for — a daily driver with an agent streaming shell output into it —
/// revocation is the NORMAL outcome of any single attempt, not evidence of a
/// problem. Three tries then permanent manual-only guaranteed the staged build
/// sat unapplied until the next relaunch, which is exactly what happened.
/// Every attempt is a lossless park/spawn/paint round trip, so the cost of
/// being wrong is bounded and the backoff below keeps it from thrashing.
const MAX_ACTIVITY_REVOKED_CYCLES: u8 = 8;

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
const PHYSICAL_FAILURES_PER_EPOCH: u8 = MAX_PHYSICAL_FAILURE_CYCLES + 1;

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
/// `retry_at: None` — the deadline-less latch that `arm` reads as
/// `SuppressManualOnly` until a strictly newer build ships or the app relaunches.
const PHYSICAL_FAILURE_LIFETIME_ATTEMPTS: u8 =
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
/// `PreparationFailed` is usually the staged bundle failing pre-park verification
/// (structural, certain to recur), but it is also what a screen-carry digest
/// losing a race with a resize reports. One retry, ten minutes out, separates
/// those at a cost of one round trip. A third would be spending round trips to
/// re-confirm a verdict already confirmed.
const STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS: u8 = 2;

const _: () = assert!(
    STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS < PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
    "the structural lane exists to converge SOONER than the transient one; at \
     parity the classification is decoration"
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
    /// [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] spent across
    /// [`MAX_PHYSICAL_FAILURE_EPOCHS`] independent epochs. Automatic apply is done
    /// with these exact bytes; a strictly newer build, a relaunch, or the Version
    /// menu is what moves next.
    Converged,
}

impl PhysicalFailureSchedule {
    /// The deadline to stamp on [`crate::AutoApplyManualOnly`]. `None` is the
    /// latch `lapse_expired_auto_apply_manual_only` will never release — minted
    /// ONLY on convergence, never for a single unlucky handoff.
    #[must_use]
    fn retry_at(self) -> Option<std::time::Instant> {
        match self {
            Self::Retry(at) | Self::StandDown(at) => Some(at),
            Self::Converged => None,
        }
    }
}

/// How long the automatic lane waits between attempts once an artifact's cheap
/// PREFLIGHT-BLOCK budget is spent.
///
/// The same number as [`crate::ACTIVITY_MANUAL_ONLY_LAPSE`] on purpose — both
/// answer the same question ("how long before it is worth disturbing a machine
/// that told us it was busy") — but a distinct name, because this one is a RETRY
/// SPACING on a retained intent, not the lapse deadline of a latch. They are free
/// to diverge; sharing a `const` would make that look like a bug.
const PREFLIGHT_BLOCK_COOLDOWN: std::time::Duration = crate::ACTIVITY_MANUAL_ONLY_LAPSE;

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
///     which is precisely the freshness window
///     [`App::physical_failure_deserves_a_pill`] uses to recognise the automatic
///     lane's own quiet retries — so the person who just asked for the update got
///     silence.
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
}

impl PhysicalFailureShape {
    /// Classify one worker outcome.
    ///
    /// STRUCTURAL, and why each one earns it:
    ///   * `AdoptionMismatch` — the child proved a PTY set the parent does not
    ///     recognise. Every instance this tree has recorded was a cross-version
    ///     disagreement about what the proof covers (a re-serialized manifest, a
    ///     screen digest taken over different bytes, a key count off by one);
    ///     none of them cared how busy the machine was;
    ///   * `PreparationFailed` — raised entirely BEFORE the spawn, and its
    ///     dominant producer is the staged candidate failing pre-park
    ///     verification. A bundle that fails `codesign` fails it again in six
    ///     hours;
    ///   * `ChildDied` — the candidate exited before writing its readiness proof.
    ///     That is the successor image refusing to boot as a successor (a
    ///     malformed inherited handoff, a prearm refusal), which is the strongest
    ///     statement about the new bytes available at this seam.
    ///
    /// TRANSIENT is the rest, and deliberately includes the two that are facts
    /// about a moment: `TimedOut` (a 15 s deadline covering a cold boot, a
    /// blocking `flock`, a bundle swap, a second exec and a full repaint — 4.5 s
    /// measured, until the page cache is cold) and a non-activity-shaped
    /// `Rejected` (a commit-time re-check of state that moves on its own).
    /// `ProofReady` cannot reach a failure lane at all; it fails closed to the
    /// forgiving shape rather than converging an artifact on a state nobody
    /// understands.
    #[must_use]
    fn of_outcome(outcome: crate::UpdateHandoffOutcome) -> Self {
        match outcome {
            crate::UpdateHandoffOutcome::AdoptionMismatch
            | crate::UpdateHandoffOutcome::PreparationFailed
            | crate::UpdateHandoffOutcome::ChildDied => Self::Structural,
            crate::UpdateHandoffOutcome::TimedOut
            | crate::UpdateHandoffOutcome::Rejected
            | crate::UpdateHandoffOutcome::ActivityRevoked
            | crate::UpdateHandoffOutcome::ProofReady => Self::Transient,
        }
    }
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
}

impl HandoffFailureLane {
    /// Classify one returned completion from the two typed facts it carries: the
    /// mode the apply was authorized under, and the worker's `outcome`.
    ///
    /// `revoked_by_activity` is the MAIN THREAD's half of the activity
    /// observation (an activity-shaped rejection it recorded against this
    /// attempt); the worker's half is the `ActivityRevoked` outcome, and either
    /// one is enough. Both are meaningful only for a background attempt — a
    /// person's apply deliberately does not arm the revocation watcher at all.
    ///
    /// THE OUTCOME IS NOW READ RATHER THAN DISCARDED. It used to reach this
    /// decision as a single `activity_revoked: bool` and stop there, so all four
    /// physical kinds were charged one schedule — the one written for the
    /// transient member of the set. See [`PhysicalFailureShape`].
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
        revoked_by_activity: bool,
    ) -> Self {
        if !mode.is_automatic() {
            return Self::Manual;
        }
        if revoked_by_activity || outcome == crate::UpdateHandoffOutcome::ActivityRevoked {
            Self::ActivityRevoked
        } else {
            Self::Physical(PhysicalFailureShape::of_outcome(outcome))
        }
    }

    /// Whether this failure is allowed to touch the automatic lane's scheduling
    /// state at all (its budgets, its latch, its live intent).
    #[must_use]
    fn charges_the_automatic_lane(self) -> bool {
        !matches!(self, Self::Manual)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutomaticRetryKind {
    PreflightBlocked,
    /// The overlap attempt parked, spawned, and was then revoked by USER OR
    /// TERMINAL ACTIVITY before Commit. The rollback is proven lossless (kill,
    /// reap, resume readers; zero bytes consumed by the parent post-park), so
    /// — unlike `PhysicalFailure` — repeating at a later quiet window is as
    /// safe as the first attempt. Budgeted and exponentially spaced so a busy
    /// terminal converges to manual-only instead of parking forever.
    ActivityRevoked,
    PhysicalFailure,
}

/// A bounded retry plan for cheap ordering/preflight races, for lossless
/// activity-revoked overlap returns, and — on a deliberately short budget and long
/// leash — for physical handoff failures, most of which are a missed deadline
/// rather than a broken pair of builds (see [`MAX_PHYSICAL_FAILURE_CYCLES`]).
///
/// For [`AutomaticRetryKind::PhysicalFailure`] the `cycles` argument is the index
/// WITHIN the current epoch, not the artifact's lifetime failure count: `None`
/// here means "this epoch is spent", and whether that ends the epoch or the whole
/// lane is [`App::spend_physical_failure_budget`]'s decision.
#[must_use]
fn automatic_retry_delay(cycles: u8, kind: AutomaticRetryKind) -> Option<std::time::Duration> {
    let budget = match kind {
        AutomaticRetryKind::ActivityRevoked => MAX_ACTIVITY_REVOKED_CYCLES,
        AutomaticRetryKind::PhysicalFailure => MAX_PHYSICAL_FAILURE_CYCLES,
        _ => MAX_AUTOMATIC_UPDATE_CYCLES,
    };
    if cycles >= budget {
        return None;
    }
    let seconds = match (kind, cycles) {
        (AutomaticRetryKind::PreflightBlocked, 0 | 1) => 5,
        (AutomaticRetryKind::PreflightBlocked, _) => 15,
        // Exponential spacing capped at 15 min: each revoked attempt costs a
        // park/spawn/paint round trip, so back off hard while the terminal
        // stays busy. The ≥500 ms quiet-epoch admission still gates every
        // re-attempt, and the budget replenishes after a long idle gap.
        (AutomaticRetryKind::ActivityRevoked, 0) => 2,
        (AutomaticRetryKind::ActivityRevoked, 1) => 8,
        (AutomaticRetryKind::ActivityRevoked, 2) => 30,
        (AutomaticRetryKind::ActivityRevoked, 3) => 60,
        (AutomaticRetryKind::ActivityRevoked, 4) => 120,
        (AutomaticRetryKind::ActivityRevoked, 5) => 300,
        (AutomaticRetryKind::ActivityRevoked, 6) => 600,
        (AutomaticRetryKind::ActivityRevoked, _) => 900,
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
fn decode_dmg_sha256(digest: &str) -> Option<[u8; 32]> {
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
pub(crate) fn durable_update_status(status: aterm_update::UpdateStatus) -> DurableUpdateStatus {
    DurableUpdateStatus {
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
    }
}

fn failed_update_status(build: u64, message: String) -> DurableUpdateStatus {
    DurableUpdateStatus {
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
/// recording-watch focus, load shedding, OS appearance, and the process-wide
/// serious override can all change while the native view generation remains stable.
/// They must nevertheless invalidate retained pixels because Settings previews
/// resolve animation, automatic window appearance, static effect output, and their
/// static/live badge from this exact context.
fn native_motion_revision(motion: crate::native_app::ViewMotionCx) -> u8 {
    (motion.system_reduced as u8)
        | ((motion.focused as u8) << 1)
        | ((motion.performance_reduced as u8) << 2)
        | ((motion.serious as u8) << 3)
        | ((motion.system_dark as u8) << 4)
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
                        self.notice = Some(crate::notice::TransientNotice::update_ready(
                            stage.version.clone(),
                            stage.build,
                            std::time::Instant::now(),
                        ));
                        // The border glow is a motion effect: gated exactly like its
                        // two sibling producers (JUST_UPDATED, the QA seam). This
                        // block was dead until the announcement fix landed, which is
                        // how an ungated producer survived.
                        if self
                            .serious_mode_policy()
                            .allows(crate::motion::SeriousEffect::LevelUp)
                        {
                            self.level_up = Some(crate::level_up::LevelUp::new(
                                stage.build,
                                std::time::Instant::now(),
                            ));
                        }
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

    fn native_ui_full_viewport(
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
    /// in-frame tab-strip rows. The painter and pointer projection share this one
    /// origin so a native card can neither cover the strip nor hit-test one pad off.
    pub(crate) fn native_content_origin_y(&self, wid: WindowId) -> usize {
        let (_, ch) = self.win_cell_size(wid);
        self.win_pad_top(wid)
            .saturating_add(self.win_head(wid))
            .saturating_add(usize::from(self.tab_strip_rows).saturating_mul(ch))
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
        }
    }

    /// Resolve one exact retained native artifact. Paint, pointer routing,
    /// accessibility and inspection consume this same semantic tree and device
    /// destination. Any lifecycle, geometry, service, theme, or document drift
    /// changes the compile stamp and therefore fails closed.
    pub(crate) fn retained_native_leaf_artifact(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        require_presented: bool,
    ) -> Option<RetainedNativeLeafArtifact<'_>> {
        let window = self.windows.get(&wid)?;
        if window.overlay.is_some() {
            return None;
        }
        let plan = self.active_visible_leaf_plan(wid)?;
        let leaf = plan.leaf(view)?;
        let crate::tab_model::View::Native(native) = self.view_store.get(view).copied()? else {
            return None;
        };
        let generation = self.native_runtime.view_generation(view)?;
        let (cw, ch) = self.win_cell_size(wid);
        let scale = window.scale.max(f64::EPSILON);
        let viewport = if plan.leaves.len() == 1 {
            self.native_ui_viewport(wid).ok()?
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
            let Some(artifact) = self.retained_native_leaf_artifact(wid, leaf.view, true) else {
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
                key: Key::Named(key @ (NamedKey::Enter | NamedKey::NumpadEnter | NamedKey::Space)),
                event_type: KeyEventType::Press,
                ..
            } = event
        {
            if self.activate_native_focus(wid).unwrap_or(false) {
                return true;
            }
            // Nothing holds keyboard focus: Return falls back to the page's
            // DEFAULT button (the highlighted Primary — "Install & Relaunch",
            // "Copy Build Information"), the native default-button convention.
            // Space never does; on macOS it only activates the focused control.
            // NumpadEnter only ever arrives from a controller (`key kpenter`);
            // winit folds the physical keypad key to Enter before this seam.
            if matches!(key, NamedKey::Enter | NamedKey::NumpadEnter)
                && self.activate_native_default(wid).unwrap_or(false)
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
            InputEvent::Text(text) | InputEvent::Paste(text) => {
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
                        Key::Named(NamedKey::Enter | NamedKey::NumpadEnter) => {
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
            InputEvent::Wheel { dir_up, lines, .. } => Some(AppEvent::ScrollLines(if *dir_up {
                -(*lines).max(1)
            } else {
                (*lines).max(1)
            })),
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
                    NativeConfigOrigin::View { .. } => None,
                });
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
        self.publish_native_config_origin(
            origin,
            outcome,
            authoritative,
            reconciled_changed,
            synchronization_error.clone(),
        );
        if let Some(error) = synchronization_error {
            self.surface_native_config_lane_error(format!(
                "Config was saved, but its exact disk generation could not be admitted: {error}"
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

    pub(crate) fn finish_native_config_reconciliation(
        &mut self,
        completion: NativeConfigReconciliationCompletion,
    ) {
        self.native_config_inflight = false;
        let prepared = match completion.observation {
            Ok(prepared) => self.rebase_prepared_config_themes(prepared),
            Err(error) => {
                self.native_config_service.mark_reconciliation_required();
                self.surface_native_config_lane_error(format!(
                    "Config reconciliation failed; queued changes remain pending: {error}"
                ));
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
                self.native_config_service.mark_reconciliation_required();
                self.surface_native_config_lane_error(error);
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
                self.native_config_service.mark_reconciliation_required();
                self.surface_native_config_lane_error(error);
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
                self.native_config_service.mark_reconciliation_required();
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

    pub(crate) fn surface_native_config_lane_error(&mut self, message: String) {
        aterm_log::warn!("native config: {message}");
        self.config_notice =
            crate::config_notice::ConfigNotice::new(vec![message], std::time::Instant::now());
        self.request_redraw_all_windows();
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
            self.config_notice =
                crate::config_notice::ConfigNotice::new(vec![message], std::time::Instant::now());
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
    fn execute_native_packages(&mut self, request: PackagesRequest) -> PackagesOutcome {
        let atpkg = crate::co_located_atpkg();
        let verb: &[&str] = match request {
            PackagesRequest::CheckUpdate => &["update"],
            PackagesRequest::InstallDefaultSet => &["install", "--default-set"],
        };
        let Some(atpkg) = atpkg else {
            return PackagesOutcome::Failed {
                message: "no co-located atpkg binary beside this executable".to_string(),
            };
        };
        if !atpkg::manager_enabled() {
            // Same trust posture the binary itself enforces; refusing here is
            // honesty, not authority — atpkg would refuse loudly anyway.
            return PackagesOutcome::Blocked {
                message: "the package manager is inert (no effective root key is available, or ATPKG_DISABLE is set)"
                    .to_string(),
            };
        }
        if self.native_packages_service.busy().is_some() {
            return PackagesOutcome::Blocked {
                message: "a packages operation is already running".to_string(),
            };
        }
        let busy = match request {
            PackagesRequest::CheckUpdate => PackagesBusy::Check,
            PackagesRequest::InstallDefaultSet => PackagesBusy::Install,
        };
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
        let verb: Vec<String> = verb.iter().map(ToString::to_string).collect();
        let spawn = std::thread::Builder::new()
            .name("aterm-packages-verb".into())
            .spawn(move || {
                // atpkg records detailed durable status in status.toml. Keep
                // the process result separately: the old status may predate a
                // failed launch/non-zero exit and must never be presented as
                // the result of this attempt.
                let result = std::process::Command::new(&atpkg)
                    .args(&verb)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let command = match result {
                    Ok(status) if status.success() => {
                        PackagesCommandOutcome::Succeeded { operation: busy }
                    }
                    Ok(status) => PackagesCommandOutcome::Failed {
                        operation: busy,
                        message: format!("atpkg {} exited with {status}", verb.join(" ")),
                    },
                    Err(error) => PackagesCommandOutcome::Failed {
                        operation: busy,
                        message: format!("could not launch atpkg {}: {error}", verb.join(" ")),
                    },
                };
                let report = crate::packages_screen::collect_packages_status(true);
                let completion = PackagesWorkerCompletion::command(report, command);
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

    /// Main-thread half of the packages worker protocol (the packages analogue
    /// of [`Self::finish_native_update_check`]): stale sequences are inert.
    pub(crate) fn finish_native_packages(
        &mut self,
        sequence: u64,
        completion: crate::packages_screen::PackagesWorkerCompletion,
    ) {
        if !self.native_packages_service.finish(sequence, completion) {
            return;
        }
        self.publish_native_packages_state();
    }

    /// Publish the shared packages projection to the Settings controller and
    /// fan the revision out to every Settings view (the packages analogue of
    /// [`Self::publish_native_update_state`]).
    pub(crate) fn publish_native_packages_state(&mut self) {
        let revision = self.native_packages_service.revision();
        let state = self.native_packages_service.state(
            self.config.packages_update_loop_enabled(),
            self.config.packages_enabled(),
            self.config.packages_auto_update(),
            self.config.packages_auto_install(),
            self.package_update_loop_running,
        );
        if !self
            .native_runtime
            .replace_settings_packages(state, revision)
        {
            return;
        }
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
                AppEvent::PackagesChanged { revision },
            );
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
                    message: "automatic updates are disabled on this build".to_string(),
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
                let (owner, repo) = self
                    .config
                    .update
                    .as_ref()
                    .map(|update| (update.owner.clone(), update.repo.clone()))
                    .unwrap_or((None, None));
                let snapshot = self.native_updater_service.snapshot();
                let build = snapshot.current_build;
                let spawn = std::thread::Builder::new()
                    .name("aterm-native-update-check".into())
                    .spawn(move || {
                        let source =
                            aterm_update::Source::resolve(owner.as_deref(), repo.as_deref());
                        let status = aterm_update::check_now(build, &source);
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
        #[cfg(test)]
        self.update_screen_refresh();

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
    /// automatic apply and the ACTIVITY retry budget for that artifact.
    ///
    /// EVERY latch this process installs now carries a deadline: an activity one
    /// after [`crate::ACTIVITY_MANUAL_ONLY_LAPSE`], a physical one at the end of
    /// its epoch ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`]). A `retry_at: None` latch
    /// survives only as the fail-safe for a policy/outcome mismatch that no code
    /// path is supposed to reach; it never lapses, which is the correct answer for
    /// a state nobody understands.
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
        let stage = snapshot.staged.as_ref().filter(|staged| staged.build == build);
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
                let passed = if installed_activation {
                    aterm_update::preverify_installed_for_handoff(current_build, build, &commit)
                } else {
                    aterm_update::preverify_staged_for_handoff(
                        current_build,
                        Some(build),
                        Some(&commit),
                    )
                };
                if let Err(error) = passed.as_ref() {
                    aterm_log::warn!(
                        "update apply: {} build {build} failed pre-park verification: {error}",
                        if installed_activation { "installed" } else { "staged" }
                    );
                }
                *slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some(crate::HandoffPreverification {
                        build,
                        commit,
                        artifact,
                        at: std::time::Instant::now(),
                        passed: passed.is_ok(),
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

    pub(crate) fn arm_native_auto_apply(&mut self, build: u64, digest: &str) -> bool {
        use crate::native_update_auto_intent::{ArmDecision, ArmFacts};

        self.lapse_expired_auto_apply_manual_only();
        let enabled = crate::app_config::update_auto_apply(&self.config)
            && std::env::var_os("ATERM_DEBUG_RELAUNCH_NUDGE").is_none();
        let Some(dmg_sha256) = decode_dmg_sha256(digest) else {
            aterm_log::warn!(
                "refusing to arm automatic update build {build}: malformed DMG identity"
            );
            return false;
        };
        let armed = self.auto_apply_intent;
        let manual_only = self.auto_apply_manual_only;
        match crate::native_update_auto_intent::arm(ArmFacts {
            enabled,
            current_build: self.native_updater_service.snapshot().current_build,
            armed_build: armed.map(|intent| intent.build),
            armed_exact: armed
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256),
            manual_only_exact: manual_only
                .is_some_and(|manual| manual.build == build && manual.dmg_sha256 == dmg_sha256),
            manual_only_build: manual_only.map(|manual| manual.build),
            incoming_build: build,
        }) {
            ArmDecision::Clear => {
                self.auto_apply_intent = None;
                false
            }
            ArmDecision::SuppressManualOnly => {
                self.auto_apply_intent = None;
                false
            }
            ArmDecision::Keep => false,
            ArmDecision::Set(build) => {
                // SEAM 1, HOISTED OUT OF THE PARKED WINDOW: authenticate the
                // staged bundle NOW, while every reader is still live and a
                // cancel costs nothing, instead of as the handoff worker's first
                // action with the whole terminal parked behind it.
                self.spawn_staged_handoff_preverification(build);
                // A different artifact at the same build, or a newer build, owns a
                // distinct budget. Older wakes were suppressed by `arm` above.
                self.auto_apply_manual_only = None;
                let now = std::time::Instant::now();
                // Say so at the default log level. The automatic lane's waits and
                // its first blocked attempt used to be debug-only, so an operator
                // reading aterm.log could not tell an armed-and-waiting lane from a
                // dead one (2026-08-19: twenty minutes staring at a stage that never
                // applied, until a control apply exposed the reason).
                aterm_log::info!(
                    "update auto-apply armed for build {build} ({}…): lands at the first \
                     quiet moment, forced no later than {} s from now",
                    &digest[..digest.len().min(12)],
                    crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE.as_secs()
                );
                self.auto_apply_intent = Some(crate::AutoApplyIntent {
                    build,
                    dmg_sha256,
                    retry_at: now + crate::AUTOMATIC_UPDATE_QUIET_EPOCH,
                    attempts: 0,
                    // The idle-preference deadline is armed ONCE per intent, off
                    // the moment the build became eligible — not off the last
                    // observed activity. Re-deriving it from activity is what an
                    // unbounded wait already was: a machine that never goes quiet
                    // would push the deadline out forever.
                    apply_by: now + crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE,
                });
                true
            }
        }
    }

    /// Attempt a retained automatic apply from an exact event or timer wake.
    /// Every caller honors the same retained deadline and activity gate, so a
    /// duplicate stage wake cannot bypass backoff or spend another attempt.
    pub(crate) fn try_pending_native_auto_apply(&mut self, announce: bool) {
        use crate::native_update_auto_intent::{
            AttemptDisposition, AttemptResult, PollDecision, PollFacts, WaitReason,
        };

        let Some(mut intent) = self.auto_apply_intent else {
            return;
        };
        let now = std::time::Instant::now();
        let deadline_ready = now >= intent.retry_at;
        let work_active = self.native_updater_service.snapshot().active.is_some();
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
                && std::env::var_os("ATERM_DEBUG_RELAUNCH_NUDGE").is_none(),
            deadline_ready,
            current_build,
            target_build: intent.build,
            work_active,
            applying,
            activity_quiet: self.automatic_update_activity_quiet(now),
            activity_grace_expired: now >= intent.apply_by,
            staged_ready,
            staged_build,
            staged_exact_target,
        });
        let (attempt_build, quiet) = match decision {
            PollDecision::Clear => {
                self.auto_apply_intent = None;
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
                    // Re-poll on the quiet cadence, but never past the intent's
                    // own idle-preference deadline: the retry that CROSSES
                    // `apply_by` is the one allowed to land, so it must not be
                    // scheduled after it.
                    WaitReason::Activity => {
                        crate::automatic_update_activity_retry_at(std::time::Instant::now())
                            .min(intent.apply_by)
                    }
                    WaitReason::WorkActive | WaitReason::StagePending => {
                        now + std::time::Duration::from_secs(2)
                    }
                    WaitReason::Deadline => unreachable!("matched above"),
                };
                self.auto_apply_intent = Some(intent);
                return;
            }
            PollDecision::Attempt { build, quiet } => (build, quiet),
        };
        intent.build = attempt_build;
        self.auto_apply_intent = None;
        // A still-busy machine takes the lane that neither waits for idleness
        // nor lets activity revoke the parked window; both are automatic.
        let outcome = self.apply_native_update(if quiet {
            ApplyMode::Automatic
        } else {
            ApplyMode::AutomaticPastGrace
        });
        if let UpdateOutcome::Deferred { reason } = outcome {
            let now = std::time::Instant::now();
            // The idle-preference deadline is ABSOLUTE and is never pushed out
            // here. It used to be rearmed on EVERY deferral, which restarted the
            // whole 2-minute bound each time — and since `apply_by` is the only
            // thing that promotes `Automatic` to `AutomaticPastGrace` above, a
            // machine that keeps deferring could never reach the forced landing
            // at all. The bound existed but was unreachable, which defeats the
            // very escape hatch `AutomaticPastGrace` was introduced to provide.
            let past_grace = now >= intent.apply_by;
            // The anti-spin concern that motivated the rearm is real, so it is
            // answered by PACING instead: past the deadline every poll costs a
            // genuine park/spawn round trip, so space those by the grace window
            // rather than the 500 ms quiet cadence. Before the deadline the
            // quiet cadence is what makes a prompt idle landing possible.
            intent.retry_at = now
                + if past_grace {
                    crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE
                } else {
                    crate::AUTOMATIC_UPDATE_QUIET_EPOCH
                };
            self.auto_apply_intent = Some(intent);
            if past_grace {
                // Paced by the grace window, so this is one line per two minutes at
                // most — and it is the line that says the lane is alive but held.
                aterm_log::info!(
                    "update auto-apply for build {} is past its idle grace and still \
                     deferred: {reason}",
                    intent.build
                );
            } else {
                aterm_log::debug!(
                    "automatic update retained exact intent after activity deferral: {reason}"
                );
            }
            return;
        }
        intent.attempts = intent.attempts.saturating_add(1);
        let disposition = crate::native_update_auto_intent::finish(match &outcome {
            UpdateOutcome::Accepted => AttemptResult::Accepted,
            UpdateOutcome::InstalledNeedsRelaunch { .. } => AttemptResult::InstalledNeedsRelaunch,
            UpdateOutcome::Blocked { .. } => AttemptResult::Blocked,
            UpdateOutcome::Failed { .. } => AttemptResult::Failed,
            UpdateOutcome::Deferred { .. } => unreachable!("returned above"),
        });
        match (disposition, outcome) {
            (AttemptDisposition::Complete, UpdateOutcome::Accepted) => {
                // A successful replacement never returns. Accepted here can only be a
                // joined in-flight request, whose owner now owns completion/recovery.
                aterm_log::info!(
                    "update auto-apply accepted for build {} after {} attempt(s)",
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
                    self.surface_update_apply_outcome(
                        "automatic",
                        UpdateOutcome::Blocked {
                            reasons: reasons.clone(),
                        },
                        false,
                    );
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
                    // The LOG says it every cooldown; the UI says it once. A log
                    // line is pull, a pill is push.
                    aterm_log::warn!("{message}");
                    if first_exhaustion {
                        // THE OLD PILL WAS A LIE ABOUT A STATE THAT NO LONGER
                        // EXISTS. "Update paused — manual retry" told the user the
                        // automatic lane had given up and that clicking was now
                        // the only way out; neither half is true. Say the two
                        // things a user can act on: it comes back by itself, and
                        // there is a control that does it right now.
                        self.surface_nonmodal_update_status(
                            "↑ Update waiting — retries on its own, or use the Version menu",
                        );
                    }
                }
            }
            (AttemptDisposition::ManualOnly, UpdateOutcome::Failed { message }) => {
                // A returned physical handoff can park/read/checkpoint sessions, so it
                // is retried rarely and only twice per epoch — but it IS retried, and
                // an epoch always ends. `retry_at: None` here used to be minted for a
                // SINGLE missed 15 s handoff deadline, which disabled automatic
                // in-session apply for that build outright; it is now reachable only
                // from `PhysicalFailureSchedule::Converged`, i.e. nine failures across
                // three independent epochs, which is the state where the automatic lane
                // genuinely has nothing left to try.
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
                let schedule = self.spend_physical_failure_budget(
                    intent.build,
                    intent.dmg_sha256,
                    PhysicalFailureShape::Transient,
                );
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: intent.build,
                    dmg_sha256: intent.dmg_sha256,
                    retry_at: schedule.retry_at(),
                });
                let wait_secs = |at: std::time::Instant| {
                    at.saturating_duration_since(std::time::Instant::now())
                        .as_secs()
                };
                match schedule {
                    PhysicalFailureSchedule::Retry(at) => aterm_log::info!(
                        "update auto-apply: physical handoff failure on build {} \
                         (attempt {}); automatic apply is latched off until the retry \
                         window in ~{}s, then eligible again",
                        intent.build,
                        intent.attempts,
                        wait_secs(at)
                    ),
                    PhysicalFailureSchedule::StandDown(at) => aterm_log::warn!(
                        "update auto-apply: physical handoff failure on build {} \
                         (attempt {}) exhausted this epoch's retry budget; standing down \
                         for ~{}s, then a fresh epoch",
                        intent.build,
                        intent.attempts,
                        wait_secs(at)
                    ),
                    PhysicalFailureSchedule::Converged => aterm_log::warn!(
                        "update auto-apply: physical handoff failure on build {} \
                         (attempt {}) spent all {} attempts across {} epochs; automatic \
                         apply for this artifact is done — the Version menu remains",
                        intent.build,
                        intent.attempts,
                        PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
                        MAX_PHYSICAL_FAILURE_EPOCHS
                    ),
                }
                self.surface_update_apply_outcome(
                    // The label used to say "manual retry required" even when
                    // `retry_at` had just scheduled another automatic attempt —
                    // it described the exhausted case for all of them. The label
                    // now names which of the three schedules answered, and the
                    // PILL (which is the part a user sees) is chosen by
                    // `physical_failure_deserves_a_pill` from the same budget.
                    match schedule {
                        PhysicalFailureSchedule::Retry(_) => "automatic · retrying later",
                        PhysicalFailureSchedule::StandDown(_) => {
                            "automatic · standing down, then retrying"
                        }
                        PhysicalFailureSchedule::Converged => "automatic · out of retries",
                    },
                    UpdateOutcome::Failed { message },
                    false,
                );
            }
            (_, outcome) => {
                // A future policy/outcome mismatch must fail safe, never panic in the
                // input loop or accidentally arm an unbounded physical retry.
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: intent.build,
                    dmg_sha256: intent.dmg_sha256,
                    retry_at: None,
                });
                aterm_log::warn!(
                    "automatic update policy mismatch; clearing timer and requiring manual retry"
                );
                self.surface_update_apply_outcome("automatic policy fallback", outcome, false);
            }
        }
    }

    fn publish_native_update_state(&mut self) {
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
            return NativeUpdateFactsResult::Deferred(facts);
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
        let NativeUpdateReconcileFacts {
            _ticket: _,
            observation_sequence: _,
            observed_at: _,
            durable,
            installed,
        } = facts;
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
                // The retired download's manual-only latch retires with it (a latch
                // naming a STRICTLY NEWER artifact survives — same rule as the
                // `InstalledNeedsRelaunch` arm below).
                if self
                    .auto_apply_manual_only
                    .is_some_and(|manual| manual.build <= retired_build)
                {
                    self.auto_apply_manual_only = None;
                }
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
                        if self
                            .auto_apply_manual_only
                            .is_some_and(|manual| manual.build <= build)
                        {
                            self.auto_apply_manual_only = None;
                        }
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
                            "Build {build} is already installed on disk; activating it in place \
                             (a relaunch also picks it up)"
                        );
                        aterm_log::warn!("update sync: {message}");
                    }
                    DurableStageDisposition::Retired => {
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
                    "build {} is already installed on disk; activating it in place",
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

    pub(crate) fn apply_native_update(&mut self, mode: ApplyMode) -> UpdateOutcome {
        // Only the still-inside-the-window automatic lane defers here.
        // `AutomaticPastGrace` already spent that window waiting for an idle
        // moment that never came (see `AUTOMATIC_UPDATE_ACTIVITY_GRACE`).
        if mode == ApplyMode::Automatic
            && !self.automatic_update_activity_quiet(std::time::Instant::now())
        {
            return UpdateOutcome::Deferred {
                reason: "terminal input/output is still inside the quiet epoch".to_string(),
            };
        }
        let start = self.native_updater_service.begin_apply_preflight(mode);
        let ticket = match start {
            ApplyPreflightStart::Inspect(ticket) => ticket,
            ApplyPreflightStart::Joined(_) => return UpdateOutcome::Accepted,
            ApplyPreflightStart::Disabled => {
                return UpdateOutcome::Blocked {
                    reasons: vec!["Automatic updates are disabled on this build".to_string()],
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
                ClosePreflight::Blocked(vec![
                    "Review or discard unsaved native-app work before relaunching".to_string(),
                ]),
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
                    self.apply_staged_update_now(safety_token, mode, Some(worker_attempt))
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
    ///   * [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] spent — `retry_at: None`, the
    ///     latch `arm` reads as `SuppressManualOnly` for these exact bytes until a
    ///     strictly newer build ships or the app relaunches. Nine failures across
    ///     three independent epochs and ~14 hours is evidence about the artifact,
    ///     not about the machine's afternoon, and the user is told once (see
    ///     [`App::physical_failure_deserves_a_pill`]) instead of every 40 minutes
    ///     forever.
    ///
    /// Keyed by (build, dmg) so a different artifact starts clean, and gated by
    /// [`PHYSICAL_RETRY_BUDGET_REPLENISH`] so half a day with no physical failure
    /// at all for these bytes starts the whole schedule over.
    ///
    /// `shape` chooses WHICH of the two answers above this failure has earned. The
    /// counter itself is shared and shape-blind on purpose — it counts physical
    /// failures for these exact bytes, which is a fact neither lane disputes — so
    /// evidence carries across the classification in the direction that matters: a
    /// structural failure arriving on an artifact that has already burned its
    /// transient budget converges immediately rather than buying a fresh pair of
    /// attempts.
    fn spend_physical_failure_budget(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        shape: PhysicalFailureShape,
    ) -> PhysicalFailureSchedule {
        let now = std::time::Instant::now();
        let spent = self
            .auto_apply_physical_retry
            .filter(|retry| {
                retry.build == build
                    && retry.dmg_sha256 == dmg_sha256
                    && now.duration_since(retry.last_attempt) < PHYSICAL_RETRY_BUDGET_REPLENISH
            })
            .map_or(0, |retry| retry.cycles);
        self.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            cycles: spent.saturating_add(1),
            last_attempt: now,
        });
        // `spent` is the number of physical failures these bytes had BEFORE this
        // one, so it is also this failure's 0-based lifetime index. Saturating
        // arithmetic keeps a `u8` that somehow ran away pinned at Converged rather
        // than wrapping back into a fresh budget.
        if spent >= PHYSICAL_FAILURE_LIFETIME_ATTEMPTS.saturating_sub(1) {
            return PhysicalFailureSchedule::Converged;
        }
        if shape == PhysicalFailureShape::Structural {
            // ONE CONFIRMING RETRY, THEN THE LANE IS DONE WITH THESE BYTES. The
            // epoch machinery below is deliberately skipped rather than
            // parameterized: an epoch is a re-sample of the MACHINE, and there is
            // nothing about the machine left to re-sample once the candidate has
            // told us twice that it cannot become this process's successor.
            if spent >= STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS.saturating_sub(1) {
                return PhysicalFailureSchedule::Converged;
            }
            // The same first rung as the transient schedule: the difference
            // between the lanes is how many rungs there are, not how far apart the
            // first two sit. Fail-closed if that rung is ever legislated away —
            // converging is the safe answer for a structural failure with no
            // schedule to ride.
            return automatic_retry_delay(0, AutomaticRetryKind::PhysicalFailure)
                .map_or(PhysicalFailureSchedule::Converged, |delay| {
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

    /// Whether a physical apply failure for `staged_build` is one of the two the
    /// user is told about, or one of the ones that pass in silence.
    ///
    /// Owner instruction: do not notify a user on a schedule for a failure that is
    /// not going to fix itself. The physical lane can cost nine failures before it
    /// converges, and painting "↑ Update delayed — retries on its own" on each of
    /// them is a pill every ~40 minutes for most of a day, describing a state the
    /// user cannot do anything about — the lane is already coming back by itself.
    ///
    /// So the same shape the preflight-block lane already proved: THE LOG SAYS IT
    /// EVERY TIME, THE UI SAYS IT TWICE.
    ///   * the FIRST physical failure for these bytes — something visibly did not
    ///     happen, and one pill is how the user learns the lane exists;
    ///   * convergence — which does not come through here at all, because a
    ///     converged artifact has `retry_at: None` and takes
    ///     `surface_update_apply_outcome`'s "see Version menu" branch, the one
    ///     that names a control the user can actually press.
    ///
    /// Everything between the two is quiet.
    ///
    /// Answering `true` for an artifact with no physical record is deliberate:
    /// activity-revoked latches and the fail-safe policy-mismatch arm keep their
    /// existing single pill, since neither spends this budget.
    ///
    /// THE FRESHNESS TERM IS NOT DECORATION — it is what keeps the suppression
    /// pointed at the automatic lane. Only the lanes that spend the budget
    /// (`finish_native_auto_apply_attempt` and the automatic completions) call
    /// [`Self::spend_physical_failure_budget`] IMMEDIATELY before surfacing, so
    /// for them the record is microseconds old. A PERSON's failure — the Version
    /// menu, the palette, or an `aterm-ctl update apply` control request — spends
    /// nothing, so it inherits a mid-budget record that is at least one retry
    /// interval old and speaks: a person who just asked for something is exactly
    /// who should be told it did not happen. The physical schedule's own minimum
    /// spacing is 600 s, so any window far below that separates the two without a
    /// new plumbing parameter.
    ///
    /// THAT SENTENCE WAS FALSE UNTIL THE COMPLETION PATH CARRIED ITS `ApplyMode`.
    /// A person's RETURNED handoff (not the submission-time failures this prose was
    /// written against) went through the same reduction as a background one, spent
    /// the budget microseconds before surfacing, and was therefore silenced by the
    /// very rule written to protect it. [`HandoffFailureLane`] is what makes the
    /// premise true; the residual is a genuine coincidence window — a person whose
    /// apply fails within `JUST_SPENT` of an automatic failure for the same bytes
    /// still inherits a fresh record — which the automatic schedule's 600 s minimum
    /// spacing makes rare and which no observation available here can separate.
    pub(crate) fn physical_failure_deserves_a_pill(&self, staged_build: u64) -> bool {
        /// How recently the physical budget must have been spent for this failure
        /// to be the one that spent it. Three orders of magnitude under the 600 s
        /// minimum retry spacing, so no real retry can be mistaken for "just now".
        const JUST_SPENT: std::time::Duration = std::time::Duration::from_secs(5);

        self.auto_apply_physical_retry
            .filter(|retry| {
                retry.build == staged_build && retry.last_attempt.elapsed() < JUST_SPENT
            })
            .is_none_or(|retry| retry.cycles <= 1)
    }

    /// Consume one activity-revoked overlap retry cycle for this exact artifact
    /// and re-arm the automatic intent at its exponentially spaced deadline.
    /// `None` = the budget is exhausted (or the artifact identity is malformed);
    /// the caller then falls back to the manual-only latch. The cycle counter
    /// lives on `auto_overlap_retry`, keyed by (build, dmg) — duplicate or
    /// reordered completions for the same artifact can never mint fresh budget,
    /// and a different artifact starts a fresh budget by construction.
    fn arm_activity_revoked_overlap_retry(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<std::time::Duration> {
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        let build = attempt.target_build();
        let now = std::time::Instant::now();
        let cycles = self
            .auto_overlap_retry
            .filter(|retry| {
                // REPLENISHING BUDGET: a busy stretch must not permanently
                // retire an artifact. Once the terminal has gone long enough
                // without a revoked attempt, start the schedule over.
                retry.build == build
                    && retry.dmg_sha256 == dmg_sha256
                    && now.duration_since(retry.last_attempt)
                        < crate::ACTIVITY_RETRY_BUDGET_REPLENISH
            })
            .map_or(0, |retry| retry.cycles);
        let delay = automatic_retry_delay(cycles, AutomaticRetryKind::ActivityRevoked)?;
        self.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            cycles: cycles.saturating_add(1),
            last_attempt: now,
        });
        self.auto_apply_manual_only = None;
        let retry_at = std::time::Instant::now() + delay;
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at,
            attempts: cycles,
            // The idle-preference window restarts AFTER the backoff, not from
            // now: the intent is not even eligible until `retry_at`, so a grace
            // measured from here would already be spent and the retry would
            // force on its first poll — turning a backoff into an immediate
            // re-attempt of the thing activity just revoked.
            apply_by: retry_at + crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE,
        });
        Some(delay)
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
            // `reconcile_returned_native_apply_with_facts`. That policy was
            // unreachable: it is gated behind a completion carrying worker
            // facts, and NO `UpdateHandoffCompletion` has ever carried them —
            // every construction site sets `reconcile: None`, so this lane took
            // every automatic overlap failure and installed a `retry_at: None`
            // latch that `lapse_expired_auto_apply_manual_only` can never
            // expire. One revoked overlap therefore retired automatic apply for
            // the process lifetime, and the whole `MAX_ACTIVITY_REVOKED_CYCLES`
            // schedule never ran once in production.
            if lane == HandoffFailureLane::ActivityRevoked
                && let Some(delay) = self.arm_activity_revoked_overlap_retry(attempt)
            {
                self.publish_native_update_state();
                aterm_log::info!(
                    "update apply: activity revoked the overlap; automatic retry in {delay:?}"
                );
                return UpdateOutcome::Deferred { reason: message };
            }
            if let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) {
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    build: attempt.target_build(),
                    dmg_sha256,
                    retry_at: self.physical_completion_retry_at(
                        attempt.target_build(),
                        dmg_sha256,
                        lane,
                    ),
                });
            }
            self.auto_apply_intent = None;
            self.publish_native_update_state();
        }
        UpdateOutcome::Failed { message }
    }

    /// When automatic apply may try this artifact again after a RETURNED handoff
    /// that is not getting an activity-revoked re-arm.
    ///
    /// THIS IS THE LANE THE FINDING WAS ABOUT. All four physical outcomes come
    /// back here, and this
    /// used to answer `retry_at: None` for every one of them — a latch `arm`
    /// honours as `SuppressManualOnly` forever and
    /// `lapse_expired_auto_apply_manual_only` cannot release. So the commonest
    /// physical failure there is (a 15 s handoff deadline missed on a cold page
    /// cache) retired automatic in-session apply for that build until a strictly
    /// newer one shipped: the exact "staged, only applies on relaunch" symptom the
    /// seamless lane exists to delete, reachable from ONE unlucky moment.
    ///
    /// Two causes, two clocks, and only one of them may ever answer "never":
    ///   * ACTIVITY exhausted its own (large, exponential) budget — the artifact
    ///     is fine and the terminal was merely busy, so the short
    ///     [`crate::ACTIVITY_MANUAL_ONLY_LAPSE`] applies and the answer is always
    ///     a deadline;
    ///   * a PHYSICAL failure — expensive to repeat, so it spends the shared
    ///     budget: a retry inside the epoch, a stand-down between epochs, and
    ///     `None` once [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] are gone. That last
    ///     case is the deliberate difference from the previous design, which could
    ///     only ever answer with a deadline and therefore retried a structurally
    ///     broken artifact for the life of the process.
    ///
    /// Only the two AUTOMATIC lanes reach here — a person's failure returns
    /// before the latch is stamped at all, because neither clock is about them.
    fn physical_completion_retry_at(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        lane: HandoffFailureLane,
    ) -> Option<std::time::Instant> {
        debug_assert!(
            lane.charges_the_automatic_lane(),
            "a person-initiated failure must never reach the automatic schedule"
        );
        match lane {
            HandoffFailureLane::ActivityRevoked => {
                Some(std::time::Instant::now() + crate::ACTIVITY_MANUAL_ONLY_LAPSE)
            }
            HandoffFailureLane::Physical(shape) => self
                .spend_physical_failure_budget(build, dmg_sha256, shape)
                .retry_at(),
            // UNREACHABLE — both callers return before the latch is stamped for a
            // person's failure, and the debug assert above says so. If a future
            // caller forgets, charging nothing is the half that matters (a human's
            // retries must never converge the background lane), and `None` is this
            // file's standing answer for a state nobody understands: a latch that
            // never lapses, rather than a schedule invented for it here.
            HandoffFailureLane::Manual => None,
        }
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
                // ACTIVITY-REVOKED + budget remaining: the exact stage was
                // re-armed on disk and the rollback was lossless, so schedule
                // one bounded quiet-window re-attempt instead of latching
                // manual-only. Exhausted budget (or a genuine failure, where the
                // lane is `Physical`) takes the sticky manual latch.
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
                if let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) {
                    // Same two clocks as the sibling reaped-abort lane, through
                    // the same helper — see `physical_completion_retry_at`.
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        build: attempt.target_build(),
                        dmg_sha256,
                        retry_at: self.physical_completion_retry_at(
                            attempt.target_build(),
                            dmg_sha256,
                            lane,
                        ),
                    });
                }
                self.auto_apply_intent = None;
                self.publish_native_update_state();
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::Failed { message })
            }
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build } => {
                self.auto_apply_intent = None;
                self.auto_apply_manual_only = None;
                self.publish_native_update_state();
                // A newer artifact is imported only when it exceeds the canonical
                // installed build; the fact reducer enforces that floor.
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: "The update is already on disk; aterm activates it at the next \
                              quiet moment (a relaunch also picks it up)"
                        .to_string(),
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
                "Review Settings Drafts: {settings_drafts} Settings view(s) have unsaved text"
            ));
        }
        if dirty_documents > 0 {
            blockers.push(format!(
                "Checkpoint Drafts: {dirty_documents} document(s) have uncheckpointed edits"
            ));
        }
        if failed_checkpoints > 0 {
            blockers.push(format!(
                "Retry: {failed_checkpoints} document checkpoint(s) previously failed"
            ));
        }
        if pending_checkpoints > 0 {
            blockers.push(format!(
                "Wait: {pending_checkpoints} document checkpoint(s) are still running"
            ));
        }
        if self.pending_restore.is_some() || !self.seamless_adopt.is_empty() {
            blockers.push("Wait for session restore to finish before relaunching".to_string());
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
                    reasons: vec![
                        "Review or discard unsaved native-app work before relaunching".to_string(),
                    ],
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
                match self.apply_staged_update_now(token, ApplyMode::Immediate, None) {
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
        accepted && self.pending_update_handoff.is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

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
                NativeConfigOrigin::View { .. } | NativeConfigOrigin::Control { .. } => {
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
                NativeConfigOrigin::View { .. } | NativeConfigOrigin::Control { .. } => {
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
                    NativeConfigOrigin::View { .. } | NativeConfigOrigin::Control { .. } => {
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
        assert!(app.config_notice.as_ref().is_some_and(|notice| {
            notice
                .lines
                .iter()
                .any(|line| line.contains("aterm.toml changed first"))
        }));
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
        let staged = app
            .native_updater_service
            .snapshot()
            .staged
            .clone()
            .expect("an installed bundle newer than the process is imported as an activation stage");
        assert_eq!(staged.build, build, "the activation names the installed build");
        assert_eq!(staged.commit.as_deref(), Some(commit), "…and its sealed commit");
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
        let staged = app.native_updater_service.snapshot().staged.clone().unwrap();
        assert!(
            app.auto_apply_intent.is_some_and(|intent| intent.build == build
                && intent.dmg_sha256 == decode_dmg_sha256(&staged.dmg_sha256).unwrap()),
            "the activation the returned facts imported is ARMED, not merely described"
        );
    }

    #[test]
    fn a_freshly_imported_stage_is_announced_once_and_then_stays_quiet() {
        let mut app = App::headless_for_test();
        let running = app.native_updater_service.snapshot().current_build;
        let build = running + 1;
        assert!(app.notice.is_none() && app.level_up.is_none() && app.relaunch.is_none());
        let facts = || {
            reconcile_facts_with_installed(
                1,
                1,
                Some(DurableUpdateStatus {
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
                }),
                Some(installed_update(running)),
            )
        };
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, facts());
        assert_eq!(
            app.native_updater_service.snapshot().staged.as_ref().map(|s| s.build),
            Some(build),
            "PRECONDITION: the stage imported"
        );
        assert!(
            app.notice.as_ref().is_some_and(crate::notice::TransientNotice::is_update_ready),
            "a newly imported stage shows the Update ready toast (present: {})",
            app.notice.is_some()
        );
        assert!(app.level_up.is_some(), "…and the level-up");
        // The SAME stage again is not news.
        app.notice = None;
        app.level_up = None;
        let mut again = facts();
        again.observation_sequence = 2;
        again._ticket = NativeUpdateReconcileTicket {
            request_sequence: 2,
        };
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::StageAvailable, again);
        assert!(app.notice.is_none() && app.level_up.is_none(), "a repeat import is quiet");
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
            outcome.contains("already installed") && outcome.contains("activating"),
            "the durable outcome says the bytes are on disk and being activated, got {outcome:?}"
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
        assert!(app.deferred_native_update_reconcile.is_some(), "PRECONDITION: parked");
        // The check completes WITH a stage.
        app.finish_native_update_check(ticket, status(Some(build), 0));
        assert_eq!(
            app.native_updater_service.snapshot().staged.as_ref().map(|s| s.build),
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
        assert!(app.native_stage_imported_at.is_some(), "PRECONDITION: the import is floored");
        // …and its wake lands after the check completed (reducer free, not parked).
        app.finish_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh, early);
        assert_eq!(
            app.native_updater_service.snapshot().staged.as_ref().map(|s| s.build),
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
            app.native_updater_service.snapshot().staged.as_ref().map(|s| s.build),
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
            app.native_updater_service.snapshot().staged.as_ref().map(|s| s.build) == Some(build),
            "the newer facts imported the stage, got outcome {outcome:?}"
        );
        assert!(
            app.notice.is_some() || app.native_updater_service.snapshot().phase != UpdaterPhase::Staged,
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
            apply_by: retry_at + crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE,
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
            apply_by: std::time::Instant::now() + std::time::Duration::from_secs(30),
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
                activity_grace_expired: false,
                staged_ready: true,
                staged_build: Some(target_build),
                staged_exact_target: true,
            }),
            crate::native_update_auto_intent::PollDecision::Attempt {
                build: target_build,
                quiet: true
            }
        );

        let retry_now = std::time::Instant::now();
        assert!(crate::automatic_update_activity_retry_at(retry_now) > retry_now);
    }

    /// Seamless seam 4 (retry budget): an activity-revoked overlap schedules
    /// bounded, exponentially spaced automatic re-attempts capped at 15 min,
    /// then exhausts to `None` so the caller latches a LAPSING manual-only
    /// state. Genuine physical failures never mint a timer retry at any cycle
    /// count, and a preflight block keeps its own much smaller budget.
    #[test]
    fn activity_revoked_retry_budget_spaces_exponentially_then_exhausts() {
        use std::time::Duration;
        let schedule = [2, 8, 30, 60, 120, 300, 600, 900];
        assert_eq!(
            schedule.len(),
            usize::from(MAX_ACTIVITY_REVOKED_CYCLES),
            "the schedule must cover exactly the budget"
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
        assert_eq!(
            automatic_retry_delay(
                MAX_ACTIVITY_REVOKED_CYCLES,
                AutomaticRetryKind::ActivityRevoked
            ),
            None
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
        // A physical failure gets a far smaller budget than activity revocation —
        // it is not free to repeat — but it is no longer zero, and it is spent
        // strictly sooner than the activity budget at every cycle.
        for cycles in 0..MAX_PHYSICAL_FAILURE_CYCLES {
            assert!(
                automatic_retry_delay(cycles, AutomaticRetryKind::PhysicalFailure).is_some(),
                "physical cycle {cycles} is inside the budget"
            );
        }
        for cycles in MAX_PHYSICAL_FAILURE_CYCLES..=MAX_ACTIVITY_REVOKED_CYCLES {
            assert_eq!(
                automatic_retry_delay(cycles, AutomaticRetryKind::PhysicalFailure),
                None,
                "a spent physical budget must never mint another timer retry"
            );
        }
        // Both budgets are constants, so this ordering is decided at COMPILE time —
        // a const block says so by failing the build rather than one test run.
        const {
            assert!(
                MAX_PHYSICAL_FAILURE_CYCLES < MAX_ACTIVITY_REVOKED_CYCLES,
                "a lossless revocation must always be retried more readily than a \
                 physical failure"
            )
        };
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
            let verdict =
                app.spend_physical_failure_budget(build, dmg, PhysicalFailureShape::Transient);
            verdicts.push(verdict);
            if verdict == PhysicalFailureSchedule::Converged {
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
                PhysicalFailureSchedule::Converged => "converged",
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
        // mint a fourth epoch out of the saturating counter.
        for extra in 0..4 {
            assert_eq!(
                app.spend_physical_failure_budget(build, dmg, PhysicalFailureShape::Transient),
                PhysicalFailureSchedule::Converged,
                "late completion {extra} must not resurrect the lane"
            );
        }
        // A DIFFERENT artifact is a different question and starts clean.
        assert!(matches!(
            app.spend_physical_failure_budget(
                build,
                [0xcd_u8; 32],
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
            app.spend_physical_failure_budget(build, dmg, PhysicalFailureShape::Transient),
            PhysicalFailureSchedule::Retry(_)
        ));
        assert!(matches!(
            app.spend_physical_failure_budget(build, dmg, PhysicalFailureShape::Transient),
            PhysicalFailureSchedule::Retry(_)
        ));
        assert_eq!(
            app.spend_physical_failure_budget(build, dmg, PhysicalFailureShape::Structural),
            PhysicalFailureSchedule::Converged,
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
            cycles: MAX_PHYSICAL_FAILURE_CYCLES,
            last_attempt: std::time::Instant::now(),
        });
        // An already-expired latch, i.e. one that lapses on this very call.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
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

        // Exhaust the bounded budget, so every cycle AND the fallback latch are
        // covered: the first cycles must arm a retry, and once the budget is
        // spent the latch that replaces it must still carry a deadline.
        for cycle in 0..=usize::from(MAX_ACTIVITY_REVOKED_CYCLES) {
            ticket.make_current_apply_for_test(&mut app.native_updater_service);
            app.abort_reaped_native_apply_before_reconcile(
                &ticket,
                "overlap handoff failed safely: handoff proof ended ActivityRevoked".to_string(),
                HandoffFailureLane::ActivityRevoked,
            );
            assert!(
                app.auto_apply_intent.is_some()
                    || app
                        .auto_apply_manual_only
                        .is_some_and(|manual| manual.retry_at.is_some()),
                "cycle {cycle}: an activity-revoked completion left neither a live \
                 retry intent nor a latch that can lapse — automatic apply is \
                 retired until relaunch"
            );
        }

        // The budget is spent, so this is the fallback latch specifically.
        let manual = app
            .auto_apply_manual_only
            .expect("a spent budget falls back to the manual-only latch");
        assert!(
            manual.retry_at.is_some(),
            "an activity-caused latch MUST carry a lapse deadline; \
             `retry_at: None` is reserved for genuine failures"
        );
        assert!(
            app.lapse_expired_auto_apply_manual_only()
                || manual
                    .retry_at
                    .is_some_and(|at| at > std::time::Instant::now()),
            "the deadline must be either already lapsable or still in the future"
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
    #[test]
    fn overlap_retry_budget_rearms_intent_per_artifact_and_exhausts_to_manual_only() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );

        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build: 77,
            dmg_sha256: [0xab; 32],
            retry_at: None,
        });
        let expected = [
            Some(std::time::Duration::from_secs(2)),
            Some(std::time::Duration::from_secs(8)),
            Some(std::time::Duration::from_secs(30)),
            Some(std::time::Duration::from_secs(60)),
            Some(std::time::Duration::from_secs(120)),
            Some(std::time::Duration::from_secs(300)),
            Some(std::time::Duration::from_secs(600)),
            Some(std::time::Duration::from_secs(900)),
            None,
        ];
        for (cycle, want) in expected.into_iter().enumerate() {
            let got = app.arm_activity_revoked_overlap_retry(&ticket);
            assert_eq!(got, want, "cycle {cycle}");
            if want.is_some() {
                let intent = app.auto_apply_intent.expect("intent re-armed");
                assert_eq!(intent.build, 77);
                assert_eq!(intent.dmg_sha256, [0xab; 32]);
                assert!(
                    app.auto_apply_manual_only.is_none(),
                    "a live retry budget clears the manual-only latch"
                );
            }
        }
        assert_eq!(
            app.auto_overlap_retry.map(|retry| retry.cycles),
            Some(MAX_ACTIVITY_REVOKED_CYCLES),
            "duplicate completions cannot mint fresh budget"
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
            retry_at: None,
        });
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(app.auto_apply_manual_only.is_some());

        // ACTIVITY-shaped, still within its window: holds.
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256: [0xab; 32],
            cycles: MAX_ACTIVITY_REVOKED_CYCLES,
            last_attempt: std::time::Instant::now(),
        });
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
            retry_at: Some(std::time::Instant::now() + std::time::Duration::from_secs(3600)),
        });
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(app.auto_apply_manual_only.is_some());

        // ACTIVITY-shaped, deadline passed: lapses, AND the artifact's retry
        // budget starts over so the next attempt is not instantly exhausted.
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
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

    /// A RETIRED BUILD MUST NOT KEEP ITS LATCH — AND A LATCH ABOUT A DIFFERENT
    /// ARTIFACT MUST SURVIVE THE RETIREMENT.
    ///
    /// `reconcile_native_update_facts` cleared `auto_apply_intent` for a stage it
    /// had just retired as INSTALLED and left `auto_apply_manual_only` standing for
    /// the same bytes. Nothing observable went wrong, because the latch is keyed by
    /// (build, dmg) and `arm` independently refuses a build that is no longer newer
    /// — but that is a second mechanism agreeing, not a reason to keep state
    /// asserting something false about the world, and "a latch that outlived its
    /// reason" is the shape of every bug this lane has had.
    ///
    /// The NEWER case is asserted in the same breath because it is what stops the
    /// repair from becoming its own bug: completions can land out of order, and a
    /// latch minted for a newer artifact must not be handed back an automatic lane
    /// it just latched off — the same rule the sibling `Retired` arm in
    /// `reconcile_returned_native_apply_with_facts` already follows.
    #[test]
    fn retiring_an_installed_build_clears_its_own_latch_and_no_one_else_s() {
        for latched_ahead in [0_u64, 1] {
            let mut app = App::headless_for_test();
            let build = app.native_updater_service.snapshot().current_build + 1;
            stage_one_build_for_test(&mut app, build);
            app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                build: build + latched_ahead,
                dmg_sha256: [0xab; 32],
                retry_at: None,
            });

            // The survivor state a swapped-but-uncommitted handoff leaves behind:
            // the canonical bundle carries this build and its receipt names this
            // exact artifact, so the reducer retires the stage as installed.
            app.finish_native_update_reconcile(
                NativeUpdateReconcilePurpose::Startup,
                reconcile_facts_with_installed(
                    11,
                    11,
                    Some(DurableUpdateStatus {
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
                    }),
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
            assert_eq!(
                app.auto_apply_manual_only.is_none(),
                latched_ahead == 0,
                "a latch for build {} against an installed build {build} must {}",
                build + latched_ahead,
                if latched_ahead == 0 {
                    "be cleared with the artifact it was about"
                } else {
                    "survive: it is about a newer artifact this reconcile said \
                     nothing about"
                }
            );
        }
    }

    /// Put the app in the exact state the field report describes: one Settings
    /// view holding an unsaved draft, parked in a BACKGROUND tab while the user
    /// works in the terminal tab. Returns the settings instance/view.
    ///
    /// THIS BLOCKER IS CHOSEN OVER `pending_restore` ON PURPOSE, and the choice is
    /// the whole point of the test that uses it. Both stop an update relaunch, but
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

    /// Make the retained intent eligible RIGHT NOW and past its idle-preference
    /// deadline, so the attempt takes the `AutomaticPastGrace` lane. Whether the
    /// machine running the suite happens to be quiet is then irrelevant.
    fn force_auto_apply_attempt_now(app: &mut App) {
        let mut intent = app
            .auto_apply_intent
            .expect("an automatic intent must be armed to force an attempt");
        let past = std::time::Instant::now() - std::time::Duration::from_secs(1);
        intent.retry_at = past;
        intent.apply_by = past;
        app.auto_apply_intent = Some(intent);
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
                && std::env::var_os("ATERM_DEBUG_RELAUNCH_NUDGE").is_none(),
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

        app.notice = None;
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

        // The ONE pill. Its wording has to survive too: the old text asserted the
        // automatic lane had given up, which is now false.
        let pill = app
            .notice
            .as_ref()
            .map(crate::notice::TransientNotice::text)
            .expect("the first exhaustion tells the user once");
        assert!(
            pill.contains("retries on its own"),
            "the pill must say the lane comes back by itself, got {pill:?}"
        );

        // SECOND ROUND. Each further attempt costs exactly ONE probe (not three)
        // and must add nothing to the screen.
        for round in 0..4 {
            app.notice = None;
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
                app.notice.is_none(),
                "round {round}: THE NAG. The user was already told once; telling \
                 them again on a two-hour schedule is the regression this test exists \
                 for (got {:?})",
                app.notice
                    .as_ref()
                    .map(crate::notice::TransientNotice::text)
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
        //   * the pill. `Blocked` paints "Update waiting"; only
        //     `UpdateOutcome::Failed` reaches the "Update delayed" arm, and the
        //     close preflight cannot produce `Failed` for a declining reducer (it
        //     produces `Blocked`; only a reducer ERROR is `Failed`, and there is
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
        app.notice = None;
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);

        let landed = app
            .notice
            .as_ref()
            .map(crate::notice::TransientNotice::text)
            .expect("the attempt past the preflight reports its physical outcome");
        assert_eq!(
            landed, "↑ Update delayed — retries on its own",
            "the cleared blocker must let the attempt reach PHYSICAL replacement. \
             A close-preflight refusal would have painted the Blocked pill \
             (\"Update waiting …\") or, inside its cooldown, nothing at all"
        );
        assert!(
            app.auto_apply_intent.is_none()
                && app
                    .auto_apply_manual_only
                    .is_some_and(|latch| latch.build == build && latch.retry_at.is_some()),
            "reaching the physical lane retires the transient cooldown intent and \
             hands the artifact to the strict physical-failure budget; still being \
             refused by the close preflight would have kept a live intent and no \
             latch"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);
    }

    /// THE OTHER HALF OF THE FIX, stated as an outcome rather than as a pill: with
    /// the SAME Settings draft in place, a background probe and a person's click
    /// get the same VERDICT and completely different SCREENS.
    ///
    /// Deleting the recovery surface outright would satisfy every "nothing moved"
    /// assertion in the lane test above, so this pins that the surface is intact
    /// where it belongs — on the lane a person actually asked for, which is also
    /// the lane the exhaustion pill sends them to ("or use the Version menu").
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
            panic!("an unsaved Settings draft blocks an update relaunch");
        };
        assert_eq!(
            reasons,
            vec!["Review or discard unsaved native-app work before relaunching".to_string()],
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
        // AUTHORIZED: it leaves the close-preflight lane entirely and is handed to
        // physical replacement. In this process that replacement cannot land (see
        // the doc comment), so the lane books it as a physical failure and
        // schedules its own comeback — which is what act three arrives on.
        discard_settings_drafts(&mut app, settings);
        app.notice = None;
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        assert_eq!(
            app.notice
                .as_ref()
                .map(crate::notice::TransientNotice::text)
                .as_deref(),
            Some("↑ Update delayed — retries on its own"),
            "a cleared blocker must take the attempt past the close preflight; a \
             refusal there paints the Blocked pill or nothing at all"
        );
        // WHICH GATE REFUSED IS PART OF THE PREMISE, not decoration. The fixture's
        // candidate carries a PASSED pre-park verification, so a real attempt is
        // carried all the way to physical replacement; a `passed: false` fixture
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
            .expect("a returned physical failure records why it stopped");
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
        let comeback = app
            .auto_apply_manual_only
            .expect("the physical lane latches with its own schedule")
            .retry_at
            .expect("and that latch always carries a deadline on its first failure");
        assert!(
            comeback > std::time::Instant::now(),
            "the lane is scheduled to come back, not finished"
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(1),
            "exactly one physical failure has been booked against these bytes"
        );
        assert_no_probe_disturbance(&app, wid, working_tab);

        // ACT THREE — THE COMEBACK IS REAL, NOT A PROMISE ON A STRUCT. Only the
        // clock is forced: the deadline is moved into the past and then production's
        // own `about_to_wait` sequence runs verbatim (lapse the latch, re-arm from
        // the reducer's stage, poll). Nothing here hand-builds an intent.
        let mut latch = app
            .auto_apply_manual_only
            .expect("act two latched the comeback");
        latch.retry_at = Some(std::time::Instant::now() - std::time::Duration::from_secs(1));
        app.auto_apply_manual_only = Some(latch);
        assert!(
            app.lapse_expired_auto_apply_manual_only(),
            "an ACTIVITY-or-physical latch with a passed deadline must lapse; this \
             is the release that the old `retry_at: None` made impossible"
        );
        app.rearm_native_auto_apply_after_lapse();
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build),
            "the lapse must re-arm automatic apply for the same staged artifact"
        );
        app.notice = None;
        force_auto_apply_attempt_now(&mut app);
        app.try_pending_native_auto_apply(false);
        // THE DISCRIMINATOR: a SECOND failure was booked, and the new deadline is the
        // second rung of the physical schedule (1800 s), not the first (600 s). A
        // test that only re-read the latch would pass just as well if the lane had
        // never re-attempted at all.
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(2),
            "the re-armed lane must actually ATTEMPT again — a second physical \
             failure is the receipt for a second park/spawn round trip"
        );
        let second = app
            .auto_apply_manual_only
            .expect("the second failure latches again")
            .retry_at
            .expect("and it is still inside its budget, so it still has a deadline")
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            second > std::time::Duration::from_secs(1700)
                && second <= std::time::Duration::from_secs(1800),
            "the comeback must advance to the SECOND physical rung (~1800s), got \
             {second:?} — the first rung again would mean the budget was replayed"
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
        app.notice = None;
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
        // stage under its own identity, and the automatic lane arms for it with a
        // fresh, bounded budget (the failed swap's DMG digest and its manual-only
        // latch are about the download, which has retired with the stage).
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
            outcome.contains("activating"),
            "…and says what happens next — activation, not a manual relaunch, got {outcome:?}"
        );
        assert!(
            app.auto_apply_intent
                .is_some_and(|intent| intent.build == build
                    && intent.dmg_sha256 == decode_dmg_sha256(&staged.dmg_sha256).unwrap()),
            "the automatic lane arms for the ACTIVATION (its own identity, its own budget)"
        );
        // …AND THE DOWNLOAD'S LATCH IS GONE. A manual-only latch is a promise about
        // ONE artifact — do not spend automatic apply on these bytes — and the
        // reducer has just retired those exact bytes with their stage. Nothing is
        // left for the promise to refuse, so keeping it is state that says something
        // false about the world.
        assert!(
            app.auto_apply_manual_only.is_none(),
            "the retired download must not keep its manual-only latch"
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
    /// TOLD TWICE — NOT NINE TIMES AND NOT FOREVER.
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
    ///     transient lane retries quietly. Exactly two pills, and the second one
    ///     names a control.
    ///
    /// The pill is surfaced here the way the production caller does it
    /// (`app_update_handoff.rs`: `surface_update_apply_outcome("automatic handoff",
    /// surfaced, false)`), because the completion lane returns the outcome and the
    /// event-loop caller paints it.
    #[test]
    fn the_async_physical_lane_converges_and_stops_nagging() {
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
            app.notice = None;
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
            pills.push(
                app.notice
                    .as_ref()
                    .map(crate::notice::TransientNotice::text),
            );
        }

        // 600 s, 1800 s, stand-down — three times over, and the last stand-down is
        // replaced by `None`. Names the SCHEDULE each deadline came from rather
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
                "no-retry",
            ],
            "the async lane must ride the shared physical schedule and then STOP; \
             a fourth epoch here is the unbounded-retry regression"
        );
        assert_eq!(
            app.auto_apply_physical_retry.map(|retry| retry.cycles),
            Some(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS),
            "each returned failure spends exactly one attempt"
        );

        // THE NAG, counted. One pill at the start so the user knows the lane
        // exists, one at the end naming the control, and silence in between.
        assert_eq!(
            pills.iter().filter(|pill| pill.is_some()).count(),
            2,
            "nine failures may cost at most two pills, got {pills:?}"
        );
        assert_eq!(
            pills.first().and_then(Clone::clone).as_deref(),
            Some("↑ Update delayed — retries on its own"),
            "the first failure tells the user the lane is handling it"
        );
        assert!(
            pills[1..usize::from(PHYSICAL_FAILURE_LIFETIME_ATTEMPTS) - 1]
                .iter()
                .all(Option::is_none),
            "every failure between the first and the last must pass in silence — \
             the user has nothing to do and the lane is already coming back, \
             got {pills:?}"
        );
        assert_eq!(
            pills.last().and_then(Clone::clone).as_deref(),
            Some("↑ Update paused — see Version menu"),
            "the ONE actionable moment — the lane is out of retries — must name a \
             control, not repeat 'retries on its own'"
        );

        // AND CONVERGED MEANS CONVERGED. `arm` refuses the artifact, and the latch
        // carries no deadline for `lapse_expired_auto_apply_manual_only` to find.
        assert!(!app.lapse_expired_auto_apply_manual_only());
        assert!(
            app.auto_apply_manual_only
                .is_some_and(|manual| manual.retry_at.is_none()),
            "the converged latch stays"
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
    /// Driven through the real lane with a real broken reducer: a Settings view
    /// whose instance has been removed from the runtime under it, which is exactly
    /// the `RuntimeError::UnknownInstance` shape `prepare_close` reports.
    #[test]
    fn a_broken_close_reducer_is_a_failure_not_a_block() {
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

    /// The retry budget REPLENISHES after a long idle gap: a busy hour must not
    /// permanently spend an artifact's automatic attempts.
    #[test]
    fn an_idle_gap_replenishes_the_activity_revoked_retry_budget() {
        let mut app = App::headless_for_test();
        let ticket = crate::native_updater_service::ApplyAttemptTicket::for_test(
            77,
            "0123456789abcdef0123456789abcdef01234567",
            &"ab".repeat(32),
        );
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build: 77,
            dmg_sha256: [0xab; 32],
            cycles: MAX_ACTIVITY_REVOKED_CYCLES,
            last_attempt: std::time::Instant::now(),
        });
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&ticket),
            None,
            "a freshly exhausted budget stays exhausted"
        );
        app.auto_overlap_retry = Some(crate::AutoOverlapRetry {
            build: 77,
            dmg_sha256: [0xab; 32],
            cycles: MAX_ACTIVITY_REVOKED_CYCLES,
            last_attempt: std::time::Instant::now()
                - crate::ACTIVITY_RETRY_BUDGET_REPLENISH
                - std::time::Duration::from_secs(1),
        });
        assert_eq!(
            app.arm_activity_revoked_overlap_retry(&ticket),
            Some(std::time::Duration::from_secs(2)),
            "after a long idle gap the schedule starts over"
        );
    }

    #[test]
    fn manual_only_latch_survives_duplicate_wakes_and_exactly_new_bytes_rearm() {
        let mut app = App::headless_for_test();
        let build = app.native_updater_service.snapshot().current_build + 1;
        app.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256: [0xab; 32],
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
            apply_by: std::time::Instant::now() + crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE,
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
            apply_by: retry + crate::AUTOMATIC_UPDATE_ACTIVITY_GRACE,
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
        let dir = std::env::temp_dir().join(format!(
            "aterm-update-dirty-preflight-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("draft.md");
        std::fs::write(&path, "draft\n").unwrap();
        let uri = format!("file://{}", path.to_string_lossy().replace(' ', "%20"));

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
        assert_eq!(app.native_config_pending.len(), 1);
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
        assert_eq!(app.native_config_pending.len(), 1);

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
