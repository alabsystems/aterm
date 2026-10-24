// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native Settings, update orchestration, and harness host integration on `App`.
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
    UpdaterPhase, UpdaterWorkTicket,
};
#[cfg(any(unix, test))]
use crate::native_updater_service::{ReturnedApplyDisposition, ReturnedApplyFacts};
use crate::packages_screen::{PackagesBusy, PackagesCommandOutcome, PackagesWorkerCompletion};
use crate::{App, Wake, WindowId};

/// How often a lane held for its candidate's pre-park read looks again.
#[cfg(unix)]
pub(crate) const PREVERIFY_READ_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// Settings ▸ Messages is republished at most this often while the center moves.
pub(crate) const MESSAGES_PUBLISH_MIN_GAP: std::time::Duration =
    std::time::Duration::from_millis(500);
/// …and, with nothing moving, no oftener than this for the page's clock.
pub(crate) const MESSAGES_PUBLISH_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// …and this long after the publish that started a transient status line's clock, the one that clears it.
pub(crate) const MESSAGES_FEEDBACK_WAIT: std::time::Duration =
    std::time::Duration::from_millis(crate::native_settings::MESSAGES_FEEDBACK_MS + 50);

#[cfg(test)]
thread_local! {
    static NATIVE_UI_COMPILES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    static NATIVE_PREVIEW_INSTANT: std::cell::Cell<Option<std::time::Instant>> = const {
        std::cell::Cell::new(None)
    };
}

/// Hold one native-preview instant across a capture's cadence and semantic reads.
#[cfg(test)]
pub(crate) fn with_native_preview_instant_for_test<T>(
    now: std::time::Instant,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<std::time::Instant>);
    impl Drop for Restore {
        fn drop(&mut self) {
            NATIVE_PREVIEW_INSTANT.with(|clock| clock.set(self.0));
        }
    }
    let _restore = Restore(NATIVE_PREVIEW_INSTANT.with(|clock| clock.replace(Some(now))));
    run()
}

/// Real semantic compiler entries on this test thread.
#[cfg(test)]
pub(crate) fn native_ui_compile_count() -> u64 {
    NATIVE_UI_COMPILES.with(std::cell::Cell::get)
}

static NEXT_CONTROL_SETTINGS_REQUEST: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(1);

pub(crate) fn next_control_settings_request() -> u64 {
    NEXT_CONTROL_SETTINGS_REQUEST.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn control_serious_mode_intent(key: &str, value: Option<&str>) -> Option<bool> {
    if key != crate::prefs::EDIT_SERIOUS_MODE {
        return None;
    }
    match value {
        None => Some(false),
        Some(value) => value.trim().parse::<bool>().ok(),
    }
}

fn control_settings_completion_reply(
    key: &str,
    written: Option<&str>,
    now: Option<&str>,
    outcome: &ConfigPatchOutcome,
    synchronization_error: Option<&str>,
) -> Result<String, String> {
    match outcome {
        ConfigPatchOutcome::Applied { undo, .. } => {
            let mut status = match (undo, written, now) {
                (None, _, _) => format!("{key}: unchanged"),
                (Some(_), Some(value), _) => format!("saved: {key} = {value}"),
                (Some(_), None, Some(now)) => format!("removed: {key}; now {now}"),
                (Some(_), None, None) => format!("removed: {key}"),
            };
            if let Some(error) = synchronization_error {
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
            if let Some(reconcile) = synchronization_error.filter(|reconcile| reconcile != message)
            {
                error.push_str("; reconciliation required: ");
                error.push_str(reconcile);
            }
            Err(error)
        }
    }
}

fn write_not_saved_cause(outcome: &ConfigPatchOutcome) -> Option<(String, bool)> {
    match outcome {
        ConfigPatchOutcome::Applied { .. } => None,
        ConfigPatchOutcome::Conflict { .. } => Some((
            "aterm.toml changed first; its current value was kept".to_string(),
            false,
        )),
        ConfigPatchOutcome::Indeterminate { message } => Some((message.clone(), true)),
        ConfigPatchOutcome::Rejected { message } => Some((message.clone(), false)),
    }
}

#[cfg(test)]
thread_local! {
    static UPDATE_FACT_PROBES_ON_THREAD: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
    /// [`App::publish_native_update_state`] calls on this test's thread.
    pub(crate) static FULL_UPDATE_PUBLISHES: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
    /// [`App::publish_quiet_update_words`] calls on this test's thread.
    pub(crate) static QUIET_WORD_PUBLISHES: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
}

#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix lanes spend its counts")
)]
pub(crate) struct NativeUpdateSafetyToken {
    carried_drafts: usize,
    carried_settings_drafts: usize,
    failed_checkpoints: usize,
}

impl NativeUpdateSafetyToken {
    #[must_use]
    pub(crate) const fn is_certified(&self) -> bool {
        true
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn carried_drafts(&self) -> usize {
        self.carried_drafts
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn carried_settings_drafts(&self) -> usize {
        self.carried_settings_drafts
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) const fn failed_checkpoints(&self) -> usize {
        self.failed_checkpoints
    }

    /// A token certifying exactly these counts, as the preflight would have minted it.
    #[cfg(all(test, unix))]
    #[must_use]
    pub(crate) fn carrying_for_test(
        carried_drafts: usize,
        carried_settings_drafts: usize,
        failed_checkpoints: usize,
    ) -> Self {
        Self {
            carried_drafts,
            carried_settings_drafts,
            failed_checkpoints,
        }
    }
}

/// The most unsaved Settings text one update carries, summed over every carried field draft.
pub(crate) const CARRIED_SETTINGS_DRAFTS_CAP_BYTES: usize = 1024 * 1024;

/// The Settings half of the update preflight, over each view's [`crate::native_settings::CarriedFieldDrafts`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SettingsDraftTally {
    pub(crate) carried_views: usize,
    pub(crate) carried_drafts: usize,
    pub(crate) carried_bytes: usize,
    pub(crate) held_views: usize,
    pub(crate) held_why: Option<String>,
}

impl SettingsDraftTally {
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateSettingsDraftCarry",
            action = "Start",
            project = "aterm_gui::settings_draft_carry_conformance::Rig::project"
        )
    )]
    pub(crate) fn of(
        views: impl IntoIterator<Item = crate::native_settings::CarriedFieldDrafts>,
        successor_reopens: bool,
        cap_bytes: usize,
    ) -> Self {
        fn hold(tally: &mut SettingsDraftTally, why: Option<String>) {
            tally.held_views += 1;
            if tally.held_views == 1 {
                tally.held_why = why;
            }
        }
        let mut tally = Self::default();
        for view in views {
            if view.carried.is_empty() && view.held == 0 {
                continue;
            }
            if !successor_reopens {
                hold(&mut tally, None);
            } else if view.held > 0 {
                hold(
                    &mut tally,
                    Some(format!(
                        "a draft is still being composed, or is longer than the {} KiB an update \
                         carries",
                        crate::restore::MAX_SETTINGS_DRAFT_TEXT_BYTES / 1024
                    )),
                );
            } else {
                tally.carried_views += 1;
                tally.carried_drafts += view.carried.len();
                tally.carried_bytes =
                    view.carried.iter().fold(tally.carried_bytes, |sum, draft| {
                        sum.saturating_add(draft.key.len())
                            .saturating_add(draft.text.len())
                    });
            }
        }
        if tally.carried_bytes > cap_bytes {
            let why = format!(
                "{} KiB of unsaved Settings text is more than the {} KiB an update carries",
                tally.carried_bytes.div_ceil(1024),
                cap_bytes / 1024
            );
            for _ in 0..tally.carried_views {
                hold(&mut tally, Some(why.clone()));
            }
            tally.carried_views = 0;
            tally.carried_drafts = 0;
            tally.carried_bytes = 0;
        }
        tally
    }

    fn blocker(&self) -> Option<String> {
        (self.held_views > 0).then(|| match &self.held_why {
            None => format!(
                "{SETTINGS_DRAFTS_BLOCK} {} Settings view(s) have unsaved text",
                self.held_views
            ),
            Some(why) => format!(
                "{SETTINGS_DRAFTS_BLOCK} {} Settings view(s) have unsaved text an update cannot \
                 carry: {why}",
                self.held_views
            ),
        })
    }
}

/// The most unsaved editor text one update carries, summed over every carried draft.
pub(crate) const CARRIED_DRAFTS_CAP_BYTES: usize = 64 * 1024 * 1024;

/// The dirty-document half of the update preflight, over each draft's [`crate::app_documents::DraftCarry`] verdict.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct DraftCarryTally {
    pub(crate) carried: usize,
    pub(crate) carried_bytes: usize,
    pub(crate) writing: usize,
    pub(crate) refused: Vec<String>,
}

impl DraftCarryTally {
    pub(crate) fn of(
        verdicts: impl IntoIterator<Item = crate::app_documents::DraftCarry>,
        cap_bytes: usize,
    ) -> Self {
        let mut tally = Self::default();
        for verdict in verdicts {
            match verdict {
                crate::app_documents::DraftCarry::Carried { bytes } => {
                    tally.carried += 1;
                    tally.carried_bytes = tally.carried_bytes.saturating_add(bytes);
                }
                crate::app_documents::DraftCarry::Writing => tally.writing += 1,
                crate::app_documents::DraftCarry::Refused(why) => tally.refused.push(why),
            }
        }
        if tally.carried_bytes > cap_bytes {
            tally.refused.extend((0..tally.carried).map(|_| {
                format!(
                    "{} MiB of unsaved drafts is more than the {} MiB an update carries",
                    tally.carried_bytes.div_ceil(1024 * 1024),
                    cap_bytes / (1024 * 1024)
                )
            }));
            tally.carried = 0;
            tally.carried_bytes = 0;
        }
        tally
    }

    fn blockers(&self) -> Vec<String> {
        let mut blockers = Vec::new();
        if let Some(why) = self.refused.first() {
            blockers.push(format!(
                "{DIRTY_DOCUMENTS_BLOCK} {} document(s) have uncheckpointed edits an update \
                 cannot carry: {why}",
                self.refused.len()
            ));
        }
        if self.writing > 0 {
            blockers.push(format!(
                "Wait: {} unsaved draft(s) are still being written to their journals",
                self.writing
            ));
        }
        blockers
    }
}

/// One request for the updater's disk facts, read off the event loop and reduced on the main thread.
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

    /// Test-only ticket for hand-built worker facts.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn for_test(request_sequence: u64) -> Self {
        Self { request_sequence }
    }
}

/// What the event loop should do only after worker-collected facts have been accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateReconcilePurpose {
    Startup,
    StageAvailable,
    ApplyControl,
    Refresh,
}

#[must_use]
const fn merge_reconcile_purpose(
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
    pub(crate) observation_sequence: u64,
    pub(crate) observed_at: std::time::Instant,
    pub(crate) durable: Option<DurableUpdateStatus>,
    pub(crate) installed: Option<InstalledUpdate>,
    pub(crate) installed_unproven: bool,
    pub(crate) installable: Option<bool>,
}

/// One retire of a latched installed activation for a newer verified release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivationSupersedeJob {
    pub(crate) current_build: u64,
    pub(crate) activation: u64,
    pub(crate) activation_commit: String,
    pub(crate) newer: u64,
    pub(crate) spent_before: u64,
}

/// What the retire worker posts back.
#[derive(Debug)]
pub(crate) struct ActivationSupersedeCompletion {
    pub(crate) job: ActivationSupersedeJob,
    pub(crate) outcome: Result<aterm_update::SupersededActivation, aterm_update::SupersedeRefusal>,
    pub(crate) facts: Option<NativeUpdateReconcileFacts>,
}

enum NativeUpdateFactDestination {
    Wake {
        purpose: NativeUpdateReconcilePurpose,
        proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
    },
    #[cfg(any(unix, test))]
    Reply(std::sync::mpsc::SyncSender<NativeUpdateReconcileFacts>),
}

pub(crate) struct NativeUpdateReconcileRequest {
    ticket: NativeUpdateReconcileTicket,
    current_build: u64,
    destination: NativeUpdateFactDestination,
}

pub(crate) enum NativeUpdateWorkerRequest {
    Reconcile(NativeUpdateReconcileRequest),
    ConfirmBootHealth {
        current_build: u64,
        witness: aterm_update::TrialWitness,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NativeUpdateDispatch {
    Queued,
    Saturated,
    Unavailable,
}

pub(crate) type NativeUpdateReconcileSender =
    std::sync::mpsc::SyncSender<NativeUpdateWorkerRequest>;

/// One buffered item plus the worker's active item suffice.
pub(crate) const NATIVE_UPDATE_WORKER_CAPACITY: usize = 1;

/// Result of reducing worker-collected disk facts on the event loop.
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
    let mut facts = read_native_update_reconcile_facts_with(
        ticket,
        observation_sequence,
        || aterm_update::status(current_build).map(durable_update_status),
        || {
            aterm_update::installed_update_facts_checked().map(|installed| {
                installed
                    .filter(|installed| {
                        !(installed.yanked && installed.build_number > current_build)
                    })
                    .map(|installed| InstalledUpdate {
                        trial_launches: aterm_update::trial_launch_count(installed.build_number),
                        build: installed.build_number,
                        commit: installed.git_commit,
                        version: installed.version,
                        receipt_build: installed.receipt_build_number,
                        receipt_dmg_sha256: installed.receipt_dmg_sha256,
                    })
            })
        },
    );
    if facts.installable.is_none() {
        facts.installable = aterm_update::running_copy_installable();
    }
    facts
}

fn read_native_update_reconcile_facts_with(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    read_durable: impl FnOnce() -> Option<DurableUpdateStatus>,
    read_installed: impl FnOnce() -> Result<Option<InstalledUpdate>, String>,
) -> NativeUpdateReconcileFacts {
    let observed_at = std::time::Instant::now();
    let durable = read_durable();
    let installable = durable.as_ref().and_then(|durable| durable.installable);
    let (installed, installed_unproven) = match read_installed() {
        Ok(installed) => (installed, false),
        Err(refusal) => (None, aterm_update::is_passing_refusal(&refusal)),
    };
    NativeUpdateReconcileFacts {
        _ticket: ticket,
        observation_sequence,
        observed_at,
        durable,
        installed,
        installed_unproven,
        installable,
    }
}

/// Start the one process-wide, bounded FIFO facts worker.
pub(crate) fn spawn_native_update_reconcile_worker(
    drain_proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) -> Result<NativeUpdateReconcileSender, String> {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel::<NativeUpdateWorkerRequest>(NATIVE_UPDATE_WORKER_CAPACITY);
    crate::qos::spawn_lane_thread(
        "aterm-update-facts".to_string(),
        crate::qos::Role::Background,
        move || {
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
        },
    )
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
            NativeUpdateWorkerRequest::ConfirmBootHealth {
                current_build,
                witness,
            } => {
                let confirmed = aterm_update::confirm_boot_health_exact(
                    current_build,
                    crate::build_info::git_commit(),
                    witness,
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
            #[cfg(any(unix, test))]
            NativeUpdateFactDestination::Reply(reply) => {
                let _ = reply.send(facts);
            }
        }
        drained();
    }
}

/// Queue one handoff-worker observation on the same FIFO and wait off the event loop for its ordered result.
#[cfg(unix)]
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

/// A refusal about the installed environment, distinct from an exhausted artifact budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyEnvironmentBlock {
    build: u64,
    dmg_sha256: [u8; 32],
    blocked_at: std::time::Instant,
    cause: EnvironmentBlockCause,
}

/// Which part of the install refused, and so which observation can say a person has changed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentBlockCause {
    InstalledSource,
    InstallLocation,
}

impl EnvironmentBlockCause {
    /// The cause a person's-to-clear `refusal` names.
    pub(crate) fn of_refusal(refusal: &str) -> Self {
        if aterm_update::is_install_location_refusal(refusal) {
            Self::InstallLocation
        } else {
            Self::InstalledSource
        }
    }

    /// What a person does about this cause.
    pub(crate) const fn remedy(self) -> crate::update_apply_trouble::PersonRemedy {
        use crate::update_apply_trouble::PersonRemedy;
        match self {
            Self::InstallLocation => PersonRemedy::MoveOrAskAdmin,
            Self::InstalledSource => PersonRemedy::InstallSignedRelease,
        }
    }
}

const ACTIVITY_REVOKED_LADDER_RUNGS: u8 = 5;

const MAX_PHYSICAL_FAILURE_CYCLES: u8 = 2;

/// Physical failures counted in one epoch.
pub(crate) const PHYSICAL_FAILURES_PER_EPOCH: u8 = MAX_PHYSICAL_FAILURE_CYCLES + 1;

const MAX_PHYSICAL_FAILURE_EPOCHS: u8 = 3;

/// Total physical failures an artifact may cost before automatic apply converges.
pub(crate) const PHYSICAL_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * MAX_PHYSICAL_FAILURE_EPOCHS;

/// Total physical failures a structurally shaped artifact may cost before automatic apply converges on those exact bytes.
pub(crate) const STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS: u8 = 2;

const UNEXPLAINED_FAILURE_EPOCHS: u8 = 2;

/// Total physical failures an artifact may cost while every one is unexplained.
pub(crate) const UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * UNEXPLAINED_FAILURE_EPOCHS;

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

const PHYSICAL_RETRY_BUDGET_REPLENISH: std::time::Duration =
    std::time::Duration::from_secs(12 * 60 * 60);

const PHYSICAL_FAILURE_EPOCH_COOLDOWN: std::time::Duration =
    std::time::Duration::from_secs(6 * 60 * 60);

const _: () = assert!(
    PHYSICAL_RETRY_BUDGET_REPLENISH.as_secs()
        > PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs() + 600 + 1800,
    "the replenish window must outlast a whole epoch — one stand-down plus the \
     in-epoch spacing — or the counter forgives itself between epochs, the epoch \
     cap is unreachable, and a structurally broken artifact retries forever"
);

/// How long a structural convergence waits before its one re-sample.
pub(crate) const STRUCTURAL_RESAMPLE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

const SLEEP_COUNTS_FROM: std::time::Duration = std::time::Duration::from_secs(1);

const _: () = assert!(
    STRUCTURAL_RESAMPLE_AFTER.as_secs() > PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs(),
    "a structural verdict is the STRONGER one; its re-sample may not come sooner \
     than the transient lane's quiet re-sample, or the classification is decoration"
);

const _: () = assert!(
    STRUCTURAL_RESAMPLE_AFTER.as_secs() > PHYSICAL_RETRY_BUDGET_REPLENISH.as_secs(),
    "the re-sample outlives the physical budget's memory of the bytes — which is \
     why the verdict, not the budget, is what converges a re-sample that fails"
);

/// The boot sentinel's revert threshold, as the automatic lane reads it.
#[cfg(target_os = "macos")]
pub(crate) const BOOT_TRIAL_LAUNCH_LIMIT: u32 = aterm_update::MAX_BOOT_ATTEMPTS;
#[cfg(not(target_os = "macos"))]
pub(crate) const BOOT_TRIAL_LAUNCH_LIMIT: u32 = u32::MAX;

const STRUCTURAL_TRIAL_REMEASURE: std::time::Duration = std::time::Duration::from_secs(75);

/// Maximum consecutive due looks where the structural latch's boot trial remains unmeasured.
pub(crate) const STRUCTURAL_TRIAL_UNMEASURED_LOOKS: u8 = 3;

/// Maximum consecutive due looks where the structural latch's installed binary remains unproven.
pub(crate) const STRUCTURAL_TRIAL_UNPROVEN_LOOKS: u8 = 3;

const SUPERSEDE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

const SUPERSEDE_RETRY_CEILING: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// The wait before a refused release is offered again after `refusals` consecutive refusals.
pub(crate) fn supersede_retry_after(refusals: u32) -> std::time::Duration {
    let doublings = refusals.saturating_sub(1).min(8);
    SUPERSEDE_RETRY_AFTER
        .saturating_mul(1_u32 << doublings)
        .min(SUPERSEDE_RETRY_CEILING)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhysicalFailureSchedule {
    Retry(std::time::Instant),
    StandDown(std::time::Instant),
    Converged {
        resample_at: Option<std::time::Instant>,
    },
}

impl PhysicalFailureSchedule {
    #[must_use]
    fn converged(shape: PhysicalFailureShape, now: std::time::Instant) -> Self {
        Self::Converged {
            resample_at: Some(
                now + if shape == PhysicalFailureShape::Structural {
                    STRUCTURAL_RESAMPLE_AFTER
                } else {
                    PHYSICAL_FAILURE_EPOCH_COOLDOWN
                },
            ),
        }
    }

    #[must_use]
    const fn retry_at(self) -> Option<std::time::Instant> {
        match self {
            Self::Retry(at) | Self::StandDown(at) => Some(at),
            Self::Converged { resample_at } => resample_at,
        }
    }
}

const PREFLIGHT_BLOCK_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// How the close preflight's per-person blockers open (`native_update_close_preflight`).
pub(crate) const SETTINGS_DRAFTS_BLOCK: &str = "Review Settings Drafts:";
pub(crate) const DIRTY_DOCUMENTS_BLOCK: &str = "Checkpoint Drafts:";
pub(crate) const FAILED_CHECKPOINTS_BLOCK: &str = "Retry:";

const _: () = assert!(PREFLIGHT_BLOCK_COOLDOWN.as_secs() <= 60);

/// How often an intent held by a capture refusal re-reads the desk to see whether the refusing state moved.
pub(crate) const CAPTURE_REFUSAL_PROBE: std::time::Duration = std::time::Duration::from_secs(30);

/// How soon the lane looks again for a busy session's baseline during a refusal's first probe.
pub(crate) const CAPTURE_REFUSAL_BASELINE_PROBE: std::time::Duration =
    std::time::Duration::from_secs(2);

const _: () = assert!(CAPTURE_REFUSAL_BASELINE_PROBE.as_secs() < CAPTURE_REFUSAL_PROBE.as_secs());

/// Backstop for an intent held by a capture refusal.
pub(crate) const CAPTURE_REFUSAL_BACKSTOP: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);

/// How long a capture refusal holds the next attempt even when the desk changed.
#[must_use]
pub(crate) const fn capture_refusal_spacing(refusals: u32) -> std::time::Duration {
    std::time::Duration::from_secs(match refusals {
        0 | 1 => CAPTURE_REFUSAL_PROBE.as_secs(),
        2 => 600,
        _ => 1800,
    })
}

const _: () = assert!(
    capture_refusal_spacing(1).as_secs() == CAPTURE_REFUSAL_PROBE.as_secs()
        && capture_refusal_spacing(2).as_secs() > capture_refusal_spacing(1).as_secs()
        && capture_refusal_spacing(u32::MAX).as_secs() > capture_refusal_spacing(2).as_secs()
        && capture_refusal_spacing(u32::MAX).as_secs() < CAPTURE_REFUSAL_BACKSTOP.as_secs()
);

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

/// What a returned physical failure is evidence about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalFailureShape {
    Transient,
    Structural,
    #[cfg(unix)]
    Unexplained,
}

impl PhysicalFailureShape {
    #[must_use]
    #[cfg(unix)]
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
            | crate::UpdateHandoffOutcome::StageUnconfirmed
            | crate::UpdateHandoffOutcome::ProofReady => Self::Transient,
        }
    }

    /// Did the successor image run and decide, or did something end a candidate that never got that far?
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn of_child_death(death: crate::ChildDeathEvidence) -> Self {
        match death {
            crate::ChildDeathEvidence::Unobserved => Self::Unexplained,
            crate::ChildDeathEvidence::Exited { code }
                if code == crate::seamless::EXIT_SUCCESSOR_PASSING =>
            {
                Self::Transient
            }
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

    /// The shape of a failure booked from the cached pre-park verdict.
    #[must_use]
    pub(crate) fn cached_refusal_shape(reason: Option<&str>) -> Self {
        match reason {
            Some(reason) if !aterm_update::is_passing_refusal(reason) => Self::Structural,
            _ => Self::Transient,
        }
    }

    /// How many failures for one artifact this shape may cost before automatic apply is done with those bytes.
    #[must_use]
    pub(crate) const fn lifetime_attempts(self) -> u8 {
        match self {
            Self::Transient => PHYSICAL_FAILURE_LIFETIME_ATTEMPTS,
            Self::Structural => STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS,
            #[cfg(unix)]
            Self::Unexplained => UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS,
        }
    }
}

#[cfg(unix)]
#[must_use]
const fn signal_is_the_image_faulting(signal: i32) -> bool {
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

/// Which lane's budget a returned handoff failure may spend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) enum HandoffFailureLane {
    ActivityRevoked,
    Physical(PhysicalFailureShape),
    Manual,
    Refused,
    #[cfg(unix)]
    AwaitsRosterConfirmation {
        automatic: bool,
    },
}

#[cfg(any(unix, test))]
impl HandoffFailureLane {
    /// Classify one returned completion from the mode the apply was authorized under and the worker's `outcome`.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn classify(
        mode: crate::native_updater_service::ApplyMode,
        outcome: crate::UpdateHandoffOutcome,
        death: crate::ChildDeathEvidence,
        revoked_by_activity: bool,
    ) -> Self {
        // Before the mode: a stage waiting for a check to confirm its signer is no
        // failure of either lane's, a person's press included.
        if outcome == crate::UpdateHandoffOutcome::StageUnconfirmed {
            return Self::AwaitsRosterConfirmation {
                automatic: mode.is_automatic(),
            };
        }
        if !mode.is_automatic() {
            return Self::Manual;
        }
        if outcome == crate::UpdateHandoffOutcome::CaptureRefused {
            return Self::Refused;
        }
        if revoked_by_activity || outcome == crate::UpdateHandoffOutcome::ActivityRevoked {
            Self::ActivityRevoked
        } else {
            Self::Physical(PhysicalFailureShape::of_outcome(outcome, death))
        }
    }

    #[must_use]
    #[cfg(any(unix, test))]
    const fn charges_the_automatic_lane(self) -> bool {
        match self {
            Self::Manual => false,
            #[cfg(unix)]
            Self::AwaitsRosterConfirmation { .. } => false,
            _ => true,
        }
    }
}

#[cfg(unix)]
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
    #[cfg(any(unix, test))]
    ActivityRevoked,
    PhysicalFailure,
}

#[must_use]
fn automatic_retry_delay(cycles: u8, kind: AutomaticRetryKind) -> Option<std::time::Duration> {
    let budget = match kind {
        #[cfg(any(unix, test))]
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
        #[cfg(any(unix, test))]
        (AutomaticRetryKind::ActivityRevoked, 0) => 2,
        #[cfg(any(unix, test))]
        (AutomaticRetryKind::ActivityRevoked, 1) => 5,
        #[cfg(any(unix, test))]
        (AutomaticRetryKind::ActivityRevoked, 2) => 10,
        #[cfg(any(unix, test))]
        (AutomaticRetryKind::ActivityRevoked, 3) => 20,
        #[cfg(any(unix, test))]
        (AutomaticRetryKind::ActivityRevoked, _) => 30,
        (AutomaticRetryKind::PhysicalFailure, 0) => 600,
        (AutomaticRetryKind::PhysicalFailure, _) => 1800,
    };
    Some(std::time::Duration::from_secs(seconds))
}

/// Whether a pre-park verification of `(build, commit, artifact)` must start at `now`.
#[cfg(unix)]
#[must_use]
pub(crate) fn preverify_read_needed(
    cached: Option<&crate::HandoffPreverification>,
    in_flight: Option<&crate::PreverifyInFlight>,
    build: u64,
    commit: &str,
    artifact: &str,
    now: std::time::Instant,
) -> bool {
    let fresh = cached.is_some_and(|entry| {
        entry.build == build
            && entry.commit == commit
            && entry.artifact == artifact
            && entry.at.elapsed() < crate::HANDOFF_PREVERIFY_FRESHNESS
    });
    let running = in_flight.is_some_and(|read| {
        read.commit == commit && read.artifact == artifact && read.running_for(build, now)
    });
    !fresh && !running
}

/// What this process knows about a stage waiting for an update check to confirm its signer.
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownRosterWait {
    pub(crate) reason: String,
    pub(crate) answered_at: std::time::Instant,
    pub(crate) held: bool,
}

#[cfg(unix)]
struct RaiseOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(unix)]
impl Drop for RaiseOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Parse the durable artifact identity without allocating.
#[must_use]
pub(crate) fn decode_dmg_sha256(digest: &str) -> Option<[u8; 32]> {
    const fn nibble(byte: u8) -> Option<u8> {
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

/// Seed the process service from the durable status marker every update surface reads.
pub(crate) fn load_native_updater_service(
    launch_installability: crate::native_updater_service::UpdateInstallability,
) -> NativeUpdaterService {
    let build = crate::build_info::build_number()
        .parse::<u64>()
        .unwrap_or(0);
    let version = crate::build_info::version_display();
    NativeUpdaterService::new(build, version, aterm_update::enabled())
        .seed_installability(launch_installability)
}

/// What the launch's own path already says about whether this copy can replace itself.
#[must_use]
pub(crate) const fn launch_installability(
    posture: Option<aterm_update::which_copy::InstallPosture>,
    dev_marked: bool,
) -> crate::native_updater_service::UpdateInstallability {
    use crate::native_updater_service::UpdateInstallability;
    use aterm_update::which_copy::InstallPosture;
    match posture {
        _ if dev_marked => UpdateInstallability::NotInstallable,
        Some(
            InstallPosture::MountedImage
            | InstallPosture::Translocated
            | InstallPosture::NotABundle,
        ) => UpdateInstallability::NotInstallable,
        Some(InstallPosture::Installed) | None => UpdateInstallability::Unobserved,
    }
}

/// Bounded, owned result suitable for a typed event-loop wake.
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
        installable: Some(status.installable),
        channel_unreadable: status.channel_unreadable,
        checked_at: aterm_update_core::pkg_check::rfc3339_to_unix(&status.updated_at),
    }
}

const fn failed_update_status(build: u64, message: String) -> DurableUpdateStatus {
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
        apply_failure: String::new(),
        apply_failure_build: 0,
        apply_failures_for_target: 0,
        installable: None,
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
    pub(crate) paint_revision: u64,
}

fn native_appearance_revision(preferences: crate::native_appearance::AppearancePreferences) -> u64 {
    use std::hash::{Hash, Hasher};

    let preferences = preferences.normalized();
    let mut revision = std::collections::hash_map::DefaultHasher::new();
    preferences.high_contrast.hash(&mut revision);
    preferences.reduced_transparency.hash(&mut revision);
    preferences.differentiate_without_color.hash(&mut revision);
    preferences.text_scale.to_bits().hash(&mut revision);
    revision.finish()
}

const fn native_motion_revision(motion: crate::native_app::ViewMotionCx) -> u8 {
    (motion.system_reduced as u8)
        | ((motion.focused as u8) << 1)
        | ((motion.performance_reduced as u8) << 2)
        | ((motion.serious as u8) << 3)
        | ((motion.system_dark as u8) << 4)
        | ((motion.backend_gpu as u8) << 5)
}

impl NativeUiCompileStamp {
    /// Regional reducer damage remains sound only when the view-local reducer generation is the sole stamp input that changed.
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
#[derive(Clone, Debug)]
pub(crate) struct NativeCompiledFrame {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) phase: NativeCompiledPhase,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Retained semantic scene for one native split leaf.
#[derive(Clone, Debug)]
pub(crate) struct NativeLeafScene {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) viewport: crate::native_ui::LogicalRect,
    pub(crate) damage: DamageRegion,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Immutable view of one compositor-retained native leaf.
pub(crate) struct RetainedNativeLeafArtifact<'a> {
    pub(crate) instance: crate::tab_model::AppInstanceId,
    pub(crate) view: crate::tab_model::ViewId,
    pub(crate) generation: u64,
    pub(crate) compiled: &'a crate::native_ui::CompiledUi,
    pub(crate) ime_caret: Option<crate::native_ui::LogicalRect>,
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
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
}

struct NativeConfigReconciliationJob {
    path: std::path::PathBuf,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
    pending_sequence: u64,
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
}

struct NativeConfigExternalPreparationJob {
    observation: crate::native_config_service::ConfigDiskObservation,
    themes: std::sync::Arc<crate::app_config::ThemeCatalog>,
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
}

enum NativeConfigJob {
    Persist(Box<NativeConfigPersistenceJob>),
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
    UndoLanding(crate::native_app::OperationId),
    SeriousMode(bool),
    ControlField {
        key: String,
        value: Option<String>,
    },
    ThemeChoice {
        id: String,
        side: aterm_types::Appearance,
    },
}

/// Completion authority for the one serialized config lane.
#[derive(Debug)]
pub(crate) enum NativeConfigOrigin {
    View {
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
        reply: crate::native_app::ReplyToken<ConfigPatchOutcome>,
        theme_request: Option<Box<(u64, String)>>,
        follows: Vec<crate::native_app::OperationId>,
    },
    SeriousMode {
        desired: bool,
    },
    Presence {
        key: &'static str,
    },
    ThemeChoice {
        request_id: u64,
        id: String,
    },
    StatsBars {
        form: crate::stats_deck::StatsBars,
    },
    DeckSize {
        pref: crate::stats_plan::DeckSizePref,
    },
    HarnessSwitch {
        key: &'static str,
    },
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

/// A Settings view's write that landed while a later request of the same view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NativeConfigLanding {
    view: crate::tab_model::ViewId,
    operation: crate::native_app::OperationId,
    revision: u64,
    undo: Option<u64>,
}

fn native_config_followed(
    pending: &std::collections::VecDeque<NativeConfigRequest>,
    view: crate::tab_model::ViewId,
    operation: crate::native_app::OperationId,
) -> bool {
    pending.iter().any(|request| {
        matches!(&request.origin, NativeConfigOrigin::View { view: owner, follows, .. }
            if *owner == view && follows.contains(&operation))
    })
}

fn native_config_followed_base(landings: &[NativeConfigLanding], base: u64) -> u64 {
    landings
        .iter()
        .map(|landing| landing.revision)
        .max()
        .unwrap_or(base)
        .max(base)
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

fn harness_switch_reads_on(key: &str, config: &crate::app_config::Config) -> bool {
    match key {
        crate::prefs::EDIT_HARNESS_ENABLED => config.harness.policy.enabled,
        crate::autopilot::HARNESS_HEADLESS_KEY => config.harness.policy.headless,
        key => {
            crate::prefs::agent_switch(key).is_some_and(|agent| config.harness_agent_enabled(agent))
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
    let observation = observe_config_for_a_person(path)?;
    crate::native_config_service::VersionedConfigService::prepare_observation(observation, themes)
}

fn observe_config_for_a_person(
    path: &std::path::Path,
) -> Result<crate::native_config_service::ConfigDiskObservation, String> {
    use crate::native_config_service::{ObserveFailure, VersionedConfigService, config_read_error};
    VersionedConfigService::observe_path_for_a_person(path, true).map_err(|failure| match failure {
        ObserveFailure::Refused(error) => {
            aterm_log::warn!("native config: {}", config_read_error(path, error.clone()));
            crate::save_words::words(
                crate::save_words::Speaker::Aterm,
                path,
                &crate::save_words::Failure::Read(&error),
            )
        }
        ObserveFailure::Text(message) => message,
    })
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
        crate::prefs::SaveOutcome::PublishedUnverified { message, .. } => {
            ConfigPatchOutcome::Indeterminate {
                message: message.clone(),
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
            .ok_or_else(|| {
                crate::save_words::words(
                    crate::save_words::Speaker::Aterm,
                    std::path::Path::new("aterm.toml"),
                    &crate::save_words::Failure::NoPath,
                )
            })
            .and_then(|path| observe_config_for_a_person(&path))
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
    pub(crate) fn native_preview_phase_ms(&self) -> u64 {
        #[cfg(test)]
        let now = NATIVE_PREVIEW_INSTANT
            .with(std::cell::Cell::get)
            .unwrap_or_else(std::time::Instant::now);
        #[cfg(not(test))]
        let now = std::time::Instant::now();
        u64::try_from(now.saturating_duration_since(self.lat_epoch).as_millis()).unwrap_or(u64::MAX)
    }

    /// Main-thread projection for the control socket's front-input authorization fence.
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
        let Some((instance, view)) = self.active_native_view(wid) else {
            return crate::control::FrontControlSurface::None;
        };
        if self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings)
        {
            // Settings ▸ Harness's session buttons write Autopilot words.
            if self.harness_page_in(view) {
                crate::control::FrontControlSurface::HarnessSettings
            } else {
                crate::control::FrontControlSurface::NativeSettings
            }
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

    fn send_native_update_work(
        &mut self,
        worker: &NativeUpdateReconcileSender,
        work: NativeUpdateWorkerRequest,
    ) -> NativeUpdateDispatch {
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

    /// Mint one monotonic identity before dispatching a durable updater read.
    pub(crate) fn mint_native_update_reconcile_ticket(
        &mut self,
    ) -> Option<NativeUpdateReconcileTicket> {
        let request_sequence = self.next_native_update_reconcile_sequence;
        self.next_native_update_reconcile_sequence = request_sequence.checked_add(1)?;
        Some(NativeUpdateReconcileTicket { request_sequence })
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
        self.send_native_update_work(&worker, work)
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

    /// Queue status and installed-bundle probes on the process worker.
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

    #[must_use]
    const fn has_pending_native_update_reconcile(&self) -> bool {
        self.pending_native_update_reconcile_purpose.is_some()
    }

    fn retry_pending_native_update_reconcile_via(
        &mut self,
        materialize_proxy: impl FnOnce(
            &Self,
        )
            -> Option<aterm_window::event_loop::EventLoopProxy<crate::Wake>>,
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
    pub(crate) fn retry_pending_native_update_reconcile(&mut self) {
        self.retry_pending_native_update_reconcile_via(|app| app.proxy.clone());
    }

    /// Queue boot-sentinel confirmation/GC behind already-requested fact reads.
    pub(crate) fn request_native_boot_health_confirmation(&mut self) -> NativeUpdateDispatch {
        let Some(worker) = self.ensure_native_update_reconcile_worker() else {
            return NativeUpdateDispatch::Unavailable;
        };
        let work = NativeUpdateWorkerRequest::ConfirmBootHealth {
            current_build: self.native_updater_service.snapshot().current_build,
            witness: crate::boot_trial_witness(self.headless),
        };
        self.send_native_update_work(&worker, work)
    }

    fn refuse_control_apply_without_facts(&mut self) {
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

    pub(crate) fn finish_native_boot_health_confirmation(
        &mut self,
        confirmed: bool,
        now: std::time::Instant,
    ) {
        if confirmed {
            self.boot_health_confirmation_dispatched = true;
            self.boot_health_confirmation_retry_at = None;
            self.boot_health_confirmation_backoff = crate::BOOT_HEALTH_RETRY_MIN;
        } else {
            self.boot_health_confirmation_dispatched = false;
            self.arm_boot_health_retry(now);
        }
    }

    /// Accept one ordered fact completion, then derive every presentation and action from the effective reducer stage.
    pub(crate) fn finish_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        facts: NativeUpdateReconcileFacts,
    ) {
        let announced_before = self.relaunch.as_ref().map(|notice| notice.build);
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
                const MAX_CONTROL_APPLY_STALE_RETRIES: u8 = 3;
                if purpose == NativeUpdateReconcilePurpose::ApplyControl
                    && (self.control_apply_stale_retries >= MAX_CONTROL_APPLY_STALE_RETRIES || {
                        self.control_apply_stale_retries += 1;
                        !self.request_native_update_reconcile(purpose)
                    })
                {
                    self.control_apply_stale_retries = 0;
                    self.refuse_control_apply_without_facts();
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
                self.say_linux_held_stage();

                if let Some(stage) = effective_stage {
                    #[cfg(target_os = "macos")]
                    self.yield_launch_record_renewal_to_update();
                    if newly_announced
                        && matches!(
                            purpose,
                            NativeUpdateReconcilePurpose::Startup
                                | NativeUpdateReconcilePurpose::StageAvailable
                        )
                    {
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

    /// A LINUX BUILD HELD FOR A PERSON IS ON THE BAND.
    pub(crate) fn say_linux_held_stage(&mut self) {
        let snapshot = self.native_updater_service.snapshot();
        let running = snapshot.current_build;
        let held = snapshot.linux.as_ref().and_then(|linux| {
            let build = linux.staged_build.filter(|build| *build > running)?;
            Some((build, linux.staged_version.clone().unwrap_or_default()))
        });
        match held {
            Some((build, _)) if self.linux_held_said == Some(build) => {}
            Some((build, version)) => {
                self.linux_held_said = Some(build);
                aterm_log::info!(
                    "update: aterm {version} (build {build}) is held for `aterm update apply`"
                );
                self.say_owed_unless_seen(crate::update_words::linux_held(&version));
            }
            None => {
                if self.linux_held_said.take().is_some() {
                    self.withdraw_owed_row(crate::update_words::KEY_HELD);
                }
            }
        }
    }

    #[cfg(any(unix, test))]
    fn reduce_returned_apply_facts(&mut self, facts: NativeUpdateReconcileFacts) {
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

    /// Resolve the logical viewport for one exact native presentation, even when another leaf is focused or its tab is inactive.
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
        self.native_ui_viewport_in_plan(wid, view, &plan)
    }

    /// [`Self::native_ui_viewport_for`] from a visible `plan` of the view's tab the caller already built.
    pub(crate) fn native_ui_viewport_in_plan(
        &self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) -> Result<crate::native_ui::LogicalRect, String> {
        let Some(window) = self.windows.get(&wid) else {
            return Err("unknown window".to_string());
        };
        let leaf = plan
            .leaf(view)
            .ok_or_else(|| "native view is hidden by pane zoom".to_string())?;
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

    /// Device-pixel Y where native content begins.
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
        self.chrome_palette(crate::app_render::ChromeSurface::NativePage)
            .hash(&mut paint);
        self.win_font_px(wid).to_bits().hash(&mut paint);
        self.font_family.hash(&mut paint);
        self.font_config.styled_paths.hash(&mut paint);
        self.font_config.synthetic_style.hash(&mut paint);
        self.font_config.fallback_fonts.hash(&mut paint);
        self.font_config.symbol_font.hash(&mut paint);
        self.font_config.emoji_font.hash(&mut paint);
        self.font_config.display_faces.hash(&mut paint);
        for (tag, value) in &self.font_variations {
            tag.hash(&mut paint);
            value.to_bits().hash(&mut paint);
        }
        self.font_weight_dark_nudge.to_bits().hash(&mut paint);
        native_appearance_revision(crate::native_appearance::current_preferences())
            .hash(&mut paint);
        let motion = self.native_view_motion_cx(wid, view);
        native_motion_revision(motion).hash(&mut paint);
        motion.automatic_tab_color.hash(&mut paint);
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

    /// Return the compiled artifact retained for glass only while every live input still matches the stamp captured with it.
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

    /// Mark exactly one retained native leaf dirty.
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

    fn repaint_native_view(
        &mut self,
        wid: WindowId,
        view: crate::tab_model::ViewId,
        damage: DamageRegion,
    ) {
        self.invalidate_native_view_cache(wid, view, damage);
        if let Some(window) = self.windows.get_mut(&wid) {
            window.settings_card = None;
            if let Some(window) = window.os_window.as_ref() {
                window.request_redraw();
            }
        }
    }

    fn repaint_native_instance(
        &mut self,
        instance: crate::tab_model::AppInstanceId,
        damage: DamageRegion,
    ) {
        let views = self
            .view_store
            .iter()
            .filter_map(|(view, link)| {
                matches!(link, crate::tab_model::View::Native(native) if native.instance == instance)
                    .then_some(view)
            })
            .collect::<Vec<_>>();
        for view in views {
            self.native_runtime.change_view_state(view, |state| {
                let common = state.common_mut();
                common.presentation_revision = common.presentation_revision.wrapping_add(1);
                true
            });
            if let Ok((wid, _)) = self.locate_native_view(view) {
                self.repaint_native_view(wid, view, damage);
            }
        }
    }

    /// Retire queued actions for every view of one shared review without changing its fields, caret, focus, or visual frame identity.
    pub(crate) fn retire_native_review(&mut self, instance: crate::tab_model::AppInstanceId) {
        let views = self
            .view_store
            .iter()
            .filter_map(|(view, link)| {
                matches!(link, crate::tab_model::View::Native(native) if native.instance == instance)
                    .then_some(view)
            })
            .collect::<Vec<_>>();
        for view in views {
            self.native_runtime.retire_native_review(view);
        }
    }

    /// Compile the active native app's one semantic tree for inspection or hit testing.
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
        #[cfg(test)]
        NATIVE_UI_COMPILES.with(|count| count.set(count.get().saturating_add(1)));
        let document = self
            .native_runtime
            .document_id(instance)
            .and_then(|document| self.document_store.snapshot(document));
        let animation_phase_ms = self.native_preview_phase_ms();
        let semantic_font = self.prepare_native_semantic_font(wid, view, animation_phase_ms);
        let tree = self
            .native_runtime
            .render(
                instance,
                view,
                &ViewCx {
                    viewport,
                    animation_phase_ms,
                    motion: self.native_view_motion_cx(wid, view),
                    terminal_font_px: self.win_font_px(wid),
                    terminal_scale: self.windows[&wid].scale.max(f64::EPSILON) as f32,
                    semantic_font,
                    document: document.as_ref(),
                },
            )
            .map_err(|error| format!("native render failed: {error:?}"))?;
        tree.compile(viewport)
            .map_err(|error| format!("native compile failed: {error:?}"))
    }

    /// Build one independently cacheable native leaf scene.
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
            display_edr: self
                .windows
                .get(&wid)
                .is_some_and(|window| window.screen_edr_potential),
            companion_look: aterm_effects::companion_look::companion_precedence(
                self.kitty_log.favourite_look_loaded(),
                self.windows
                    .get(&wid)
                    .and_then(|ws| ws.fx.look().tenure().worn().map(|i| i.look)),
                self.launch_kitty,
            )
            .0,
            automatic_tab_color: self.automatic_tab_color(wid),
        }
    }

    /// Resolve one exact retained native artifact, the tree and destination paint, pointer routing, accessibility and inspection all consume.
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
            ime_caret: raster.ime_caret,
            device_x,
            device_y,
            width: raster.width,
            height: raster.height,
            scale,
        })
    }

    /// Anchor native IME candidates to the exact focused caret just presented.
    pub(crate) fn report_native_ime_cursor_area(
        &mut self,
        wid: WindowId,
        plan: &crate::tab_model::VisibleLeafPlan,
    ) {
        let Some((_, view)) = self.active_native_view(wid) else {
            return;
        };
        let Some(artifact) = self.retained_native_leaf_artifact_from_plan(wid, view, true, plan)
        else {
            return;
        };
        let Some(caret) = artifact.ime_caret else {
            return;
        };
        let mut left = (artifact.device_x as f64 + f64::from(caret.x) * artifact.scale).floor();
        let mut top = (artifact.device_y as f64 + f64::from(caret.y) * artifact.scale).floor();
        let mut right =
            (artifact.device_x as f64 + f64::from(caret.right()) * artifact.scale).ceil();
        let mut bottom =
            (artifact.device_y as f64 + f64::from(caret.bottom()) * artifact.scale).ceil();
        if let Some(size) = self.windows.get(&wid).and_then(|window| window.win_px) {
            left = left.max(0.0);
            top = top.max(0.0);
            right = right.min(f64::from(size.width));
            bottom = bottom.min(f64::from(size.height));
        }
        if right <= left || bottom <= top {
            return;
        }
        let rect = (
            left as i32,
            top as i32,
            (right - left).max(1.0) as u32,
            (bottom - top).max(1.0) as u32,
        );
        self.report_ime_cursor_rect(wid, rect);
    }

    /// Resolve the retained leaf containing a raw window-space pointer and map it into that leaf's canonical logical coordinates.
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

    /// Convert a window-space pointer into the focused native app's retained content coordinates.
    #[cfg(test)]
    pub(crate) fn native_content_point(&self, wid: WindowId, x: f64, y: f64) -> Option<(f32, f32)> {
        let (_, focused) = self.active_native_view(wid)?;
        let (artifact, x, y) = self.retained_native_leaf_at_pointer(wid, x, y)?;
        (artifact.view == focused).then_some((x, y))
    }

    fn shown_config_assist_rows(
        &self,
        instance: crate::tab_model::AppInstanceId,
        view: crate::tab_model::ViewId,
    ) -> Option<crate::native_ui::EditorPalette> {
        let crate::native_app::AppViewState::Editor(state) =
            self.native_runtime.view_state(view)?
        else {
            return None;
        };
        let buffer = state.buffer.as_ref()?;
        if buffer.minibuffer_active() || buffer.chord_pending() {
            return None;
        }
        let (context, assist) = self.editor_config_assist(instance, view)?;
        if state.config_completion_dismissed == Some(context) {
            return None;
        }
        let listed = assist.typed || state.config_completion_interaction == Some(context);
        if listed {
            (assist.help.is_some() || !assist.completions.is_empty())
                .then(|| crate::native_ui::EditorPalette::Rows(assist.completions.len().max(1)))
        } else {
            assist
                .resting_help
                .is_some()
                .then_some(crate::native_ui::EditorPalette::Help)
        }
    }

    /// Tell the active editor how many lines its viewport shows and invalidate its leaf.
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
                    Some(crate::native_ui::EditorPalette::Rows(
                        crate::native_editor::command_completions(query)
                            .len()
                            .max(1),
                    ))
                }
                _ => None,
            })
            .or_else(|| self.shown_config_assist_rows(instance, view))
            .unwrap_or(crate::native_ui::EditorPalette::None);
        let toolbar = match (
            self.native_runtime.app(instance),
            self.native_runtime.view_state(view),
        ) {
            (
                Some(crate::native_app::NativeApp::Editor(editor)),
                Some(crate::native_app::AppViewState::Editor(state)),
            ) => editor.toolbar_in(state),
            (Some(crate::native_app::NativeApp::Editor(editor)), _) => editor.toolbar(),
            _ => crate::native_ui::EditorToolbar::default(),
        };
        let visible_lines = crate::native_ui::editor_visible_line_capacity_with_palette(
            self.native_ui_viewport(wid)?,
            palette_candidates,
            toolbar,
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

    /// Dispatch through the active app reducer, execute every typed effect, then refresh tab chrome.
    pub(crate) fn dispatch_native_event(
        &mut self,
        wid: WindowId,
        event: AppEvent,
    ) -> Result<EventResult, String> {
        // A scroll the person asked for (a wheel, a page key) moves the page
        // under a still pointer while they watch it: what the pointer rests on
        // afterwards is theirs to press (`app_mouse::NativePointerRest`).
        if matches!(
            event,
            AppEvent::ScrollLines(_) | AppEvent::ScrollPage(_) | AppEvent::ScrollAt { .. }
        ) && let Some(ws) = self.windows.get_mut(&wid)
        {
            ws.native_rest = None;
        }
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let kind = self
            .native_runtime
            .app(instance)
            .map(crate::native_app::NativeApp::kind)
            .ok_or_else(|| "active native app disappeared".to_string())?;
        if kind == crate::native_app::AppKind::SystemState
            && match &event {
                AppEvent::TextInput(event) => crate::system_state::text_input_interacts(event),
                AppEvent::Action(_)
                | AppEvent::ScrollLines(_)
                | AppEvent::ScrollPage(_)
                | AppEvent::ScrollAt { .. } => true,
                _ => false,
            }
        {
            self.interact_system_state(wid);
        }
        let event = match event {
            AppEvent::ScrollAt { lines, .. } if kind != crate::native_app::AppKind::Settings => {
                AppEvent::ScrollLines(lines)
            }
            event => event,
        };
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
                    "editor/finish" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::FinishEdit)
                    }
                    "editor/abort" => {
                        AppEvent::EditorCommand(crate::native_editor::EditorCommand::AbortEdit)
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
        if kind == crate::native_app::AppKind::Editor
            && let Some(refusal) = self.refuse_editor_command(view, &event)
        {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
            self.refresh_native_presentation(wid, instance, view);
            return Err(refusal);
        }
        if let Some(result) = self.dispatch_editor_event(instance, view, &event)? {
            // The editor workspace is an independent reducer and currently has
            // no regional-damage effect lane. Invalidate its exact leaf only,
            // and not for an input it declined, which changed nothing, nor for
            // a view the command closed.
            if result == EventResult::Handled && self.view_store.get(view).is_some() {
                self.invalidate_native_view_cache(wid, view, DamageRegion::All);
                self.refresh_native_presentation(wid, instance, view);
                self.note_native_handled_dispatch(wid);
            }
            return Ok(result);
        }
        let outcome = self
            .native_runtime
            .dispatch(instance, view, event)
            .map_err(|error| format!("native dispatch failed: {error:?}"))?;
        if !outcome.effects.iter().any(|effect| {
            matches!(
                effect,
                AppEffect::RepaintSelf(_) | AppEffect::RepaintInstance(_)
            )
        }) {
            self.invalidate_native_view_cache(wid, view, DamageRegion::All);
        }
        for effect in outcome.effects {
            self.execute_native_effect(wid, instance, view, effect)?;
        }
        if kind == crate::native_app::AppKind::SystemState {
            self.drain_system_state_actions();
        }
        self.refresh_native_presentation(wid, instance, view);
        if outcome.result == EventResult::Handled {
            self.note_native_handled_dispatch(wid);
        }
        Ok(outcome.result)
    }

    /// Dispatch to one exact native view.
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

    fn note_native_handled_dispatch(&mut self, wid: WindowId) {
        if let Some(window) = self.windows.get_mut(&wid) {
            window.native_dispatch_serial = window.native_dispatch_serial.saturating_add(1);
        }
    }

    /// Time only a handled native input that changed the current view's revision.
    pub(crate) fn timed_native_input_event(
        &mut self,
        wid: WindowId,
        event: &crate::input::InputEvent,
        arrival: Option<u64>,
    ) -> bool {
        let Some(arrival) = arrival else {
            return self.native_input_event(wid, event);
        };
        let before = self.native_ui_compile_stamp(wid).ok();
        let serial = self
            .windows
            .get(&wid)
            .map(|window| window.native_dispatch_serial);
        let taken = self.native_input_event(wid, event);
        let dispatched = self
            .windows
            .get(&wid)
            .zip(serial)
            .is_some_and(|(window, before)| window.native_dispatch_serial != before);
        if taken
            && dispatched
            && let (Some(before), Ok(after)) = (before, self.native_ui_compile_stamp(wid))
            && let Some(window) = self.windows.get_mut(&wid)
        {
            crate::metrics::note_native_window_input(
                &mut window.present_metrics.pending_native_input,
                before,
                after,
                arrival,
            );
        }
        taken
    }

    /// Native tabs are an input boundary.
    pub(crate) fn native_input_event(
        &mut self,
        wid: WindowId,
        event: &crate::input::InputEvent,
    ) -> bool {
        use crate::input::{InputEvent, ScrollIntent};
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};

        if matches!(
            event,
            InputEvent::Key { .. }
                | InputEvent::Text(_)
                | InputEvent::KeySequence(_)
                | InputEvent::Paste(..)
                | InputEvent::MouseButton { pressed: true, .. }
                | InputEvent::Wheel { .. }
                | InputEvent::ScrollView(_)
        ) {
            self.interact_system_state(wid);
        }

        let Some((instance, active_view)) = self.active_native_view(wid) else {
            return false;
        };
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
            editor_config_assist_drawn,
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
                    let listed = assist.typed || interacting;
                    let drawn = (listed && (assist.help.is_some() || count > 0))
                        || assist.resting_help.is_some();
                    Some((
                        count,
                        selected,
                        Some(context),
                        interacting,
                        present,
                        dismissed,
                        drawn,
                    ))
                })
                .unwrap_or((0, 0, None, false, false, false, false))
        } else {
            (0, 0, None, false, false, false, false)
        };
        let editor_config_completion_active =
            !editor_config_assist_dismissed && editor_config_completion_count > 0;
        let editor_config_assist_visible =
            !editor_config_assist_dismissed && editor_config_assist_drawn;
        let settings_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Settings);
        let markdown_active = self
            .native_runtime
            .app(instance)
            .is_some_and(|app| app.kind() == crate::native_app::AppKind::Markdown);
        let config_completion_page =
            |context: crate::native_config_language::ConfigCompletionContext| {
                AppEvent::EditorConfigNavigate {
                    navigation: crate::native_app::ConfigCompletionNavigation::Page(
                        editor_config_completion_selected,
                    ),
                    candidates: editor_config_completion_count,
                    context,
                }
            };

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
            let _ = self.dispatch_native_event(wid, config_completion_page(context));
            return true;
        }

        if !editor_chord_pending
            && let InputEvent::Key {
                key: Key::Named(NamedKey::Tab),
                mods,
                event_type: event_type @ (KeyEventType::Press | KeyEventType::Repeat),
                ..
            } = event
        {
            if editor_config_completion_active && !mods.contains(Modifiers::SHIFT) {
                // A held Tab pages the list once: its repeats neither page on
                // nor indent the buffer behind the list.
                if *event_type == KeyEventType::Repeat {
                    return true;
                }
                let Some(context) = editor_config_completion_context else {
                    return true;
                };
                let _ = self.dispatch_native_event(wid, config_completion_page(context));
                if editor_config_completion_interacting
                    && self.activate_native_focus(wid).unwrap_or(false)
                {
                    return true;
                }
                return true;
            }
            if !editor_active && *event_type == KeyEventType::Press {
                let _ = self.move_native_focus(wid, mods.contains(Modifiers::SHIFT));
                return true;
            }
        }
        if let InputEvent::Key {
            key: Key::Named(NamedKey::F6),
            mods,
            event_type: KeyEventType::Press,
            ..
        } = event
            && !mods.intersects(
                Modifiers::CTRL
                    | Modifiers::ALT
                    | Modifiers::SUPER
                    | Modifiers::HYPER
                    | Modifiers::META,
            )
        {
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
            let _ = self.dispatch_native_event(wid, config_completion_page(context));
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
            && let InputEvent::Key {
                key: Key::Named(key @ (NamedKey::Enter | NamedKey::Space)),
                event_type: KeyEventType::Press,
                ..
            } = event
            && !self.native_text_field_has_focus(wid)
            && !(matches!(key, NamedKey::Space) && self.native_space_types_into_search(wid))
        {
            if self.activate_native_focus(wid).unwrap_or(false) {
                return true;
            }
            if matches!(key, NamedKey::Enter) && self.activate_native_default(wid).unwrap_or(false)
            {
                return true;
            }
        }

        if !editor_active
            && let InputEvent::Key {
                key: Key::Named(arrow @ (NamedKey::ArrowLeft | NamedKey::ArrowRight)),
                mods,
                event_type: KeyEventType::Press,
                ..
            } = event
            && mods.is_empty()
            && let Some(target) = self.native_segment_step(
                wid,
                if matches!(arrow, NamedKey::ArrowLeft) {
                    -1
                } else {
                    1
                },
            )
        {
            if let Some(target) = target {
                let _ = self.focus_native_control(wid, target);
            }
            return true;
        }

        let app_event = match event {
            InputEvent::Resize { .. }
            | InputEvent::ResizeWindowPx { .. }
            | InputEvent::Focus(_) => return false,
            InputEvent::Text(text) if editor_active => Some(AppEvent::EditorInput(
                amacs::input::EditorInput::Text(text.clone()),
            )),
            InputEvent::Paste(text, _) if editor_active => Some(AppEvent::EditorInput(
                amacs::input::EditorInput::Paste(text.clone()),
            )),
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
                layout_text,
                event_type: KeyEventType::Press | KeyEventType::Repeat,
                ..
            } if editor_active => {
                let navigation = match key {
                    Key::Named(NamedKey::ArrowUp) => {
                        Some(crate::native_app::ConfigCompletionNavigation::Previous)
                    }
                    Key::Named(NamedKey::ArrowDown) => {
                        Some(crate::native_app::ConfigCompletionNavigation::Next)
                    }
                    _ => None,
                };
                if let Some(navigation) = navigation
                    && editor_config_completion_active
                    && editor_config_completion_focused
                    && !editor_chord_pending
                    && !mods.intersects(Modifiers::CTRL | Modifiers::ALT | Modifiers::META)
                {
                    editor_config_completion_context.map(|context| AppEvent::EditorConfigNavigate {
                        navigation,
                        candidates: editor_config_completion_count,
                        context,
                    })
                } else {
                    let text = match key {
                        Key::Character(character) => Some(
                            crate::input::typed_glyph(*character, *mods, *layout_text).to_string(),
                        ),
                        _ => None,
                    };
                    Some(AppEvent::EditorInput(amacs::input::EditorInput::Key {
                        key: key.clone(),
                        mods: *mods,
                        text,
                    }))
                }
            }
            InputEvent::Key {
                key,
                mods,
                layout_text,
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
                } else {
                    let readline = settings_active
                        && mods.contains(Modifiers::CTRL)
                        && !mods.intersects(
                            Modifiers::SUPER | Modifiers::ALT | Modifiers::HYPER | Modifiers::META,
                        );
                    let alt_word = settings_active
                        && mods.intersects(Modifiers::ALT | Modifiers::META)
                        && !mods.intersects(Modifiers::CTRL | Modifiers::SUPER | Modifiers::HYPER);
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
                                crate::input::typed_glyph(*character, *mods, *layout_text)
                                    .to_string(),
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
                        Key::Named(NamedKey::Enter) => {
                            Some(AppEvent::TextInput(TextInputEvent::Submit))
                        }
                        Key::Named(NamedKey::Escape) => {
                            Some(AppEvent::TextInput(TextInputEvent::Cancel))
                        }
                        Key::Named(NamedKey::ArrowUp) if settings_active => {
                            Some(AppEvent::StepFocus(-1))
                        }
                        Key::Named(NamedKey::ArrowDown) if settings_active => {
                            Some(AppEvent::StepFocus(1))
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
                        Key::Named(NamedKey::PageUp) if settings_active => {
                            Some(AppEvent::ScrollPage(-1))
                        }
                        Key::Named(NamedKey::PageDown) if settings_active => {
                            Some(AppEvent::ScrollPage(1))
                        }
                        Key::Named(NamedKey::PageUp) => Some(AppEvent::ScrollLines(-8)),
                        Key::Named(NamedKey::PageDown) => Some(AppEvent::ScrollLines(8)),
                        Key::Named(NamedKey::Home) if settings_active && !command => {
                            Some(AppEvent::TextInput(TextInputEvent::Home { extend }))
                        }
                        Key::Named(NamedKey::End) if settings_active && !command => {
                            Some(AppEvent::TextInput(TextInputEvent::End { extend }))
                        }
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

        if let Some(app_event) = app_event {
            let declined = matches!(
                self.dispatch_native_event(wid, app_event),
                Ok(EventResult::Bubble)
            );
            if declined
                && editor_active
                && let InputEvent::Key {
                    key: Key::Named(NamedKey::Tab),
                    mods,
                    event_type: KeyEventType::Press,
                    ..
                } = event
            {
                let _ = self.move_native_focus(wid, mods.contains(Modifiers::SHIFT));
            }
        }
        true
    }

    /// The active native page's compiled tree as the window holds it now.
    pub(crate) fn current_native_ui(
        &self,
        wid: WindowId,
    ) -> Result<std::borrow::Cow<'_, crate::native_ui::CompiledUi>, String> {
        match self.cached_native_ui(wid) {
            Some(frame) => Ok(std::borrow::Cow::Borrowed(&frame.compiled)),
            None => self.compiled_native_ui(wid).map(std::borrow::Cow::Owned),
        }
    }

    fn native_segment_step(
        &self,
        wid: WindowId,
        step: isize,
    ) -> Option<Option<crate::native_ui::UiKey>> {
        let (_, view) = self.active_native_view(wid)?;
        let focus = self
            .native_runtime
            .view_state(view)?
            .common()
            .last_focus
            .clone()?;
        self.current_native_ui(wid).ok()?.segment_step(&focus, step)
    }

    fn focus_native_control(
        &mut self,
        wid: WindowId,
        key: crate::native_ui::UiKey,
    ) -> Result<(), String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
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

    fn move_native_focus(&mut self, wid: WindowId, backwards: bool) -> Result<(), String> {
        let (instance, view) = self
            .active_native_view(wid)
            .ok_or_else(|| "active tab is not a native app".to_string())?;
        let current = self
            .native_runtime
            .view_state(view)
            .and_then(|state| state.common().last_focus.clone());
        let key = {
            let compiled = self.current_native_ui(wid)?;
            let focus_order = &compiled.focus_order;
            if focus_order.is_empty() {
                return Ok(());
            }
            let order_of = |key: &crate::native_ui::UiKey| {
                compiled.semantics.iter().position(|node| &node.key == key)
            };
            let list_at = compiled.semantics.iter().position(|node| {
                node.role == crate::native_ui::SemanticRole::List
                    && node.key.as_str() != "settings/navigation/list"
            });
            let next = current
                .as_ref()
                .and_then(|key| focus_order.iter().position(|candidate| candidate == key))
                .map_or_else(
                    || {
                        let resume = current.as_ref().and_then(order_of).and_then(|at| {
                            if backwards {
                                focus_order
                                    .iter()
                                    .rposition(|key| order_of(key).is_some_and(|p| p < at))
                            } else {
                                focus_order
                                    .iter()
                                    .position(|key| order_of(key).is_some_and(|p| p > at))
                            }
                        });
                        resume.unwrap_or(if backwards { focus_order.len() - 1 } else { 0 })
                    },
                    |index| {
                        if backwards {
                            index.checked_sub(1).unwrap_or(focus_order.len() - 1)
                        } else {
                            (index + 1) % focus_order.len()
                        }
                    },
                );
            let next = match self.native_runtime.view_state(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => state
                    .tab_past_navigation(
                        current.as_ref(),
                        &focus_order[next],
                        !backwards,
                        focus_order,
                    )
                    .and_then(|target| focus_order.iter().position(|key| *key == target))
                    .unwrap_or(next),
                _ => next,
            };
            let passes_list = list_at
                .zip(current.as_ref().and_then(order_of))
                .is_some_and(|(list, from)| {
                    order_of(&focus_order[next]).is_none_or(|to| {
                        let (from, to) = if backwards { (to, from) } else { (from, to) };
                        if from < to {
                            from < list && list < to
                        } else {
                            list > from || list < to
                        }
                    })
                });
            // Settings keeps a plan of its list; any other page's list that
            // holds every row says its stops in the compiled tree
            // ([`crate::native_ui::CompiledUi::list_stops`]).
            let settings = match self.native_runtime.view_state(view) {
                Some(crate::native_app::AppViewState::Settings(state)) => Some(&**state),
                _ => None,
            };
            let mut key = focus_order[next].clone();
            if passes_list
                && let Some(target) = match settings {
                    Some(state) => state.tab_into_unshown_list(!backwards, focus_order),
                    None => compiled.tab_into_unshown_list(!backwards),
                }
            {
                key = target;
            }
            if let Some(current) = current.as_ref()
                && let Some(beyond) = match settings {
                    Some(state) => state.tab_past_screen(current, &key, !backwards, focus_order),
                    None => compiled.tab_past_screen(current, &key, !backwards),
                }
            {
                key = beyond;
            }
            if let Some(state) = settings
                && let Some(target) =
                    state.tab_past_options(current.as_ref(), &key, !backwards, focus_order)
            {
                key = target;
            }
            key
        };
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

    fn native_space_types_into_search(&self, wid: WindowId) -> bool {
        let Some((_, view)) = self.active_native_view(wid) else {
            return false;
        };
        matches!(
            self.native_runtime.view_state(view),
            Some(crate::native_app::AppViewState::Settings(state))
                if state.font_picker_takes_space()
        )
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
        self.current_native_ui(wid).ok().and_then(|compiled| {
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
        let compiled = self.current_native_ui(wid)?;
        let Some(semantic) = compiled.semantic(&key) else {
            return Ok(false);
        };
        if semantic.state.is_some_and(|state| !state.enabled) {
            return Ok(true);
        }
        let Some(action) = semantic.action.clone() else {
            return Ok(false);
        };
        drop(compiled);
        self.dispatch_native_event(
            wid,
            AppEvent::Action(crate::native_app::ActionInvocation {
                id: action,
                value: None,
            }),
        )?;
        Ok(true)
    }

    fn activate_native_default(&mut self, wid: WindowId) -> Result<bool, String> {
        let Some((_, action)) = self.current_native_ui(wid)?.default_action.clone() else {
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
            AppEffect::ConfigPatch {
                patch,
                reply,
                follows,
            } => {
                let theme_request = patch
                    .edits
                    .iter()
                    .find(|edit| edit.key == crate::prefs::EDIT_THEME)
                    .map(|edit| {
                        let request_id = next_control_settings_request();
                        let keeping = self.native_runtime.view_state(view).and_then(|state| {
                            let crate::native_app::AppViewState::Settings(state) = state else {
                                return None;
                            };
                            match &state.showcase.trial {
                                crate::native_settings::showcase_page::ShowcaseTrial::Keeping {
                                    id,
                                    operation,
                                    ..
                                } if *operation == reply.operation => Some(id.clone()),
                                _ => None,
                            }
                        });
                        if let Some(id) = keeping {
                            self.bind_theme_preview_commit(&id, request_id);
                        }
                        (
                            request_id,
                            edit.value.clone().unwrap_or_else(|| "Default".to_string()),
                        )
                    })
                    .map(Box::new);
                let theme_request_id = theme_request.as_deref().map(|(id, _)| *id);
                self.native_config_pending.push_back(NativeConfigRequest {
                    origin: NativeConfigOrigin::View {
                        instance,
                        view,
                        reply,
                        theme_request,
                        follows,
                    },
                    work: NativeConfigWork::Patch(patch),
                });
                if let Err(error) = self.pump_native_config() {
                    if let Some(request_id) = theme_request_id
                        && let Some(at) = self.native_config_pending.iter().position(|request| {
                            matches!(&request.origin, NativeConfigOrigin::View { theme_request: Some(theme), .. } if theme.0 == request_id)
                        })
                        && let Some(request) = self.native_config_pending.remove(at)
                    {
                        self.publish_native_config_origin(request.origin, ConfigPatchOutcome::Rejected { message: error.clone() }, None, false, None);
                    }
                    return Err(error);
                }
            }
            AppEffect::ConfigUndo { target, reply } => {
                let (work, follows) = match target {
                    crate::native_app::UndoTarget::Token(token) => {
                        (NativeConfigWork::Undo(token), Vec::new())
                    }
                    crate::native_app::UndoTarget::Landing(write) => {
                        (NativeConfigWork::UndoLanding(write), vec![write])
                    }
                };
                self.native_config_pending.push_back(NativeConfigRequest {
                    origin: NativeConfigOrigin::View {
                        instance,
                        view,
                        reply,
                        theme_request: None,
                        follows,
                    },
                    work,
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
                // Only an affirmative selection writes config, through the same
                // versioned lane as control `settings set`, so the image is
                // re-decoded and every Settings view converges on one verdict.
                if let Some(path) =
                    crate::menu::choose_local_image("Choose a wallpaper image", "Set Wallpaper")
                {
                    let value = path.to_string_lossy().into_owned();
                    let (reply, outcome) = std::sync::mpsc::channel();
                    self.queue_control_settings_field(
                        crate::prefs::EDIT_WALLPAPER.to_string(),
                        Some(value),
                        reply,
                    );
                    if let Ok(Err(error)) = outcome.try_recv() {
                        aterm_log::warn!("wallpaper picker: {error}");
                    }
                }
            }
            AppEffect::OpenLogFolder { reply } => {
                let outcome = match crate::logging::log_dir() {
                    Some(dir) if crate::menu::open_file_in_workspace(&dir) => {
                        ExternalOpenOutcome::Opened
                    }
                    Some(dir) => {
                        aterm_log::warn!("could not open the log folder {}", dir.display());
                        ExternalOpenOutcome::Denied {
                            message: "the log folder did not open".to_string(),
                        }
                    }
                    None => {
                        aterm_log::warn!("the log folder has no directory to live in");
                        ExternalOpenOutcome::Denied {
                            message: "the log folder has no directory".to_string(),
                        }
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
            AppEffect::Themes { request, reply } => {
                let outcome = crate::settings_themes_host::perform(
                    &request,
                    crate::settings_themes_host::ThemesHostArms::for_instance(self.headless),
                );
                if matches!(
                    outcome,
                    crate::native_app::ThemesOutcome::Saved { .. }
                        | crate::native_app::ThemesOutcome::Imported { .. }
                        | crate::native_app::ThemesOutcome::Trashed { .. }
                        | crate::native_app::ThemesOutcome::Added { .. }
                ) {
                    self.rediscover_themes_now();
                }
                if self.native_runtime.completion_is_current(&reply) {
                    self.dispatch_native_completion(
                        wid,
                        instance,
                        view,
                        AppEvent::ThemesFinished {
                            operation: reply.operation,
                            outcome,
                        },
                    )?;
                }
            }
            AppEffect::OpenPackagesLog => match atpkg::packages_log::log_path() {
                Some(path) if crate::menu::open_file_in_workspace(&path) => {}
                Some(path) => {
                    aterm_log::warn!("could not open the package log {}", path.display());
                }
                None => aterm_log::warn!("the package log has no directory to live in"),
            },
            AppEffect::Harness(crate::native_app::HarnessRequest::AiSignIn { id }) => {
                self.ai_sign_in(wid, id);
            }
            AppEffect::Harness(request) => self.execute_native_harness(request),
            AppEffect::HarnessTimer(request) => self.execute_harness_timer(request),
            AppEffect::InvalidateOwnPresentation => {
                self.refresh_native_presentation(wid, instance, view);
            }
            AppEffect::RepaintSelf(damage) => {
                self.repaint_native_view(wid, view, damage);
            }
            AppEffect::RepaintInstance(damage) => {
                self.repaint_native_instance(instance, damage);
            }
            AppEffect::RetireReview => self.retire_native_review(instance),
        }
        Ok(())
    }

    /// Enqueue one `settings set|unset` command into the serialized.
    pub(crate) fn queue_control_settings_field(
        &mut self,
        key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) {
        let _ = self.queue_control_settings_field_admitted(
            key,
            value,
            reply,
            next_control_settings_request(),
        );
    }

    /// [`Self::queue_control_settings_field`] under the caller's `request_id`.
    pub(crate) fn queue_control_settings_field_admitted(
        &mut self,
        key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
        request_id: u64,
    ) -> Option<u64> {
        let Some(field) = crate::prefs::editable_fields(&self.config)
            .into_iter()
            .find(|field| field.key == key)
        else {
            let message = crate::native_config_language::control_settings_refusal(&key);
            let _ = reply.send(Err(message));
            return None;
        };
        let canonical_key = field.key.to_string();
        if let Some(Err(refusal)) = value
            .as_deref()
            .map(|value| crate::native_settings::control_input_value(&field, value))
        {
            let _ = reply.send(Err(format!("save failed: {refusal}")));
            return None;
        }
        if self.proxy.is_none() {
            let _ = reply.send(Err(
                "save failed: config persistence needs an event-loop proxy".to_string(),
            ));
            return None;
        }
        if let Err(error) = native_config_queue() {
            let _ = reply.send(Err(format!("save failed: {error}")));
            return None;
        }

        self.enqueue_control_settings_field_intent_with_id(canonical_key, value, reply, request_id);
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
            let _ = reply.send(Err(format!("save failed: {error}")));
            self.refresh_serious_mode_queued_projection();
            return None;
        }
        Some(request_id)
    }

    #[cfg(test)]
    fn enqueue_control_settings_field_intent(
        &mut self,
        canonical_key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
    ) -> u64 {
        let request_id = next_control_settings_request();
        self.enqueue_control_settings_field_intent_with_id(canonical_key, value, reply, request_id);
        request_id
    }

    fn enqueue_control_settings_field_intent_with_id(
        &mut self,
        canonical_key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
        request_id: u64,
    ) {
        let value = value.map(|value| {
            crate::prefs::editable_fields(&self.config)
                .iter()
                .find(|field| field.key == canonical_key)
                .and_then(|field| crate::native_settings::control_input_value(field, &value).ok())
                .unwrap_or(value)
        });
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
        if let Some(desired) = serious_mode_intent {
            self.serious_mode_queued_projection = Some(desired);
        }
    }

    /// Enqueue the menu/keybinding/palette Serious Mode command in the versioned lane native Settings uses.
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
                    NativeConfigOrigin::View { .. }
                    | NativeConfigOrigin::Presence { .. }
                    | NativeConfigOrigin::ThemeChoice { .. }
                    | NativeConfigOrigin::StatsBars { .. }
                    | NativeConfigOrigin::DeckSize { .. }
                    | NativeConfigOrigin::HarnessSwitch { .. } => None,
                });
    }

    fn publish_presence_completion(
        &mut self,
        key: &'static str,
        outcome: &ConfigPatchOutcome,
        authoritative: Option<&crate::native_config_service::ConfigSnapshot>,
    ) {
        let label = crate::app_take_over::PRESENCE_RIM_LABEL;
        let feedback = match outcome {
            ConfigPatchOutcome::Applied { .. } => {
                if let Some(snapshot) = authoritative {
                    self.config.presence = snapshot.config.presence.clone();
                }
                None
            }
            other => write_not_saved_cause(other),
        };
        if let Some((cause, indeterminate)) = feedback {
            let durable = authoritative.map_or_else(
                || self.config.presence_rim_enabled(),
                |snapshot| snapshot.config.presence_rim_enabled(),
            );
            if key == crate::app_take_over::PRESENCE_RIM_KEY {
                self.set_presence_bit(durable);
            }
            self.post_message(crate::message_reporters::presence_not_saved(
                key,
                label,
                &cause,
                indeterminate,
            ));
        }
    }

    /// Queue the durable settings write for the presence rim switch.
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
            origin: NativeConfigOrigin::Presence { key },
            work: NativeConfigWork::ControlField {
                key: key.to_string(),
                value: Some(desired.to_string()),
            },
        });
        self.pump_native_config()
    }

    /// View ▸ Stats Bars.
    pub(crate) fn user_set_stats_bars(&mut self, form: crate::stats_deck::StatsBars) {
        if form == self.stats_bars_form() {
            return;
        }
        let origin = NativeConfigOrigin::StatsBars { form };
        if let Err(error) = self.queue_stats_write(origin, "sys.stats_bars", form.word()) {
            self.post_message(crate::message_reporters::stats_bars_not_saved(
                form.title(),
                &error,
                false,
            ));
        }
    }

    /// View ▸ Deck Size.
    pub(crate) fn user_set_deck_size(&mut self, pref: crate::stats_plan::DeckSizePref) {
        if pref == self.deck_size_pref() {
            return;
        }
        let origin = NativeConfigOrigin::DeckSize { pref };
        if let Err(error) = self.queue_stats_write(origin, "sys.stats_deck_size", pref.word()) {
            self.post_message(crate::message_reporters::deck_size_not_saved(
                pref.title(),
                &error,
                false,
            ));
        }
    }

    fn queue_stats_write(
        &mut self,
        origin: NativeConfigOrigin,
        key: &str,
        word: &str,
    ) -> Result<(), String> {
        self.proxy
            .as_ref()
            .ok_or_else(|| "config persistence needs an event-loop proxy".to_string())?;
        let _ = native_config_queue()?;
        self.native_config_pending.push_back(NativeConfigRequest {
            origin,
            work: NativeConfigWork::ControlField {
                key: key.to_string(),
                value: Some(word.to_string()),
            },
        });
        self.pump_native_config()
    }

    /// Queue the durable settings write for a toggled harness switch `key`.
    pub(crate) fn queue_harness_switch_write(&mut self, key: &'static str) {
        let queued = (|| {
            self.proxy
                .as_ref()
                .ok_or_else(|| "config persistence needs an event-loop proxy".to_string())?;
            let _ = native_config_queue()?;
            self.native_config_pending.push_back(NativeConfigRequest {
                origin: NativeConfigOrigin::HarnessSwitch { key },
                work: NativeConfigWork::ControlField {
                    key: key.to_string(),
                    value: Some("true".to_string()),
                },
            });
            self.pump_native_config()
        })();
        if let Err(error) = queued {
            self.harness_switch_not_saved(key, &error, false);
        }
    }

    fn publish_harness_switch_completion(
        &mut self,
        key: &'static str,
        outcome: &ConfigPatchOutcome,
        authoritative: Option<&crate::native_config_service::ConfigSnapshot>,
    ) {
        let feedback = match outcome {
            ConfigPatchOutcome::Applied { .. } => match authoritative {
                Some(snapshot) if harness_switch_reads_on(key, &snapshot.config) => None,
                Some(_) => Some((
                    "the saved config still holds this switch off; correct the limiting harness setting in aterm.toml before pressing on again".to_string(),
                    false,
                )),
                None => Some((
                    "the save completed, but the harness switch's effective value could not be confirmed".to_string(),
                    true,
                )),
            },
            other => write_not_saved_cause(other),
        };
        if let Some((cause, indeterminate)) = feedback {
            self.harness_switch_not_saved(key, &cause, indeterminate);
        }
    }

    fn harness_switch_not_saved(&mut self, key: &'static str, cause: &str, indeterminate: bool) {
        for ws in self.windows.values_mut() {
            if ws.claude_lights.let_switches_go() {
                crate::claude_lights::redraw(ws);
            }
        }
        self.post_message(crate::message_reporters::harness_switch_not_saved(
            key,
            cause,
            indeterminate,
        ));
    }

    fn publish_stats_bars_completion(
        &mut self,
        form: crate::stats_deck::StatsBars,
        outcome: &ConfigPatchOutcome,
    ) {
        if let Some((cause, indeterminate)) = write_not_saved_cause(outcome) {
            self.post_message(crate::message_reporters::stats_bars_not_saved(
                form.title(),
                &cause,
                indeterminate,
            ));
        }
    }

    fn publish_deck_size_completion(
        &mut self,
        pref: crate::stats_plan::DeckSizePref,
        outcome: &ConfigPatchOutcome,
    ) {
        if let Some((cause, indeterminate)) = write_not_saved_cause(outcome) {
            self.post_message(crate::message_reporters::deck_size_not_saved(
                pref.title(),
                &cause,
                indeterminate,
            ));
        }
    }

    /// Queue the durable `theme` write for a theme chosen in the command palette.
    pub(crate) fn queue_theme_choice(&mut self, id: &str) -> Result<(), String> {
        self.proxy
            .as_ref()
            .ok_or_else(|| "config persistence needs an event-loop proxy".to_string())?;
        let _ = native_config_queue()?;
        let request_id = self.enqueue_theme_choice(id);
        if let Err(error) = self.pump_native_config() {
            if let Some(at) = self.native_config_pending.iter().position(|request| {
                matches!(&request.origin, NativeConfigOrigin::ThemeChoice { request_id: queued, .. }
                    if *queued == request_id)
            }) {
                self.native_config_pending.remove(at);
            }
            return Err(error);
        }
        Ok(())
    }

    /// The queue half of [`Self::queue_theme_choice`]: one request, no pump.
    pub(crate) fn enqueue_theme_choice(&mut self, id: &str) -> u64 {
        let request_id = next_control_settings_request();
        self.bind_theme_preview_commit(id, request_id);
        self.native_config_pending.push_back(NativeConfigRequest {
            origin: NativeConfigOrigin::ThemeChoice {
                request_id,
                id: id.to_string(),
            },
            work: NativeConfigWork::ThemeChoice {
                id: id.to_string(),
                side: self.os_appearance,
            },
        });
        request_id
    }

    fn publish_theme_choice_completion(
        &mut self,
        request_id: u64,
        id: &str,
        outcome: &ConfigPatchOutcome,
        authoritative: Option<&crate::native_config_service::ConfigSnapshot>,
        notify: bool,
    ) {
        let owning = |preview: &crate::control_theme::ThemePreview| {
            preview.commit_request == Some(request_id)
        };
        if matches!(
            outcome,
            ConfigPatchOutcome::Applied { .. } | ConfigPatchOutcome::Conflict { .. }
        ) && let Some(snapshot) = authoritative
        {
            self.config.theme.clone_from(&snapshot.config.theme);
            if matches!(outcome, ConfigPatchOutcome::Applied { .. })
                && self
                    .theme_preview
                    .as_ref()
                    .is_some_and(|p| p.generation <= request_id)
            {
                self.theme_preview = None;
            }
            if self.theme_preview.is_some() {
                let restore = self
                    .config
                    .resolved_colors(self.os_appearance, &self.config_assets.themes, None)
                    .id;
                if let Some(preview) = self.theme_preview.as_mut() {
                    preview.restore = restore;
                }
            }
        }
        let feedback = match outcome {
            ConfigPatchOutcome::Applied { .. } if authoritative.is_some() => {
                self.install_resolved_colors(crate::app_colorscheme::ThemeChange::Preview);
                None
            }
            ConfigPatchOutcome::Applied { .. } => Some((
                "the save completed, but its theme could not be verified".to_string(),
                true,
            )),
            other => write_not_saved_cause(other),
        };
        if let Some((cause, indeterminate)) = feedback {
            self.end_theme_preview_if(owning);
            if !notify {
                return;
            }
            let label = self
                .config_assets
                .themes
                .meta(id)
                .map_or_else(|| id.to_string(), |meta| meta.display_name);
            self.post_message(crate::message_reporters::theme_not_saved(
                &label,
                &cause,
                indeterminate,
            ));
        }
    }

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

    fn theme_choice_patch_request(
        &self,
        id: &str,
        side: aterm_types::Appearance,
    ) -> Result<crate::native_app::ConfigPatch, String> {
        let snapshot = self.native_config_service.snapshot();
        let expected = snapshot.values()?.remove(crate::prefs::EDIT_THEME);
        let (value, _) = crate::prefs::theme_sideless_choice_value(
            expected.as_deref(),
            side,
            id,
            &snapshot.assets.themes,
        );
        Ok(crate::native_app::ConfigPatch {
            base_revision: snapshot.revision,
            edits: vec![crate::native_app::ConfigEdit {
                key: crate::prefs::EDIT_THEME.to_string(),
                expected: ExpectedConfigValue::Exact(expected),
                value: Some(value),
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
            NativeConfigWork::ThemeChoice { id, side } => {
                NativeConfigWork::Patch(self.theme_choice_patch_request(&id, side)?)
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
                        .map(|edit| {
                            let ExpectedConfigValue::Exact(expected) = edit.expected;
                            ConfigKeyEdit {
                                key: edit.key,
                                expected: ExpectedValue::Exact(expected),
                                value: edit.value,
                            }
                        })
                        .collect(),
                })
            }
            NativeConfigWork::Undo(token) => self
                .native_config_service
                .undo(crate::native_config_service::UndoToken::from_stored(token)),
            NativeConfigWork::UndoLanding(_) => {
                return Err("the change to undo is no longer known".to_string());
            }
            NativeConfigWork::SeriousMode(_)
            | NativeConfigWork::ControlField { .. }
            | NativeConfigWork::ThemeChoice { .. } => {
                unreachable!("semantic config work is materialized above")
            }
        })
    }

    fn reduce_native_config_request(
        &mut self,
        request: NativeConfigRequest,
    ) -> (NativeConfigOrigin, Result<ConfigPatchResult, String>) {
        let NativeConfigRequest { origin, mut work } = request;
        if let NativeConfigOrigin::View { view, follows, .. } = &origin {
            let landings = self.take_native_config_landings(*view, follows);
            if let NativeConfigWork::Patch(patch) = &mut work {
                patch.base_revision = native_config_followed_base(&landings, patch.base_revision);
            }
            if let NativeConfigWork::UndoLanding(write) = work {
                match landings.iter().find(|landing| landing.operation == write) {
                    Some(NativeConfigLanding {
                        undo: Some(token), ..
                    }) => work = NativeConfigWork::Undo(*token),
                    Some(_) => {
                        let snapshot = self.native_config_service.snapshot();
                        return (origin, Ok(ConfigPatchResult::Unchanged { snapshot }));
                    }
                    None => {
                        return (
                            origin,
                            Err("the change it would undo was not saved".to_string()),
                        );
                    }
                }
            }
        }
        let result = self.reduce_native_config_work(work);
        (origin, result)
    }

    pub(crate) fn pump_native_config(&mut self) -> Result<(), String> {
        while !self.native_config_inflight {
            // A watcher candidate owns file authority from receipt through
            // parse/assets/font preparation; hold semantic writes until that
            // baseline is admitted or rejected. Once a generation is retained,
            // reconciliation orders it against a concurrent durable write.
            if self.config_watch_admission_pending()
                && self.native_config_external_pending.is_none()
            {
                return Ok(());
            }
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
            let proxy = self
                .proxy
                .clone()
                .ok_or_else(|| "native config persistence needs an event-loop proxy".to_string())?;
            let queue = native_config_queue()?;
            let request = self
                .native_config_pending
                .pop_front()
                .expect("queue was checked before capability acquisition");
            let (origin, result) = self.reduce_native_config_request(request);
            let result = match result {
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
                    let job = NativeConfigJob::Persist(Box::new(NativeConfigPersistenceJob {
                        plan,
                        undo: Some(undo.get()),
                        origin,
                        proxy,
                    }));
                    if let Err(error) = queue.send(job) {
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

    /// Main-thread half of the config worker protocol.
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
        let saved_wording = match &outcome {
            ConfigPatchOutcome::Applied { .. } => "Config was saved, but its exact disk generation",
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
                self.fail_native_config_reconciliation(&error);
                return;
            }
        };

        if self.native_config_external_sequence > completion.pending_sequence {
            // Admit the sample as an intermediate generation, retain the newer
            // watcher payload, and sample once more before any write.
            if let Err(error) = self
                .native_config_service
                .synchronize_prepared_observation(prepared)
            {
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
                trail_style: std::sync::Arc::clone(&prepared.assets.trail_style),
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
                // Invalid TOML is still valid UTF-8: Manual gets the exact watcher
                // bytes so its diagnostics can help repair them, while live Config
                // stays unchanged.
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

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "HarnessPolicyOneWriter",
            action = "Deliver",
            project = "aterm_gui::config_watcher::tests::project_harness_writer"
        )
    )]
    pub(crate) fn adopt_harness_policy(
        &mut self,
        stamped: crate::harness_policy_cell::StampedHarnessPolicy,
    ) {
        // The join: a stamp no newer than the one in force changes nothing.
        if !self.harness_policy.offer(stamped) {
            return;
        }
        let running = self.harness_policy.get().clone();
        if let Some(host) = self.harness.as_ref() {
            host.set_config(running.policy.clone());
        }
        self.config.harness = running;
        self.note_occupancy_mode(self.config.harness.policy.occupancy);
        self.announce_harness_policy();
        self.publish_harness_posture();
        self.note_harness_arms();
        self.harness_screen_moved();
        self.publish_needs_all();
        self.mark_status_dirty();
        self.request_redraw_all_windows();
    }

    /// The native config lane's own error.
    pub(crate) fn surface_native_config_lane_error(&mut self, message: String) {
        aterm_log::warn!("native config: {message}");
        self.post_message(crate::message_reporters::config_lane_error(&message));
    }

    /// Answer every still-queued semantic request with one bounded rejection and empty the lane.
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

    fn take_native_config_landings(
        &mut self,
        view: crate::tab_model::ViewId,
        follows: &[crate::native_app::OperationId],
    ) -> Vec<NativeConfigLanding> {
        let landings = self
            .native_config_view_landings
            .iter()
            .filter(|landing| landing.view == view && follows.contains(&landing.operation))
            .copied()
            .collect();
        self.forget_unfollowed_native_config_landings();
        landings
    }

    fn forget_unfollowed_native_config_landings(&mut self) {
        let pending = &self.native_config_pending;
        self.native_config_view_landings
            .retain(|landing| native_config_followed(pending, landing.view, landing.operation));
    }

    fn note_native_config_view_landing(
        &mut self,
        view: crate::tab_model::ViewId,
        operation: crate::native_app::OperationId,
        outcome: &ConfigPatchOutcome,
    ) {
        self.forget_unfollowed_native_config_landings();
        if let ConfigPatchOutcome::Applied { revision, undo } = outcome
            && native_config_followed(&self.native_config_pending, view, operation)
        {
            self.native_config_view_landings.push(NativeConfigLanding {
                view,
                operation,
                revision: *revision,
                undo: *undo,
            });
        }
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
                theme_request,
                follows: _,
            } => {
                self.note_native_config_view_landing(view, reply.operation, &outcome);
                if let Some(theme_request) = theme_request {
                    let (request_id, id) = *theme_request;
                    self.publish_theme_choice_completion(
                        request_id,
                        &id,
                        &outcome,
                        authoritative.as_ref(),
                        false,
                    );
                }
                self.publish_native_config_completion(
                    instance,
                    view,
                    reply,
                    outcome,
                    reconciled_changed,
                );
            }
            NativeConfigOrigin::SeriousMode { desired } => self.publish_serious_mode_completion(
                desired,
                outcome,
                authoritative,
                synchronization_error,
            ),
            NativeConfigOrigin::Presence { key } => {
                self.publish_presence_completion(key, &outcome, authoritative.as_ref());
            }
            NativeConfigOrigin::StatsBars { form } => {
                self.publish_stats_bars_completion(form, &outcome)
            }
            NativeConfigOrigin::DeckSize { pref } => {
                self.publish_deck_size_completion(pref, &outcome)
            }
            NativeConfigOrigin::HarnessSwitch { key } => {
                self.publish_harness_switch_completion(key, &outcome, authoritative.as_ref());
            }
            NativeConfigOrigin::ThemeChoice { request_id, id } => {
                self.publish_theme_choice_completion(
                    request_id,
                    &id,
                    &outcome,
                    authoritative.as_ref(),
                    true,
                );
            }
            NativeConfigOrigin::Control {
                request_id,
                key,
                value,
                reply,
            } => {
                let serious_mode = key == crate::prefs::EDIT_SERIOUS_MODE;
                if serious_mode && let Some(snapshot) = authoritative.as_ref() {
                    self.apply_serious_mode_config_snapshot(snapshot);
                }
                let landed = authoritative.clone().or_else(|| {
                    matches!(outcome, ConfigPatchOutcome::Applied { .. })
                        .then(|| self.native_config_service.snapshot())
                });
                let written = landed.as_ref().map_or_else(
                    || value.clone(),
                    |snapshot| snapshot.semantic_values.get(&key).cloned(),
                );
                let system_dark = self.os_appearance == aterm_types::Appearance::Dark;
                let now = landed
                    .as_ref()
                    .filter(|_| written.is_none())
                    .and_then(|snapshot| {
                        crate::native_settings::effective_value_words(
                            snapshot,
                            &key,
                            system_dark,
                            crate::app_settings::settings_font_families(),
                        )
                    });
                let mut response = control_settings_completion_reply(
                    &key,
                    written.as_deref(),
                    now.as_deref(),
                    &outcome,
                    synchronization_error.as_deref(),
                );
                if key == crate::prefs::EDIT_THEME {
                    self.publish_theme_choice_completion(
                        request_id,
                        value.as_deref().unwrap_or("Default"),
                        &outcome,
                        authoritative.as_ref(),
                        false,
                    );
                    if matches!(outcome, ConfigPatchOutcome::Applied { .. })
                        && authoritative.is_none()
                    {
                        response =
                            Err("the save completed, but its theme could not be verified"
                                .to_string());
                    }
                }
                if key == crate::prefs::EDIT_THEME
                    && let Ok(status) = response.as_mut()
                    && let Some(snapshot) = authoritative.as_ref()
                {
                    for miss in
                        crate::app_config::theme_misses(&snapshot.config, &snapshot.assets.themes)
                    {
                        status.push_str("; ");
                        status.push_str(&miss);
                    }
                }
                self.refresh_serious_mode_queued_projection();
                let _ = reply.send(response);
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

        self.refresh_serious_mode_queued_projection();
        self.request_redraw_all_windows();
    }

    fn apply_serious_mode_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) -> bool {
        self.config.serious_mode = snapshot.config.serious_mode;
        let enabled = self.apply_serious_mode(snapshot.config.serious_mode_or_default());
        self.publish_native_config_snapshot(snapshot);
        enabled
    }

    /// Feed one stable file observation through the serialized config lane.
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

    /// Publish one admitted config generation to every live Settings view.
    pub(crate) fn publish_native_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) {
        self.note_config_keys(snapshot);
        self.dispatch_settings_event(&AppEvent::ConfigChanged(snapshot.clone()));
        if let Some(document) = self.native_runtime.config_editor_document() {
            self.request_config_host_diagnostics(document);
        }
    }

    fn note_config_keys(&mut self, snapshot: &crate::native_config_service::ConfigSnapshot) {
        if std::sync::Arc::ptr_eq(&self.node_log_values, &snapshot.semantic_values) {
            return;
        }
        let (names, unnamed) = crate::session_log_node::changed_config_keys(
            &self.node_log_values,
            &snapshot.semantic_values,
        );
        if !names.is_empty() || unnamed > 0 {
            self.node_log.note_config(&names, unnamed);
        }
        self.node_log_values = std::sync::Arc::clone(&snapshot.semantic_values);
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
        }
    }

    /// Admit one Packages verb.
    pub(crate) fn execute_native_packages(&mut self, request: PackagesRequest) -> PackagesOutcome {
        let atpkg = crate::co_located_atpkg();
        if let Err(message) = packages_request_admissible(&request) {
            return PackagesOutcome::Failed { message };
        }
        let private_state_root = aterm_types::dirs::runs_under_private_state_root();
        if let Some(message) = packages_request_refusal(&request, private_state_root) {
            return PackagesOutcome::Blocked { message };
        }
        let argv = packages_argv(&request);
        let machine_apply = matches!(request, PackagesRequest::MachineApply);
        let check = matches!(request, PackagesRequest::CheckUpdate);
        let install = matches!(request, PackagesRequest::InstallDefaultSet);
        let person = person_verb(&request);
        let Some(atpkg) = atpkg else {
            return PackagesOutcome::Failed {
                message: "the package tool is not installed beside aterm".to_string(),
            };
        };
        if !machine_apply && !atpkg::manager_enabled() {
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
            return PackagesOutcome::Blocked {
                message: "Still collecting package status — try again in a moment.".to_string(),
            };
        };
        self.publish_native_packages_state();
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-packages-verb",
            crate::qos::Role::Background,
            move || {
                let mut command = PackagesCommandOutcome::Succeeded { operation: busy };
                let mut machine_verdict: Option<String> = None;
                let mut machine_state: Option<atpkg::machine::MachineState> = None;
                let layout = atpkg::store::resolve_configured();
                if check && let Some(pid) = layout.as_ref().and_then(atpkg::lock::person_holder) {
                    command = PackagesCommandOutcome::Failed {
                        operation: busy,
                        message: check_held_by_person_message(pid),
                    };
                }
                let process =
                    (!matches!(command, PackagesCommandOutcome::Failed { .. })).then_some(argv);
                let tell = |event: crate::messages_host::PersonPass| {
                    if person.is_some() {
                        let _ = proxy.send_event(Wake::PkgPersonPass(event));
                    }
                };
                for verb in process.iter() {
                    if let Some(v) = person {
                        tell(crate::messages_host::PersonPass::Began(v));
                    }
                    let reads_stdout = machine_apply || busy.applies_machine_settings();
                    let mut child = crate::qos::atpkg_command(&atpkg);
                    if check {
                        child
                            .args(crate::pass_lead(private_state_root))
                            .args(crate::pass_args(
                                crate::PassVerb::Update,
                                layout.as_ref(),
                                None,
                            ))
                            .env(atpkg::cli::SPAWNER_PID_ENV, std::process::id().to_string())
                            .env("PATH", crate::spawn::atpkg_child_path());
                    } else if install {
                        child
                            .args(crate::pass_lead(private_state_root))
                            .args(verb)
                            .args(crate::pass_wait_and_progress_args(layout.as_ref()))
                            .env(atpkg::cli::SPAWNER_PID_ENV, std::process::id().to_string())
                            .env("PATH", crate::spawn::atpkg_child_path());
                    } else {
                        child.args(verb);
                    }
                    child
                        .stdin(aterm_uds::spawnfd::Stdio::null())
                        .stdout(if reads_stdout {
                            aterm_uds::spawnfd::Stdio::piped()
                        } else {
                            aterm_uds::spawnfd::Stdio::null()
                        })
                        .stderr(aterm_uds::spawnfd::Stdio::piped());
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
                    if check || install {
                        let post_proxy = proxy.clone();
                        let post = move |event: Wake| {
                            let person = match &event {
                                Wake::PkgMachineState(_) => return,
                                Wake::PkgLockWaiting { .. } => {
                                    Some(crate::messages_host::PersonPass::Waiting)
                                }
                                Wake::PkgLockAcquired { .. } => {
                                    Some(crate::messages_host::PersonPass::Running)
                                }
                                _ => None,
                            };
                            let _ = post_proxy.send_event(event);
                            if let Some(person) = person {
                                let _ = post_proxy.send_event(Wake::PkgPersonPass(person));
                            }
                        };
                        let ran = crate::run_pass_child(
                            &mut child,
                            layout.as_ref(),
                            false,
                            aterm_messages::Waiter::Person,
                            &post,
                        );
                        let (failure, ran_pass) = match &ran {
                            Ok((run, status)) => (
                                check_pass_failure(
                                    run,
                                    status.as_ref().copied(),
                                    verb,
                                    layout.as_ref(),
                                ),
                                run.ran(),
                            ),
                            Err(error) => (packages_child_failure(Err(error), verb, ""), true),
                        };
                        tell(crate::messages_host::PersonPass::Ended(person_echo(
                            failure.is_some(),
                            ran_pass,
                        )));
                        if let Some(message) = failure {
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
                    let failure = packages_child_failure(result, verb, &said);
                    tell(crate::messages_host::PersonPass::Ended(person_echo(
                        failure.is_some(),
                        true,
                    )));
                    if let Some(message) = failure {
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
            },
        );
        match spawn {
            Ok(_) => PackagesOutcome::Accepted,
            Err(error) => {
                let _ = self.native_packages_service.abort(sequence);
                self.publish_native_packages_state();
                PackagesOutcome::Failed {
                    message: format!("could not start the packages worker: {error}"),
                }
            }
        }
    }

    /// Record metadata for the co-located `atpkg` binary.
    pub(crate) fn note_native_package_tool(&mut self) -> bool {
        let available = crate::co_located_atpkg().is_some();
        self.native_packages_service.note_tool(available);
        available
    }

    /// Start one status-collection worker (no verb).
    pub(crate) fn start_native_packages_refresh(&mut self) {
        let available = self.note_native_package_tool();
        let Some(proxy) = self.proxy.clone() else {
            self.publish_native_packages_state();
            return;
        };
        let Some(sequence) = self.native_packages_service.begin(None) else {
            self.publish_native_packages_state();
            return;
        };
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-packages-status",
            crate::qos::Role::Background,
            move || {
                let report = crate::packages_screen::collect_packages_status(available);
                let completion = PackagesWorkerCompletion::refresh(report);
                let _ = proxy.send_event(Wake::NativePackagesFinished {
                    sequence,
                    completion,
                });
            },
        );
        if spawn.is_err() {
            let _ = self.native_packages_service.abort(sequence);
        }
        self.publish_native_packages_state();
    }

    /// Start an asynchronous `atpkg machine` state refresh.
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

    /// A queued rerun is already admitted by the completion reducer.
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

    /// Main-thread half of the packages worker protocol.
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

    /// Publish the Settings ▸ Messages projection to the Settings controller and fan the revision out to every Settings view.
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
        let words_offset_s = state.utc_offset_s;
        let words_change_in = state
            .words_change_at(|at| crate::presence::local_offset_at(i64::try_from(at / 1000).ok()?))
            .map(|at| std::time::Duration::from_millis(at.saturating_sub(state.now_unix_ms)));
        if !self
            .native_runtime
            .replace_settings_messages(state, revision)
        {
            return;
        }
        self.messages_last_publish = Some(std::time::Instant::now());
        self.messages_published_revision = revision;
        self.messages_words_change_in = words_change_in;
        self.messages_words_offset_s = words_offset_s;
        self.dispatch_to_settings_views(&AppEvent::MessagesChanged { revision });
    }

    /// [`Self::publish_native_messages_state`] when it is due.
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
        let moved = self.messages.projection_revision() != self.messages_published_revision;
        let due = match since {
            None => true,
            Some(since) if moved => since >= MESSAGES_PUBLISH_MIN_GAP,
            Some(since) => self
                .settings_messages_quiet_gap()
                .is_some_and(|gap| since >= gap),
        };
        if due {
            self.publish_native_messages_state();
        }
    }

    /// The instant the next publish falls due, for the loop's deadline fold.
    pub(crate) fn messages_publish_due_at(&self) -> Option<std::time::Instant> {
        self.native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)?;
        let gap = if self.messages.projection_revision() != self.messages_published_revision {
            MESSAGES_PUBLISH_MIN_GAP
        } else {
            self.settings_messages_quiet_gap()?
        };
        Some(
            self.messages_last_publish
                .map_or_else(std::time::Instant::now, |at| at + gap),
        )
    }

    fn settings_messages_quiet_gap(&self) -> Option<std::time::Duration> {
        let mut gap: Option<std::time::Duration> = None;
        let zone_moved = std::cell::OnceCell::new();
        let zone_moved = || {
            *zone_moved.get_or_init(|| {
                crate::presence::local_offset_s().unwrap_or(0) != self.messages_words_offset_s
            })
        };
        let shown = |ws: &crate::WindowState| {
            crate::messages_host::band_on_screen(ws)
                || (self.headless
                    && ws.os_window.is_none()
                    && ws.pending_reveal.is_none()
                    && !ws.occluded)
        };
        for tab in self
            .windows
            .values()
            .filter(|ws| shown(ws))
            .filter_map(|ws| ws.tab_set.active())
        {
            tab.root.visit(&mut |view| {
                if !matches!(
                    self.view_store.get(*view),
                    Some(crate::tab_model::View::Native(_))
                ) {
                    return;
                }
                if let Some(crate::native_app::AppViewState::Settings(state)) =
                    self.native_runtime.view_state(*view)
                {
                    let clock = match state.status_clock() {
                        Some(false) => Some(MESSAGES_PUBLISH_MIN_GAP),
                        Some(true) => Some(MESSAGES_FEEDBACK_WAIT),
                        None => None,
                    };
                    let tick = (state.route == crate::native_settings::SettingsRoute::Messages)
                        .then_some(self.messages_words_change_in)
                        .flatten()
                        .map(|until| {
                            if zone_moved() {
                                std::time::Duration::ZERO
                            } else {
                                until
                            }
                            .max(MESSAGES_PUBLISH_TICK)
                        });
                    for next in [clock, tick].into_iter().flatten() {
                        gap = Some(gap.map_or(next, |gap| gap.min(next)));
                    }
                }
            });
        }
        gap
    }

    /// Publish the shared packages projection to the Settings controller and fan the revision out to every Settings view.
    pub(crate) fn publish_native_packages_state(&mut self) {
        let revision = self.native_packages_service.revision();
        let state = self
            .native_packages_service
            .state(
                self.config.packages_enabled(),
                self.config.packages_auto_install(),
                self.package_update_loop_running,
            )
            .with_machine_config(self.config.machine_universal_control())
            .with_machine_rosetta(self.config.machine_rosetta())
            .with_retired_switch_note(self.config.packages_retired_switch_note());
        if !self
            .native_runtime
            .replace_settings_packages(state, revision)
        {
            return;
        }
        self.dispatch_to_settings_views(&AppEvent::PackagesChanged { revision });
    }

    /// Publish the `App::harness_state` projection to Settings ▸ Harness.
    pub(crate) fn publish_native_harness_state(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        self.note_land_at_offer();
        let state = self.harness_state();
        self.harness_screen.revision = self.harness_screen.revision.saturating_add(1);
        let revision = self.harness_screen.revision;
        if !self
            .native_runtime
            .replace_settings_harness(state, revision)
        {
            return;
        }
        self.dispatch_to_settings_views(&AppEvent::HarnessChanged { revision });
    }

    /// Deliver `event` to every Settings view and repaint every window that shows one.
    pub(crate) fn dispatch_to_settings_views(&mut self, event: &AppEvent) {
        let setting_views = self.dispatch_settings_event(event);
        for ws in self.windows.values_mut() {
            let shows_settings = ws
                .tab_set
                .tabs()
                .iter()
                .any(|tab| tab.root.any_leaf(&mut |view| setting_views.contains(view)));
            if shows_settings {
                ws.last_present = None;
                if let Some(window) = ws.os_window.as_ref() {
                    window.request_redraw();
                }
            }
        }
    }

    fn dispatch_settings_event(&mut self, event: &AppEvent) -> Vec<crate::tab_model::ViewId> {
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
        for &(instance, view) in &views {
            let Ok(outcome) = self.native_runtime.dispatch(instance, view, event.clone()) else {
                continue;
            };
            if outcome.effects.is_empty() {
                continue;
            }
            let Ok((wid, _)) = self.locate_native_view(view) else {
                continue;
            };
            for effect in outcome.effects {
                let _ = self.execute_native_effect(wid, instance, view, effect);
            }
        }
        views.into_iter().map(|(_, view)| view).collect()
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
                        message: "This document can no longer be reopened".to_string(),
                    };
                }
                match app.open_document_tab_in_window(wid, kind, &uri) {
                    Ok(_) => RecoveryOutcome::Opened {
                        message: format!("Opened in {}", kind.display_name()),
                    },
                    Err(message) => RecoveryOutcome::Failed { message },
                }
            };

        match request {
            RecoveryRequest::Retry(RecoveryCapability::SystemState) => {
                if self.open_system_state_tab_in_window(wid) {
                    RecoveryOutcome::Opened {
                        message: "Reopened System State".into(),
                    }
                } else {
                    RecoveryOutcome::Failed {
                        message: "System State could not be reopened".into(),
                    }
                }
            }
            RecoveryRequest::Retry(RecoveryCapability::Settings { route }) => {
                let Some(route) = crate::native_settings::SettingsRoute::from_path(&route) else {
                    return RecoveryOutcome::Denied {
                        message: "That Settings page no longer exists".to_string(),
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
        // Joining does no physical work and mints no event-loop capability, which
        // keeps the no-duplicate promise testable headless.
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
                let spawn = crate::qos::spawn_lane_thread(
                    "aterm-native-update-check",
                    crate::qos::Role::Background,
                    move || {
                        let status = aterm_update::check_now_with_settings(build, &provider);
                        let _ = proxy.send_event(Wake::NativeUpdateFinished {
                            ticket,
                            status: durable_update_status(status),
                        });
                    },
                );
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

    /// Main-thread half of the updater worker protocol.
    pub(crate) fn finish_native_update_check(
        &mut self,
        ticket: UpdaterWorkTicket,
        status: DurableUpdateStatus,
    ) {
        let completion = self.native_updater_service.finish_check(ticket, status);
        if completion != CheckCompletion::Reduced {
            return;
        }
        if self.native_updater_service.snapshot().staged.is_some() {
            self.native_stage_imported_at = Some(std::time::Instant::now());
        }
        self.publish_native_update_state();

        if let Some((purpose, _stale)) = self.deferred_native_update_reconcile.take()
            && !self.request_native_update_reconcile(purpose)
        {
            if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                self.refuse_control_apply_without_facts();
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
            #[cfg(unix)]
            self.roster_wait_check_reported(build);
            self.arm_native_auto_apply(build, &digest);
        }
        self.try_pending_native_auto_apply(true);
    }

    /// Bring the automatic lane's deadlines forward by the time the Mac slept since the last turn of the event loop.
    pub(crate) fn count_sleep_toward_update_deadlines(
        &mut self,
        now: std::time::Instant,
        wall: std::time::SystemTime,
    ) -> Option<std::time::Duration> {
        let (then, then_wall) = self.update_deadline_clock.replace((now, wall))?;
        let slept = wall
            .duration_since(then_wall)
            .unwrap_or_default()
            .saturating_sub(now.saturating_duration_since(then));
        if slept < SLEEP_COUNTS_FROM {
            return None;
        }
        let forward = |at: std::time::Instant| {
            if at > now {
                at.checked_sub(slept).map_or(now, |at| at.max(now))
            } else {
                at
            }
        };
        if let Some(latch) = self.auto_apply_manual_only.as_mut() {
            latch.retry_at = latch.retry_at.map(forward);
        }
        #[cfg(unix)]
        if let Some(wait) = self.auto_apply_roster_wait.as_mut() {
            wait.reread_at = forward(wait.reread_at);
        }
        if let Some(verdict) = self.auto_apply_structural_verdict.as_mut() {
            verdict.resample_at = verdict.resample_at.map(forward);
            if let Some(retry) = verdict.newer_retry.as_mut() {
                retry.at = forward(retry.at);
            }
        }
        Some(slept)
    }

    /// The time the Mac slept since the last turn of the event loop, counted toward every calendar clock.
    pub(crate) fn count_sleep_since_last_turn(
        &mut self,
        now: std::time::Instant,
        wall: std::time::SystemTime,
    ) {
        if let Some(slept) = self.count_sleep_toward_update_deadlines(now, wall) {
            self.count_sleep_toward_messages_clock(slept);
        }
    }

    const fn count_sleep_toward_messages_clock(&mut self, slept: std::time::Duration) {
        if let Some(until) = self.messages_words_change_in.as_mut() {
            *until = until.saturating_sub(slept);
        }
    }

    /// Let a manual-only latch lapse once its deadline passes, restoring automatic apply for that artifact.
    pub(crate) fn lapse_expired_auto_apply_manual_only(&mut self) -> bool {
        let now = std::time::Instant::now();
        let lapsed = self
            .auto_apply_manual_only
            .is_some_and(|manual| manual.retry_at.is_some_and(|at| now >= at));
        let verdict = self
            .auto_apply_manual_only
            .and_then(|manual| self.structural_verdict_of(manual));
        let retry_due = self.native_activation_supersede.is_none()
            && verdict
                .is_some_and(|verdict| verdict.newer_retry.is_some_and(|retry| now >= retry.at));
        if retry_due
            || (lapsed
                && verdict.is_some_and(|verdict| verdict.resample_at.is_some_and(|at| now >= at)))
        {
            let on_disk = self.native_verified_download_on_disk;
            return self.look_at_structural_latch(now, (on_disk > 0).then_some(on_disk));
        }
        if lapsed {
            aterm_log::info!(
                "update apply: the manual-only latch lapsed; automatic apply is \
                 eligible again"
            );
            self.auto_apply_manual_only = None;
            self.auto_overlap_retry = None;
        }
        lapsed
    }

    fn structural_verdict_of(
        &self,
        latch: crate::AutoApplyManualOnly,
    ) -> Option<crate::AutoApplyStructuralVerdict> {
        if self.auto_apply_environment_block.is_some_and(|blocked| {
            blocked.build == latch.build && blocked.dmg_sha256 == latch.dmg_sha256
        }) {
            return None;
        }
        self.auto_apply_structural_verdict
            .filter(|verdict| verdict.covers(latch.build, latch.dmg_sha256, latch.activation))
    }

    /// When a refused release behind the structural latch may be offered the install path again.
    pub(crate) fn structural_supersede_retry_at(&self) -> Option<std::time::Instant> {
        if self.native_activation_supersede.is_some() {
            return None;
        }
        let latch = self.auto_apply_manual_only?;
        self.structural_verdict_of(latch)?
            .newer_retry
            .map(|retry| retry.at)
    }

    fn structural_latch_trial_room(&self, latch: crate::AutoApplyManualOnly) -> Option<bool> {
        if !latch.activation {
            return Some(true);
        }
        self.native_installed_trial
            .filter(|(build, _)| *build == latch.build)
            .map(|(_, launches)| launches.saturating_add(1) < BOOT_TRIAL_LAUNCH_LIMIT)
    }

    /// Whether `build` is a latched installed activation with no deadline left whose boot trial measured no launch to spare.
    pub(crate) fn latched_activation_trial_spent(&self, build: u64) -> bool {
        self.auto_apply_manual_only
            .filter(|latch| latch.build == build && latch.activation && latch.retry_at.is_none())
            .is_some_and(|latch| self.structural_latch_trial_room(latch) == Some(false))
    }

    #[cfg(any(unix, test))]
    fn forget_installed_trial_count(&mut self) {
        self.native_installed_trial = None;
        self.native_installed_trial_stale_before = Some(std::time::Instant::now());
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateStructuralLatch",
            action = "Decide",
            project = "aterm_gui::native_updater_conformance::project_structural_latch"
        )
    )]
    fn look_at_structural_latch(
        &mut self,
        now: std::time::Instant,
        newest_download: Option<u64>,
    ) -> bool {
        use crate::native_update_auto_intent::{
            StructuralLatchDecision, StructuralLatchFacts, structural_latch,
        };
        let Some(latch) = self.auto_apply_manual_only else {
            return false;
        };
        let Some(mut verdict) = self.structural_verdict_of(latch) else {
            return false;
        };
        if self.native_activation_supersede.is_some() {
            // The retire runs off the event loop; the latch stands until it
            // answers (`finish_activation_supersede`). A latch past its deadline
            // must move it, or the deadline fold wakes on it forever.
            if latch.retry_at.is_some_and(|at| now >= at) {
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    retry_at: Some(now + STRUCTURAL_TRIAL_REMEASURE),
                    ..latch
                });
            }
            return false;
        }
        if verdict
            .newer_retry
            .is_some_and(|retry| newest_download != Some(retry.newer))
        {
            verdict.newer_retry = None;
            self.auto_apply_structural_verdict = Some(verdict);
        }
        let offerable = newest_download.filter(|newer| {
            latch.activation && *newer > latch.build && *newer > verdict.newer_spent
        });
        let waiting =
            offerable.is_some() && verdict.newer_retry.is_some_and(|retry| now < retry.at);
        let retry_due = verdict.newer_retry.is_some_and(|retry| now >= retry.at);
        let facts = StructuralLatchFacts {
            resample_due: verdict.resample_at.is_some_and(|at| now >= at),
            resample_owed: verdict.resample_at.is_some(),
            unspent_newer_download: offerable.filter(|_| !waiting),
            newer_download_waiting: waiting,
            trial_room: self.structural_latch_trial_room(latch),
        };
        if facts.trial_room.is_some() && verdict.unmeasured_looks != 0 {
            verdict.unmeasured_looks = 0;
            verdict.unmeasured_through = 0;
            self.auto_apply_structural_verdict = Some(verdict);
        }
        if !self.native_installed_trial_unproven && verdict.unproven_looks != 0 {
            verdict.unproven_looks = 0;
            verdict.unproven_through = 0;
            self.auto_apply_structural_verdict = Some(verdict);
        }
        match structural_latch(facts) {
            StructuralLatchDecision::Hold => {
                // A look its own deadline brought (the re-sample, now outranked by a release waiting out a refusal) must move that deadline or the deadline fold wakes on it forever.
                if facts.resample_due
                    && latch.retry_at.is_some_and(|at| now >= at)
                    && let Some(retry) = verdict.newer_retry
                {
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        retry_at: Some(retry.at),
                        ..latch
                    });
                }
                if verdict.held_unmeasured && facts.trial_room == Some(false) {
                    verdict.held_unmeasured = false;
                    self.auto_apply_structural_verdict = Some(verdict);
                    self.auto_apply_stranded_announced = None;
                    self.say_structural_hold_for_trial(latch.build);
                }
                false
            }
            StructuralLatchDecision::Supersede { newer } => {
                // Spent only once the retire is under way; a look that cannot start
                // it now leaves the release for the next look.
                if self.request_activation_supersede(latch, newer, verdict.newer_spent) {
                    verdict.newer_spent = newer;
                    verdict.newer_retry = None;
                    self.auto_apply_structural_verdict = Some(verdict);
                } else {
                    // Not started, nothing spent; a deadline that brought this look
                    // moves, so the deadline fold does not spin on it.
                    if retry_due {
                        verdict.newer_retry =
                            verdict.newer_retry.map(|retry| crate::SupersedeRetry {
                                at: now + STRUCTURAL_TRIAL_REMEASURE,
                                ..retry
                            });
                        self.auto_apply_structural_verdict = Some(verdict);
                    }
                    if facts.resample_due && latch.retry_at.is_some_and(|at| now >= at) {
                        self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                            retry_at: Some(now + STRUCTURAL_TRIAL_REMEASURE),
                            ..latch
                        });
                    }
                }
                false
            }
            StructuralLatchDecision::Unmeasured => {
                let due = latch.retry_at.is_some_and(|at| now >= at);
                if self.native_installed_trial_unproven {
                    self.look_over_unproven_reads(latch, verdict, now, due);
                    return false;
                }
                let read_since = self.native_installed_trial_read > verdict.unmeasured_through;
                if due
                    && read_since
                    && verdict.unmeasured_looks >= STRUCTURAL_TRIAL_UNMEASURED_LOOKS
                {
                    verdict.resample_at = None;
                    verdict.unmeasured_looks = 0;
                    verdict.unmeasured_through = 0;
                    verdict.held_unmeasured = true;
                    self.auto_apply_structural_verdict = Some(verdict);
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        retry_at: None,
                        ..latch
                    });
                    self.say_structural_hold(
                        latch.build,
                        &format!(
                            "automatic install of build {} is paused: aterm could not read how \
                             many times it has started ({} tries) — install it from the \
                             Version menu, or a newer release replaces it by itself",
                            latch.build, STRUCTURAL_TRIAL_UNMEASURED_LOOKS
                        ),
                    );
                } else if due {
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        retry_at: Some(now + STRUCTURAL_TRIAL_REMEASURE),
                        ..latch
                    });
                    if read_since {
                        verdict.unmeasured_looks = verdict.unmeasured_looks.saturating_add(1);
                        verdict.unmeasured_through = self.native_installed_trial_read;
                        self.auto_apply_structural_verdict = Some(verdict);
                    }
                    let asked =
                        self.request_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh);
                    aterm_log::info!(
                        "update auto-apply: build {}'s structural latch has earned an attempt, \
                         but its boot trial's launch count is not measured yet ({}); looking \
                         again in {} s",
                        latch.build,
                        if asked {
                            "an observation is on its way"
                        } else {
                            "no observation could be requested"
                        },
                        STRUCTURAL_TRIAL_REMEASURE.as_secs()
                    );
                } else {
                    aterm_log::debug!(
                        "update auto-apply: build {}'s structural latch waits for its boot \
                         trial's launch count to be measured",
                        latch.build
                    );
                }
                false
            }
            StructuralLatchDecision::Release => {
                verdict.resample_at = None;
                self.auto_apply_structural_verdict = Some(verdict);
                self.auto_apply_manual_only = None;
                self.auto_overlap_retry = None;
                aterm_log::info!(
                    "update auto-apply: build {}'s structural latch takes its ONE \
                     re-sample, {} h after it converged; if it fails the same way, \
                     automatic apply is done with these bytes",
                    latch.build,
                    STRUCTURAL_RESAMPLE_AFTER.as_secs() / 3600
                );
                true
            }
            StructuralLatchDecision::HoldForTrial => {
                verdict.resample_at = None;
                self.auto_apply_structural_verdict = Some(verdict);
                self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                    retry_at: None,
                    ..latch
                });
                self.say_structural_hold_for_trial(latch.build);
                false
            }
        }
    }

    fn say_structural_hold_for_trial(&mut self, build: u64) {
        let what = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .filter(|staged| staged.build == build)
            .map_or_else(
                || format!("build {build}"),
                |staged| crate::update_words::staged_build_name(build, &staged.version),
            );
        let reason = crate::update_words::trial_spent_refusal(&what);
        self.say_structural_hold(build, &reason);
    }

    fn look_over_unproven_reads(
        &mut self,
        latch: crate::AutoApplyManualOnly,
        mut verdict: crate::AutoApplyStructuralVerdict,
        now: std::time::Instant,
        due: bool,
    ) {
        if !due {
            aterm_log::debug!(
                "update auto-apply: build {}'s structural latch waits for a read that can \
                 check the installed bundle",
                latch.build
            );
            return;
        }
        let read_since = self.native_installed_trial_read > verdict.unproven_through;
        if read_since && verdict.unproven_looks >= STRUCTURAL_TRIAL_UNPROVEN_LOOKS {
            let tomorrow = now + STRUCTURAL_RESAMPLE_AFTER;
            verdict.resample_at = Some(tomorrow);
            verdict.unproven_looks = 0;
            verdict.unproven_through = 0;
            self.auto_apply_structural_verdict = Some(verdict);
            self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                retry_at: Some(tomorrow),
                ..latch
            });
            let reason = format!(
                "automatic install of build {} waits a day: aterm could not verify the \
                 installed copy just then ({} tries); it tries build {} again tomorrow",
                latch.build, STRUCTURAL_TRIAL_UNPROVEN_LOOKS, latch.build
            );
            aterm_log::warn!("update auto-apply: {reason}");
            aterm_update::record_apply_refusal(
                self.native_updater_service.snapshot().current_build,
                &reason,
            );
            return;
        }
        self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            retry_at: Some(now + STRUCTURAL_TRIAL_REMEASURE),
            ..latch
        });
        if read_since {
            verdict.unproven_looks = verdict.unproven_looks.saturating_add(1);
            verdict.unproven_through = self.native_installed_trial_read;
            self.auto_apply_structural_verdict = Some(verdict);
        }
        let asked = self.request_native_update_reconcile(NativeUpdateReconcilePurpose::Refresh);
        aterm_log::info!(
            "update auto-apply: build {}'s structural latch has earned an attempt, but the \
             installed bundle could not be checked just then ({}); looking again in {} s",
            latch.build,
            if asked {
                "an observation is on its way"
            } else {
                "no observation could be requested"
            },
            STRUCTURAL_TRIAL_REMEASURE.as_secs()
        );
    }

    fn say_structural_hold(&mut self, build: u64, reason: &str) {
        aterm_log::warn!("update auto-apply: {reason}");
        aterm_update::record_apply_refusal(
            self.native_updater_service.snapshot().current_build,
            reason,
        );
        let failures = self
            .auto_apply_physical_retry
            .filter(|retry| retry.build == build)
            .map_or(u32::from(STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS), |retry| {
                u32::from(retry.cycles)
            });
        self.announce_automatic_apply_stranded(
            build,
            PhysicalFailureShape::Structural,
            failures,
            None,
            reason,
        );
        self.restate_staged_bar_posture(build);
    }

    fn request_activation_supersede(
        &mut self,
        latch: crate::AutoApplyManualOnly,
        newer: u64,
        spent_before: u64,
    ) -> bool {
        let snapshot = self.native_updater_service.snapshot();
        if snapshot.active.is_some() || snapshot.phase == UpdaterPhase::Applying {
            return false;
        }
        let Some(activation_commit) = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == latch.build && staged.is_installed_activation())
            .and_then(|staged| staged.commit.clone())
        else {
            return false;
        };
        let job = ActivationSupersedeJob {
            current_build: snapshot.current_build,
            activation: latch.build,
            activation_commit,
            newer,
            spent_before,
        };
        aterm_log::info!(
            "update auto-apply: verified build {newer} arrived behind build {}'s structural \
             latch — retiring build {}'s installed activation so build {newer} installs over \
             the running build {} instead (no launch of build {})",
            latch.build,
            latch.build,
            job.current_build,
            latch.build
        );
        if !self.spawn_activation_supersede(job.clone()) {
            aterm_log::warn!(
                "update auto-apply: could not start retiring build {}'s activation for build \
                 {newer}; the next observation offers it again",
                latch.build
            );
            return false;
        }
        self.native_activation_supersede = Some(job);
        true
    }

    #[cfg(not(test))]
    fn spawn_activation_supersede(&mut self, job: ActivationSupersedeJob) -> bool {
        let Some(proxy) = self.proxy.clone() else {
            return false;
        };
        let reconcile = self
            .ensure_native_update_reconcile_worker()
            .zip(self.mint_native_update_reconcile_ticket());
        crate::qos::spawn_lane_thread(
            "aterm-update-supersede".to_string(),
            crate::qos::Role::Background,
            move || {
                let outcome = aterm_update::supersede_installed_activation(
                    job.current_build,
                    crate::build_info::git_commit(),
                    job.activation,
                    &job.activation_commit,
                    job.newer,
                );
                #[cfg(unix)]
                let facts = reconcile.and_then(|(worker, ticket)| {
                    collect_native_update_reconcile_facts(&worker, ticket, job.current_build)
                });
                #[cfg(not(unix))]
                let facts = {
                    let _ = reconcile;
                    None
                };
                let _ = proxy.send_event(crate::Wake::NativeActivationSuperseded(Box::new(
                    ActivationSupersedeCompletion {
                        job,
                        outcome,
                        facts,
                    },
                )));
            },
        )
        .is_ok()
    }

    #[cfg(test)]
    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    fn spawn_activation_supersede(&mut self, _job: ActivationSupersedeJob) -> bool {
        true
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateStructuralLatch",
            action = "Supersede",
            project = "aterm_gui::native_updater_conformance::project_structural_latch"
        )
    )]
    pub(crate) fn finish_activation_supersede(
        &mut self,
        completion: ActivationSupersedeCompletion,
    ) {
        let ActivationSupersedeCompletion {
            job,
            outcome,
            facts,
        } = completion;
        if self.native_activation_supersede.as_ref() == Some(&job) {
            self.native_activation_supersede = None;
        }
        match outcome {
            Ok(superseded) => {
                match superseded.trial_disarm_deferred.as_deref() {
                    None => aterm_log::info!(
                        "update auto-apply: build {} is back at the install path and build \
                         {}'s activation is retired (its boot trial disarmed at {} counted \
                         launch(es), none added); build {} installs next",
                        superseded.restored_build,
                        superseded.activation_build,
                        superseded.trial_launches,
                        superseded.newer_build
                    ),
                    Some(deferred) => aterm_log::info!(
                        "update auto-apply: build {} is back at the install path and build \
                         {}'s activation is retired ({deferred}; no launch added); build {} \
                         installs next",
                        superseded.restored_build,
                        superseded.activation_build,
                        superseded.newer_build
                    ),
                }
                match facts {
                    Some(facts) => self.finish_native_update_reconcile(
                        NativeUpdateReconcilePurpose::StageAvailable,
                        facts,
                    ),
                    None => {
                        let _ = self.request_native_update_reconcile(
                            NativeUpdateReconcilePurpose::StageAvailable,
                        );
                    }
                }
            }
            Err(refusal) => {
                let retry_in = self.give_back_refused_release(&job, std::time::Instant::now());
                self.say_activation_supersede_refused(
                    job.activation,
                    job.newer,
                    &refusal,
                    retry_in,
                );
                if let Some(facts) = facts {
                    self.finish_native_update_reconcile(
                        NativeUpdateReconcilePurpose::Refresh,
                        facts,
                    );
                }
            }
        }
    }

    fn give_back_refused_release(
        &mut self,
        job: &ActivationSupersedeJob,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        let verdict = self
            .auto_apply_structural_verdict
            .as_mut()
            .filter(|verdict| {
                verdict.build == job.activation && verdict.newer_spent == job.newer
            })?;
        verdict.newer_spent = job.spent_before;
        verdict.newer_refusals = verdict.newer_refusals.saturating_add(1);
        let wait = supersede_retry_after(verdict.newer_refusals);
        verdict.newer_retry = Some(crate::SupersedeRetry {
            newer: job.newer,
            at: now + wait,
        });
        Some(wait)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateStructuralLatch",
            action = "SupersedeRefused",
            project = "aterm_gui::native_updater_conformance::project_structural_latch"
        )
    )]
    fn say_activation_supersede_refused(
        &mut self,
        build: u64,
        newer: u64,
        refusal: &aterm_update::SupersedeRefusal,
        retry_in: Option<std::time::Duration>,
    ) {
        let again = retry_in.map_or_else(String::new, |wait| {
            format!(
                "; the lane tries again in {} min",
                wait.as_secs().div_ceil(60)
            )
        });
        let reason = if refusal.is_for_now() {
            format!(
                "automatic apply of build {build} waits: verified build {newer} could not take \
                 its place at the install path yet ({refusal}){again}"
            )
        } else {
            format!(
                "automatic apply of build {build} holds: verified build {newer} could not take \
                 its place at the install path ({refusal}) — the Version menu moves it{again}"
            )
        };
        aterm_log::warn!("update auto-apply: {reason}");
        aterm_update::record_apply_refusal(
            self.native_updater_service.snapshot().current_build,
            &reason,
        );
        if refusal.is_for_now() {
            return;
        }
        let failures = self
            .auto_apply_physical_retry
            .filter(|retry| retry.build == build)
            .map_or(u32::from(STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS), |retry| {
                u32::from(retry.cycles)
            });
        let resample_at = self
            .auto_apply_manual_only
            .filter(|manual| manual.build == build)
            .and_then(|manual| manual.retry_at);
        self.announce_automatic_apply_stranded(
            build,
            PhysicalFailureShape::Structural,
            failures,
            resample_at,
            &reason,
        );
        self.restate_staged_bar_posture(build);
    }

    /// Authenticate the staged candidate off the GUI thread and outside the parked window, caching the verdict for the next handoff attempt.
    #[cfg(unix)]
    pub(crate) fn spawn_staged_handoff_preverification(&mut self, build: u64) {
        let snapshot = self.native_updater_service.snapshot();
        let current_build = snapshot.current_build;
        let stage = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == build);
        let installed_activation = stage.is_some_and(|staged| staged.is_installed_activation());
        let commit = stage.and_then(|staged| staged.commit.clone());
        let artifact = stage.map(|staged| staged.dmg_sha256.clone());
        let (Some(commit), Some(artifact)) = (commit, artifact) else {
            return;
        };
        let now = std::time::Instant::now();
        {
            let cached = self
                .handoff_preverified
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !preverify_read_needed(
                cached.as_ref(),
                self.handoff_preverify_in_flight.as_ref(),
                build,
                &commit,
                &artifact,
                now,
            ) {
                return;
            }
        }
        let slot = std::sync::Arc::clone(&self.handoff_preverified);
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let passing = std::sync::Arc::new(std::sync::OnceLock::new());
        let flight = crate::PreverifyInFlight {
            build,
            commit: commit.clone(),
            artifact: artifact.clone(),
            deadline: now + crate::HANDOFF_PREVERIFY_READ_CEILING,
            done: std::sync::Arc::clone(&done),
            passing: std::sync::Arc::clone(&passing),
        };
        let spawned = std::thread::Builder::new()
            .name("aterm-update-preverify".to_string())
            .spawn(move || {
                let _ends = RaiseOnDrop(done);
                let passed = if installed_activation {
                    aterm_update::preverify_installed_for_handoff(current_build, build, &commit)
                } else {
                    aterm_update::preverify_staged_for_handoff(
                        current_build,
                        Some(crate::build_info::git_commit()),
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
                    if aterm_update::is_passing_refusal(error) {
                        let _ = passing.set(crate::PassingPreverification {
                            reason: format!("{which} failed pre-park verification: {error}"),
                            ended: std::time::Instant::now(),
                        });
                        return;
                    }
                }
                let reason = passed
                    .as_ref()
                    .err()
                    .map(|error| format!("{which} failed pre-park verification: {error}"));
                let policy = passed.as_ref().ok().and_then(|facts| {
                    aterm_update_core::handoff_policy::adopt(
                        &facts.policy,
                        current_build,
                        &format!("{which} build {build}"),
                    )
                });
                let grant_chunks = passed.as_ref().is_ok_and(|facts| facts.grant_chunks);
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
                        policy,
                        grant_chunks,
                    });
            });
        match spawned {
            Ok(_) => self.handoff_preverify_in_flight = Some(flight),
            Err(_) => aterm_log::warn!("update apply: pre-park verification thread unavailable"),
        }
    }

    /// Re-arm automatic apply for whatever is still staged after a latch lapsed.
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

    /// The close-preflight blocker for unsaved native-app (editor / Settings) work.
    pub(crate) const UNSAVED_NATIVE_WORK_BLOCKS_APPLY: &'static str =
        "Save or close the open editor to finish updating";
    /// The update row's words when only unsaved Settings text holds the install.
    pub(crate) const UNSAVED_SETTINGS_BLOCKS_APPLY: &'static str =
        "Save or close the unsaved Settings tabs to finish updating";
    /// The update row's words when an editor and Settings text both hold it.
    pub(crate) const UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY: &'static str =
        "Save or close the open editor and unsaved Settings tabs to finish updating";
    /// The close-preflight blocker while session restore / adoption is in flight.
    pub(crate) const RESTORE_IN_FLIGHT_BLOCKS_APPLY: &'static str =
        "Updating after session restore finishes";
    /// The `InstalledNeedsRelaunch` outcome's sentence.
    #[cfg(any(unix, test))]
    pub(crate) const INSTALLED_ACTIVATES_IN_PLACE: &'static str =
        "The update is installed; switching to it keeps your shells running";

    /// Whether a staged build is expected to land by itself.
    pub(crate) fn update_lands_by_itself(&self, auto_apply_on: bool) -> bool {
        self.update_lands_by_itself_with(auto_apply_on, self.seamless_handoff_unavailable())
    }

    fn update_lands_by_itself_with(
        &self,
        auto_apply_on: bool,
        handoff_unavailable: Option<crate::app_update_handoff::HandoffUnavailable>,
    ) -> bool {
        auto_apply_on
            && handoff_unavailable.is_none()
            && self.update_waits_on_person.is_none()
            && !self.kept_image_waits_for_terminal()
    }

    /// The update row for a close-preflight refusal the person can clear.
    pub(crate) fn update_blocker_for_person(reasons: &[String]) -> Option<&'static str> {
        let editor = reasons.iter().any(|reason| {
            reason == Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY
                || reason.starts_with(DIRTY_DOCUMENTS_BLOCK)
                || reason.starts_with(FAILED_CHECKPOINTS_BLOCK)
        });
        let settings = reasons
            .iter()
            .any(|reason| reason.starts_with(SETTINGS_DRAFTS_BLOCK));
        match (editor, settings) {
            (true, false) => Some(Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY),
            (false, true) => Some(Self::UNSAVED_SETTINGS_BLOCKS_APPLY),
            (true, true) => Some(Self::UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY),
            (false, false) => None,
        }
    }

    /// Return true while `ATERM_DEBUG_RELAUNCH_NUDGE` suppresses automatic relaunch.
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

    fn anchor_auto_apply_ladder(&mut self, build: u64, now: std::time::Instant) {
        if self
            .auto_apply_ladder
            .is_none_or(|ladder| ladder.build != build)
        {
            self.auto_apply_ladder = Some(crate::AutoApplyLadder {
                build,
                armed_at: now,
                announced: crate::native_update_auto_intent::ApplyPhase::PreferIdle,
                restored_hold_said: false,
            });
        }
    }

    /// Retain automatic apply intent for one exact (or superseding) staged build.
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
        let current_build = self.native_updater_service.snapshot().current_build;
        let decision = crate::native_update_auto_intent::arm(ArmFacts {
            enabled,
            current_build,
            armed_build: armed.map(|intent| intent.build),
            armed_exact: armed
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256),
            manual_only_exact: manual_only
                .is_some_and(|manual| manual.covers(build, dmg_sha256, incoming_activation)),
            manual_only_build: manual_only.map(|manual| manual.build),
            incoming_build: build,
        });
        #[cfg(unix)]
        if crate::native_update_auto_intent::arm_reads_candidate(decision, build, current_build) {
            self.spawn_staged_handoff_preverification(build);
        }
        match decision {
            ArmDecision::Clear => {
                self.auto_apply_intent = None;
                self.settle_update_wait_on_person();
                false
            }
            ArmDecision::SuppressManualOnly => {
                self.auto_apply_intent = None;
                self.settle_update_wait_on_person();
                self.restate_staged_bar_posture(build);
                false
            }
            ArmDecision::Keep => false,
            ArmDecision::Set(build) => {
                self.auto_apply_environment_block = None;
                #[cfg(unix)]
                if self
                    .auto_apply_roster_wait
                    .as_ref()
                    .is_some_and(|wait| !wait.covers(build, dmg_sha256))
                {
                    self.auto_apply_roster_wait = None;
                }
                self.auto_apply_manual_only = None;
                let now = std::time::Instant::now();
                let ladder = self
                    .auto_apply_ladder
                    .filter(|ladder| ladder.build == build);
                self.anchor_auto_apply_ladder(build, now);
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
                    retry_at: now,
                    attempts: 0,
                });
                self.restate_staged_bar_posture(build);
                true
            }
        }
    }

    /// Where the automatic lane stands on its ladder at `now` for its build (`auto_apply_ladder`).
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

    /// The ladder phase an attempt under `mode` is gated by at `now`.
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

    /// The terminal facts the ladder reads, sampled at `now`.
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
            harness_restored_pending: mode.is_automatic()
                && self
                    .harness
                    .as_ref()
                    .is_some_and(|host| host.restored_pending(now)),
        }
    }

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

    fn announce_restored_hold(&mut self, now: std::time::Instant) {
        let holding = self
            .harness
            .as_ref()
            .is_some_and(|host| host.restored_pending(now));
        let Some(ladder) = self.auto_apply_ladder.as_mut() else {
            return;
        };
        if let Some(line) = crate::native_update_auto_intent::restored_hold_line(
            ladder.build,
            holding,
            &mut ladder.restored_hold_said,
        ) {
            aterm_log::info!("{line}");
        }
    }

    /// Whether a person's Install is held for the candidate's pre-park read.
    #[must_use]
    pub(crate) const fn person_apply_is_held(&self) -> bool {
        #[cfg(unix)]
        {
            self.person_apply_held.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// Hold the automatic lane for an update check to confirm the stage's signer.
    #[cfg(unix)]
    pub(crate) fn hold_for_roster_confirmation(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        reason: &str,
        answered_at: std::time::Instant,
        rearm: bool,
        book: bool,
    ) -> crate::AutoApplyRosterWait {
        let now = std::time::Instant::now();
        let renewed = self
            .auto_apply_roster_wait
            .as_ref()
            .is_some_and(|wait| wait.covers(build, dmg_sha256) && wait.said);
        let wait = crate::AutoApplyRosterWait {
            build,
            dmg_sha256,
            reason: reason.to_string(),
            reread_at: now + crate::ROSTER_WAIT_REREAD,
            answered_at,
            rereading: false,
            said: true,
        };
        self.auto_apply_roster_wait = Some(wait.clone());
        let attempts = self
            .auto_apply_intent
            .filter(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256)
            .map(|intent| intent.attempts);
        if rearm || attempts.is_some() {
            self.auto_apply_intent = Some(crate::AutoApplyIntent {
                build,
                dmg_sha256,
                retry_at: wait.reread_at,
                attempts: attempts.unwrap_or(0),
            });
        }
        if renewed {
            aterm_log::debug!(
                "update auto-apply: build {build} still waits for an update check to confirm \
                 its signer: {reason}"
            );
        } else {
            aterm_log::info!(
                "update auto-apply: build {build} waits for an update check to confirm its \
                 signer against the machine roster ({reason}); it installs once one has, and \
                 nothing is counted against it meanwhile"
            );
            if book {
                aterm_update::record_apply_refusal(
                    self.native_updater_service.snapshot().current_build,
                    reason,
                );
            }
        }
        wait
    }

    /// Whether an update check reported the stage at `build` at or after `since`.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn roster_check_reported_since(
        &self,
        build: u64,
        since: std::time::Instant,
    ) -> bool {
        self.roster_check_reported
            .is_some_and(|(reported, at)| reported == build && at >= since)
    }

    #[cfg(unix)]
    fn hold_provisionally_for_a_fresh_read(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        stale_ended: std::time::Instant,
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        aterm_log::debug!(
            "update auto-apply: build {build}'s pre-park read answered that it waits for an \
             update check, but it began before a check reported the stage; reading it again"
        );
        if !self.start_fresh_preverification(build, dmg_sha256) {
            self.end_roster_wait_hold(build, dmg_sha256);
            return None;
        }
        let held = self
            .auto_apply_roster_wait
            .clone()
            .filter(|wait| wait.covers(build, dmg_sha256));
        self.auto_apply_roster_wait = Some(match held {
            Some(wait) => crate::AutoApplyRosterWait {
                answered_at: wait.answered_at.max(stale_ended),
                rereading: true,
                ..wait
            },
            None => crate::AutoApplyRosterWait {
                build,
                dmg_sha256,
                reason: String::new(),
                reread_at: now + crate::ROSTER_WAIT_REREAD,
                answered_at: stale_ended,
                rereading: true,
                said: false,
            },
        });
        Some(now + crate::ROSTER_WAIT_POLL)
    }

    #[cfg(unix)]
    fn end_roster_wait_hold(&mut self, build: u64, dmg_sha256: [u8; 32]) {
        let Some(wait) = self
            .auto_apply_roster_wait
            .take_if(|wait| wait.covers(build, dmg_sha256))
        else {
            return;
        };
        if wait.said {
            aterm_log::info!(
                "update auto-apply: build {build} is no longer held for an update check to \
                 confirm its signer; trying it"
            );
        }
    }

    #[cfg(unix)]
    fn roster_wait_holds(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        now: std::time::Instant,
    ) -> Option<std::time::Instant> {
        let reads_these = |read: &&crate::PreverifyInFlight| {
            read.build == build && decode_dmg_sha256(&read.artifact) == Some(dmg_sha256)
        };
        let (running, waited) = self
            .handoff_preverify_in_flight
            .as_ref()
            .filter(reads_these)
            .map_or((false, None), |read| {
                (
                    read.running_for(build, now),
                    read.ended_passing_at(now)
                        .filter(|(reason, _)| aterm_update::is_roster_confirmation_wait(reason))
                        .map(|(reason, ended)| (reason.to_string(), ended, read.started())),
                )
            });
        let held = self
            .auto_apply_roster_wait
            .clone()
            .filter(|wait| wait.covers(build, dmg_sha256));
        if let Some((reason, ended, started)) = waited
            && held.as_ref().is_none_or(|wait| ended > wait.answered_at)
        {
            if self.roster_check_reported_since(build, started) {
                return self.hold_provisionally_for_a_fresh_read(build, dmg_sha256, ended, now);
            }
            let wait =
                self.hold_for_roster_confirmation(build, dmg_sha256, &reason, ended, false, true);
            return Some(wait.reread_at);
        }
        let wait = held?;
        if running {
            return Some(now + crate::ROSTER_WAIT_POLL);
        }
        if !wait.rereading {
            if now < wait.reread_at {
                return Some(wait.reread_at);
            }
            self.auto_apply_roster_wait = Some(crate::AutoApplyRosterWait {
                rereading: true,
                ..wait
            });
            if self.start_fresh_preverification(build, dmg_sha256) {
                return Some(now + crate::ROSTER_WAIT_POLL);
            }
        }
        self.end_roster_wait_hold(build, dmg_sha256);
        None
    }

    /// Start a fresh pre-park read of the stage at `build` with `dmg_sha256`.
    #[cfg(unix)]
    pub(crate) fn start_fresh_preverification(&mut self, build: u64, dmg_sha256: [u8; 32]) -> bool {
        let before = self
            .handoff_preverify_in_flight
            .as_ref()
            .map(|read| read.deadline);
        self.spawn_staged_handoff_preverification(build);
        self.handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| {
                read.build == build
                    && decode_dmg_sha256(&read.artifact) == Some(dmg_sha256)
                    && Some(read.deadline) != before
            })
    }

    /// A check reported the stage at `build` (its `UpdateStaged` wake, or a person's check).
    #[cfg(unix)]
    pub(crate) fn roster_wait_check_reported(&mut self, build: u64) {
        let now = std::time::Instant::now();
        self.roster_check_reported = Some((build, now));
        let Some(wait) = self
            .auto_apply_roster_wait
            .clone()
            .filter(|wait| wait.build == build && !wait.rereading)
        else {
            return;
        };
        let (build, dmg_sha256) = (wait.build, wait.dmg_sha256);
        self.auto_apply_roster_wait = Some(crate::AutoApplyRosterWait {
            reread_at: now,
            ..wait
        });
        if let Some(intent) = self
            .auto_apply_intent
            .as_mut()
            .filter(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256)
        {
            intent.retry_at = intent.retry_at.min(now);
        }
    }

    /// What this process knows about the stage `attempt` targets waiting for a check to confirm its signer, and when that was answered.
    #[cfg(unix)]
    #[must_use]
    pub(crate) fn known_roster_wait(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        now: std::time::Instant,
    ) -> Option<KnownRosterWait> {
        let build = attempt.target_build();
        if let Some(read) = self
            .handoff_preverify_in_flight
            .as_ref()
            .filter(|read| read.reads(attempt))
            && let Some((reason, ended)) = read.ended_passing_at(now)
            && aterm_update::is_roster_confirmation_wait(reason)
            && !self.roster_check_reported_since(build, read.started())
        {
            return Some(KnownRosterWait {
                reason: reason.to_string(),
                answered_at: ended,
                held: false,
            });
        }
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        self.auto_apply_roster_wait
            .as_ref()
            .filter(|wait| {
                wait.covers(build, dmg_sha256)
                    && wait.said
                    && now.saturating_duration_since(wait.answered_at)
                        < crate::HANDOFF_PREVERIFY_FRESHNESS
                    && !self.roster_check_reported_since(build, wait.answered_at)
            })
            .map(|wait| KnownRosterWait {
                reason: wait.reason.clone(),
                answered_at: wait.answered_at,
                held: true,
            })
    }

    /// Attempt a retained automatic apply from an exact event or timer wake.
    pub(crate) fn try_pending_native_auto_apply(&mut self, announce: bool) {
        use crate::native_update_auto_intent::{
            ApplyPhase, AttemptDisposition, AttemptResult, PollDecision, PollFacts, WaitReason,
        };

        let Some(mut intent) = self.auto_apply_intent else {
            return;
        };
        let now = std::time::Instant::now();
        let deadline_ready = now >= intent.retry_at;
        if deadline_ready {
            self.capture_refusal_take_baseline();
        }
        let phase = self.automatic_apply_phase(now);
        self.announce_apply_phase(phase);
        self.announce_restored_hold(now);
        let work_active = self.native_updater_service.snapshot().active.is_some()
            || self.update_handoff_in_flight()
            || self.person_apply_is_held();
        let applying = self.native_updater_service.snapshot().phase == UpdaterPhase::Applying;
        if deadline_ready && !work_active && !applying {
            let Some(authoritative_intent) = self.auto_apply_intent else {
                self.settle_update_wait_on_person();
                return;
            };
            intent = authoritative_intent;
        }
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
                // The lane dropped its promise: restate the staged words to the
                // posture that now holds. A person's wait was a wait for this lane,
                // so it ends here too, with its standing row and the overdue notice's
                // exemption.
                if let Some(intent) = self.auto_apply_intent.take() {
                    self.restate_staged_bar_posture(intent.build);
                }
                self.settle_update_wait_on_person();
                return;
            }
            PollDecision::Wait(WaitReason::Deadline) => return,
            PollDecision::Wait(
                reason @ (WaitReason::WorkActive | WaitReason::Activity | WaitReason::StagePending),
            ) => {
                intent.retry_at = match reason {
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
        let activation = updater.staged.as_ref().is_some_and(|staged| {
            staged.build == attempt_build && staged.is_installed_activation()
        });
        if self.capture_refusal_holds(attempt_build, intent.dmg_sha256, activation, now) {
            intent.retry_at = self.capture_refusal_reprobe_at(now);
            self.auto_apply_intent = Some(intent);
            return;
        }
        #[cfg(unix)]
        if let Some(retry_at) = self.roster_wait_holds(attempt_build, intent.dmg_sha256, now) {
            intent.retry_at = retry_at;
            self.auto_apply_intent = Some(intent);
            return;
        }
        #[cfg(unix)]
        if self
            .handoff_preverify_in_flight
            .as_ref()
            .is_some_and(|read| {
                read.build == attempt_build
                    && decode_dmg_sha256(&read.artifact) == Some(intent.dmg_sha256)
                    && read.running_for(attempt_build, now)
            })
        {
            intent.retry_at = now + PREVERIFY_READ_POLL;
            self.auto_apply_intent = Some(intent);
            return;
        }
        intent.build = attempt_build;
        self.auto_apply_intent = None;
        let outcome = self.apply_native_update(if attempt_phase == ApplyPhase::PreferIdle {
            ApplyMode::Automatic
        } else {
            ApplyMode::AutomaticPastGrace
        });
        if let UpdateOutcome::Deferred { reason } = outcome {
            // A deferral is the ladder saying "not this instant". It returned
            // before `begin_apply_preflight` (no park, launch, ticket or budget),
            // so it is re-polled on the quiet cadence and not logged per poll;
            // the phase change is (`announce_apply_phase`). A path that re-armed
            // its own intent keeps it. A fork-lane park that missed its budget
            // was already surfaced by `fork_park_miss_outcome`.
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
        self.update_waits_on_person = match &outcome {
            UpdateOutcome::Blocked { reasons } => {
                Self::update_blocker_for_person(reasons).map(|_| intent.build)
            }
            _ => None,
        };
        if self.update_waits_on_person.is_none() {
            self.withdraw_update_waits_for_you();
        }
        let disposition = crate::native_update_auto_intent::finish(match &outcome {
            UpdateOutcome::Accepted => AttemptResult::Accepted,
            #[cfg(any(unix, test))]
            UpdateOutcome::InstalledNeedsRelaunch { .. } => AttemptResult::InstalledNeedsRelaunch,
            UpdateOutcome::Blocked { .. } => AttemptResult::Blocked,
            #[cfg(any(unix, test))]
            UpdateOutcome::CaptureRefused { .. } => AttemptResult::Blocked,
            UpdateOutcome::Failed { .. } => AttemptResult::Failed,
            UpdateOutcome::Deferred { .. } => unreachable!("returned above"),
        });
        match (disposition, outcome) {
            (AttemptDisposition::Complete, UpdateOutcome::Accepted) => {
                aterm_log::info!(
                    "update auto-apply started for build {} (new process launching; \
                     attempt {})",
                    intent.build,
                    intent.attempts
                );
            }
            #[cfg(any(unix, test))]
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
                let blocked = UpdateOutcome::Blocked {
                    reasons: reasons.clone(),
                };
                self.record_apply_outcome_in_ledger(&blocked);
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
                let first_exhaustion =
                    cooling_down && intent.attempts == MAX_AUTOMATIC_UPDATE_CYCLES;
                self.auto_apply_intent = Some(intent);
                self.restate_staged_bar_posture(intent.build);
                if intent.attempts == 1 {
                    aterm_log::info!(
                        "update auto-apply attempt 1 for build {} blocked by preflight: {}",
                        intent.build,
                        reasons.join(" · ")
                    );
                }
                if announce && intent.attempts == 1 {
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
                    if first_exhaustion {
                        aterm_log::warn!("{message}");
                        self.note_update_blockers(&reasons, true);
                    } else {
                        aterm_log::debug!("{message}");
                        if self.update_waits_on_person.is_some() && !self.update_wait_said {
                            self.note_update_blockers(&reasons, true);
                        }
                    }
                }
            }
            #[cfg(any(unix, test))]
            (AttemptDisposition::Retry, UpdateOutcome::CaptureRefused { message }) => {
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
                self.auto_apply_intent = None;
                let activation = self
                    .native_updater_service
                    .snapshot()
                    .staged
                    .as_ref()
                    .is_some_and(|staged| {
                        staged.build == intent.build && staged.is_installed_activation()
                    });
                #[cfg(unix)]
                let cached_refusal = self.cached_handoff_refusal_reason(intent.build);
                #[cfg(not(unix))]
                let cached_refusal: Option<String> = None;
                let needs_person = cached_refusal
                    .as_deref()
                    .is_some_and(crate::update_apply_trouble::ApplyTrouble::needs_person);
                if needs_person {
                    self.suspend_automatic_apply_for_person(
                        intent.build,
                        intent.dmg_sha256,
                        cached_refusal.as_deref().unwrap_or(""),
                        message,
                    );
                    return;
                }
                #[cfg(unix)]
                if let Some(wait) = cached_refusal
                    .as_deref()
                    .filter(|reason| aterm_update::is_roster_confirmation_wait(reason))
                {
                    let _ = self.hold_for_roster_confirmation(
                        intent.build,
                        intent.dmg_sha256,
                        wait,
                        std::time::Instant::now(),
                        true,
                        true,
                    );
                    self.restate_staged_bar_posture(intent.build);
                    return;
                }
                let shape = PhysicalFailureShape::cached_refusal_shape(cached_refusal.as_deref());
                let schedule = self.latch_after_physical_failure(
                    intent.build,
                    intent.dmg_sha256,
                    activation,
                    shape,
                    &message,
                );
                self.restate_staged_bar_posture(intent.build);
                self.surface_update_apply_outcome(
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
            }
            (_, outcome) => {
                self.auto_apply_intent = None;
                if self
                    .auto_apply_structural_verdict
                    .is_some_and(|verdict| verdict.build == intent.build)
                {
                    self.auto_apply_structural_verdict = None;
                }
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

    /// The quiet download's words changed.
    pub(crate) fn publish_quiet_update_words(&mut self) {
        #[cfg(test)]
        QUIET_WORD_PUBLISHES.with(|n| n.set(n.get() + 1));
        let snapshot = self.native_updater_service.snapshot();
        let revision = snapshot.revision;
        let checking = matches!(
            snapshot.phase,
            UpdaterPhase::Checking | UpdaterPhase::Available | UpdaterPhase::Downloading
        );
        let update = self.update_snapshot(checking);
        if self
            .native_runtime
            .replace_settings_update(update, revision)
        {
            self.dispatch_to_settings_views(&AppEvent::UpdateChanged { revision });
        }
    }

    pub(crate) fn publish_native_update_state(&mut self) {
        #[cfg(test)]
        FULL_UPDATE_PUBLISHES.with(|n| n.set(n.get() + 1));
        let snapshot = self.native_updater_service.snapshot();
        let revision = snapshot.revision;
        let checking = matches!(
            snapshot.phase,
            UpdaterPhase::Checking | UpdaterPhase::Available | UpdaterPhase::Downloading
        );
        let attention = snapshot.attention_pending();
        crate::update_control::publish_staged_activation(snapshot.staged.as_ref());
        let staged = snapshot
            .staged
            .as_ref()
            .map(|staged| (staged.build, staged.version.clone()));
        let update = self.update_snapshot(checking);
        self.native_runtime
            .replace_settings_update(update, revision);

        let setting_views = self.dispatch_settings_event(&AppEvent::UpdateChanged { revision });
        for ws in self.windows.values_mut() {
            let count = ws.tab_set.len();
            for index in 0..count {
                let Some(tab) = ws.tab_set.tab_at_mut(index) else {
                    continue;
                };
                if tab.root.any_leaf(&mut |view| setting_views.contains(view)) {
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
            observed_at,
            durable,
            installed,
            installed_unproven,
            installable,
        } = facts;
        if let Some(installable) = installable
            && self.native_updater_service.note_installability(installable)
        {
            self.publish_native_update_state();
        }
        if self
            .native_updater_service
            .note_checked_at(durable.as_ref().and_then(|durable| durable.checked_at))
        {
            self.publish_native_update_state();
        }
        let build = self.native_updater_service.snapshot().current_build;
        let trial_stale_before = self.native_installed_trial_stale_before;
        let trial_readable = trial_stale_before.is_none_or(|stale| observed_at >= stale);
        self.native_installed_trial = installed
            .as_ref()
            .filter(|_| trial_readable)
            .map(|installed| (installed.build, installed.trial_launches));
        if trial_readable {
            self.native_installed_trial_read = self.last_native_update_reconcile_sequence;
            self.native_installed_trial_unproven = installed_unproven;
        }
        if let Some(durable) = durable.as_ref() {
            self.native_verified_download_on_disk =
                crate::native_updater_service::verified_download_build(build, durable).unwrap_or(0);
        }
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
                        self.carry_manual_only_latch_across_swap(build, build, installed.as_ref());
                        let what = installed
                            .as_ref()
                            .and_then(|installed| installed.version.as_deref())
                            .map_or_else(|| format!("build {build}"), crate::update_words::aterm_v);
                        aterm_log::warn!(
                            "update sync: {what} is installed; switching to it keeps your \
                             shells running"
                        );
                    }
                    DurableStageDisposition::Retired => {
                        // The latch belongs to the exact retired artifact.
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
                            "update sync: the update on offer changed on disk; dropped it"
                        );
                    }
                    DurableStageDisposition::Unchanged => {}
                }
            }
        }

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
            let effective_stage = effective.cloned();
            self.look_at_structural_latch_after_reconcile();
            return NativeUpdateFactsResult::Reduced { effective_stage };
        }

        let stage_floor = build.max(observed_stage_floor);
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
                durable.outcome = NativeUpdaterService::installed_activation_outcome(
                    installed
                        .as_ref()
                        .and_then(|installed| installed.version.as_deref()),
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
                // Logged only when the import happens: while an activation is held,
                // `request_check` refuses and every ~75 s reconcile lands here again.
                if activation.is_some() {
                    aterm_log::info!("update sync: {}", durable.outcome);
                }
                let _ = self.native_updater_service.finish_check(ticket, durable);
                self.publish_native_update_state();
            }
        }
        self.look_at_structural_latch_after_reconcile();
        NativeUpdateFactsResult::Reduced {
            effective_stage: self.native_updater_service.snapshot().staged.clone(),
        }
    }

    fn look_at_structural_latch_after_reconcile(&mut self) {
        let on_disk = self.native_verified_download_on_disk;
        let _released = self
            .look_at_structural_latch(std::time::Instant::now(), (on_disk > 0).then_some(on_disk));
    }

    fn suspend_automatic_apply_for_person(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        refusal: &str,
        message: String,
    ) {
        let cause = EnvironmentBlockCause::of_refusal(refusal);
        self.block_native_auto_apply_environment(build, dmg_sha256, cause);
        self.restate_staged_bar_posture(build);
        aterm_log::warn!(
            "update auto-apply: build {build} cannot be applied by any lane until a person \
             changes {} — {refusal}; automatic apply is suspended for this artifact (no retry \
             is scheduled)",
            match cause {
                EnvironmentBlockCause::InstalledSource => "the installed bundle",
                EnvironmentBlockCause::InstallLocation => "the folder it is installed in",
            }
        );
        self.surface_update_apply_outcome(
            "automatic · needs you",
            UpdateOutcome::Failed { message },
            false,
        );
    }

    fn block_native_auto_apply_environment(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        cause: EnvironmentBlockCause,
    ) {
        self.auto_apply_intent = None;
        self.auto_apply_environment_block = Some(AutoApplyEnvironmentBlock {
            build,
            dmg_sha256,
            blocked_at: std::time::Instant::now(),
            cause,
        });
        self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
            build,
            dmg_sha256,
            activation: false,
            retry_at: None,
        });
    }

    /// The remedy of the environment block standing for `build`, or `None` when none stands.
    pub(crate) fn auto_apply_environment_remedy(
        &self,
        build: u64,
    ) -> Option<crate::update_apply_trouble::PersonRemedy> {
        self.auto_apply_environment_block
            .filter(|blocked| {
                self.auto_apply_environment_blocked(build)
                    && !(blocked.cause == EnvironmentBlockCause::InstallLocation
                        && self.install_location_answered_since(blocked))
            })
            .map(|blocked| blocked.cause.remedy())
    }

    #[cfg(unix)]
    fn rekey_environment_block_to(&mut self, newest: Option<&crate::HandoffPreverification>) {
        let Some(blocked) = self.auto_apply_environment_block else {
            return;
        };
        if !self.auto_apply_environment_blocked(blocked.build) {
            return;
        }
        let Some((refusal, at)) = newest
            .filter(|entry| {
                !entry.passed
                    && entry.build == blocked.build
                    && decode_dmg_sha256(&entry.artifact) == Some(blocked.dmg_sha256)
                    && entry.at > blocked.blocked_at
            })
            .and_then(|entry| {
                entry
                    .reason
                    .as_deref()
                    .filter(|reason| {
                        crate::update_apply_trouble::ApplyTrouble::needs_person(reason)
                    })
                    .map(|reason| (reason, entry.at))
            })
        else {
            return;
        };
        let cause = EnvironmentBlockCause::of_refusal(refusal);
        if cause == blocked.cause {
            return;
        }
        self.auto_apply_environment_block = Some(AutoApplyEnvironmentBlock {
            blocked_at: at,
            cause,
            ..blocked
        });
        aterm_log::warn!(
            "update auto-apply: build {} now waits on a person changing {} — {refusal}; \
             automatic apply stays suspended for this artifact",
            blocked.build,
            match cause {
                EnvironmentBlockCause::InstalledSource => "the installed bundle",
                EnvironmentBlockCause::InstallLocation => "the folder it is installed in",
            }
        );
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

    fn release_repaired_auto_apply_environment(&mut self, facts: &NativeUpdateReconcileFacts) {
        self.release_repaired_auto_apply_environment_for_commit(
            facts,
            crate::build_info::git_commit(),
        );
    }

    fn release_repaired_auto_apply_environment_for_commit(
        &mut self,
        facts: &NativeUpdateReconcileFacts,
        running_commit: &str,
    ) {
        #[cfg(unix)]
        {
            let cache = std::sync::Arc::clone(&self.handoff_preverified);
            let newest = match cache.try_lock() {
                Ok(cached) => Some(cached),
                Err(std::sync::TryLockError::Poisoned(error)) => Some(error.into_inner()),
                Err(std::sync::TryLockError::WouldBlock) => None,
            };
            if let Some(newest) = newest {
                self.rekey_environment_block_to(newest.as_ref());
            }
        }
        let Some(blocked) = self.auto_apply_environment_block else {
            return;
        };
        if !self.auto_apply_environment_blocked(blocked.build) {
            self.auto_apply_environment_block = None;
            return;
        }
        if blocked.cause == EnvironmentBlockCause::InstallLocation {
            if facts
                .durable
                .as_ref()
                .is_some_and(|status| status.enabled && status.installable == Some(true))
                && self.install_location_answered_since(&blocked)
            {
                self.auto_apply_environment_block = None;
                self.auto_apply_manual_only = None;
                aterm_log::info!(
                    "update apply: the install location took the swap's write in a \
                     verification of build {} after it refused; automatic eligibility restored",
                    blocked.build
                );
            }
            return;
        }
        let snapshot = self.native_updater_service.snapshot();
        if facts.observed_at <= blocked.blocked_at
            || !facts
                .durable
                .as_ref()
                .is_some_and(|status| status.enabled && status.installable == Some(true))
            || !facts.installed.as_ref().is_some_and(|installed| {
                installed.build == snapshot.current_build
                    && aterm_update::commit_matches(&installed.commit, running_commit)
            })
        {
            return;
        }
        #[cfg(unix)]
        {
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
        }
        self.auto_apply_environment_block = None;
        self.auto_apply_manual_only = None;
        aterm_log::info!(
            "update apply: installed source now verifies against the running image; \
             automatic eligibility restored for build {}",
            blocked.build
        );
    }

    fn install_location_answered_since(&self, blocked: &AutoApplyEnvironmentBlock) -> bool {
        #[cfg(unix)]
        {
            let staged_download = self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .is_some_and(|stage| {
                    stage.build == blocked.build && !stage.is_installed_activation()
                });
            if !staged_download {
                return false;
            }
            let cached = match self.handoff_preverified.try_lock() {
                Ok(cached) => cached,
                Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
                Err(std::sync::TryLockError::WouldBlock) => return false,
            };
            cached.as_ref().is_some_and(|entry| {
                entry.passed
                    && entry.build == blocked.build
                    && decode_dmg_sha256(&entry.artifact) == Some(blocked.dmg_sha256)
                    && entry.at > blocked.blocked_at
            })
        }
        #[cfg(not(unix))]
        {
            let _ = blocked;
            false
        }
    }

    pub(crate) fn apply_native_update(&mut self, mode: ApplyMode) -> UpdateOutcome {
        // A person's press already held for the candidate's pre-park read: a second
        // press joins it and runs with it once the read ends
        // (`App::run_held_person_apply`).
        if !mode.is_automatic() && self.person_apply_is_held() {
            return UpdateOutcome::Accepted;
        }
        if let Some(job) = self.native_activation_supersede.as_ref()
            && self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .is_some_and(|staged| {
                    staged.build == job.activation && staged.is_installed_activation()
                })
        {
            return UpdateOutcome::Deferred {
                reason: format!(
                    "the installed build {} is being replaced by the verified build {}",
                    job.activation, job.newer
                ),
            };
        }
        if !mode.is_automatic()
            && let Some(staged) = self
                .native_updater_service
                .snapshot()
                .staged
                .as_ref()
                .filter(|staged| self.latched_activation_trial_spent(staged.build))
        {
            return UpdateOutcome::Blocked {
                reasons: vec![crate::update_words::trial_spent_refusal(
                    &crate::update_words::staged_build_name(staged.build, &staged.version),
                )],
            };
        }
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
        let start = self.native_updater_service.begin_apply_preflight();
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
            ApplyPreflightStart::Applying => return UpdateOutcome::Accepted,
            ApplyPreflightStart::GenerationExhausted => {
                return UpdateOutcome::Failed {
                    message: "updater preflight identity space is exhausted".to_string(),
                };
            }
        };
        let mut shutdown_error = None;
        let visibility = if mode.is_automatic() {
            crate::app_tabs::ClosePreflightVisibility::Quiet
        } else {
            crate::app_tabs::ClosePreflightVisibility::Interactive
        };
        self.drive_owed_document_journals();
        let (readiness, safety_token) = match self
            .prepare_all_native_shutdown(self.update_close_scope(), visibility)
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
                    #[cfg(unix)]
                    Err(error) if error.held_for_read_of().is_some() => {
                        let held = error.held_for_read_of();
                        let message = error.into_message();
                        if self.native_updater_service.abort_apply(&attempt, message) {
                            self.publish_native_update_state();
                        }
                        if let Some(held) = held {
                            self.hold_person_apply(held);
                        }
                        UpdateOutcome::Accepted
                    }
                    #[cfg(unix)]
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
                    #[cfg(unix)]
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
                    #[cfg(unix)]
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
                    Err(error) => self.abort_unstarted_native_apply(&attempt, error.into_message()),
                }
            }
            ApplyDecision::Blocked(reasons) => {
                self.publish_native_update_state();
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

    /// Main-thread completion for any apply attempt that left this process alive, including the asynchronous overlap waiter.
    #[cfg(any(unix, test))]
    pub(crate) fn finish_async_native_update_handoff(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        lane: HandoffFailureLane,
    ) -> Option<UpdateOutcome> {
        self.reconcile_returned_native_apply_with_facts(attempt, facts, message, lane)
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateStructuralLatch",
            action = "AttemptFails",
            project = "aterm_gui::native_updater_conformance::project_structural_latch"
        )
    )]
    fn spend_physical_failure_budget(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        shape: PhysicalFailureShape,
    ) -> PhysicalFailureSchedule {
        let now = std::time::Instant::now();
        let verdict = self.auto_apply_structural_verdict.filter(|verdict| {
            shape == PhysicalFailureShape::Structural
                && verdict.covers(build, dmg_sha256, activation)
        });
        let spent = self
            .auto_apply_physical_retry
            .filter(|retry| {
                retry.covers(build, dmg_sha256, activation)
                    && (verdict.is_some()
                        || now.duration_since(retry.last_attempt) < PHYSICAL_RETRY_BUDGET_REPLENISH)
            })
            .map_or(0, |retry| retry.cycles);
        self.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
            build,
            dmg_sha256,
            activation,
            cycles: spent.saturating_add(1),
            last_attempt: now,
        });
        if let Some(verdict) = verdict {
            return PhysicalFailureSchedule::Converged {
                resample_at: verdict.resample_at,
            };
        }
        if spent >= shape.lifetime_attempts().saturating_sub(1) {
            if shape == PhysicalFailureShape::Structural {
                return self.mint_structural_verdict(build, dmg_sha256, activation, now);
            }
            return PhysicalFailureSchedule::converged(shape, now);
        }
        if shape == PhysicalFailureShape::Structural {
            return match automatic_retry_delay(0, AutomaticRetryKind::PhysicalFailure) {
                Some(delay) => PhysicalFailureSchedule::Retry(now + delay),
                None => self.mint_structural_verdict(build, dmg_sha256, activation, now),
            };
        }
        let within_epoch = spent % PHYSICAL_FAILURES_PER_EPOCH;
        match automatic_retry_delay(within_epoch, AutomaticRetryKind::PhysicalFailure) {
            Some(delay) => PhysicalFailureSchedule::Retry(now + delay),
            None => PhysicalFailureSchedule::StandDown(now + PHYSICAL_FAILURE_EPOCH_COOLDOWN),
        }
    }

    /// What this lane has booked against `attempt`'s bytes.
    #[cfg(unix)]
    pub(crate) fn cold_lane_budget(
        &self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        now: std::time::Instant,
    ) -> crate::app_update_handoff::ColdLaneBudget {
        use crate::app_update_handoff::{CarriedFailures, CarriedVerdict, ColdLaneBudget};
        let build = attempt.target_build();
        let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) else {
            return ColdLaneBudget::default();
        };
        let activation = attempt.is_installed_activation();
        ColdLaneBudget {
            failures: self
                .auto_apply_physical_retry
                .filter(|retry| retry.covers(build, dmg_sha256, activation))
                .map(|retry| CarriedFailures {
                    cycles: retry.cycles,
                    since_secs: now.saturating_duration_since(retry.last_attempt).as_secs(),
                    activation: retry.activation,
                }),
            verdict: self
                .auto_apply_structural_verdict
                .filter(|verdict| verdict.covers(build, dmg_sha256, activation))
                .map(|verdict| CarriedVerdict {
                    resample_in_secs: verdict
                        .resample_at
                        .map(|at| at.saturating_duration_since(now).as_secs()),
                    activation: verdict.activation,
                }),
        }
    }

    /// A refused cold relaunch handed to this process's automatic lane.
    #[cfg(unix)]
    pub(crate) fn seed_refused_cold_relaunch(
        &mut self,
        seed: crate::app_update_handoff::ColdRelaunchSeed,
        now: std::time::Instant,
    ) {
        let crate::app_update_handoff::ColdRelaunchSeed {
            verdict,
            roster_wait,
            dmg_sha256,
            budget,
        } = seed;
        let build = verdict.build;
        if roster_wait.is_none() {
            *self
                .handoff_preverified
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(verdict.clone());
            self.cold_relaunch_refused = Some(verdict);
        }
        let Some(dmg_sha256) = dmg_sha256 else {
            return;
        };
        if let Some(wait) = roster_wait.as_deref() {
            let _ = self.hold_for_roster_confirmation(build, dmg_sha256, wait, now, false, false);
        }
        if let Some(failures) = budget.failures {
            self.auto_apply_physical_retry = Some(crate::AutoOverlapRetry {
                build,
                dmg_sha256,
                activation: failures.activation,
                cycles: failures.cycles,
                last_attempt: now
                    .checked_sub(std::time::Duration::from_secs(failures.since_secs))
                    .unwrap_or(now),
            });
        }
        if let Some(carried) = budget.verdict {
            self.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
                build,
                dmg_sha256,
                activation: carried.activation,
                resample_at: carried
                    .resample_in_secs
                    .map(|secs| now + std::time::Duration::from_secs(secs)),
                newer_spent: 0,
                newer_retry: None,
                newer_refusals: 0,
                unmeasured_looks: 0,
                unmeasured_through: 0,
                unproven_looks: 0,
                unproven_through: 0,
                held_unmeasured: false,
            });
        }
    }

    fn mint_structural_verdict(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        now: std::time::Instant,
    ) -> PhysicalFailureSchedule {
        let schedule = PhysicalFailureSchedule::converged(PhysicalFailureShape::Structural, now);
        self.auto_apply_structural_verdict = Some(crate::AutoApplyStructuralVerdict {
            build,
            dmg_sha256,
            activation,
            resample_at: schedule.retry_at(),
            newer_spent: 0,
            newer_retry: None,
            newer_refusals: 0,
            unmeasured_looks: 0,
            unmeasured_through: 0,
            unproven_looks: 0,
            unproven_through: 0,
            held_unmeasured: false,
        });
        schedule
    }

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

    fn say_physical_failure_schedule(
        &mut self,
        build: u64,
        shape: PhysicalFailureShape,
        schedule: PhysicalFailureSchedule,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        let wait_secs = |at: std::time::Instant| at.saturating_duration_since(now).as_secs();
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
        let horizon = |at: std::time::Instant| {
            aterm_types::rfc3339::format_rfc3339(
                aterm_time::unix_secs()
                    .unwrap_or(0)
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
                 handoffs and re-samples {}at {}; the Version menu applies it sooner",
                if shape == PhysicalFailureShape::Structural {
                    "once "
                } else {
                    ""
                },
                horizon(at)
            ),
        });
        if let PhysicalFailureSchedule::Converged { resample_at } = schedule {
            self.announce_automatic_apply_stranded(build, shape, lifetime, resample_at, detail);
        }
    }

    /// The automatic lane stopped, said at once.
    pub(crate) fn announce_automatic_apply_stranded(
        &mut self,
        build: u64,
        shape: PhysicalFailureShape,
        failures: u32,
        resample_at: Option<std::time::Instant>,
        detail: &str,
    ) {
        let verdict = (build, resample_at.is_some());
        if self.auto_apply_stranded_announced == Some(verdict) {
            return;
        }
        self.auto_apply_stranded_announced = Some(verdict);
        let cause = match shape {
            PhysicalFailureShape::Structural => "the new version couldn't take over",
            PhysicalFailureShape::Transient => "the moment to switch kept passing",
            #[cfg(unix)]
            PhysicalFailureShape::Unexplained => "the new version kept quitting",
        };
        let next = match (shape, resample_at) {
            (_, None) if self.latched_activation_trial_spent(build) => {
                crate::update_words::NEWER_RELEASE_REPLACES.to_string()
            }
            (_, None) => crate::update_words::INSTALL_FROM_MENU.to_string(),
            (PhysicalFailureShape::Structural, Some(_)) => format!(
                "it {} in {} h, or {}",
                crate::update_words::TRIES_AGAIN,
                STRUCTURAL_RESAMPLE_AFTER.as_secs() / 3600,
                crate::update_words::INSTALL_FROM_MENU
            ),
            (_, Some(_)) => format!(
                "it {} every {} h, or {}",
                crate::update_words::TRIES_AGAIN,
                PHYSICAL_FAILURE_EPOCH_COOLDOWN.as_secs() / 3600,
                crate::update_words::INSTALL_FROM_MENU
            ),
        };
        let who = self
            .native_updater_service
            .snapshot()
            .staged
            .as_ref()
            .filter(|stage| stage.build == build)
            .map_or_else(
                || "the new version".to_string(),
                |stage| crate::update_words::aterm_v(&stage.version),
            );
        let body = format!("{who} didn't install after {failures} tries ({cause}); {next}");
        aterm_log::warn!(
            "update auto-apply: build {build} converged after {failures} failed handoffs \
             ({shape:?}); `aterm ctl update status` has the ledger; last failure: {detail}"
        );
        let title = aterm_update::health_failing_title("apply");
        let said_before = self.auto_apply_stranded_said.replace(build) == Some(build);
        let announced = self.note_update_health(title, &body);
        if announced {
            self.post_update_health_banner(crate::update_words::HealthKind::Install, title, &body);
        } else if said_before {
            self.restate_update_health(
                crate::update_words::HealthKind::of_updater_title(title),
                title,
                &body,
            );
        }
    }

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

    /// What a fork-lane park that missed its freeze budget comes back as.
    #[cfg(unix)]
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

    #[cfg(any(unix, test))]
    fn arm_activity_revoked_overlap_retry(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<std::time::Duration> {
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        let build = attempt.target_build();
        let activation = attempt.is_installed_activation();
        let now = std::time::Instant::now();
        let cycles = self
            .auto_overlap_retry
            .filter(|retry| {
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
        self.anchor_auto_apply_ladder(build, now);
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: now + delay,
            attempts: cycles,
        });
        Some(delay)
    }

    /// The desk as a capture refusal sees it ([`crate::CaptureDesk`]).
    #[must_use]
    pub(crate) fn capture_desk(&self) -> crate::CaptureDesk {
        use std::hash::{Hash as _, Hasher as _};
        crate::CaptureDesk::from_readings(
            self.pool
                .iter()
                .map(|session| {
                    let reading = crate::term_try_lock(&session.term).map(|terminal| {
                        let mut cwd = std::collections::hash_map::DefaultHasher::new();
                        terminal.current_working_directory().hash(&mut cwd);
                        let mut reading = std::collections::hash_map::DefaultHasher::new();
                        (
                            terminal.rows(),
                            terminal.cols(),
                            terminal.is_alternate_screen(),
                            cwd.finish(),
                        )
                            .hash(&mut reading);
                        reading.finish()
                    });
                    (session.id, reading)
                })
                .collect(),
        )
    }

    /// The park's capture refused this artifact's handoff.
    #[cfg(any(unix, test))]
    pub(crate) fn arm_capture_refusal_retry(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        let refusals = self
            .auto_apply_capture_refusal
            .as_ref()
            .filter(|prior| prior.covers(build, dmg_sha256, activation))
            .map_or(1, |prior| prior.refusals.saturating_add(1));
        let spacing = capture_refusal_spacing(refusals);
        let desk = self.capture_desk();
        let unread = desk.unread();
        self.auto_apply_capture_refusal = Some(crate::AutoApplyCaptureRefusal {
            build,
            dmg_sha256,
            activation,
            desk,
            refused_at: now,
            refusals,
            released: false,
        });
        self.auto_apply_manual_only = None;
        self.anchor_auto_apply_ladder(build, now);
        let attempts = self
            .auto_apply_intent
            .filter(|intent| intent.build == build)
            .map_or(1, |intent| intent.attempts);
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: self.capture_refusal_reprobe_at(now),
            attempts,
        });
        aterm_log::warn!(
            "update auto-apply: build {build} was refused by the park's capture ({detail}); \
             recorded as an apply failure (refusal {refusals} in a row), never latched — the \
             lane retries once the desk changes, no sooner than {}s from now (probed every \
             {}s{}), or after {} min",
            spacing.as_secs(),
            CAPTURE_REFUSAL_PROBE.as_secs(),
            if unread == 0 {
                String::new()
            } else {
                format!(
                    "; {unread} session(s) were busy, so each takes its first readable state as \
                     its baseline, looked for every {}s",
                    CAPTURE_REFUSAL_BASELINE_PROBE.as_secs()
                )
            },
            CAPTURE_REFUSAL_BACKSTOP.as_secs() / 60
        );
    }

    /// When the lane next looks at a standing capture refusal.
    #[must_use]
    pub(crate) fn capture_refusal_reprobe_at(&self, now: std::time::Instant) -> std::time::Instant {
        let probe = now + CAPTURE_REFUSAL_PROBE;
        let Some(refusal) = self
            .auto_apply_capture_refusal
            .as_ref()
            .filter(|refusal| !refusal.released)
        else {
            return probe;
        };
        if refusal.desk.complete() {
            probe.max(refusal.refused_at + capture_refusal_spacing(refusal.refusals))
        } else if now < refusal.refused_at + CAPTURE_REFUSAL_PROBE {
            now + CAPTURE_REFUSAL_BASELINE_PROBE
        } else {
            probe
        }
    }

    /// Read the sessions a standing capture refusal's baseline is missing, whatever the poll then decides.
    pub(crate) fn capture_refusal_take_baseline(&mut self) {
        if !self
            .auto_apply_capture_refusal
            .as_ref()
            .is_some_and(|refusal| !refusal.released && !refusal.desk.complete())
        {
            return;
        }
        let desk = self.capture_desk();
        if let Some(refusal) = self.auto_apply_capture_refusal.as_mut() {
            refusal.desk.fill_from(&desk);
        }
    }

    /// How much longer the standing capture refusal holds the next attempt at the soonest, however the desk moves.
    #[must_use]
    #[cfg(any(unix, test))]
    pub(crate) fn capture_refusal_spacing_left(
        &self,
        now: std::time::Instant,
    ) -> std::time::Duration {
        self.auto_apply_capture_refusal
            .as_ref()
            .filter(|refusal| !refusal.released)
            .map_or(std::time::Duration::ZERO, |refusal| {
                (refusal.refused_at + capture_refusal_spacing(refusal.refusals))
                    .saturating_duration_since(now)
            })
    }

    /// Whether a standing capture refusal still holds the attempt the poll just admitted.
    pub(crate) fn capture_refusal_holds(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        now: std::time::Instant,
    ) -> bool {
        let Some(refused_at) = self
            .auto_apply_capture_refusal
            .as_ref()
            .filter(|refusal| !refusal.released && refusal.covers(build, dmg_sha256, activation))
            .map(|refusal| refusal.refused_at)
        else {
            return false;
        };
        let held_for = now.saturating_duration_since(refused_at);
        let why = if held_for >= CAPTURE_REFUSAL_BACKSTOP {
            "the one-hour backstop elapsed"
        } else {
            let desk = self.capture_desk();
            let Some(refusal) = self.auto_apply_capture_refusal.as_mut() else {
                return false;
            };
            refusal.desk.fill_from(&desk);
            if held_for < capture_refusal_spacing(refusal.refusals) || !refusal.desk.moved_to(&desk)
            {
                return true;
            }
            "the desk changed"
        };
        if let Some(refusal) = self.auto_apply_capture_refusal.as_mut() {
            refusal.released = true;
        }
        aterm_log::info!(
            "update auto-apply: {why} since the park's capture refused build {build}; \
             attempting again"
        );
        false
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

    /// A handoff worker has killed and reaped its child, but ordered disk reconciliation may still be queued.
    #[cfg(any(unix, test))]
    pub(crate) fn abort_reaped_native_apply_before_reconcile(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
        lane: HandoffFailureLane,
    ) -> UpdateOutcome {
        self.forget_installed_trial_count();
        if self
            .native_updater_service
            .abort_apply(attempt, message.clone())
        {
            return self.settle_rearmed_apply_failure(attempt, message, lane, None);
        }
        UpdateOutcome::Failed { message }
    }

    #[cfg(any(unix, test))]
    fn settle_rearmed_apply_failure(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
        message: String,
        lane: HandoffFailureLane,
        facts: Option<NativeUpdateReconcileFacts>,
    ) -> UpdateOutcome {
        let publish = |app: &mut Self| {
            app.publish_native_update_state();
            if let Some(facts) = facts {
                app.reduce_returned_apply_facts(facts);
            }
        };
        #[cfg(unix)]
        {
            let cache = std::sync::Arc::clone(&self.handoff_preverified);
            let newest = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.rekey_environment_block_to(newest.as_ref());
        }
        #[cfg(unix)]
        if let HandoffFailureLane::AwaitsRosterConfirmation { automatic } = lane {
            if let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256()) {
                let _ = self.hold_for_roster_confirmation(
                    attempt.target_build(),
                    dmg_sha256,
                    &message,
                    std::time::Instant::now(),
                    automatic,
                    false,
                );
            }
            self.restate_staged_bar_posture(attempt.target_build());
            publish(self);
            return UpdateOutcome::Deferred { reason: message };
        }
        if !lane.charges_the_automatic_lane() {
            publish(self);
            return UpdateOutcome::Failed { message };
        }
        if lane == HandoffFailureLane::ActivityRevoked
            && let Some(delay) = self.arm_activity_revoked_overlap_retry(attempt)
        {
            publish(self);
            aterm_log::info!(
                "update apply: activity revoked the overlap; automatic retry in {delay:?}"
            );
            return UpdateOutcome::Deferred { reason: message };
        }
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
            publish(self);
            return UpdateOutcome::CaptureRefused { message };
        }
        #[cfg(unix)]
        if let HandoffFailureLane::Physical(PhysicalFailureShape::Structural) = lane
            && let Some(dmg_sha256) = decode_dmg_sha256(attempt.target_dmg_sha256())
            && let Some(refusal) = self
                .cached_handoff_refusal_reason(attempt.target_build())
                .filter(|refusal| crate::update_apply_trouble::ApplyTrouble::needs_person(refusal))
        {
            let build = attempt.target_build();
            self.block_native_auto_apply_environment(
                build,
                dmg_sha256,
                EnvironmentBlockCause::of_refusal(&refusal),
            );
            aterm_log::warn!(
                "update auto-apply: build {build} cannot be applied by any lane until a person \
                 acts — {refusal}; automatic apply is suspended for this artifact (no retry \
                 is scheduled)"
            );
            self.restate_staged_bar_posture(build);
            publish(self);
            return UpdateOutcome::Failed { message };
        }
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
        self.auto_apply_intent = None;
        self.restate_staged_bar_posture(attempt.target_build());
        publish(self);
        UpdateOutcome::Failed { message }
    }

    #[cfg(any(unix, test))]
    fn reconcile_returned_native_apply_with_facts(
        &mut self,
        attempt: crate::native_updater_service::ApplyAttemptTicket,
        facts: NativeUpdateReconcileFacts,
        message: String,
        lane: HandoffFailureLane,
    ) -> Option<UpdateOutcome> {
        self.forget_installed_trial_count();
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
                // The stage is re-armed on disk.
                Some(self.settle_rearmed_apply_failure(&attempt, message, lane, Some(facts)))
            }
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build } => {
                self.auto_apply_intent = None;
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
                self.reduce_returned_apply_facts(facts);
                Some(UpdateOutcome::InstalledNeedsRelaunch {
                    build,
                    message: Self::INSTALLED_ACTIVATES_IN_PLACE.to_string(),
                })
            }
            ReturnedApplyDisposition::Retired => {
                self.auto_apply_intent = None;
                if self.auto_apply_manual_only.is_some_and(|manual| {
                    manual.build == attempt.target_build()
                        && decode_dmg_sha256(attempt.target_dmg_sha256()) == Some(manual.dmg_sha256)
                }) {
                    self.auto_apply_manual_only = None;
                }
                self.publish_native_update_state();
                self.reduce_returned_apply_facts(facts);
                let changed = if attempt.is_installed_activation() {
                    "aterm on disk has since changed"
                } else {
                    "the download on disk has since changed"
                };
                Some(UpdateOutcome::Failed {
                    message: format!("{message}; {changed}"),
                })
            }
            ReturnedApplyDisposition::Ignored => None,
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Start",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "NativeUpdateEditorCarry",
            action = "Commit",
            project = "aterm_gui::editor_carry_conformance::Rig::project"
        )
    )]
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
        let drafts = DraftCarryTally::of(
            documents
                .iter()
                .filter(|document| self.document_store.dirty(**document) == Some(true))
                .map(|document| self.draft_carry(*document)),
            CARRIED_DRAFTS_CAP_BYTES,
        );
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
        let successor_reopens = self.successor_restores_document_tabs();
        let settings = SettingsDraftTally::of(
            self.view_store.iter().filter_map(|(view, _)| {
                match self.native_runtime.view_state(view)? {
                    crate::native_app::AppViewState::Settings(state) => {
                        Some(state.carried_field_drafts())
                    }
                    _ => None,
                }
            }),
            successor_reopens,
            CARRIED_SETTINGS_DRAFTS_CAP_BYTES,
        );

        let mut blockers = Vec::new();
        blockers.extend(settings.blocker());
        blockers.extend(drafts.blockers());
        if self.document_journal_writes_in_flight() {
            blockers.push("Wait: a draft journal write is in flight".to_string());
        }
        if self.document_journal_landed_while_parked() {
            blockers.push(
                "Wait: a draft journal was written after the successor began restoring".to_string(),
            );
        }
        if failed_checkpoints > 0 && !successor_reopens {
            blockers.push(format!(
                "{FAILED_CHECKPOINTS_BLOCK} {failed_checkpoints} document checkpoint(s) previously failed"
            ));
        }
        if pending_checkpoints > 0 {
            blockers.push(format!(
                "Wait: {pending_checkpoints} document checkpoint(s) are still running"
            ));
        }
        if self.restore_pass_queued() {
            blockers.push(Self::RESTORE_IN_FLIGHT_BLOCKS_APPLY.to_string());
        }
        if blockers.is_empty() {
            if drafts.carried > 0 {
                aterm_log::info!(
                    "update preflight: {} unsaved editor draft(s), {} bytes, are carried — each \
                     durable in its journal and verified on disk to replay to the editor's exact \
                     text; the files themselves are not written",
                    drafts.carried,
                    drafts.carried_bytes
                );
            }
            if settings.carried_drafts > 0 {
                aterm_log::info!(
                    "update preflight: {} unsaved Settings draft(s) in {} view(s), {} bytes, ride \
                     in the handoff layout; aterm.toml is not written",
                    settings.carried_drafts,
                    settings.carried_views,
                    settings.carried_bytes
                );
            }
            if failed_checkpoints > 0 {
                aterm_log::info!(
                    "update preflight: {failed_checkpoints} document(s) whose last save failed \
                     ride the update; any unsaved text reopens still unsaved"
                );
            }
            (
                ClosePreflight::Ready,
                Some(NativeUpdateSafetyToken {
                    carried_drafts: drafts.carried,
                    carried_settings_drafts: settings.carried_drafts,
                    failed_checkpoints,
                }),
            )
        } else {
            (ClosePreflight::Blocked(blockers), None)
        }
    }

    /// Re-run native document/restore safety at asynchronous handoff completion.
    #[cfg(unix)]
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

    /// The close scope an update asks every native app with.
    pub(crate) fn update_close_scope(&self) -> crate::native_app::CloseScope {
        if self.successor_restores_document_tabs() {
            crate::native_app::CloseScope::CarriedRelaunch
        } else {
            crate::native_app::CloseScope::Relaunch
        }
    }

    /// The automatic lane's close preflight, asked quietly (no tab switch, no focus move, no Close Recovery), for a launch-record renewal.
    #[cfg(target_os = "macos")]
    pub(crate) fn quiet_native_update_safety_token(
        &mut self,
    ) -> crate::launch_record_renewal::QuietPreflight {
        use crate::launch_record_renewal::QuietPreflight;
        self.drive_owed_document_journals();
        match self.prepare_all_native_shutdown(
            self.update_close_scope(),
            crate::app_tabs::ClosePreflightVisibility::Quiet,
        ) {
            Ok(true) => {}
            Ok(false) => {
                return QuietPreflight::Held(vec![
                    Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY.to_string(),
                ]);
            }
            Err(message) => return QuietPreflight::Broken(message),
        }
        match self.native_update_close_preflight() {
            (ClosePreflight::Ready, Some(token)) => QuietPreflight::Ready(token),
            (ClosePreflight::Ready, None) => QuietPreflight::Broken(
                "the close preflight returned Ready without safety evidence".to_string(),
            ),
            (ClosePreflight::Blocked(reasons), _) => QuietPreflight::Held(reasons),
        }
    }

    /// QA same-binary reexec through the production native-state preflight.
    pub(crate) fn apply_debug_seamless_update(&mut self) -> UpdateOutcome {
        self.drive_owed_document_journals();
        match self.prepare_all_native_shutdown(
            self.update_close_scope(),
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

    /// Viewing the exact published update revision quiets its one announcement without hiding the staged artifact or inventing a second notification state.
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

    /// Fold presentation across every leaf of the active split.
    pub(crate) fn refresh_active_split_presentation(&mut self, wid: WindowId) {
        let Some((tab_id, focused, leaves, stale_title)) =
            self.windows.get(&wid).and_then(|window| {
                let tab = window.tab_set.active()?;
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
                        icon: Some(presentation.icon),
                        indicators: crate::tab_model::TabIndicators {
                            dirty: presentation.indicators.dirty,
                            busy: presentation.indicators.busy,
                            attention: presentation.indicators.attention,
                            status_attention: false,
                        },
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

fn check_held_by_person_message(pid: u32) -> String {
    format!(
        "A package command running in a terminal (pid {pid}) is using the package store; \
         check again when it finishes."
    )
}

fn check_pass_failure(
    run: &crate::PassRun,
    result: Result<std::process::ExitStatus, &std::io::Error>,
    verb: &[String],
    layout: Option<&atpkg::store::Layout>,
) -> Option<String> {
    if run.code == Some(1)
        && let Some(cause) = run.recorded_cached_index_cause(layout)
    {
        return Some(cached_index_check_message(&cause));
    }
    packages_child_failure(result, verb, run.said.trim())
}

fn cached_index_check_message(cause: &str) -> String {
    format!(
        "The package index could not be reached ({cause}), so the pass ran on the last \
         verified copy of it \u{2014} try again later."
    )
}

fn packages_child_failure(
    result: Result<std::process::ExitStatus, &std::io::Error>,
    verb: &[String],
    said: &str,
) -> Option<String> {
    match result {
        Ok(status) if status.success() => None,
        Ok(status) if status.code() == Some(2) && verb.iter().any(|v| v == "--default-set") => {
            Some(
                "Nothing was installed: the registry served no package \
                 this machine can run. This is not a temporary error — \
                 retrying will not change it."
                    .to_string(),
            )
        }
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
        Ok(status) if status.code() == Some(i32::from(atpkg::lock::CONTENDED_EXIT)) => Some(
            "A toolchain pass is already running on this store; it finishes by itself \
                 \u{2014} try again in a moment."
                .to_string(),
        ),
        Ok(status) => Some(if said.is_empty() {
            format!("atpkg {} exited with {status}", verb.join(" "))
        } else {
            said.lines().last().unwrap_or(said).to_string()
        }),
        Err(error) => Some(format!(
            "could not launch atpkg {}: {error}",
            verb.join(" ")
        )),
    }
}

enum MachineReadEvent {
    Request,
    Finished(Result<atpkg::machine::MachineState, String>),
}

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
        let rerun = service.replace_machine_state(Err(error));
        debug_assert!(!rerun);
    }
}

fn launch_native_machine_read(
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) -> Result<(), String> {
    let atpkg = crate::co_located_atpkg()
        .ok_or_else(|| "the package tool is not installed beside aterm".to_string())?;
    spawn_native_machine_read(proxy, atpkg)
        .map_err(|error| format!("could not start the machine read: {error}"))
}

fn spawn_native_machine_read(
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
    atpkg: std::path::PathBuf,
) -> std::io::Result<()> {
    crate::qos::spawn_lane_thread(
        "aterm-machine-status",
        crate::qos::Role::Background,
        move || {
            let result = read_machine_state(&atpkg);
            let _ = proxy.send_event(Wake::NativeMachineStateFinished { result });
        },
    )
    .map(drop)
}

fn read_machine_state(atpkg: &std::path::Path) -> Result<atpkg::machine::MachineState, String> {
    let mut command = crate::qos::atpkg_command(atpkg);
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
            "atpkg machine printed no state".to_string()
        } else {
            format!("atpkg machine exited with {}", output.status)
        }
    })
}

fn machine_command_output(
    command: &mut aterm_uds::spawnfd::Command,
    operation: &str,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    machine_command_output_bounded(command, operation, limit, Overflow::Fail)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    Fail,
    Truncate,
}

#[cfg(test)]
thread_local! {
    static LAST_MACHINE_CHILD: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(unix)]
fn machine_command_output_bounded(
    command: &mut aterm_uds::spawnfd::Command,
    operation: &str,
    limit: std::time::Duration,
    overflow: Overflow,
) -> Result<std::process::Output, String> {
    use aterm_uds::spawnfd::Stdio;
    use std::io::ErrorKind;
    use std::os::fd::AsRawFd as _;
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
    #[cfg(test)]
    LAST_MACHINE_CHILD.set(Some(child.id()));
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
        let cleanup_deadline = deadline.min(Instant::now() + reserve);
        if !stop_machine_read_child(&mut child, cleanup_deadline) {
            error.push_str("; child exit could not be confirmed within the cleanup budget");
            drop(child);
        }
        error
    })
}

#[cfg(unix)]
fn stop_machine_read_child(
    child: &mut aterm_uds::spawnfd::Child,
    deadline: std::time::Instant,
) -> bool {
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

#[cfg(not(unix))]
fn machine_command_output_bounded(
    _command: &mut aterm_uds::spawnfd::Command,
    _operation: &str,
    _limit: std::time::Duration,
    _overflow: Overflow,
) -> Result<std::process::Output, String> {
    Err("machine settings commands require macOS".into())
}

/// The `machine-state:` record in one `atpkg machine` stdout, if any line carries a parseable one.
pub(crate) fn parse_machine_state_output(stdout: &str) -> Option<atpkg::machine::MachineState> {
    stdout.lines().find_map(|line| {
        crate::r6_marker_body(line, atpkg::cli::MACHINE_STATE_MARKER)
            .and_then(atpkg::machine::parse_machine_state)
    })
}

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
    pub(crate) events: Vec<Wake>,
    pub(crate) verdict: Option<String>,
    pub(crate) refusal: Option<String>,
    pub(crate) record: Option<atpkg::machine::MachineState>,
}

/// Sort an `atpkg machine apply` stdout into marker events, the verdict sentence and any refusal or failed write.
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
                Wake::PkgMachineState(body) => {
                    read.record = atpkg::machine::parse_machine_state(&body).or(read.record);
                }
                event => read.events.push(event),
            }
        }
    }
    read
}

/// The exact argv of the `atpkg` process a [`PackagesRequest`] runs, the one table the Settings verbs go through.
pub(crate) fn packages_argv(request: &PackagesRequest) -> Vec<String> {
    let parts: &[&str] = match request {
        PackagesRequest::CheckUpdate => &["update"],
        PackagesRequest::InstallDefaultSet => &["install", "--default-set"],
        PackagesRequest::UninstallAll => &["uninstall", "--all"],
        PackagesRequest::MachineApply => &["machine", "apply"],
    };
    parts.iter().map(ToString::to_string).collect()
}

/// The verb a person pressed and waits on.
pub(crate) const fn person_verb(
    request: &PackagesRequest,
) -> Option<crate::message_reporters::PackagesVerb> {
    use crate::message_reporters::PackagesVerb;
    match request {
        PackagesRequest::CheckUpdate => Some(PackagesVerb::Check),
        PackagesRequest::InstallDefaultSet => Some(PackagesVerb::Install),
        PackagesRequest::UninstallAll => Some(PackagesVerb::Remove),
        PackagesRequest::MachineApply => None,
    }
}

/// How a person's verb row ends.
pub(crate) const fn person_echo(failed: bool, ran: bool) -> aterm_messages::EchoKind {
    if failed {
        aterm_messages::EchoKind::Fault
    } else if ran {
        aterm_messages::EchoKind::Complete
    } else {
        aterm_messages::EchoKind::Vanish
    }
}

pub(crate) const fn packages_busy(request: &PackagesRequest) -> PackagesBusy {
    match request {
        PackagesRequest::CheckUpdate => PackagesBusy::Check,
        PackagesRequest::InstallDefaultSet => PackagesBusy::Install,
        PackagesRequest::UninstallAll => PackagesBusy::Uninstall,
        PackagesRequest::MachineApply => PackagesBusy::MachineApply,
    }
}

/// The structural admission a request must pass before the busy gate.
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

/// Why this process refuses `request` for what it is, or `None`.
pub(crate) fn packages_request_refusal(
    request: &PackagesRequest,
    private_state_root: bool,
) -> Option<String> {
    (private_state_root && matches!(request, PackagesRequest::MachineApply)).then(|| {
        "this instance runs under a private state root (ATERM_STATE_HOME), and the [machine] \
         settings are this Mac's \u{2014} `aterm pkg machine apply` in a terminal applies them"
            .to_string()
    })
}

#[cfg(test)]
#[path = "app_native_packages_argv_tests.rs"]
mod packages_argv_tests;

#[cfg(test)]
#[path = "editor_input_characterization.rs"]
mod editor_input_characterization;

#[cfg(test)]
#[path = "app_native_tests.rs"]
mod tests;
