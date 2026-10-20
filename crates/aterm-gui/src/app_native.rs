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
    UpdaterPhase, UpdaterWorkTicket,
};
#[cfg(any(unix, test))]
use crate::native_updater_service::{ReturnedApplyDisposition, ReturnedApplyFacts};
use crate::packages_screen::{PackagesBusy, PackagesCommandOutcome, PackagesWorkerCompletion};
use crate::{App, Wake, WindowId};

/// Settings ▸ Messages is republished at most this often while the center
/// moves: a downloading update restates at 10 Hz; the page repaints at 2 Hz.
pub(crate) const MESSAGES_PUBLISH_MIN_GAP: std::time::Duration =
    std::time::Duration::from_millis(500);
/// …and at least this often while a view is on the route with nothing
/// moving, so the relative times tick (`3 min ago` becomes `4 min ago`).
pub(crate) const MESSAGES_PUBLISH_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// …and this long after the publish that started a transient status line's
/// clock, the one that clears it. The page times the line by the projections'
/// wall-clock stamps, so the wait is a hair past
/// [`crate::native_settings::MESSAGES_FEEDBACK_MS`].
pub(crate) const MESSAGES_FEEDBACK_WAIT: std::time::Duration =
    std::time::Duration::from_millis(crate::native_settings::MESSAGES_FEEDBACK_MS + 50);

#[cfg(test)]
thread_local! {
    static NATIVE_UI_COMPILES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Real semantic compiler entries on this test thread; warm native redraws
/// must reuse their retained tree instead of merely skipping pixel raster work.
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
        // Removing the authored field restores Config's documented default.
        None => Some(false),
        Some(value) => value.trim().parse::<bool>().ok(),
    }
}

/// The `settings` verb's answer, from what the write did: `written` is the
/// value `key` holds once it lands (`None` where the key is absent — an
/// `unset`, or a value equal to its default, which the service writes as a
/// removal), and `now` what a removed key reads as (the words its Settings
/// row shows). An applied write says `saved: k = v` or `removed: k; now w`; a
/// write that changed nothing says so, never "saved" with an empty value.
/// Whether the save still needs reconciling comes last, after what it did.
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
                // The worker produced a durable proof, so the save succeeded, but
                // the service gate stays closed until a later stable observation.
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
    /// [`App::publish_native_update_state`] calls on this test's thread.
    pub(crate) static FULL_UPDATE_PUBLISHES: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
    /// [`App::publish_quiet_update_words`] calls on this test's thread.
    pub(crate) static QUIET_WORD_PUBLISHES: std::cell::Cell<u32> = const {
        std::cell::Cell::new(0)
    };
}

/// Evidence that the main-thread native document/restore preflight was Ready.
/// Only this module constructs it; process replacement carries the token to its
/// final admission gate.
#[cfg_attr(
    not(unix),
    expect(dead_code, reason = "only the unix lanes spend its counts")
)]
pub(crate) struct NativeUpdateSafetyToken {
    /// Unsaved editor drafts certified as carried (journaled and verified on
    /// disk). Only a lane whose successor restores document tabs may spend the
    /// token: the seamless handoff does; the cold exec refuses it
    /// (`App::start_unix_update_handoff`).
    carried_drafts: usize,
    /// Unsaved Settings field drafts certified as carried by the handoff layout
    /// (`App::capture_handoff_layout`). Spent only by the seamless lane, toward a
    /// successor at least this build; an older successor ignores the field.
    carried_settings_drafts: usize,
    /// Documents whose last checkpoint failed but ride because the successor
    /// reopens them: unsaved text rides as a carried draft (counted in
    /// `carried_drafts`). The cold lane refuses them with a `Retry:` reason.
    failed_checkpoints: usize,
}

impl NativeUpdateSafetyToken {
    #[must_use]
    pub(crate) fn is_certified(&self) -> bool {
        true
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) fn carried_drafts(&self) -> usize {
        self.carried_drafts
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) fn carried_settings_drafts(&self) -> usize {
        self.carried_settings_drafts
    }

    #[cfg(unix)]
    #[must_use]
    pub(crate) fn failed_checkpoints(&self) -> usize {
        self.failed_checkpoints
    }

    /// A token certifying exactly these counts, as the preflight would have
    /// minted it — for the lane gate's own tests.
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

/// The most unsaved Settings text one update carries, summed over every carried
/// field draft. Keeps the handoff layout, which the successor reads whole before
/// adopting a shell, small; past it the update is held and says why.
pub(crate) const CARRIED_SETTINGS_DRAFTS_CAP_BYTES: usize = 1024 * 1024;

/// The Settings half of the update preflight, over each view's
/// [`crate::native_settings::CarriedFieldDrafts`]: views and drafts the handoff
/// layout carries, and views holding a draft no update carries, with why. Pure,
/// so the lane rule and the cap are testable without a window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SettingsDraftTally {
    pub(crate) carried_views: usize,
    pub(crate) carried_drafts: usize,
    pub(crate) carried_bytes: usize,
    pub(crate) held_views: usize,
    /// Why the first held view is held; `None` when the successor reopens no
    /// Settings tab at all — the pre-carry reason, word for word.
    pub(crate) held_why: Option<String>,
}

impl SettingsDraftTally {
    /// `successor_reopens`: whether this process's update successor reopens
    /// native tabs from a handed-over layout
    /// ([`App::successor_restores_document_tabs`]); without it nothing rides.
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
            // Over the cap nothing rides, as for editor drafts: every drafted
            // view holds the update.
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

    /// The preflight's reason for the held views, in its person-facing
    /// [`SETTINGS_DRAFTS_BLOCK`] shape; `None` when every draft rides.
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

/// The most unsaved editor text one update carries, summed over every carried
/// draft. The successor replays each draft before the outgoing process may
/// Commit, and the Commit re-reads every one; past this the update is held.
pub(crate) const CARRIED_DRAFTS_CAP_BYTES: usize = 64 * 1024 * 1024;

/// The dirty-document half of the update preflight, over each draft's
/// [`crate::app_documents::DraftCarry`] verdict: how many are carried and their
/// size, how many are still being journaled (the lane waits), and why each one
/// that cannot ride cannot. Pure, so it is testable without 64 MiB of text.
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
            // Over the cap nothing rides: the update is held for every draft, as
            // for an uncarried dirty document.
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

    /// The preflight's reasons: a refusal opens with [`DIRTY_DOCUMENTS_BLOCK`]
    /// (Save or close), a draft still being written with `Wait:`.
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

/// Disk facts collected off the event loop after an asynchronous handoff stops.
/// Reduction stays on the main thread; ledger I/O and the installed-bundle
/// probe stay off the completion callback.
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

    /// Test-only ticket for hand-built worker facts, so a conformance test can
    /// feed the real reducer the disk a failed candidate leaves behind.
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
    /// Re-read the ledger with no announcement semantics: the background
    /// check's health hook fired (a persistent failure streak), or Settings
    /// opened (on any page: Software Update, Modified and search all say what
    /// the update switches do in this copy). The screen shows the current
    /// verdict, not the last one a stage or startup imported.
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
    /// When the worker began reading the disk. The sequence orders observations
    /// against each other; this orders them against the reducer's stage imports:
    /// a read that began before a check staged its build must not retire what
    /// the check imported, however late it lands.
    pub(crate) observed_at: std::time::Instant,
    pub(crate) durable: Option<DurableUpdateStatus>,
    pub(crate) installed: Option<InstalledUpdate>,
    /// `installed` is `None` for want of PROOF, not because the bundle left: an
    /// installed bundle was there and its verification did not FINISH this pass
    /// (`spctl` past its ceiling on a captive-portal network, a `codesign` killed
    /// or out of its budget — a passing refusal). Such a read observes nothing
    /// about the installed facts, so it is no evidence the boot trial's count is
    /// gone (round eight, #80).
    pub(crate) installed_unproven: bool,
    /// Whether this copy can replace itself, as this read observed it: the durable
    /// status's own answer, or — where that read found no ledger — the bundle probe
    /// alone (`aterm_update::running_copy_installable`). `None` observes nothing.
    pub(crate) installable: Option<bool>,
}

/// One retire of a latched installed activation for a newer verified release
/// (`App::request_activation_supersede`): the worker's job, and the App's
/// record that it is in flight.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivationSupersedeJob {
    pub(crate) current_build: u64,
    /// The latched activation's build and sealed commit — the exact bytes the
    /// retire may take off the install path, and nothing else.
    pub(crate) activation: u64,
    pub(crate) activation_commit: String,
    /// The verified download that takes its place.
    pub(crate) newer: u64,
    /// The verdict's `newer_spent` before this retire spent `newer`, restored on
    /// refusal so the release is offered again at its retry deadline.
    pub(crate) spent_before: u64,
}

/// What the retire worker posts back: the job, what
/// `aterm_update::supersede_installed_activation` did, and the disk read after
/// the retire (`None` when no read could be had; the completion then asks for
/// one).
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
        /// What this process can prove about the trial (round eight, #8).
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

/// One buffered item plus the worker's active item suffice; further UI intents
/// are held by the purpose coalescer, so stale disk work never queues ahead of
/// user-visible updates.
pub(crate) const NATIVE_UPDATE_WORKER_CAPACITY: usize = 1;

/// Result of reducing worker-collected disk facts on the event loop. Active
/// updater work defers the facts without rereading disk; otherwise presentation
/// uses only the returned effective reducer stage, never the trigger payload.
///
/// `Deferred` boxes its facts: it is the rare arm, and the payload is several
/// hundred bytes the common arms would otherwise move too.
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
                    // A YANKED bundle newer than this process is not an activation
                    // candidate: reporting it as installed would retire a good download
                    // for an activation every handoff then refuses at the floor
                    // (2026-08-19 round-3 audit). A yanked bundle at or below the running
                    // build is still the bundle we run from and stays reported.
                    .filter(|installed| {
                        !(installed.yanked && installed.build_number > current_build)
                    })
                    .map(|installed| InstalledUpdate {
                        // The boot sentinel's count, measured HERE — on this worker,
                        // beside the codesign and plist reads — so the event loop
                        // never touches the staging root to ask it (gap 14).
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
    // A ledger that could not be read (no resolvable HOME) still has a copy to
    // ask: the bundle probe alone answers whether it can replace itself.
    if facts.installable.is_none() {
        facts.installable = aterm_update::running_copy_installable();
    }
    facts
}

/// Read the facts: the durable status, then the installed bundle — `Ok(None)` when
/// there is none (or it is not reported), `Err` with the refusal when one is there
/// and could not be proven ([`aterm_update::installed_update_facts_checked`]).
fn read_native_update_reconcile_facts_with(
    ticket: NativeUpdateReconcileTicket,
    observation_sequence: u64,
    read_durable: impl FnOnce() -> Option<DurableUpdateStatus>,
    read_installed: impl FnOnce() -> Result<Option<InstalledUpdate>, String>,
) -> NativeUpdateReconcileFacts {
    // Load the ready marker before canonical bundle identity. Shipping swaps the
    // bundle first and removes ready afterward, so this sees ready+old,
    // ready+new or missing+new; exact installed proof dominates either
    // surviving-marker case.
    let observed_at = std::time::Instant::now();
    let durable = read_durable();
    let installable = durable.as_ref().and_then(|durable| durable.installable);
    // A refusal that is a VERDICT (an unsigned or tampered bundle) is a fact: the
    // installed facts name no verified bundle. Only a check that did not finish is
    // the absence of an observation.
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

/// Start the one process-wide, bounded FIFO facts worker. Every ledger/provenance
/// observation is serialized here, and freshness is stamped at read time.
pub(crate) fn spawn_native_update_reconcile_worker(
    drain_proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) -> Result<NativeUpdateReconcileSender, String> {
    let (sender, receiver) =
        std::sync::mpsc::sync_channel::<NativeUpdateWorkerRequest>(NATIVE_UPDATE_WORKER_CAPACITY);
    crate::qos::spawn_lane_thread(
        "aterm-update-facts".to_string(),
        crate::qos::Role::Background,
        move || {
            // Reconcile facts feed a settings screen, not a frame; results return
            // through the proxy and no lock is held.
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
        // A Reply or boot-health item need not emit any other UI event. This edge
        // guarantees a Full-queue pending latch gets another nonblocking chance.
        drained();
    }
}

/// Queue one handoff-worker observation on the same FIFO and wait off the event
/// loop for its ordered result.
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

/// A refusal about the installed environment, distinct from an exhausted
/// artifact budget. Only a later verified observation OF WHAT REFUSED can
/// release this latch ([`EnvironmentBlockCause`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AutoApplyEnvironmentBlock {
    build: u64,
    dmg_sha256: [u8; 32],
    blocked_at: std::time::Instant,
    cause: EnvironmentBlockCause,
}

/// WHICH PART OF THE INSTALL REFUSED, and so which observation can say a person
/// has changed it (round eight of the update audit, finding 14).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnvironmentBlockCause {
    /// The INSTALLED BUNDLE cannot be the swap's rollback source, or this
    /// process does not run from one (`aterm_update::refusal_needs_person`'s
    /// other terms): the reconcile's verified observation of that copy against
    /// the running image is the evidence that it was put right
    /// ([`App::release_repaired_auto_apply_environment`]).
    InstalledSource,
    /// The install location's FOLDER cannot be written by this user
    /// (`aterm_update::is_install_location_refusal`): an admin-owned or
    /// MDM-managed `/Applications`. The bundle in it verifies — that is not the
    /// problem — so the reconcile's observation of the installed copy says
    /// nothing about the folder and never releases it. What does is the one
    /// check that asks the folder: a pre-park verification of exactly the
    /// blocked artifact that PASSED after the block was taken — the staged
    /// lane's check asks the folder for the swap's write
    /// (`install_location_check`) before it can pass. The person's own retry,
    /// the press the remedy names, is such a check (the worker's
    /// `PreverifyPublisher::publish_pass`), and the next reconcile releases the
    /// block on it ([`App::install_location_answered_since`]) whether or not
    /// that retry went on to land. A newer stage arms afresh, and a changed
    /// installed copy releases it too (`carry_manual_only_latch_across_swap`).
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

    /// What a person does about this cause — the remedy every surface names
    /// while the block stands ([`App::apply_retry_for`]), whatever reason the
    /// ledger booked since (round eight, finding 14's review).
    pub(crate) fn remedy(self) -> crate::update_apply_trouble::PersonRemedy {
        use crate::update_apply_trouble::PersonRemedy;
        match self {
            Self::InstallLocation => PersonRemedy::MoveOrAskAdmin,
            Self::InstalledSource => PersonRemedy::InstallSignedRelease,
        }
    }
}

/// Distinct rungs of the activity-revoked retry ladder (2 s → 30 s,
/// [`automatic_retry_delay`]); past the last one each revocation waits the last
/// rung again. Activity never stops the lane: with a shell streaming output,
/// revocation is the normal outcome of an attempt. Each attempt is a lossless
/// launch/park/paint round trip, so the spacing bounds the cost (at most two a
/// minute) while staying inside `LANDS_WITHIN`.
const ACTIVITY_REVOKED_LADDER_RUNGS: u8 = 5;

/// Physical handoff retries within one epoch.
///
/// `TimedOut` is classified physical, and the handoff deadline (15 s) covers a
/// cold boot, a blocking `flock`, re-verification, the bundle swap, a second
/// exec, a boot of the new image, PTY adoption and a repaint, so it is often
/// environmental and transient. Two retries, tens of minutes apart, give a slow
/// moment another chance. Convergence is bounded by
/// [`MAX_PHYSICAL_FAILURE_EPOCHS`]; structural failures use
/// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] and never reach an epoch boundary.
const MAX_PHYSICAL_FAILURE_CYCLES: u8 = 2;

/// Physical failures counted in ONE epoch: the initial failure plus its
/// [`MAX_PHYSICAL_FAILURE_CYCLES`] retries.
pub(crate) const PHYSICAL_FAILURES_PER_EPOCH: u8 = MAX_PHYSICAL_FAILURE_CYCLES + 1;

/// Whole epochs an artifact gets before automatic apply gives up on those exact
/// bytes, so the transient lane converges instead of replenishing forever.
///
/// The full schedule is 9 failures over roughly 14 hours (40 min of epoch + 6 h
/// stand-down, three times), so the last epoch samples the machine after a
/// night's idle time; an artifact that cannot hand off in any of them is
/// evidence about the bytes, and the lane latches manual-only and says so once.
const MAX_PHYSICAL_FAILURE_EPOCHS: u8 = 3;

/// Total physical failures an artifact may cost before automatic apply
/// converges. After the ninth, [`App::spend_physical_failure_budget`] answers
/// [`PhysicalFailureSchedule::Converged`] with a quiet re-sample one
/// [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] out; only a structural convergence can
/// end in a latch with no deadline.
pub(crate) const PHYSICAL_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * MAX_PHYSICAL_FAILURE_EPOCHS;

/// Total physical failures a structurally shaped artifact may cost before
/// automatic apply converges on those exact bytes.
///
/// An `AdoptionMismatch` is a property of the two images and a failed
/// pre-park verification recurs, so the transient schedule's generosity does
/// not apply. Two, not one: the class is not perfectly separable at this seam
/// (an `AdoptionMismatch` can be a cross-version race), and one retry ten
/// minutes out separates those for one round trip.
pub(crate) const STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS: u8 = 2;

/// Whole epochs an artifact gets when its handoff failure is unexplained: the
/// candidate closed the readiness channel and the parent observed nothing
/// about why (see [`crate::ChildDeathEvidence::Unobserved`]).
///
/// This is where a `ChildDied` lands whenever evidence runs out, which on macOS
/// is common: a refusing candidate keeps running while it unwinds, so the
/// status seen afterwards is this process's own SIGKILL. An epoch re-samples
/// the machine (a starved child under heavy load looks the same), and one
/// [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] is the minimum that separates a bad hour
/// from a successor that will not boot; a third epoch would add nothing.
const UNEXPLAINED_FAILURE_EPOCHS: u8 = 2;

/// Total physical failures an artifact may cost while every one is unexplained:
/// six, across ~7.7 hours (40 min of epoch, one 6 h stand-down, 40 min more).
///
/// Strictly between the transient and structural budgets; the asserts below
/// keep it there. Unexplained failures forgive their counted trial launch
/// (only [`PhysicalFailureShape::Structural`] keeps it), so six attempts never
/// reach `MAX_BOOT_ATTEMPTS` and revert the bundle.
pub(crate) const UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS: u8 =
    PHYSICAL_FAILURES_PER_EPOCH * UNEXPLAINED_FAILURE_EPOCHS;

/// A `ChildDied` classified structural keeps its counted boot-trial launch
/// (`app_update_handoff`'s reject path forgives every other shape), so its
/// budget is spent against the counter the boot sentinel reverts on. Converging
/// strictly before `MAX_BOOT_ATTEMPTS` keeps a retry schedule from walking a
/// build to the revert threshold.
///
/// A converged verdict's one re-sample sits outside this budget; it is gated on
/// the sentinel's measured count instead (`InstalledUpdate::trial_launches`
/// against [`BOOT_TRIAL_LAUNCH_LIMIT`]): an attempt whose launch would trigger
/// the revert is held and said. A newer release retires the activation without
/// launching it (`App::request_activation_supersede`).
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

/// Idle gap after which an artifact's physical-failure counter starts over.
///
/// Must outlast a whole epoch so epochs can be counted: one stand-down
/// ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`], 6 h) plus the in-epoch spacing
/// (600 s + 1800 s), with room for a late failure queued behind other updater
/// work. A shorter window would reset the counter on the schedule's own cadence
/// and make the lane unbounded. Half a day with no physical failure for these
/// bytes (after convergence, only a manual Version-menu retry) starts over.
const PHYSICAL_RETRY_BUDGET_REPLENISH: std::time::Duration =
    std::time::Duration::from_secs(12 * 60 * 60);

/// How long automatic apply stands down after an artifact spends one epoch's
/// physical retry budget.
///
/// A spent epoch is not proof the artifact is broken (`TimedOut` is usually
/// the machine's moment), so it is a long wait and nothing more; only
/// [`MAX_PHYSICAL_FAILURE_EPOCHS`] ends the lane. Six hours makes the epochs
/// independent samples while three still fit in a day. Worst case for an
/// artifact that can never hand off: 9 round trips over ~14 h, then silence.
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
///
/// It used to wait forever: `retry_at: None`, which on the installed activation
/// also blocked every newer release (a newer release now supersedes the
/// activation instead — round three), so a single disagreement between two
/// builds stopped automatic updates until a person acted. Why a re-sample at
/// all: the class is not perfectly separable at this seam (see
/// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] — an `AdoptionMismatch` can still
/// be a cross-version race), so a verdict confirmed twice is strong evidence,
/// not proof. Why a DAY: it is about the bytes, so it waits longer than the
/// transient lane's stand-down ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`]), which
/// re-samples a busy afternoon; a day later the machine has slept, its caches
/// are cold again and whatever held a lock or filled a disk has moved on. One
/// re-sample, not a schedule: the verdict records it, and a re-sample that
/// fails again converges at once for good. A day of CALENDAR time, the Mac's
/// sleep counted ([`App::count_sleep_toward_update_deadlines`]).
pub(crate) const STRUCTURAL_RESAMPLE_AFTER: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

/// The least the wall clock must run ahead of the monotonic one between two
/// turns of the event loop to count as a SLEEP
/// ([`App::count_sleep_toward_update_deadlines`]). A Mac sleeps for seconds at
/// the least; two clocks read one after the other drift apart by nanoseconds a
/// turn, which must never add up to a retry.
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

/// The boot sentinel's revert threshold, as the automatic lane reads it: a
/// launch of a trialed build that brings its count to this is the one
/// `check_boot_health` reverts on. Off macOS there is no sentinel
/// (`aterm_update::trial_launch_count` is always 0), so nothing can reach it.
#[cfg(target_os = "macos")]
pub(crate) const BOOT_TRIAL_LAUNCH_LIMIT: u32 = aterm_update::MAX_BOOT_ATTEMPTS;
#[cfg(not(target_os = "macos"))]
pub(crate) const BOOT_TRIAL_LAUNCH_LIMIT: u32 = u32::MAX;

/// How soon a structural latch whose earned attempt found the boot trial
/// unmeasured looks again. A latch past its deadline folds a wake the event
/// loop takes at once, so an unanswerable look must move the deadline or it
/// spins; this is about as often as a facts observation arrives.
const STRUCTURAL_TRIAL_REMEASURE: std::time::Duration = std::time::Duration::from_secs(75);

/// How many due looks in a row may find the structural latch's boot trial
/// unmeasured before the lane stops waiting for a count that is not coming
/// (round six, finding 55; the model's `UnreadLooks`). Each look asks for an
/// observation, and a count forgotten by a returned attempt is re-read by the
/// very next one, so three looks in a row, each over an observation read
/// since the one before, that found no count (about four minutes at
/// [`STRUCTURAL_TRIAL_REMEASURE`] on an idle facts worker; longer while it is
/// busy, since a look with no new observation counts nothing, and neither does
/// one over an observation that could not prove the installed bundle — round
/// eight, #80) mean the
/// installed facts no longer name the latched activation — a channel floor
/// raised above it drops it from them — and the wait had no bound and no
/// notice (L5). The answer is the one a count with no room gets: hold, lose
/// the deadline, and say so; a newer release still supersedes.
pub(crate) const STRUCTURAL_TRIAL_UNMEASURED_LOOKS: u8 = 3;

/// How many due looks in a row may find the structural latch's installed bundle
/// UNPROVEN — every facts read since the look before failed to check it at all, a
/// passing refusal such as `spctl` past its ceiling on a captive portal — before the
/// day's re-sample moves to the next day (round eight, #80's review; the model's
/// `UnprovenLooks`). Such a read is no evidence the count is gone, so it spends none
/// of [`STRUCTURAL_TRIAL_UNMEASURED_LOOKS`] and drops nothing; but on a network
/// filter that always times `spctl` out it would otherwise re-arm the latch every
/// [`STRUCTURAL_TRIAL_REMEASURE`] — a deep `codesign` and an `spctl` on the facts
/// worker each time — for as long as the filter stands, and say nothing (L5).
pub(crate) const STRUCTURAL_TRIAL_UNPROVEN_LOOKS: u8 = 3;

/// How long a release whose retire of a latched activation was REFUSED waits
/// before the lane offers it again, the first time; each refusal in a row
/// doubles it, up to [`SUPERSEDE_RETRY_CEILING`] (round three review).
///
/// A retire often refuses on a moment (bundle verifications share an 8 s
/// budget, Gatekeeper rides the network, the apply lock waits 120 s at most),
/// so it is retried; not at once, because a disk-shaped refusal repeats and
/// each try holds the apply lock across three codesign reads.
const SUPERSEDE_RETRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// The longest a refused release waits between tries: a disk the retire can
/// never undo costs one locked verification every two hours.
const SUPERSEDE_RETRY_CEILING: std::time::Duration = std::time::Duration::from_secs(2 * 60 * 60);

/// The wait before a refused release is offered again after `refusals`
/// consecutive refusals (1 = the first): [`SUPERSEDE_RETRY_AFTER`], doubled
/// per refusal, capped at [`SUPERSEDE_RETRY_CEILING`].
pub(crate) fn supersede_retry_after(refusals: u32) -> std::time::Duration {
    let doublings = refusals.saturating_sub(1).min(8);
    SUPERSEDE_RETRY_AFTER
        .saturating_mul(1_u32 << doublings)
        .min(SUPERSEDE_RETRY_CEILING)
}

/// What [`App::spend_physical_failure_budget`] says about the attempt that just
/// came back: a mid-epoch retry is invisible, a stand-down is a long quiet
/// wait, and convergence is the only state where the Version menu beats waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhysicalFailureSchedule {
    /// Another automatic attempt, inside the current epoch.
    Retry(std::time::Instant),
    /// This epoch is spent; the next one begins after a long stand-down.
    StandDown(std::time::Instant),
    /// This shape's [`PhysicalFailureShape::lifetime_attempts`] are spent (for
    /// the transient shape, [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] across
    /// [`MAX_PHYSICAL_FAILURE_EPOCHS`] epochs). The schedule says so once
    /// ([`App::announce_automatic_apply_stranded`]).
    ///
    /// `resample_at` is what happens next. Transient and unexplained lifetimes
    /// end in a quiet re-sample one [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`] out,
    /// every time; those shapes forgive their trial launch, so re-sampling
    /// cannot walk the boot sentinel toward a revert.
    ///
    /// A structural lifetime gets one re-sample, [`STRUCTURAL_RESAMPLE_AFTER`]
    /// out, recorded on its [`crate::AutoApplyStructuralVerdict`]. A verified
    /// download strictly newer than the latched activation supersedes it
    /// (`native_update_auto_intent::structural_latch`): the running build takes
    /// the install path back and applies the newer build as an ordinary stage.
    /// `None` remains once the re-sample is spent and no newer release has
    /// come; the convergence notice has said so and the Version menu moves it.
    Converged {
        resample_at: Option<std::time::Instant>,
    },
}

impl PhysicalFailureSchedule {
    /// Convergence for `shape` at `now`: a silent re-sample one epoch cooldown
    /// out for the machine-shaped failures, and one re-sample a day out for the
    /// structural one (the caller records it on the verdict).
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

    /// The deadline to stamp on [`crate::AutoApplyManualOnly`]. `None` is the
    /// latch `lapse_expired_auto_apply_manual_only` never releases, minted only
    /// once a structural verdict's one re-sample is spent.
    #[must_use]
    fn retry_at(self) -> Option<std::time::Instant> {
        match self {
            Self::Retry(at) | Self::StandDown(at) => Some(at),
            Self::Converged { resample_at } => resample_at,
        }
    }
}

/// How long the automatic lane waits between attempts once an artifact's
/// preflight-block budget is spent. Probes are screen-silent, so this is only
/// retry spacing: saving or closing the blocking Settings edit lands the update
/// within a minute.
const PREFLIGHT_BLOCK_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);

/// How the close preflight's per-person blockers open
/// (`native_update_close_preflight`); [`App::update_blocker_for_person`]
/// recognises these as work a person has to save.
pub(crate) const SETTINGS_DRAFTS_BLOCK: &str = "Review Settings Drafts:";
pub(crate) const DIRTY_DOCUMENTS_BLOCK: &str = "Checkpoint Drafts:";
pub(crate) const FAILED_CHECKPOINTS_BLOCK: &str = "Retry:";

const _: () = assert!(PREFLIGHT_BLOCK_COOLDOWN.as_secs() <= 60);

/// How often an intent held by a capture refusal re-reads the desk to see
/// whether the refusing state moved. A read is a few `try_lock`s and a hash,
/// and nothing launches unless the fingerprint changed; thirty seconds lands the
/// update soon after the offending tab closes without relaunching on every
/// resize drag.
pub(crate) const CAPTURE_REFUSAL_PROBE: std::time::Duration = std::time::Duration::from_secs(30);

/// How soon the lane looks again for a busy session's baseline, during a
/// refusal's first probe. A refusal comes back while the readers drain what
/// queued during the freeze, or beside a tab streaming output, so a reader can
/// hold a session's engine when the desk is read, and a change the person makes
/// to that session before its first reading is part of its baseline
/// ([`crate::CaptureDesk::fill_from`]; which sessions are open is always read).
/// After the first probe it is looked for at the probe, so a reader that never
/// lets go does not wake the lane every few seconds for an hour.
pub(crate) const CAPTURE_REFUSAL_BASELINE_PROBE: std::time::Duration =
    std::time::Duration::from_secs(2);

const _: () = assert!(CAPTURE_REFUSAL_BASELINE_PROBE.as_secs() < CAPTURE_REFUSAL_PROBE.as_secs());

/// Backstop for an intent held by a capture refusal: after an hour with the
/// desk unchanged the lane tries once more, in case the refusal was about
/// something the fingerprint does not see. Each attempt records an apply
/// failure, so `failing_applies` keeps climbing (and the persistent-failure
/// notice stays reachable) for a desk that never moves.
pub(crate) const CAPTURE_REFUSAL_BACKSTOP: std::time::Duration =
    std::time::Duration::from_secs(60 * 60);

/// How long a capture refusal holds the next attempt even when the desk
/// changed: `refusals` is how many in a row refused this update, this one
/// included. The first is released at the next probe, so the update lands soon
/// after the offending tab closes; a second in a row waits 600 s, a third and
/// later 1800 s, the spacing a producer failure had (round eight of the update
/// audit, U43). The fingerprint only guesses at what a refusal was about, and a
/// busy desk moves it (a `cd`, a program on the alternate screen, a pane
/// opened) at nearly every probe whatever refused: a manifest over its cap was
/// re-attempted every 30 s, each attempt a launch, a freeze of every tab and a
/// history export. Never past [`CAPTURE_REFUSAL_BACKSTOP`], and never a latch.
#[must_use]
pub(crate) const fn capture_refusal_spacing(refusals: u32) -> std::time::Duration {
    std::time::Duration::from_secs(match refusals {
        0 | 1 => CAPTURE_REFUSAL_PROBE.as_secs(),
        2 => 600,
        _ => 1800,
    })
}

// The spacing only ever grows, from one probe, and stays under the backstop.
const _: () = assert!(
    capture_refusal_spacing(1).as_secs() == CAPTURE_REFUSAL_PROBE.as_secs()
        && capture_refusal_spacing(2).as_secs() > capture_refusal_spacing(1).as_secs()
        && capture_refusal_spacing(u32::MAX).as_secs() > capture_refusal_spacing(2).as_secs()
        && capture_refusal_spacing(u32::MAX).as_secs() < CAPTURE_REFUSAL_BACKSTOP.as_secs()
);

/// The updater's "a newer build has waited too long" notice must fire well past
/// the ladder's bound plus switch allowance (at least 4x) and past the
/// physical lane's first retry (600 s), yet within the hour.
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

/// What a returned physical failure is evidence about: the machine's moment or
/// the two images. Carried from the worker's typed
/// [`crate::UpdateHandoffOutcome`], never its message string. Decides how many
/// round trips an artifact may cost; see
/// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PhysicalFailureShape {
    /// The attempt lost a race the next one may win. Rides the full
    /// [`PHYSICAL_FAILURE_LIFETIME_ATTEMPTS`] schedule.
    Transient,
    /// The candidate could not become this process's successor, for a reason that
    /// belongs to the bytes rather than to the afternoon. Converges after
    /// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`].
    Structural,
    /// Something ended the candidate and the parent did not see what: a failure
    /// of unknown kind, not filed as either kind the parent can prove. Rides the
    /// epoch machinery on the shorter
    /// [`UNEXPLAINED_FAILURE_LIFETIME_ATTEMPTS`] budget.
    #[cfg(unix)]
    Unexplained,
}

impl PhysicalFailureShape {
    /// Classify one worker outcome, with whatever the worker observed about a
    /// candidate that died.
    ///
    /// Structural:
    ///   * `AdoptionMismatch`: the child proved a PTY set the parent does not
    ///     recognise, a cross-version disagreement about what the proof covers;
    ///   * `PreparationFailed`: the staged candidate failed pre-park verification
    ///     with a verdict. A timed-out `codesign`, a full disk, `EMFILE` and
    ///     similar are `ProducerFailed`, which is transient.
    ///
    /// Transient is the rest: `TimedOut`, a non-activity `Rejected` (a
    /// commit-time re-check of state that moves on its own), and, failing
    /// closed, `ProofReady` and `CaptureRefused` (which
    /// [`HandoffFailureLane::classify`] routes to
    /// [`HandoffFailureLane::Refused`] before asking for a shape).
    ///
    /// `ChildDied` decides nothing on its own: a starved child also exits before
    /// writing its readiness proof, so [`Self::of_child_death`] reads the
    /// evidence, and the absence of evidence is its own state.
    #[must_use]
    #[cfg(unix)]
    fn of_outcome(outcome: crate::UpdateHandoffOutcome, death: crate::ChildDeathEvidence) -> Self {
        match outcome {
            crate::UpdateHandoffOutcome::AdoptionMismatch
            | crate::UpdateHandoffOutcome::PreparationFailed => Self::Structural,
            crate::UpdateHandoffOutcome::ChildDied => Self::of_child_death(death),
            // `StageUnconfirmed` never reaches a shape ([`HandoffFailureLane::classify`]
            // files it first); listed here, it fails closed onto the converging one.
            crate::UpdateHandoffOutcome::TimedOut
            | crate::UpdateHandoffOutcome::ProducerFailed
            | crate::UpdateHandoffOutcome::Rejected
            | crate::UpdateHandoffOutcome::ActivityRevoked
            | crate::UpdateHandoffOutcome::CaptureRefused
            | crate::UpdateHandoffOutcome::StageUnconfirmed
            | crate::UpdateHandoffOutcome::ProofReady => Self::Transient,
        }
    }

    /// Did the successor image run and decide, or did something end a
    /// candidate that never got that far?
    ///
    ///   * `Exited` — the candidate reached an `exit` instruction, which a starved
    ///     process never does. STRUCTURAL, with ONE code read: this tree's
    ///     commonest refusal is a clean `0` (`main_entry` returns without a window
    ///     when the overlap authority is incomplete), so treating a non-zero code
    ///     as the structural one would classify exactly the wrong half — but
    ///     [`crate::seamless::EXIT_SUCCESSOR_PASSING`] (75, `EX_TEMPFAIL`) is the
    ///     candidate saying, in a status only it can produce, that it refused
    ///     because its boot swap was deferred for a MOMENT (a held apply lock, a
    ///     verification past its budget, another install's trial holding the
    ///     install) or had nothing it may swap now (a sibling swapped the same
    ///     build in while it waited, the running build's own trial still armed —
    ///     round eight, #75). That is TRANSIENT (round four, plan item 2): the
    ///     new image never ran, the same bytes are retried on the schedule that
    ///     converges, and the trial launch is forgiven. Every 75 is sent only to
    ///     a parent that declared it reads it (`seamless::ENV_EXIT_CAPS`,
    ///     published with the parent's identity on both lanes) — this build's own
    ///     image refusing for one of those, a NEWER image whose intake refused
    ///     this build's handoff artifacts, or a fixed-socket successor whose
    ///     carried control-socket witness this parent's listener rebind had moved
    ///     (round eight, #69, [`crate::seamless::endpoint_refusal_exit`]) — so
    ///     this arm and that declaration ship
    ///     together, and a parent without the arm (0.100.0 and older) is never
    ///     sent 75 (round eight of the update audit, finding 0). An older
    ///     successor never produces it, and every other code keeps the
    ///     structural verdict.
    ///   * `Signalled` — split by WHO raised it. A fault is the image executing
    ///     itself into a wall, which is the bytes; every other signal arrives from
    ///     outside the image (macOS jetsam SIGKILLs under memory pressure, which is
    ///     precisely the pathological-load state this classification exists for),
    ///     which is the machine.
    ///   * `Unobserved` — no statement either way. UNEXPLAINED: retried, bounded,
    ///     converging.
    ///
    /// There is deliberately no arm for "the image started": the boot
    /// sentinel's launch counter moves at the first statement of `main`, which
    /// is true of a starved candidate and a refusing one alike.
    ///
    /// Also the forgiveness rule: `app_update_handoff`'s reject path keeps a
    /// counted boot-trial launch only for `Structural` deaths, and the assert
    /// beside [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] keeps that budget under
    /// `MAX_BOOT_ATTEMPTS`.
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

    /// The shape of a failure booked from the cached pre-park verdict (see
    /// `try_pending_native_auto_apply`).
    ///
    /// A refusal the verifier reached as a verdict on the bytes is structural,
    /// as through the worker (`PreparationFailed`). One it did not reach (a
    /// helper out of budget, the apply lock held past its wait, a helper that
    /// could not start; [`aterm_update::is_passing_refusal`]) is transient, as
    /// is no cached refusal at all.
    ///
    /// Bound to `NativeUpdateApplyLadder`'s `PassingFailure` by the conformance
    /// test `real_passing_refusal_keeps_a_deadline_as_the_model_s_passing_failure`,
    /// which drives the real booking with no `#[refines]` anchor (one anchor would
    /// oblige every other action of the machine to carry an anchor or waiver).
    #[must_use]
    pub(crate) fn cached_refusal_shape(reason: Option<&str>) -> Self {
        match reason {
            Some(reason) if !aterm_update::is_passing_refusal(reason) => Self::Structural,
            _ => Self::Transient,
        }
    }

    /// How many failures for one artifact this shape may cost before automatic
    /// apply is done with those bytes. The counter is shared and shape-blind
    /// (see [`App::spend_physical_failure_budget`]); only the ceiling varies.
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

/// Whether `signal` is one an image raises on itself by executing (a fault)
/// rather than one delivered from outside.
///
/// Matched by name, not number: `SIGBUS` is 10 on macOS and 7 on Linux, and 7
/// on macOS is `SIGEMT`. The set is the synchronous exceptions both platforms
/// name; `SIGEMT` is absent because libc defines it only for mips/sparc Linux.
/// Everything else, `SIGKILL` above all (jetsam's signal), is someone else's
/// decision about a process that may never have run.
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

/// Which lane's budget a returned handoff failure may spend, from the
/// [`crate::native_updater_service::ApplyMode`] the attempt was authorized under.
/// A person's failure charges nothing: no budget, no latch, no retired intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(unix, test))]
pub(crate) enum HandoffFailureLane {
    /// Automatic, and the lossless rollback was caused by user/terminal activity:
    /// spends [`AutomaticRetryKind::ActivityRevoked`] budget.
    ActivityRevoked,
    /// Automatic, and the failure is evidence about the artifact or the
    /// machine: spends the converging physical budget on the schedule its
    /// [`PhysicalFailureShape`] earns.
    Physical(PhysicalFailureShape),
    /// A person asked for this apply (the Version menu, the palette, a control
    /// request, or an install-on-clean-quit gesture). Charges nothing.
    Manual,
    /// Automatic, and the park's capture refused the desk
    /// ([`crate::UpdateHandoffOutcome::CaptureRefused`]). Spends no activity
    /// spacing and no physical budget, is never re-parked and never latches. It
    /// is recorded as an apply failure naming the session (so `failing_applies`
    /// moves and `update status` says `apply_failure=`) and retried once the
    /// desk changes, or after [`CAPTURE_REFUSAL_BACKSTOP`].
    Refused,
    /// The stage waits for an update check to confirm its signer against a current
    /// machine roster ([`crate::UpdateHandoffOutcome::StageUnconfirmed`], round
    /// eight #503's review), whoever asked (`automatic` says who). No failure:
    /// nothing is charged, latched or booked as one; the automatic lane holds for
    /// the check ([`crate::AutoApplyRosterWait`]) and the outcome is a deferral,
    /// which the ledger keeps as the standing refusal that names the wait.
    /// Unix: only [`Self::classify`] (the unix handoff's) files one.
    #[cfg(unix)]
    AwaitsRosterConfirmation { automatic: bool },
}

#[cfg(any(unix, test))]
impl HandoffFailureLane {
    /// Classify one returned completion from the mode the apply was authorized
    /// under and the worker's `outcome`.
    ///
    /// `revoked_by_activity` is the main thread's half of the activity
    /// observation; the worker's half is the `ActivityRevoked` outcome, and
    /// either suffices. Both matter only for a background attempt: a person's
    /// apply does not wait for a quiet epoch (it still arms the revocation
    /// watcher and gates Commit on `exact_activity`).
    ///
    /// `death` is the worker's evidence about a `ChildDied` candidate, which a
    /// refusing, faulting or starved successor all produce identically (see
    /// [`crate::ChildDeathEvidence`]); every other outcome ignores it.
    #[must_use]
    #[cfg(unix)]
    pub(crate) fn classify(
        mode: crate::native_updater_service::ApplyMode,
        outcome: crate::UpdateHandoffOutcome,
        death: crate::ChildDeathEvidence,
        revoked_by_activity: bool,
    ) -> Self {
        // Before the mode: a stage waiting for a check to confirm its signer is no
        // failure of either lane's, and a person's press of it is not booked as one
        // either (round eight, #503's review).
        if outcome == crate::UpdateHandoffOutcome::StageUnconfirmed {
            return Self::AwaitsRosterConfirmation {
                automatic: mode.is_automatic(),
            };
        }
        if !mode.is_automatic() {
            return Self::Manual;
        }
        // First, and over the main thread's activity flag: re-filing a refusal
        // as activity is the loop `RefusalNeverRetriesAsActivity` rules out.
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
    #[cfg(any(unix, test))]
    fn charges_the_automatic_lane(self) -> bool {
        match self {
            Self::Manual => false,
            #[cfg(unix)]
            Self::AwaitsRosterConfirmation { .. } => false,
            _ => true,
        }
    }
}

/// The fork lane's answer to a capture that refused the desk, by who asked,
/// matching [`HandoffFailureLane::classify`] on the launched lane. Automatic:
/// [`UpdateOutcome::CaptureRefused`], whose caller arms the desk-fingerprint
/// retry. A person's press: [`UpdateOutcome::Failed`], as
/// `abort_reaped_native_apply_before_reconcile` answers it; nothing arms a
/// retry for a press, so Settings must not say "retries when it changes".
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
    /// The overlap attempt parked, spawned, and was revoked by user or terminal
    /// activity before Commit. The rollback is lossless (kill, reap, resume
    /// readers; the parent consumed nothing post-park), so repeating is as safe
    /// as the first attempt. Spaced 2 s growing to 30 s; never exhausted.
    #[cfg(any(unix, test))]
    ActivityRevoked,
    PhysicalFailure,
}

/// A bounded retry plan for cheap ordering/preflight races and, on a short
/// budget and long leash, for physical handoff failures (see
/// [`MAX_PHYSICAL_FAILURE_CYCLES`]); an unbounded, saturating spacing for
/// lossless activity-revoked overlap returns.
///
/// For [`AutomaticRetryKind::PhysicalFailure`], `cycles` is the index within
/// the current epoch: `None` means this epoch is spent, and
/// [`App::spend_physical_failure_budget`] decides whether that ends the epoch
/// or the lane. [`AutomaticRetryKind::ActivityRevoked`] never answers `None`.
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
        // Saturates at 30 s: each revoked attempt costs a launch/park/paint
        // round trip, but the lane promises a landing within a minute. The
        // spacing starts over after a long idle gap.
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
        // Tens of minutes: a physical retry costs a park/spawn/paint round trip,
        // so wait until the machine is plausibly in a different state (caches
        // warm, load down). Two retries per epoch; [`MAX_PHYSICAL_FAILURE_EPOCHS`]
        // bounds the epochs.
        (AutomaticRetryKind::PhysicalFailure, 0) => 600,
        (AutomaticRetryKind::PhysicalFailure, _) => 1800,
    };
    Some(std::time::Duration::from_secs(seconds))
}

/// Whether a pre-park verification of `(build, commit, artifact)` must start at
/// `now`: not when a fresh verdict for that key is cached, and not while a read
/// of that key is still in flight within its ceiling (a second `codesign` only
/// adds load).
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

/// What this process knows about a stage waiting for an update check to confirm its
/// signer ([`App::known_roster_wait`]; round eight, #503's second review).
#[cfg(unix)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KnownRosterWait {
    /// The wait's words ([`aterm_update::is_roster_confirmation_wait`]).
    pub(crate) reason: String,
    /// When that answer ended.
    pub(crate) answered_at: std::time::Instant,
    /// Known from the lane's hold rather than from a read: already held.
    pub(crate) held: bool,
}

/// Raises its flag when dropped: the pre-park verification's "I have ended",
/// on every path out of its thread, a panic included.
#[cfg(unix)]
struct RaiseOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(unix)]
impl Drop for RaiseOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Parse the durable artifact identity without allocating. Reducer-imported
/// stages are already canonical, but automatic application fails closed on
/// malformed identity bytes.
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

/// Seed the process service from the durable status marker every update
/// surface reads. A missing marker yields an enabled/idle (or disabled)
/// service; no network work occurs here.
pub(crate) fn load_native_updater_service(
    launch_installability: crate::native_updater_service::UpdateInstallability,
) -> NativeUpdaterService {
    let build = crate::build_info::build_number()
        .parse::<u64>()
        .unwrap_or(0);
    let version = crate::build_info::version_display();
    // Startup ledger reconciliation is posted by an off-loop worker, so first
    // input never queues behind TOML or PlistBuddy work on the event loop.
    NativeUpdaterService::new(build, version, aterm_update::enabled())
        .seed_installability(launch_installability)
}

/// What the launch's own path already says about whether this copy can replace
/// itself: a disk image, a translocated download, a bare binary and a dev-marked
/// build cannot, read before the event loop runs. An ordinary install stays
/// unobserved until the reconcile reads its bundle.
#[must_use]
pub(crate) fn launch_installability(
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
///
/// Runs only on a worker thread (facts and check workers), so it can take the
/// second ledger read: `aterm_update` keeps the apply lane's reasons out of
/// `UpdateStatus` and exposes them through `apply_lane_report`. This joins
/// them so every surface sees the failure count and reason together.
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
        installable: None,
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
/// These are not reducer state: focus, OS Reduce Motion, recording-watch focus,
/// load shedding, OS appearance, the serious override and GPU capability can
/// change while the view generation stays stable. They still invalidate
/// retained pixels, because Settings previews resolve animation, window
/// appearance, static effect output and their static/live badge from them.
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
/// Control inspection, pixels, hit testing and the accessibility tree all observe this one
/// artifact rather than a second compile.
#[derive(Clone, Debug)]
pub(crate) struct NativeCompiledFrame {
    pub(crate) stamp: NativeUiCompileStamp,
    pub(crate) phase: NativeCompiledPhase,
    pub(crate) compiled: crate::native_ui::CompiledUi,
}

/// Retained semantic scene for one native split leaf. Backend-neutral: raster
/// consumes `compiled.tray`, and a direct `UiScene` backend could consume the
/// same view/stamp/viewport without changing geometry, cache identity, hit
/// testing or accessibility.
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
    pub(crate) ime_caret: Option<crate::native_ui::LogicalRect>,
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
    /// A process command stays a semantic intent until it reaches the head of
    /// the serialized config lane; materializing its OCC patch earlier would
    /// give rapid toggles the same base revision and make them conflict.
    SeriousMode(bool),
    /// Control-protocol `settings set|unset`: an absolute semantic intent,
    /// materialized against the newest service revision at the lane head. It
    /// never runs a standalone read/edit/write beside native Settings.
    ControlField {
        key: String,
        value: Option<String>,
    },
    /// A theme chosen in the command palette: `theme` becomes the value
    /// [`crate::prefs::theme_choice_value`] gives against the newest service
    /// revision at the lane head, so a Follow-system pair keeps its other
    /// side even when an earlier write in the lane changed it.
    ThemeChoice {
        id: String,
        side: aterm_types::Appearance,
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
        /// Real host request identity for a theme edit, independent of view
        /// liveness and ordered with control/palette preview intent.
        theme_request: Option<Box<(u64, String)>>,
    },
    SeriousMode {
        desired: bool,
    },
    /// A `[presence]` leaf's durable write (View ▸ Show Needs in Windows writes
    /// `presence.rim`): `key` is the leaf, `desired` the chosen bit. The live
    /// bit flipped at the click (`App::user_toggle_needs_in_windows`); this
    /// completion adopts the durable truth or, on a refused write, reverts to it.
    Presence {
        key: &'static str,
        desired: bool,
    },
    /// A theme chosen in the command palette (`App::palette_choose_theme`):
    /// `id` is the row's theme. Its preview stands
    /// ([`PreviewHold::Committing`](crate::control_theme::PreviewHold)) until
    /// this completion: an applied write adopts the durable `theme` and draws
    /// it, a refused one reverts the preview and says why.
    ThemeChoice {
        request_id: u64,
        id: String,
    },
    /// Completion sink for the `settings set|unset` wire command. The control
    /// thread blocks on this one-shot while the main loop stays free. Non-success
    /// outcomes return as `Err`, so the wire formatter cannot prefix them with OK.
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
    /// Main-thread projection for the control socket's front-input
    /// authorization fence. Overlay identity wins (it consumes events first);
    /// otherwise native Settings is distinguished from other native apps.
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

    /// Hand `work` to the updater worker; a worker that has hung up is
    /// restarted and offered it once more. A full queue answers `Saturated`.
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
    /// Exhaustion fails closed: reuse could let an old completion outrank a
    /// newer observation.
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
    /// `try_send` never blocks input; on saturation the request stays pending
    /// until a later event-loop turn enqueues it.
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
        // Called at every park point. Check for real work before touching
        // service state or building a destination: on macOS cloning the
        // EventLoopProxy installs a CFRunLoop source, which would turn an idle
        // park into a self-waking hot loop.
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

    /// Pure guard for the event-loop retry wrapper, separate from proxy
    /// materialization so Tier-1 can prove an idle park never clones or wakes
    /// an `EventLoopProxy` or reaches the unavailable/log path.
    #[must_use]
    fn has_pending_native_update_reconcile(&self) -> bool {
        self.pending_native_update_reconcile_purpose.is_some()
    }

    /// Injectable wrapper around proxy materialization, so Tier-1 can count the
    /// proxy side effect itself, not just the worker dispatch below it.
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
            // A headless process's checkpoint is its control socket, and it
            // settles only a trial no window has launched (round eight, #8).
            witness: crate::boot_trial_witness(self.headless),
        };
        self.send_native_update_work(&worker, work)
    }

    /// Refuse a control `update apply` whose updater facts could not be
    /// collected: recorded durably (a Blocked verdict is not a ledger failure)
    /// and surfaced like every other apply outcome.
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

    /// Accept one ordered fact completion, then derive every presentation and
    /// action from the effective reducer stage; the stage wake's build/version
    /// are never authority.
    pub(crate) fn finish_native_update_reconcile(
        &mut self,
        purpose: NativeUpdateReconcilePurpose,
        facts: NativeUpdateReconcileFacts,
    ) {
        // Read before reducing: `reconcile_native_update_facts` publishes the new
        // stage into `self.relaunch`, so a later comparison would never see a
        // newly announced stage.
        let announced_before = self.relaunch.as_ref().map(|notice| notice.build);
        // A deferred purpose rides the newest facts, so it is never dropped as
        // IgnoredStale behind a newer observation.
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
                // The facts were stale but the purpose is not: a control apply
                // whose read began before this process imported its stage is
                // re-observed. Bounded, so a facts worker whose sequence restarted
                // cannot turn one request into a hot loop; past it the request is
                // refused, loudly.
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
                    if newly_announced
                        && matches!(
                            purpose,
                            NativeUpdateReconcilePurpose::Startup
                                | NativeUpdateReconcilePurpose::StageAvailable
                        )
                    {
                        // A newly staged build is announced by the status bar:
                        // "Updating to aterm vX" while the automatic lane is armed,
                        // "aterm vX is ready · click to install" where a press
                        // installs it. The border glow fires when the apply runs.
                        //
                        // Posted here, not by the downloader: `Progress::Staged` is
                        // gated on this process having downloaded the artifact, so
                        // a build staged by an earlier launch or a sibling aterm
                        // would never open the row. `note_update_progress` is
                        // idempotent for a row already up.
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
                        // A control apply that did not land (preflight blocked,
                        // deferred) leaves the stage armed like any other import.
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

    /// A LINUX BUILD HELD FOR A PERSON IS ON THE BAND (ATERM-KI-1015-2, U21).
    /// With automatic install off, a Linux check downloads a newer build and
    /// holds it for `aterm update apply`; the macOS apply authority takes no
    /// Linux stage (`staged_from_status`), so Settings ▸ Software Update showed
    /// it and the band never did. Every reduced reconcile reads the copy's
    /// facts: a held build newer than this process is said once per build
    /// ([`crate::update_words::linux_held`]) — found at launch or after a pass —
    /// and owed to a band that can show it; its row leaves with the stage
    /// (installed by the command, or replaced). Reads nothing on macOS.
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

    /// Reduce the disk facts a returned apply carried through the same door as
    /// every other reconcile, so whatever they import is also armed. A pending
    /// deferred purpose rides these newer facts; otherwise this is a plain
    /// refresh (arms, never announces).
    #[cfg(any(unix, test))]
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
    /// when another leaf is focused or its tab is inactive. Never borrows the
    /// focused leaf's rectangle.
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

    /// Device-pixel Y where native content begins: top pad + OS head band +
    /// in-frame chrome rows (tab strip + status bars). Painter and pointer
    /// projection share this origin.
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
        // The page palette is a second paint input (`window_theme` can force
        // it off the terminal theme, and color overrides do not reach it);
        // hashing the resolved fields re-lowers retained pages exactly when
        // their pixels move.
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
        // The automatic selected-tab colour Settings shows follows the chrome
        // theme, High Contrast and the system accent.
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
        #[cfg(test)]
        NATIVE_UI_COMPILES.with(|count| count.set(count.get().saturating_add(1)));
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
            display_edr: self
                .windows
                .get(&wid)
                .is_some_and(|window| window.screen_edr_potential),
            // The companion verdict's three rungs, read without advancing the
            // tenure gate (`promotable_kitty`'s read).
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

    /// Resolve one exact retained native artifact, the tree and destination
    /// paint, pointer routing, accessibility and inspection all consume. Any
    /// lifecycle, geometry, service, theme or document drift changes the compile
    /// stamp and fails closed.
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
            ime_caret: raster.ime_caret,
            device_x,
            device_y,
            width: raster.width,
            height: raster.height,
            scale,
        })
    }

    /// Anchor native IME candidates to the exact focused caret just presented.
    /// The retained artifact owns the same origin/scale used for pointer input.
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
        // Inverse of retained_native_leaf_at_pointer: the presented leaf supplies
        // the native scale and signed window origin, including a mixed pane's
        // offset and transient centred crop.
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

    /// The aterm.toml completion panel the editor view draws (the same rule
    /// as `EditorApp::view`), so the host sizes the text for what is really
    /// left and the caret line stays on screen.
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
            // A list asked for with nothing to list and no help draws no
            // panel, as the view filters it out.
            (assist.help.is_some() || !assist.completions.is_empty())
                .then(|| crate::native_ui::EditorPalette::Rows(assist.completions.len().max(1)))
        } else {
            // Resting on a key: the help line alone, as the view draws it.
            assist
                .resting_help
                .is_some()
                .then_some(crate::native_ui::EditorPalette::Help)
        }
    }

    /// Dispatch through the active app reducer, execute every typed effect, then
    /// refresh tab chrome. Shared by pointer/keyboard input and the control
    /// `act` path.
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
                // The view lists "No matching command" in a row of its own.
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
        // The toolbar the view draws, so the rows it leaves the file are the
        // rows the caret is kept within.
        let toolbar = match self.native_runtime.app(instance) {
            Some(crate::native_app::NativeApp::Editor(editor)) => editor.toolbar(),
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
        // Only Settings scrolls a second region (its sidebar): every other
        // view's aimed scroll is its one list's, lowered before its own
        // mappings (Markdown's measured scroll, the editor's view scroll).
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
        if let AppEvent::EditorCommand(command) = &event {
            let _ = crate::command_registry::editor_command(command);
        }
        // Palette/buttons and the Emacs keymap lower into the same editor
        // reducer; the command registry above classifies authority first.
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
            // no regional-damage effect lane. Invalidate its exact leaf only,
            // and not for an input it declined, which changed nothing.
            if result == EventResult::Handled {
                self.invalidate_native_view_cache(wid, view, DamageRegion::All);
                self.refresh_native_presentation(wid, instance, view);
            }
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
    /// view no longer active in the window, so a tab switch cannot redirect a
    /// semantic control action. Human input uses [`Self::dispatch_native_event`].
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
    /// the app and never reach a hidden PTY. Geometry and focus return to the
    /// host seam as window properties.
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
        // A keypad key is its main-block twin on a native page:
        // `keymap::build_key_input` keeps the keypad identity (`Numpad5`,
        // `NumpadEnter`, `NumpadEnd`) for the PTY encoder, and no page needs it.
        // A keypad digit commits its glyph; Enter/arrows/Home/End/Page/Insert/
        // Delete drive the main-block arms. `InputEvent::keypad_folded` is the
        // one fold, shared with the seam's press classifier. KP_Begin has no
        // twin and lowers to nothing.
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
                    // What the view draws: the help line, and the
                    // suggestions only while typed or asked for.
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
        // The one completion-page step every assist key (Ctrl-Space, Tab, Enter) sends.
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

        // Tab moves keyboard focus between a native page's controls, except
        // in an Editor, where it is the buffer's (indent-for-tab-command, or
        // M-x completion) unless the config editor's completion list takes it.
        // A modified Tab the Editor declines moves focus, below.
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
                // First Tab enters the completion list without editing the
                // document; arrows choose, Enter or a second Tab accepts.
                return true;
            }
            if !editor_active && *event_type == KeyEventType::Press {
                let _ = self.move_native_focus(wid, mods.contains(Modifiers::SHIFT));
                return true;
            }
        }
        // F6 and Shift+F6 move keyboard focus between a native page's
        // controls in every app, and in an Editor, whose Tab indents, they are
        // how focus moves at all (design D-15). Ctrl+Tab stays the host's
        // next tab.
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
        // THE KEY IS TESTED BEFORE THE PAGE IS: only a Return or Space press
        // asks whether a text field has the keyboard (a lookup in the page's
        // compiled tree), so an arrow, a page key or a typed letter never
        // reaches that lookup at all.
        if !editor_active
            && let InputEvent::Key {
                key: Key::Named(key @ (NamedKey::Enter | NamedKey::Space)),
                event_type: KeyEventType::Press,
                ..
            } = event
            && !self.native_text_field_has_focus(wid)
            // A font list being searched takes Space as part of the name.
            && !(matches!(key, NamedKey::Space) && self.native_space_types_into_search(wid))
        {
            if self.activate_native_focus(wid).unwrap_or(false) {
                return true;
            }
            // Nothing holds keyboard focus: Return (and KP_Enter, folded above)
            // activates the page's default button, the native convention. Space
            // never does; on macOS it only activates the focused control.
            if matches!(key, NamedKey::Enter) && self.activate_native_default(wid).unwrap_or(false)
            {
                return true;
            }
        }

        // A SEGMENTED CONTROL'S ARROWS ([`crate::native_ui::UiNode::segmented`]):
        // Left and Right move the keyboard between its segments, and stop at
        // its ends; Return or Space (above) chooses the one it is on. Read
        // from the page's compiled tree, so every segmented control on every
        // native page answers the same way.
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
            // Geometry and focus are window-level facts, not text input, so they
            // fall through to the ordinary path; `ResizeWindowPx` only requests an
            // OS window change.
            InputEvent::Resize { .. }
            | InputEvent::ResizeWindowPx { .. }
            | InputEvent::Focus(_) => return false,
            // An Editor's keys, text and pastes go to amacs's input reducer
            // whole (`EditorWorkspace::handle_input`), which alone decides what
            // a key does there.
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
                // The config editor's completion list, while it holds focus,
                // takes the vertical arrows: it is this host's panel, not the
                // buffer's.
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
                    // The text a character key types is the layout's: `key`
                    // is its UNSHIFTED key (Shift+a is `a`, German Shift+2 is
                    // `2`).
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
                    // Readline caret bindings for the Settings text fields:
                    // Ctrl-A/E home/end, Ctrl-B/F left/right, Ctrl-D delete
                    // forward, Ctrl-K/U kill to end/start, Ctrl-W word back.
                    // Ctrl only (never ⌘/⌥), matched before the `command` arms
                    // because `command` folds Ctrl in. Scoped to Settings so other
                    // native apps keep their Ctrl shortcuts.
                    let readline = settings_active
                        && mods.contains(Modifiers::CTRL)
                        && !mods.intersects(
                            Modifiers::SUPER | Modifiers::ALT | Modifiers::HYPER | Modifiers::META,
                        );
                    // The ⌥ twin of `readline`, for word motion only.
                    let alt_word = settings_active
                        && mods.intersects(Modifiers::ALT | Modifiers::META)
                        && !mods.intersects(Modifiers::CTRL | Modifiers::SUPER | Modifiers::HYPER);
                    // Off macOS, Ctrl is the platform word modifier (Ctrl+←/→), so
                    // it joins the ⌥ twin for the named-key arms below. The Ctrl
                    // letter arms match first, so a Ctrl chord reaches an
                    // `alt_word` arm only on an arrow or ⌫. Must match
                    // `app_input::field_edit_action`'s `by_word` so the find/rename
                    // fields and Settings agree about one chord.
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
                        // Word motion in Settings fields: ⌥←/⌥→, and ⌥B/⌥F (the
                        // emacs spelling, like the terminal's ESC-b/ESC-f). ⌥ only,
                        // so ⌘← and the Ctrl readline set are untouched.
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
                        // Ctrl+⌫ = backward-kill-word off macOS, ahead of the plain
                        // ⌫ arm. Ctrl+⌦ has no `TextInputEvent` to ride, so forward
                        // word delete is out of scope.
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
                        // The text press types the character the terminal would:
                        // `key` is the layout's UNSHIFTED key, so Shift+a used to
                        // commit 'a' and German Shift+2 '2'.
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
                        // Settings: a text field's caret, else the list's
                        // top or bottom (the view decides which).
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
            // A native view scrolls one vertical list, so a horizontal wheel
            // maps to no event (`vertical_up()` returns `None`).
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
            // A Tab an Editor declines (a modified Tab with no M-x query or
            // key sequence open) is focus traversal, as on every native
            // page: Shift+Tab back, and Ctrl+Tab no host binding took
            // (`next_tab` is seeded off macOS) forward, Cocoa's Control-Tab
            // out of a view whose Tab is its own.
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

    /// The active native page's compiled tree as the window holds it now:
    /// the compile its last redraw retained, while every input still matches
    /// the stamp taken with it ([`Self::cached_native_ui`]), else a fresh
    /// one. Input reads the page through this, so a key never compiles a
    /// page the window has already compiled.
    pub(crate) fn current_native_ui(
        &self,
        wid: WindowId,
    ) -> Result<std::borrow::Cow<'_, crate::native_ui::CompiledUi>, String> {
        match self.cached_native_ui(wid) {
            Some(frame) => Ok(std::borrow::Cow::Borrowed(&frame.compiled)),
            None => self.compiled_native_ui(wid).map(std::borrow::Cow::Owned),
        }
    }

    /// From the segment the keyboard is on, `step` segments on in its
    /// segmented control ([`crate::native_ui::CompiledUi::segment_step`]):
    /// `None` where the keyboard is on no segment.
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

    /// Put the keyboard on control `key` of the active native page, ringed,
    /// as Tab does ([`Self::move_native_focus`]).
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
        let (focus_order, next) = {
            let compiled = self.current_native_ui(wid)?;
            let focus_order = &compiled.focus_order;
            if focus_order.is_empty() {
                return Ok(());
            }
            // Where a key stands in the page's reading order, focusable or not.
            let order_of = |key: &crate::native_ui::UiKey| {
                compiled.semantics.iter().position(|node| &node.key == key)
            };
            let next = current
                .as_ref()
                .and_then(|key| focus_order.iter().position(|candidate| candidate == key))
                .map_or_else(
                    || {
                        // A focus that left the focus order (its control disabled
                        // itself under the keyboard, s-25) resumes from where it
                        // stood: the nearest focusable after it, or before it going
                        // back — never the page's first or last control.
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
            (focus_order.clone(), next)
        };
        let mut key = focus_order[next].clone();
        // The Settings sidebar is one Tab stop: Tab leaves it for the page
        // in one step, and entering it lands on the selected row (its arrow
        // keys move between rows).
        if let Some(crate::native_app::AppViewState::Settings(state)) =
            self.native_runtime.view_state(view)
            && let Some(target) =
                state.tab_past_navigation(current.as_ref(), &key, !backwards, &focus_order)
        {
            key = target;
        }
        // A Settings list goes on past its screen: Tab from the last control
        // it shows moves to its next one, which the list scrolls into view.
        if let Some(current) = current.as_ref()
            && let Some(crate::native_app::AppViewState::Settings(state)) =
                self.native_runtime.view_state(view)
            && let Some(beyond) = state.tab_past_screen(current, &key, !backwards)
        {
            key = beyond;
        }
        // Settings ▸ Themes' list is one Tab stop: in at the highlighted
        // row, out in one step.
        if let Some(crate::native_app::AppViewState::Settings(state)) =
            self.native_runtime.view_state(view)
            && let Some(target) =
                state.tab_past_options(current.as_ref(), &key, !backwards, &focus_order)
        {
            key = target;
        }
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

    /// Whether the active Settings view's font list is being searched, so
    /// Space types a space into the search instead of pressing a font.
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

    /// Activate the page's default button (`CompiledUi::default_action`), the
    /// bare-Return fallback when no control holds keyboard focus. `false` when
    /// the page declares no enabled Primary button; the key then flows on to
    /// the text-input lowering.
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
            AppEffect::ConfigPatch { patch, reply } => {
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
            AppEffect::ConfigUndo { token, reply } => {
                self.native_config_pending.push_back(NativeConfigRequest {
                    origin: NativeConfigOrigin::View {
                        instance,
                        view,
                        reply,
                        theme_request: None,
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
            // The page named an entry and a button; the host resolves both and
            // performs the intent in this window (`perform_message_act`).
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
                // The modal picker runs its own nested loop on the main thread.
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
                    // The lane replies asynchronously after persistence; only a
                    // synchronously-known failure is worth surfacing here.
                    if let Ok(Err(error)) = outcome.try_recv() {
                        aterm_log::warn!("wallpaper picker: {error}");
                    }
                }
            }
            AppEffect::OpenLogFolder { reply } => {
                // The host's own log directory — never a path from the view — opened
                // through NSWorkspace like Open Log: no shell, no Terminal. The page
                // is told how it went (ruling 441 (t)), as `OpenExternal` tells it.
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
                // The themes folder is the host's to resolve: the view named
                // a catalog id at most (`settings_themes_host`).
                let outcome = crate::settings_themes_host::perform(&request);
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
            // Settings ▸ Harness: the page named a request; the host performs it
            // and the page converges on the republished projection.
            // An AI CLI's Sign in opens its tab in the Settings view's own
            // window.
            AppEffect::Harness(crate::native_app::HarnessRequest::AiSignIn { id }) => {
                self.ai_sign_in(wid, id);
            }
            AppEffect::Harness(request) => self.execute_native_harness(request),
            // Its timer press, through the App's one writer.
            AppEffect::HarnessTimer(request) => self.execute_harness_timer(request),
            AppEffect::InvalidateOwnPresentation => {
                self.refresh_native_presentation(wid, instance, view);
            }
            AppEffect::RepaintSelf(damage) => {
                self.invalidate_native_view_cache(wid, view, damage);
                if let Some(ws) = self.windows.get_mut(&wid) {
                    ws.last_present = None;
                    // The native tray is a retained full-page raster; dropping it
                    // keeps a control capture or the next present from reusing
                    // the previous route's pixels under the new semantic tree.
                    ws.settings_card = None;
                }
                if let Some(window) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                    window.request_redraw();
                }
            }
        }
        Ok(())
    }

    /// Enqueue one `settings set|unset` command into the serialized, versioned
    /// persistence lane native Settings uses, so validation, reconciliation, OCC,
    /// atomic publication and follow-up observation have one owner. The main
    /// loop never blocks on the disk worker: completion carries the socket's
    /// one-shot reply back through [`NativeConfigOrigin::Control`].
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

    /// The queue handoff's request identity, when preflight and dispatch admit
    /// it. The caller's reply remains owned by the same serialized lane.
    pub(crate) fn queue_control_settings_field_admitted(
        &mut self,
        key: String,
        value: Option<String>,
        reply: std::sync::mpsc::Sender<Result<String, String>>,
        request_id: u64,
    ) -> Option<u64> {
        let Some(canonical_key) = crate::prefs::editable_fields(&self.config)
            .into_iter()
            .find(|field| field.key == key)
            .map(|field| field.key.to_string())
        else {
            // A key the file still loads but nothing reads (retired, or kept for
            // compatibility) gets the band's and Manual's own story, never
            // "unknown": a script that set it before its feature went is told
            // why it no longer can, not sent to a Settings search that finds
            // nothing. A key written by hand (`conpty`) is told where it is
            // written.
            let message = crate::native_config_language::control_settings_refusal(&key);
            let _ = reply.send(Err(message));
            return None;
        };
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
            // Reconciliation/capability failure happened before this request was
            // reduced. Remove only this caller's tagged request so the control
            // thread gets a bounded ERR; older UI intents stay queued for retry.
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
        // Shares the semantic queue with the native Serious Mode command.
        // Project a valid absolute intent before pumping so a following toggle
        // composes against it, not the durable value. Invalid bool text has no
        // projection; reduction rejects it without changing the service.
        if let Some(desired) = serious_mode_intent {
            self.serious_mode_queued_projection = Some(desired);
        }
    }

    /// Enqueue the menu/keybinding/palette Serious Mode command in the versioned
    /// lane native Settings uses. Capability preflight happens before the
    /// request is visible; the desired value becomes an exact OCC patch only at
    /// the head of the lane, against the last completed/optimistic revision. The
    /// live policy is untouched until durable completion returns.
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

    /// Compose a new click against the newest queued intent, not the durable
    /// policy, so rapid toggles keep their parity while completions are in
    /// flight.
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

    /// Recompute the semantic projection after a completion or failed queue
    /// admission. The newest valid intent wins; malformed bool text is ignored
    /// because its reduction always rejects without a write.
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
                    | NativeConfigOrigin::ThemeChoice { .. } => None,
                });
    }

    /// A presence toggle's durable write completed. The live bit flipped at the
    /// click; an applied write adopts the durable `[presence]` table into
    /// `self.config`, while a refused one (a conflict with a hand edit, an
    /// indeterminate or rejected write) says so and reverts the live bit to the
    /// durable truth, as Serious Mode's completion does.
    fn publish_presence_completion(
        &mut self,
        key: &'static str,
        desired: bool,
        outcome: &ConfigPatchOutcome,
        authoritative: Option<&crate::native_config_service::ConfigSnapshot>,
    ) {
        let label = crate::app_take_over::PRESENCE_RIM_LABEL;
        // `(cause, indeterminate)`: the toggle reverted, or may not have
        // persisted.
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
                control_settings_completion_reply(
                    key,
                    Some(&desired.to_string()),
                    None,
                    other,
                    None,
                )
                .err()
                .unwrap_or_else(|| "the write was refused".to_string()),
                false,
            )),
        };
        if let Some((cause, indeterminate)) = feedback {
            // The durable truth wins over the click: from the authoritative
            // snapshot when there is one, else the config the App last adopted.
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

    /// Queue the durable write for the needs switch (`presence.rim` set to
    /// `desired`, View ▸ Show Needs in Windows) through the lane and OCC
    /// materialization Serious Mode uses, so rapid clicks cannot conflict with
    /// each other's completions.
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

    /// Queue the durable `theme` write for a theme chosen in the command
    /// palette, through the lane and OCC materialization the presence toggles
    /// use. The value is the Settings pickers' ([`crate::prefs::theme_choice_value`]):
    /// over a plain name or an unset `theme`, the name itself (Default too);
    /// while `theme` is a `dark:…,light:…` pair, the pair with only the side
    /// on screen replaced, so the choice keeps following the OS.
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

    /// A palette theme choice's durable write completed. Applied: the durable
    /// `theme` is adopted into `self.config` at once (the watcher's reload of
    /// the same text then changes nothing), the choice's preview ends, and the
    /// configured theme — now the chosen one — is installed, so the screen
    /// never falls back to the old theme in between. Refused: the preview
    /// reverts to the configured theme and the band says why.
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
            ConfigPatchOutcome::Conflict { .. } => Some((
                "aterm.toml changed first; its current value was kept".to_string(),
                false,
            )),
            ConfigPatchOutcome::Indeterminate { message } => Some((message.clone(), true)),
            other => Some((
                control_settings_completion_reply(
                    crate::prefs::EDIT_THEME,
                    Some(id),
                    None,
                    other,
                    None,
                )
                .err()
                .unwrap_or_else(|| "the write was refused".to_string()),
                false,
            )),
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

    /// Build the compare-and-swap request for a Serious Mode intent at dequeue
    /// time, when every earlier request has reduced into the service and the
    /// base revision and expected value are current.
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

    /// Build the `theme` compare-and-swap for a palette choice at dequeue
    /// time, from the raw value and catalog of the newest revision.
    fn theme_choice_patch_request(
        &self,
        id: &str,
        side: aterm_types::Appearance,
    ) -> Result<crate::native_app::ConfigPatch, String> {
        let snapshot = self.native_config_service.snapshot();
        let expected = snapshot.values()?.remove(crate::prefs::EDIT_THEME);
        let (value, _) = crate::prefs::theme_choice_value(
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
            NativeConfigWork::SeriousMode(_)
            | NativeConfigWork::ControlField { .. }
            | NativeConfigWork::ThemeChoice { .. } => {
                unreachable!("semantic config work is materialized above")
            }
        })
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
            // A conflict or post-publication proof failure invalidates the disk
            // baseline: dispatch a bounded worker observation before popping the
            // next request. The event loop never opens the path or its assets.
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
                // Park; do not fail the caller. The probe just dispatched always
                // reports back and either outcome settles this queue: success
                // clears the fence and re-pumps
                // (`finish_native_config_reconciliation`); failure runs
                // `fail_native_config_reconciliation`, which answers every queued
                // origin. The wait is one worker file read.
                //
                // Not failing fast on a remembered broken probe: that would
                // answer with a stale reason while a fresh probe is in flight,
                // turn a transient failure into a hard ERR, and could tell a
                // queued `SeriousMode` intent (which has no tag to withdraw) that
                // it failed while the probe went on to apply it.
                return Ok(());
            }
            if self.native_config_pending.is_empty() {
                return Ok(());
            }
            // Acquire every fallible host capability before reducing, so an
            // unavailable proxy/worker cannot advance the in-memory revision
            // without making it durable.
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
                    let job = NativeConfigJob::Persist(Box::new(NativeConfigPersistenceJob {
                        plan,
                        undo: Some(undo.get()),
                        origin,
                        proxy,
                    }));
                    if let Err(error) = queue.send(job) {
                        // The reducer advanced but no worker owns the candidate.
                        // Restore from durable authority before completing; a
                        // control-origin request owns a blocked socket reply and
                        // must not be dropped.
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
    /// completed even if its initiating view closed; revision publication fans
    /// out to every live Settings view.
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
        // Word the log line and notice from the outcome: a `Rejected` outcome can
        // fail its preflight before any byte reaches disk, so "Config was saved"
        // would be false there.
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
            // A watcher candidate was observed while persistence owned the lane.
            // Its order is ambiguous until a worker samples the path after this
            // point, so fence queued semantic writes now.
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

    /// Settle "a reconciliation worker reported back and the lane is still
    /// fenced": answer everything queued behind this sample and tell the user.
    /// The fence stays armed; only an admitted disk generation clears it.
    ///
    /// Does not re-pump: the fence check in `pump_native_config` precedes the
    /// emptiness check, so pumping would re-sample the same unreadable path and
    /// loop. The next `settings set` or Settings save dispatches a fresh sample,
    /// which is how a transiently broken lane recovers. No failure is
    /// remembered, so the next caller is never failed pre-emptively.
    ///
    /// The user is told only when something was lost. The common case is a
    /// hand-edited `aterm.toml` that does not parse: the watcher already posted
    /// "Config observation was not valid TOML" on the lane's row, and with
    /// nothing queued that row is left standing and the failure goes to the log.
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
                // This sample was the one thing that could clear the fence and
                // it came back broken; hand its reason to every request queued
                // behind it, since nothing else drains this queue.
                self.fail_native_config_reconciliation(&error);
                return;
            }
        };

        if self.native_config_external_sequence > completion.pending_sequence {
            // A watcher completion arrived after this worker sampled the path.
            // Admit the sample as an intermediate generation, retain the newer
            // watcher payload, and sample once more before any write.
            if let Err(error) = self
                .native_config_service
                .synchronize_prepared_observation(prepared)
            {
                // The sample read fine but the service refused it: the fence
                // stays armed with nothing scheduled, so answer the queue.
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

    /// Consume the deferred generation whose baseline a reconciliation worker
    /// just observed. Prepared runtime generations bypass the closed
    /// reconciliation gate exactly once; their own admission clears it
    /// atomically with the matching bytes/assets.
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
                // Reached from `finish_native_config_reconciliation`'s
                // deferred-generation drain: the re-armed fence has nothing
                // dispatched to clear it, so answer the queue here too.
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
            // The worker queue itself is gone, so the fence just armed can never
            // get a sample; queued requests are unanswerable.
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
                // The running settings stay. The watcher already posted this
                // read's `[harness]` lines (`Wake::HarnessPolicyObserved`), kill
                // switch included.
                self.surface_native_config_lane_error(format!(
                    "Config observation was not valid TOML: {error}"
                ));
                if let Err(error) = self.pump_native_config() {
                    self.surface_native_config_lane_error(error);
                }
            }
        }
    }

    /// Run the supervisor under `harness` and keep it on the running config so
    /// the next reload compares against what runs. Sole caller: the watcher's
    /// `Wake::HarnessPolicyObserved`, posted for every read in disk order
    /// (`config_watcher::observed_harness`), so the window runs the newest
    /// read's `[harness]`. Workers restart under a changed policy, and
    /// `enabled = false` stops them within one wait (`harness_host`).
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
        // The running config mirrors the cell, for Settings and for the next
        // reload's comparison.
        self.config.harness = running;
        // An agent that answers for itself re-reads `harness policy` on its
        // `subscribe` stream's `harness-policy gen=<n>` (no clock).
        self.announce_harness_policy();
        // Every session's effective Autopilot reads the policy in force: the
        // roster's readers off this thread are told (`autopilot=`), and every
        // footer's chip repaints.
        self.publish_harness_posture();
        // Every session's A/B arm reads the posture: the stretch so far goes
        // to the arm that was in force; Settings ▸ Harness shows the policy.
        self.note_harness_arms();
        self.harness_screen_moved();
        // Every need verdict reads whose boxes the host answers under the
        // policy in force (`HarnessHosting::hosts`): each is published again.
        self.publish_needs_all();
        // Summarize in a New Tab's row reads the policy too
        // (`App::summarize_start_block`).
        self.mark_status_dirty();
        self.request_redraw_all_windows();
    }

    /// The native config lane's own error (an observation that was not TOML, a
    /// failed pump, a dropped request): logged and posted as the single
    /// `config.lane` row, newest cause.
    pub(crate) fn surface_native_config_lane_error(&mut self, message: String) {
        aterm_log::warn!("native config: {message}");
        self.post_message(crate::message_reporters::config_lane_error(&message));
    }

    /// Answer every still-queued semantic request with one bounded rejection and
    /// empty the lane.
    ///
    /// Call this from every arm that re-arms the write fence without a worker
    /// sample scheduled to clear it: the baseline each queued request needs is
    /// gone, and a parked request would leave a control caller blocked on its
    /// one-shot `Sender` until the wire timeout. The caller's next command
    /// retries.
    ///
    /// Nothing queued has been reduced (`pump_native_config` pops and reduces
    /// in one step), so there is no optimistic state to roll back.
    ///
    /// Callers must not pump afterwards: the fence check runs before the
    /// emptiness check, so pumping would spin another reconciliation.
    ///
    /// Returns how many requests were rejected.
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
                theme_request,
            } => {
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
            NativeConfigOrigin::Presence { key, desired } => {
                self.publish_presence_completion(key, desired, &outcome, authoritative.as_ref());
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
                // What the key holds once the write landed: a removal, or a
                // value its default made one, is answered as removed, with
                // what it now reads as — the words its Settings row shows.
                // Read from the authoritative snapshot, or, where the write
                // is durable but the disk generation after it could not be
                // admitted, from the candidate the service wrote, which is
                // what the worker proved on disk (it stays the service's
                // state, and the lane runs one write at a time). Never the
                // requested text: a value equal to its default was written
                // as a removal, and a number as its kind spells it.
                let landed = authoritative.clone().or_else(|| {
                    matches!(outcome, ConfigPatchOutcome::Applied { .. })
                        .then(|| self.native_config_service.snapshot())
                });
                let written = landed.as_ref().map_or_else(
                    || value.clone(),
                    |snapshot| snapshot.semantic_values.get(&key).cloned(),
                );
                let now = landed
                    .as_ref()
                    .filter(|_| written.is_none())
                    .and_then(|snapshot| {
                        crate::prefs::effective_value_label(&snapshot.config, &key)
                    });
                let mut response = control_settings_completion_reply(
                    &key,
                    written.as_deref(),
                    now.as_deref(),
                    &outcome,
                    synchronization_error.as_deref(),
                );
                if key == crate::prefs::EDIT_THEME {
                    // Keep actual saved truth even when a newer preview stands;
                    // only this request's own committing hold may be retired.
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
                // A saved `theme` the admitted catalog cannot draw (or a selector
                // with grammar issues) is said in the reply, not only on the band
                // the caller cannot see: `settings set theme Nrod` is not a quiet OK.
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

        // Settings already received the exact snapshot synchronously above;
        // the worker-prepared runtime generation is scheduled by the caller.
        self.refresh_serious_mode_queued_projection();
        self.request_redraw_all_windows();
    }

    /// Install only this command's process-global projection from the
    /// worker-validated snapshot, without reparsing TOML on the event loop, then
    /// fan the generation out to Settings.
    fn apply_serious_mode_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) -> bool {
        self.config.serious_mode = snapshot.config.serious_mode;
        let enabled = self.apply_serious_mode(snapshot.config.serious_mode_or_default());
        self.publish_native_config_snapshot(snapshot);
        enabled
    }

    /// Feed one stable file observation through the serialized config lane. If
    /// a Settings commit is in flight, retain the bytes + disk generation;
    /// completion rejects it if that generation is no longer current.
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

    /// Publish one admitted config generation to every live Settings view. Full
    /// reload callers install `App.config` and assets first; a narrow process
    /// command may install only its own live projection, then publish the full
    /// durable snapshot while the ordinary reload applies the rest.
    pub(crate) fn publish_native_config_snapshot(
        &mut self,
        snapshot: &crate::native_config_service::ConfigSnapshot,
    ) {
        self.note_config_keys(snapshot);
        self.dispatch_settings_event(&AppEvent::ConfigChanged(snapshot.clone()));
        // Stable observations also refresh host semantics for an open Manual
        // buffer. `analysis_generation` advances on byte-identical observations,
        // so asset/font diagnostics refresh even when the TOML text did not.
        if let Some(document) = self.native_runtime.config_editor_document() {
            self.request_config_host_diagnostics(document);
        }
    }

    /// The node log's `config key=` (session log 5J-2): the schema keys whose
    /// authored values `snapshot` moved since the last one this App
    /// compared, by NAME only. Every admitted snapshot is published here, so
    /// this is the one place a config change reaches the node log.
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
        // Completion reducers remove pending work even when the host rejected or
        // blocked it; refresh from the final presentation so the tab's busy/
        // attention state never lags its Settings view.
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

    /// Admit one Packages verb. Physical work (spawning the co-located `atpkg`,
    /// reading its `status.toml`) happens on a worker thread; admission is
    /// memory-only plus one `current_exe`-sibling stat.
    pub(crate) fn execute_native_packages(&mut self, request: PackagesRequest) -> PackagesOutcome {
        let atpkg = crate::co_located_atpkg();
        if let Err(message) = packages_request_admissible(&request) {
            return PackagesOutcome::Failed { message };
        }
        // Apply writes the Mac, not the instance, so it is refused by name under
        // a private state root, before any child. The same flag leads the passes
        // below (`crate::pass_lead`).
        let private_state_root = aterm_types::dirs::runs_under_private_state_root();
        if let Some(message) = packages_request_refusal(&request, private_state_root) {
            return PackagesOutcome::Blocked { message };
        }
        let argv = packages_argv(&request);
        // `atpkg machine apply` is a local verb (no store, index or root key), so
        // the manager gate below does not apply. Its stdout carries the
        // `machine-settings:` row and the verdict sentence.
        let machine_apply = matches!(request, PackagesRequest::MachineApply);
        let check = matches!(request, PackagesRequest::CheckUpdate);
        let install = matches!(request, PackagesRequest::InstallDefaultSet);
        // A person pressed it and waits on it, so its pass takes the band's row.
        // The machine apply is a local, sub-second edit.
        let person = person_verb(&request);
        let Some(atpkg) = atpkg else {
            return PackagesOutcome::Failed {
                message: "the package tool is not installed beside aterm".to_string(),
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
            // Backstop: the page disables its buttons whenever any worker is in
            // flight, so this is a click that raced the disable.
            return PackagesOutcome::Blocked {
                message: "Still collecting package status — try again in a moment.".to_string(),
            };
        };
        // The busy flip is observable: publish before the worker starts.
        self.publish_native_packages_state();
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-packages-verb",
            crate::qos::Role::Background,
            move || {
                // Same lane, store and children as the six-hourly pass. Nobody
                // blocks on it, and a QoS role does not pass from a thread's
                // creator (see qos.rs).
                // atpkg records durable status in status.toml; the process result
                // is kept separately because that status may predate a failed
                // launch or non-zero exit. Stderr is kept: atpkg explains refusals
                // there (commonly another process holding the store lock during a
                // multi-GB download).
                let mut command = PackagesCommandOutcome::Succeeded { operation: busy };
                let mut machine_verdict: Option<String> = None;
                let mut machine_state: Option<atpkg::machine::MachineState> = None;
                let layout = atpkg::store::resolve_configured();
                // The check's one fail-fast: a store held by a person's typed verb
                // (`aterm pkg …`, `claude update`) is answered at once. Behind any
                // lane it waits like the window's passes.
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
                // The person's row: told before the child runs and again at its
                // exit, whatever that exit was; every `Began` has its `Ended`.
                let tell = |event: crate::messages_host::PersonPass| {
                    if person.is_some() {
                        let _ = proxy.send_event(Wake::PkgPersonPass(event));
                    }
                };
                for verb in process.iter() {
                    if let Some(v) = person {
                        tell(crate::messages_host::PersonPass::Began(v));
                    }
                    // No stdin: a windowed child must never sit on a prompt nobody
                    // sees.
                    //
                    // Stdout is null unless the verb may apply host settings: the
                    // machine apply, `packages/check` and every install print the
                    // `machine-settings:` marker (wanted by the pull-down and the
                    // This Mac status), parsed by the launch pass's marker parser
                    // (`spawn_machine_settings_once`). Otherwise the install passes
                    // stream a marker contract this worker does not read.
                    let reads_stdout = machine_apply || busy.applies_machine_settings();
                    // QoS-classed spawner: a background pass must not compete with
                    // the window for the scheduler.
                    let mut child = crate::qos::atpkg_command(&atpkg);
                    // The check is the window's own pass: the same argv
                    // (`--wait-lock` with the lanes' bound, `--progress-file`), the
                    // spawner's pid and the login shell's PATH, so a window quitting
                    // mid-check hands the child to the orphan watch, and its waiting
                    // row streams like any pass's. Both passes lead with the private
                    // window's flag: the `atpkg` beside a dev build may lack a seam.
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
                        // The install streams too (lock wait and progress file), so
                        // it queues like a pass and its row reads the plan.
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
                    // The check and install stream like the lanes' passes
                    // ([`crate::run_pass_child`]), so a pass queued behind another
                    // shows its waiting row at once. Their `machine-state:`
                    // record is not taken; see below.
                    if check || install {
                        let post_proxy = proxy.clone();
                        let post = move |event: Wake| {
                            // Its own queue at the store lock is the person's row's;
                            // the lanes' `lock-waiting:` stays a log line.
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
                        // The pass's machine lines, through the reader the launch
                        // lanes use. Not its `machine-state:` record: stdout is
                        // collected at exit and the pass measured the machine at
                        // its top, possibly minutes earlier, so the completion
                        // re-reads instead, which closes the expectation the change
                        // line opens.
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
                // Roll the reservation back so the surface does not stay busy; the
                // synchronous failure is the feedback. `abort` (not `finish`) keeps
                // any previously observed report, since no worker ran.
                let _ = self.native_packages_service.abort(sequence);
                self.publish_native_packages_state();
                PackagesOutcome::Failed {
                    message: format!("could not start the packages worker: {error}"),
                }
            }
        }
    }

    /// Note what this copy's package tool IS (the co-located `atpkg` stat: cheap
    /// `current_exe`-sibling metadata, and the compiled root key) before any
    /// status read, so the package rows' admission is drawn from what is known
    /// on the first frame — never "not read yet" while an unrelated read runs.
    /// Returns the stat for the worker that follows.
    pub(crate) fn note_native_package_tool(&mut self) -> bool {
        let available = crate::co_located_atpkg().is_some();
        self.native_packages_service.note_tool(available);
        available
    }

    /// Start one status-collection worker (no verb). A running worker makes
    /// this a no-op join; headless hosts (no proxy) skip silently.
    pub(crate) fn start_native_packages_refresh(&mut self) {
        // The tool's facts first, whatever happens to the read: a host with no
        // proxy, or a refresh joining one already running, still admits on them.
        let available = self.note_native_package_tool();
        let Some(proxy) = self.proxy.clone() else {
            self.publish_native_packages_state();
            return;
        };
        let Some(sequence) = self.native_packages_service.begin(None) else {
            self.publish_native_packages_state();
            return;
        };
        // The status.toml parse stays on the worker.
        let spawn = crate::qos::spawn_lane_thread(
            "aterm-packages-status",
            crate::qos::Role::Background,
            move || {
                // A package inventory probe is cosmetic to the terminal; it lands
                // via the proxy when done.
                let report = crate::packages_screen::collect_packages_status(available);
                let completion = PackagesWorkerCompletion::refresh(report);
                let _ = proxy.send_event(Wake::NativePackagesFinished {
                    sequence,
                    completion,
                });
            },
        );
        if spawn.is_err() {
            // Release the reservation via abort, never a fabricated report: a
            // never-observed surface stays on "Reading package status…", and an
            // observed one keeps its facts.
            let _ = self.native_packages_service.abort(sequence);
        }
        self.publish_native_packages_state();
    }

    /// Start one machine read (bare `atpkg machine`, no verb, no store lock) so
    /// Security's This Mac status confirms the `[machine]` settings from
    /// what the co-located atpkg measured; the atpkg child is the one seam that
    /// touches the machine. A read requested while one runs is queued behind it
    /// (`PackagesService::request_machine_read`), never joined, since the running
    /// read may predate an apply. Headless hosts and non-macOS builds skip
    /// silently and draw no status (the This Mac group and its row stay).
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

    /// A queued rerun is already admitted by the completion reducer; dispatch
    /// it directly, since requesting admission again would read the reservation
    /// as a running worker and leave the status refreshing forever.
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
    /// of [`Self::finish_native_update_check`]); stale sequences are inert. A
    /// finished verb that ran atpkg's machine pass
    /// ([`PackagesBusy::applies_machine_settings`]) re-reads the machine so the
    /// status confirms rather than assumes, except `machine apply` when its
    /// completion carries the apply's own `machine-state:` record. The collected
    /// lanes (`update`, every `install`) post no record, so the completion's one
    /// read closes the expectation their change line opened.
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

    /// Publish the Settings ▸ Messages projection to the Settings controller and
    /// fan the revision out to every Settings view. The projection is built once
    /// from the center and the log (`App::messages_state`). Unconditional: the
    /// caller opening Settings wants it current now; the gated twin
    /// [`Self::publish_native_messages_state_if_due`] serves the sync and the
    /// park. A no-op with no Settings instance.
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
        self.dispatch_to_settings_views(&AppEvent::MessagesChanged { revision });
    }

    /// [`Self::publish_native_messages_state`] when it is due: the center moved
    /// and at least [`MESSAGES_PUBLISH_MIN_GAP`] has passed, or
    /// [`MESSAGES_PUBLISH_TICK`] has passed while a Settings view is on the
    /// Messages route (sooner while its status line is transient:
    /// [`Self::settings_messages_quiet_gap`]). Runs after every settle and before
    /// every wait; a publish the gap holds back is re-armed through
    /// `messages_deadline`. One enum compare per window off the route.
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

    /// The instant the next publish falls due, for the loop's deadline fold; the
    /// twin of [`Self::publish_native_messages_state_if_due`]'s predicate: the
    /// 2 Hz hold-back of a publish the center's move earned, or, with the center
    /// quiet, the [`MESSAGES_PUBLISH_TICK`] (or a transient status line's
    /// publishes, [`Self::settings_messages_quiet_gap`]) while a view is on the
    /// Messages route. The loop waits when idle, so an unarmed tick never
    /// fires. `None` with no Settings instance, or with the center quiet and no
    /// view on the route, so an idle band arms no wake.
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

    /// The publish an on-screen Settings view asks for with the center quiet
    /// (on screen: a pane of the active tab of a window whose content is on
    /// glass, `band_on_screen`): on the Messages route the
    /// [`MESSAGES_PUBLISH_TICK`], so relative times move; and on any page, while
    /// its status line is transient (`Copied`, `Build information copied`), the
    /// publish that starts its clock and the one [`MESSAGES_FEEDBACK_WAIT`]
    /// later that clears it (xa-13). The shortest over all such views. A background tab or hidden window arms nothing; once
    /// revealed, what it owes is overdue and publishes at once.
    fn settings_messages_quiet_gap(&self) -> Option<std::time::Duration> {
        let mut gap: Option<std::time::Duration> = None;
        // A headless instance has no glass, but what its active tab shows is
        // what a capture of it shows — the lane UI is validated in — so its
        // status lines run the same clock as a window's (xa-13).
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
            // Runs before parking and while selecting the next deadline, so visit
            // the tree directly instead of allocating its leaves.
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
                    // A transient status line's publishes, on every page
                    // (xa-13); the minute tick on Messages, whose times move.
                    let clock = match state.status_clock() {
                        Some(false) => Some(MESSAGES_PUBLISH_MIN_GAP),
                        Some(true) => Some(MESSAGES_FEEDBACK_WAIT),
                        None => None,
                    };
                    let tick = (state.route == crate::native_settings::SettingsRoute::Messages)
                        .then_some(MESSAGES_PUBLISH_TICK);
                    for next in [clock, tick].into_iter().flatten() {
                        gap = Some(gap.map_or(next, |gap| gap.min(next)));
                    }
                }
            });
        }
        gap
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
            .with_machine_config(self.config.machine_universal_control())
            .with_retired_switch_note(self.config.packages_retired_switch_note());
        if !self
            .native_runtime
            .replace_settings_packages(state, revision)
        {
            return;
        }
        self.dispatch_to_settings_views(&AppEvent::PackagesChanged { revision });
    }

    /// Publish the Settings ▸ Harness projection (`App::harness_state`) to the
    /// Settings controller and fan the revision out to every Settings view (the
    /// packages twin). A no-op with no Settings instance: nothing is built.
    pub(crate) fn publish_native_harness_state(&mut self) {
        if self
            .native_runtime
            .instance_by_kind(crate::native_app::AppKind::Settings)
            .is_none()
        {
            return;
        }
        // When the Timer's `at` offer turns its words, noted before they are
        // read (`App::note_land_at_offer`).
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

    /// Deliver `event` to every Settings view and repaint every window that shows one — the
    /// fan-out after a shared Settings projection (packages, the log) was replaced.
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

    /// Deliver `event` to every live Settings view in view-store order and
    /// return those views; every shared Settings projection (config, messages,
    /// packages, the updater) is published through here.
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
            // A shared projection can complete a view's own pending work —
            // Settings ▸ Themes selects a saved theme once the published
            // catalog lists it — so its effects run as any reducer's do.
            // Dropped, that select's config patch never ran and stayed
            // pending: the page said "Selected" and nothing was written.
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
                        // A network update probe; nobody at the keyboard waits on
                        // it.
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

    /// Main-thread half of the updater worker protocol. Completion is
    /// service-owned, stays valid after every Settings view closes, and
    /// publishes one revision to all subscribers.
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

        // Facts parked while this check ran were observed before it staged
        // anything; replaying them would retire the stage it just imported. Keep
        // the purpose (a control apply must not be lost) and re-observe the disk.
        if let Some((purpose, _stale)) = self.deferred_native_update_reconcile.take()
            && !self.request_native_update_reconcile(purpose)
        {
            if purpose == NativeUpdateReconcilePurpose::ApplyControl {
                // A control apply never vanishes silently: the same refusal the control
                // entry path surfaces when facts cannot be collected.
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
            // This check may have confirmed the stage's signer: a lane held for
            // that asks the stage again (round eight, #503's review).
            #[cfg(unix)]
            self.roster_wait_check_reported(build);
            self.arm_native_auto_apply(build, &digest);
        }
        self.try_pending_native_auto_apply(true);
    }

    /// THE AUTOMATIC LANE'S DEADLINES ARE CALENDAR TIME (2026-09-30). Each is a
    /// promise to the owner: `update status` names the wall-clock time of the
    /// latch's next retry, stand-down end or re-sample, the convergence notice
    /// says "in 24 h" and "every 6 h", and a refused retire "tries again in N
    /// min". Each is an `Instant`, and on macOS that is `CLOCK_UPTIME_RAW`, which
    /// stops while the Mac sleeps: "24 h" meant a day AWAKE, several on a laptop
    /// that sleeps at night.
    ///
    /// So every turn of the event loop brings them forward by the time the Mac
    /// slept since the turn before: how far the wall clock (`wall`) ran ahead of
    /// the monotonic one (`now`) since the two were last read
    /// (`App::update_deadline_clock`). A wall clock stepped back counts nothing,
    /// so no deadline ever comes later than it would have; none is brought past
    /// `now`; and the drift of two clock reads never counts
    /// ([`SLEEP_COUNTS_FROM`]). A wall clock stepped FORWARD reads as a sleep and
    /// can bring a retry sooner, the trade the harness host's wall-clock look
    /// makes too; so can a sleep that lands mid-turn, after this read, on a
    /// deadline that turn then mints. What a deadline does once due is
    /// unchanged, and the physical budget's replenish window stays awake time,
    /// which can only remember the bytes longer.
    pub(crate) fn count_sleep_toward_update_deadlines(
        &mut self,
        now: std::time::Instant,
        wall: std::time::SystemTime,
    ) {
        let Some((then, then_wall)) = self.update_deadline_clock.replace((now, wall)) else {
            return;
        };
        let slept = wall
            .duration_since(then_wall)
            .unwrap_or_default()
            .saturating_sub(now.saturating_duration_since(then));
        if slept < SLEEP_COUNTS_FROM {
            return;
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
    }

    /// Retain automatic apply intent for one exact (or superseding) staged build.
    /// Returns true only when this call armed a new intent.
    /// Let a manual-only latch lapse once its deadline passes, restoring
    /// automatic apply for that artifact.
    ///
    /// Only the physical-failure schedule writes a latch: a retry inside its
    /// epoch, a stand-down between epochs ([`PHYSICAL_FAILURE_EPOCH_COOLDOWN`]),
    /// a re-sample at convergence, and `None` once a structural verdict's one
    /// re-sample is spent. A `retry_at: None` latch is also the fail-safe for an
    /// unreachable policy/outcome mismatch; it never lapses.
    ///
    /// A structural verdict's re-sample deadline is one look at
    /// [`Self::look_at_structural_latch`], which releases the latch only when one
    /// more launch cannot be the one its boot trial reverts on.
    ///
    /// Returns true when a latch was released.
    pub(crate) fn lapse_expired_auto_apply_manual_only(&mut self) -> bool {
        let now = std::time::Instant::now();
        let lapsed = self
            .auto_apply_manual_only
            .is_some_and(|manual| manual.retry_at.is_some_and(|at| now >= at));
        let verdict = self
            .auto_apply_manual_only
            .and_then(|manual| self.structural_verdict_of(manual));
        // A refused release's retry coming due is a look too, whatever the
        // latch's own deadline (folded separately, `structural_supersede_retry_at`).
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
            // `auto_apply_ladder` is not cleared: the landing bound is per
            // artifact and counts from the first arming, and the latch's spacing
            // is all the quiet-moment preference a physical retry gets. A lapse
            // at `Land` lands at the next poll.
            // `auto_apply_physical_retry` is not cleared either: it is the
            // remaining physical budget, and clearing it on every lapse would
            // turn a bounded retry into a permanent loop. Its own replenish
            // window forgives an artifact.
        }
        lapsed
    }

    /// The structural verdict a latch rides on, when it has one. A latch without
    /// one (a transient schedule's, the policy fail-safe's) is not the
    /// structural latch's business, nor is the environment block even where a
    /// verdict covers its build: it waits on the installed copy changing.
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

    /// When a refused release behind the structural latch may be offered the
    /// install path again, folded beside the latch's own deadline so an idle
    /// machine still retries. `None` with no latch, verdict or refused release,
    /// or while a retire runs (it answers through `finish_activation_supersede`).
    pub(crate) fn structural_supersede_retry_at(&self) -> Option<std::time::Instant> {
        if self.native_activation_supersede.is_some() {
            return None;
        }
        let latch = self.auto_apply_manual_only?;
        self.structural_verdict_of(latch)?
            .newer_retry
            .map(|retry| retry.at)
    }

    /// Whether one more automatic launch of the latched build keeps its boot
    /// trial under [`BOOT_TRIAL_LAUNCH_LIMIT`], from the sentinel's count the
    /// last updater observation measured; `None` when none has yet.
    ///
    /// A DOWNLOAD's latched build has no launch counted yet: it is not
    /// installed, and the swap that installs it arms its trial from zero
    /// (`prepare_trial`). That does NOT make its candidate safe from being a
    /// reverting launch. The candidate's first image is the RUNNING build, whose
    /// own trial `check_boot_health` may count before any swap. A candidate
    /// that stays that build because its boot apply had nothing it may do now,
    /// or deferred on a passing refusal, refuses its target as no verdict on
    /// it — exit 75 — and gives that launch back on its way out (round eight,
    /// #75). One whose boot apply DEFERRED ON A VERDICT (the running build's
    /// own trial whose recovery proof fails, "trial recovery proof: …") does
    /// neither: `seamless::passing_refusal_exit` answers no 75, and the parent
    /// gives back only the TARGET's launch, under a build no trial is armed
    /// for. That launch stays counted on the running build's trial, so such
    /// attempts can spend its budget, and the launch that exhausts it is the
    /// one `check_boot_health` reverts on (or disarms, with no verified
    /// predecessor) — a download's candidate among them.
    /// The installed ACTIVATION is where the count matters: a structural
    /// `ChildDied` keeps its counted launch, so after two of them the next
    /// launch of the bundle is the one `check_boot_health` reverts on.
    fn structural_latch_trial_room(&self, latch: crate::AutoApplyManualOnly) -> Option<bool> {
        if !latch.activation {
            // The latched build's own trial has counted nothing (above). This is
            // no answer for the RUNNING build's trial, which a deferred-on-a-
            // verdict candidate can spend: that count is not this latch's, and
            // the lane does not read it here.
            return Some(true);
        }
        self.native_installed_trial
            .filter(|(build, _)| *build == latch.build)
            .map(|(_, launches)| launches.saturating_add(1) < BOOT_TRIAL_LAUNCH_LIMIT)
    }

    /// Whether `build` is a latched installed ACTIVATION with no deadline left
    /// whose boot trial MEASURED no launch to spare: its next start — the
    /// automatic lane's, a person's, a fresh launch — is the one
    /// `check_boot_health` reverts on, since no press re-arms an activation's
    /// trial (only a swap does, `prepare_trial`). Only a newer release moves
    /// it, so no surface asks for the press: the band row has no `Install now`
    /// (`ApplyPosture::TrialSpent`); the Version menu, its palette twin and
    /// Settings read "on hold" from this one fact, whether or not the ledger
    /// still names the failures. The menu's press opens the details
    /// (`apply_update_or_details`); Settings' button and a control `update
    /// apply` start nothing and say it is on hold (`apply_native_update`,
    /// `update_words::trial_spent_refusal`). An unmeasured count is not spent.
    pub(crate) fn latched_activation_trial_spent(&self, build: u64) -> bool {
        self.auto_apply_manual_only
            .filter(|latch| latch.build == build && latch.activation && latch.retry_at.is_none())
            .is_some_and(|latch| self.structural_latch_trial_room(latch) == Some(false))
    }

    /// An apply attempt returned, so any count the lane held predates its
    /// launch: the attempt's launch was counted in the candidate, and whether
    /// it stays counted is decided on the handoff worker after this completion
    /// (`forgives_the_counted_trial_launch`). The lane holds no count until an
    /// observation read after now measures one; facts read mid-attempt measure
    /// nothing. Called on every returned attempt, a person's included.
    #[cfg(any(unix, test))]
    fn forget_installed_trial_count(&mut self) {
        self.native_installed_trial = None;
        self.native_installed_trial_stale_before = Some(std::time::Instant::now());
    }

    /// One look at a structural latch: ask
    /// [`crate::native_update_auto_intent::structural_latch`] what a verified
    /// download strictly newer than the latched build (`newest_download`), or
    /// the latch's re-sample coming due, has earned, and act on it. Called when
    /// the re-sample deadline passes (`lapse_expired_auto_apply_manual_only`) and
    /// on every reduced updater observation (`reconcile_native_update_facts`).
    ///
    /// A newer release counts only against the installed activation (on the
    /// download side it is imported as the stage and arms on its own), and there
    /// it supersedes the activation ([`Self::request_activation_supersede`]). The
    /// verdict remembers which release was offered, so none asks twice.
    ///
    /// Returns true when it released the latch.
    ///
    /// Anchored as the model's `Decide`: the bound on an unreadable count (the
    /// model's `looks` / `UnreadLooks`) is this host's, bound by
    /// `a_latch_whose_activation_leaves_the_installed_facts_is_decided_within_a_bound`.
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
        // A refused release's retry is about that release on disk: once the disk
        // no longer holds it, the retry is moot and whatever is there is offered.
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
        // Any PROVEN read ends a run of unproven ones (#80's review).
        if !self.native_installed_trial_unproven && verdict.unproven_looks != 0 {
            verdict.unproven_looks = 0;
            verdict.unproven_through = 0;
            self.auto_apply_structural_verdict = Some(verdict);
        }
        match structural_latch(facts) {
            StructuralLatchDecision::Hold => {
                // A look its own deadline brought (the re-sample, now outranked
                // by a release waiting out a refusal) must move that deadline or
                // the deadline fold wakes on it forever; the release's retry is
                // the next thing that can change anything.
                if facts.resample_due
                    && latch.retry_at.is_some_and(|at| now >= at)
                    && let Some(retry) = verdict.newer_retry
                {
                    self.auto_apply_manual_only = Some(crate::AutoApplyManualOnly {
                        retry_at: Some(retry.at),
                        ..latch
                    });
                }
                // The unmeasured hold named the Version menu; a count since read
                // as spent makes that start the reverting one, so say the hold
                // again in the trial's words (same notice key, new words).
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
                // Nothing is spent; the next observation re-measures. A latch past
                // its deadline must move it or the deadline fold spins; that move
                // is said, a reconcile's repeat look is not. The lapse asks for an
                // observation rather than waiting on the check's cadence, and every
                // returned attempt leaves the count unmeasured on purpose
                // (`forget_installed_trial_count`).
                //
                // BOUNDED (round six, finding 55): a count asked for
                // `STRUCTURAL_TRIAL_UNMEASURED_LOOKS` times in a row and never
                // measured is not coming, and the re-sample is held and said
                // rather than promised every 75 s forever. Each counted look,
                // and the decision at the bound, reads an observation read
                // since the last counted look: a look the facts worker had
                // nothing new for (saturated, or no observation could be asked
                // for) is no evidence the count is gone, and spends none of
                // the bound.
                //
                // NOR IS AN OBSERVATION THAT PROVED NOTHING (round eight, #80). The
                // facts worker fails closed on every verification error, so on a
                // captive-portal network (`spctl` past its 30 s ceiling) each read
                // finds the bundle and reports none. The bound is for facts that
                // stopped NAMING the activation (a floor above it yanked it); counted
                // over reads that could not CHECK it, three looks dropped the day's
                // re-sample for good and the recovered network released nothing — a
                // passing condition made a standing verdict (L4). Such a look counts on
                // a bound of its own, and the first proven read decides.
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
                // The hold spends the re-sample, due or not: every launch left is
                // the reverting one, so later looks would repeat themselves. Only a
                // newer release can supersede now, and the latch loses its
                // deadline to say so. A refused release keeps its own retry
                // deadline (`AutoApplyStructuralVerdict::newer_retry`); a latch
                // deadline with no re-sample behind it would lapse and release the
                // build for its reverting launch.
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

    /// A re-sample the structural latch earned and will not take, or promised
    /// and withdrew at the first count that rules it out, said in the log,
    /// `update status`'s `apply_refusal=`, and the convergence notice again.
    /// Never silent.
    fn say_structural_hold_for_trial(&mut self, build: u64) {
        // Named as the Version menu's row names it: the stage's version, or
        // its build number where no stage of it is in hand.
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

    /// A due look at a structural latch whose latest facts read could not PROVE
    /// the installed bundle (round eight, #80's review; the model's `Decide` over
    /// `unread = 2`). Each due look over a read newer than the last counted one
    /// counts ([`crate::AutoApplyStructuralVerdict::unproven_looks`]), moves the
    /// deadline [`STRUCTURAL_TRIAL_REMEASURE`] and asks for an observation, as a
    /// look with no count does. At [`STRUCTURAL_TRIAL_UNPROVEN_LOOKS`] the day's
    /// re-sample MOVES TO THE NEXT DAY rather than being promised every 75 s while
    /// a network filter times `spctl` out for good (L5) — and is not dropped, which
    /// was #80's own defect (L4). The move is said in the log and on `update
    /// status`; it asks nothing of a person, so nothing is raised.
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

    /// A structural latch's hold in `reason`'s words, said where
    /// [`Self::say_structural_hold_for_trial`] says its own.
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
        // The staged row's posture is now "a person moves it": restated where it
        // is up, raised as the decision where it is not.
        self.restate_staged_bar_posture(build);
    }

    /// Start retiring a latched activation for a newer release: hand
    /// [`aterm_update::supersede_installed_activation`] to a worker (it verifies
    /// three bundles and holds the apply lock) and mark the retire in flight.
    /// The latch stays until the worker answers
    /// ([`Self::finish_activation_supersede`]); a refusal leaves everything as it
    /// was.
    ///
    /// Not another attempt at the activation: the boot apply refuses to swap
    /// past the handoff's authorized target (`aterm_update::apply_staged_if_ready`),
    /// so that path costs another counted launch of the failing build. The retire
    /// launches nothing: the running build takes the install path back, the
    /// activation's trial is disarmed, and the newer build becomes an ordinary
    /// stage that the following reconcile arms.
    ///
    /// `false` when it cannot start now (an apply is in flight, or the stage in
    /// hand is not this activation); the caller spends nothing. `spent_before`
    /// is the verdict's current `newer_spent`, given back on refusal.
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

    /// Run [`aterm_update::supersede_installed_activation`] on its own thread,
    /// then read the disk through the facts worker's FIFO (ordered after the
    /// retire) and post both to the event loop. `false` when no thread or event
    /// loop could be had.
    ///
    /// QoS `Background`, never `Housekeeping`: it holds the machine's apply lock
    /// across codesign reads, and a cold aterm launch elsewhere waits on that
    /// lock; a lock holder must not be in the starvable class.
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

    /// Unit tests never run the retire (it would act on this machine's install):
    /// the job is recorded in flight and the test answers it with
    /// [`Self::finish_activation_supersede`].
    #[cfg(test)]
    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    fn spawn_activation_supersede(&mut self, _job: ActivationSupersedeJob) -> bool {
        true
    }

    /// The retire answered. On success the observation read after the retire is
    /// reduced as a new stage: it sees the running build installed, retires the
    /// activation stage (and, in the reconcile's `Retired` arm, the latch keyed
    /// on it), imports the newer download and arms it as an ordinary stage.
    ///
    /// The latch is not cleared here: only an observation of the new disk may
    /// release it. An older observation still in flight names the activation
    /// installed, and without the latch it would arm the activation against an
    /// install path that now holds the running build.
    ///
    /// A refusal leaves the latch as it stood, says so, and gives the release
    /// back: it is offered again after a retry deadline that doubles per
    /// refusal ([`supersede_retry_after`]). What is said follows the refusal's
    /// kind ([`Self::say_activation_supersede_refused`]).
    ///
    /// A retire whose disarm was deferred (`trial_disarm_deferred`) is still a
    /// retire (the next boot apply finishes the crash cut); it is logged as
    /// such, never said as a refusal.
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

    /// A refused retire gives its release back: the verdict's `newer_spent`
    /// returns to its value before this retire, and the release waits out a
    /// backoff doubling per consecutive refusal ([`supersede_retry_after`]).
    /// Returns that wait, or `None` when the verdict no longer names this retire
    /// (its latch is gone or another release was offered since).
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

    /// A newer release that could not yet take the latched activation's place.
    /// Every refusal reaches the log and `update status`'s `apply_refusal=`,
    /// with when the lane tries again. Only a refusal about the disk's shape is
    /// a convergence notice naming the Version menu (retries meet the same disk);
    /// a refusal about the moment (the apply lock, a verification that failed or
    /// ran out of budget, a stage being replaced) puts nothing on glass, since
    /// the lane retries by itself.
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

    /// Authenticate the staged candidate off the GUI thread and outside the
    /// parked window, caching the verdict for the next handoff attempt.
    ///
    /// Idempotent and cheap: a fresh verdict for the same `(build, commit)`
    /// short-circuits, as does a read of that key still in flight
    /// ([`preverify_read_needed`]). A failure is cached too, so a doomed
    /// candidate fails preparation before parking any reader.
    #[cfg(unix)]
    pub(crate) fn spawn_staged_handoff_preverification(&mut self, build: u64) {
        let snapshot = self.native_updater_service.snapshot();
        let current_build = snapshot.current_build;
        let stage = snapshot
            .staged
            .as_ref()
            .filter(|staged| staged.build == build);
        // An activation verifies the bundle under the executable, not a staged `.app`.
        let installed_activation = stage.is_some_and(|staged| staged.is_installed_activation());
        let commit = stage.and_then(|staged| staged.commit.clone());
        let artifact = stage.map(|staged| staged.dmg_sha256.clone());
        // Without a pinned commit the worker's own call would be a different
        // (weaker) query, so do not cache a verdict that would not match it.
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
                // The read signals its end on every path (a verdict published, an
                // unfinished check, a panic), only after its verdict is in the
                // slot, so a waiter that sees the flag sees the verdict.
                let _ends = RaiseOnDrop(done);
                // No `qos::set_self` here: this worker keeps its inherited class.
                // It publishes into `handoff_preverified`, which the UI thread
                // locks with a blocking `lock()`, so qos.rs forbids demoting it
                // below `Responsive` (priority inversion). Promoting it buys
                // nothing: nothing waits on it, and a miss re-verifies on the
                // handoff worker. The default class fits.
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
                    // A CHECK THAT DID NOT FINISH IS NOT A VERDICT, and is not
                    // cached as one (round four, plan item 2): a cached refusal
                    // short-circuits every automatic attempt for ten minutes and
                    // is read back as the verifier's word on the bytes. Leaving
                    // the slot alone makes the next attempt verify again, on the
                    // handoff worker, where the same moment is filed transient.
                    //
                    // BUT THE MOMENT IS KEPT, ON THIS READ (round eight, S1's
                    // second review): the cold lane has no handoff worker to
                    // measure it again — its successor is this build, whose boot
                    // swap waits on the same lock — so it reads this answer before
                    // it relaunches (`PreverifyInFlight::ended_passing`), and a
                    // later read replaces it.
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
                // The successor's handoff policy, read only because the bundle
                // passed; a refusal carries none.
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
            // No thread: the worker simply verifies in-line as it always did.
            Err(_) => aterm_log::warn!("update apply: pre-park verification thread unavailable"),
        }
    }

    /// Re-arm automatic apply for whatever is still staged after a latch
    /// lapsed. Reads the service snapshot only (no disk or network); a no-op
    /// when nothing is staged.
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
    /// work: the one blocker a person can clear, so it is also the update row's
    /// words ([`Self::update_blocker_for_person`]). The install is already in
    /// place, so it never says "before relaunching".
    pub(crate) const UNSAVED_NATIVE_WORK_BLOCKS_APPLY: &'static str =
        "Save or close the open editor to finish updating";
    /// The update row's words when only unsaved Settings text holds the
    /// install ([`SETTINGS_DRAFTS_BLOCK`]): no editor need be open.
    pub(crate) const UNSAVED_SETTINGS_BLOCKS_APPLY: &'static str =
        "Save or close the unsaved Settings tabs to finish updating";
    /// The update row's words when an editor and Settings text both hold it.
    pub(crate) const UNSAVED_EDITOR_AND_SETTINGS_BLOCK_APPLY: &'static str =
        "Save or close the open editor and unsaved Settings tabs to finish updating";
    /// The close-preflight blocker while session restore / adoption is in flight
    /// — it clears by itself.
    pub(crate) const RESTORE_IN_FLIGHT_BLOCKS_APPLY: &'static str =
        "Updating after session restore finishes";
    /// The `InstalledNeedsRelaunch` outcome's sentence: the bundle on disk is
    /// already the newer build, and switching to it has its own admission and
    /// policy, so the sentence promises no timing. Matches
    /// `native_updater_service`'s `INSTALLED_SWITCHES_IN_PLACE` word for word.
    #[cfg(any(unix, test))]
    pub(crate) const INSTALLED_ACTIVATES_IN_PLACE: &'static str =
        "The update is installed; switching to it keeps your shells running";

    /// Whether a staged build is expected to land by itself, which the updater's
    /// overdue notice ([`aterm_update::set_automatic_apply`]) claims failed when
    /// a newer build has waited over an hour. `[update] auto_apply` on, and
    /// neither posture where waiting is the design: an in-session handoff this
    /// process cannot run
    /// ([`crate::update_words::ApplyPosture::HandoffDisabled`]), or unsaved work
    /// a person has to save ([`Self::update_blocker_for_person`]), whose own row
    /// STANDS while it holds the lane ([`crate::update_words::waits_for_you`];
    /// round eight, lead U38: it was a 45-s warning, and after it folded nothing
    /// said why the update never landed), or a process that keeps its image with
    /// no terminal open, whose only lane waits for one
    /// ([`Self::kept_image_waits_for_terminal`]).
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
        auto_apply_on
            && handoff_unavailable.is_none()
            && self.update_waits_on_person.is_none()
            && !self.kept_image_waits_for_terminal()
    }

    /// The update row for a close-preflight refusal the person can clear
    /// (unsaved editor or Settings work, a failed document checkpoint), or
    /// `None` when every reason clears by itself (a running checkpoint, a
    /// restore landing). The reasons are this file's own
    /// (`native_update_close_preflight`,
    /// [`Self::UNSAVED_NATIVE_WORK_BLOCKS_APPLY`]).
    /// Named by the work that holds it: an editor's, Settings text, or both.
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

    /// `true` while the `ATERM_DEBUG_RELAUNCH_NUDGE` screenshot seam suppresses
    /// the automatic update lane, logging it once per process the first time
    /// the arm or the poll asks. A development seam (`aterm_types::dev_seam!`);
    /// a shipped binary never reads it.
    ///
    /// The seam seeds a fake `Wake::UpdateStaged` so the update-ready nudge can
    /// be captured with `ctl image`, and a fake nudge must not install anything.
    /// The test at the bottom of this file asserts the variable is absent, or its
    /// polls would all answer Clear and prove nothing.
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

    /// Start the automatic-apply ladder for `build` at `now` unless one already
    /// anchors that build: the ladder starts once per build, so a re-arm can only
    /// delay the landing, never restart the clock.
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
        // Which side of the swap the incoming artifact is on: every caller arms
        // the reducer's own stage, which says whether these bytes are the
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
        let current_build = self.native_updater_service.snapshot().current_build;
        let decision = crate::native_update_auto_intent::arm(ArmFacts {
            enabled,
            current_build,
            armed_build: armed.map(|intent| intent.build),
            armed_exact: armed
                .is_some_and(|intent| intent.build == build && intent.dmg_sha256 == dmg_sha256),
            // The latch holds the update, not one side of its swap: the same build
            // as a download or as its installed activation is one logical update,
            // folded as the budget behind the latch folds it, so the candidate's
            // own swap cannot re-arm the confirming retry early.
            manual_only_exact: manual_only
                .is_some_and(|manual| manual.covers(build, dmg_sha256, incoming_activation)),
            manual_only_build: manual_only.map(|manual| manual.build),
            incoming_build: build,
        });
        // Authenticate the staged bundle now, while every reader is live and a
        // cancel costs nothing, not inside the parked window — and read the
        // handoff policy it seals. Only the unix seamless handoff consumes the
        // verdict. Whatever the automatic lane decides (round eight of the update
        // audit, U35): a stage a PERSON installs (`[update] auto_apply = false`,
        // or a build latched manual-only) parks on the fork lane before its
        // worker verifies, so its policy is read here too, never first after the
        // capture it forbids.
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
                // The status bar's Staged line was painted from policy alone; this
                // build's own latch is the finer fact — say so while the bar is up.
                self.restate_staged_bar_posture(build);
                false
            }
            ArmDecision::Keep => false,
            ArmDecision::Set(build) => {
                self.auto_apply_environment_block = None;
                // A hold for other bytes waited on a stage that is gone.
                #[cfg(unix)]
                if self
                    .auto_apply_roster_wait
                    .as_ref()
                    .is_some_and(|wait| !wait.covers(build, dmg_sha256))
                {
                    self.auto_apply_roster_wait = None;
                }
                // A different artifact at the same build, or a newer build, owns a
                // distinct budget. Older wakes were suppressed by `arm` above.
                self.auto_apply_manual_only = None;
                let now = std::time::Instant::now();
                // The ladder starts once per build. A re-arm of the same build (a
                // duplicate stage wake, a superseded intent for the same bytes, a
                // lapsing physical latch) keeps the anchor, so it can only delay
                // the landing, never restart the clock.
                let ladder = self
                    .auto_apply_ladder
                    .filter(|ladder| ladder.build == build);
                self.anchor_auto_apply_ladder(build, now);
                // Logged at the default level so an operator reading aterm.log can
                // tell an armed-and-waiting lane from a dead one. A re-arm on a
                // retained anchor says where the ladder stands rather than
                // promising the bound again.
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
                // The status bar's "applies in place" line is now an armed fact
                // (or, without the handoff, the line saying nothing applies while a
                // terminal is open); restate it while the bar is up.
                self.restate_staged_bar_posture(build);
                true
            }
        }
    }

    /// Where the automatic lane stands on its ladder at `now` for its build
    /// (`auto_apply_ladder`). With no anchor yet, it is at the first phase.
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
    /// anchored phase, raised to the mode's own promise. `AutomaticPastGrace`
    /// never asks for a machine-wide idle moment, so it is at least the
    /// output-gap phase even with no anchor; `Automatic` is the anchored phase.
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

    /// The terminal facts the ladder reads, sampled at `now`. The keystroke
    /// gate and the two bounded holds (warm-up, and a cold restore's agents
    /// still relaunching) apply only to an automatic lane; an explicit apply
    /// parks at once and the successor carries any restored agents still owed.
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

    /// Log once, at the default level, that the lane moved to `phase` and what
    /// it now waits for; per-poll deferrals are not logged.
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

    /// Log, once each way, that the restored agents' hold began or ended while
    /// the automatic lane waits on this ladder's build
    /// ([`crate::native_update_auto_intent::restored_hold_line`]). It is the one
    /// hold that also refuses the `Land` phase.
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

    /// Whether a person's Install is held for the candidate's pre-park read
    /// ([`Self::run_held_person_apply`]): work, to the automatic lane, which would
    /// otherwise retire the held press's row and close its record with its own
    /// deferral. Never off unix.
    #[must_use]
    pub(crate) fn person_apply_is_held(&self) -> bool {
        #[cfg(unix)]
        {
            self.person_apply_held.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// HOLD THE AUTOMATIC LANE FOR AN UPDATE CHECK TO CONFIRM THE STAGE'S SIGNER
    /// (round eight, #503's review; [`crate::AutoApplyRosterWait`]): `reason` is
    /// the stage's refusal ([`aterm_update::is_roster_confirmation_wait`]), and
    /// `answered_at` when it was answered. Returns the hold.
    ///
    /// `rearm` keeps the lane's intent for these bytes, due when the hold next
    /// asks the stage itself: an attempt that met the wait gave its intent up on
    /// the way in, and a deferral is not a failure that ends it. A person's press
    /// that met it re-arms nothing (the lane may be off, or latched for these bytes
    /// by its own budget), but an intent already armed holds all the same.
    ///
    /// Said once per hold, at the default level, so `aterm.log` tells a lane
    /// waiting on a check from a dead one. `book` puts a NEW hold's reason in the
    /// ledger as its standing refusal (what `update status` and the overdue notice
    /// name), for the callers whose outcome books nothing itself; an attempt's
    /// deferral is booked by its surfacing, and a launch's by its boot swap. The
    /// check that confirms the stage withdraws it. A provisional hold
    /// ([`Self::hold_provisionally_for_a_fresh_read`]) has said nothing yet, so the
    /// answer it was waiting for is said and booked as new.
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

    /// Whether an update check REPORTED the stage at `build` at or after `since`
    /// ([`Self::roster_wait_check_reported`]): any answer that the stage waits for
    /// a check, read from `since` on, may predate that check's confirmation and is
    /// stale (round eight, #503's second review).
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

    /// A STALE ANSWER IS ASKED AGAIN, NEVER HELD ON (round eight, #503's second
    /// review). A pre-park read of the stage ended waiting for a check, but began
    /// before a check reported the stage — which may have confirmed it, and whose
    /// `UpdateStaged` wake came before any hold existed to hear it, or while the
    /// hold's own re-read ran. Held on, that read made the lane wait a whole
    /// [`crate::ROSTER_WAIT_REREAD`] after the stage was confirmed, and booked
    /// again the wait the check had just withdrawn.
    ///
    /// So a fresh read is started, and the attempt held PROVISIONALLY while it
    /// runs (`said` false: nothing is said or booked, and a person's press does
    /// not take it for a wait): `Some(when)` to look again, every
    /// [`crate::ROSTER_WAIT_POLL`]. `None` when no read could start (a fresh
    /// verdict stands): the hold ends, and the attempt answers for itself.
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

    /// End the roster-confirmation hold of these bytes, if there is one, saying so
    /// when it had said it was holding.
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

    /// The roster-confirmation hold at the automatic lane's poll, for the attempt
    /// at `build` with `dmg_sha256` ([`crate::AutoApplyRosterWait`]): `Some(when)`
    /// to keep the intent and look again then, `None` to attempt. Memory only,
    /// like the rest of the poll; the re-read it starts runs off this thread.
    ///
    /// * A pre-park read of these bytes that ENDED waiting for a check, after the
    ///   answer the hold already stands on, starts or renews the hold. At a launch
    ///   that met a stale stage, the read the arm started answers before the first
    ///   attempt, so that attempt (and its surge) is never made. A read that BEGAN
    ///   before a check last reported the stage is stale: it is read again at once
    ///   ([`Self::hold_provisionally_for_a_fresh_read`]), never held on or booked.
    /// * Holding, it waits while a re-read of the stage runs, and until the hold's
    ///   re-read is due (a check that reports the stage makes it due at once:
    ///   [`Self::roster_wait_check_reported`]).
    /// * Due, it starts a re-read and waits for its answer. One that answered
    ///   anything but the wait (a pass, another refusal), or none that could
    ///   start (a fresh cached verdict stands), ends the hold, and the attempt
    ///   answers for itself.
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

    /// Start a fresh pre-park read of the stage at `build` with `dmg_sha256`
    /// ([`Self::spawn_staged_handoff_preverification`]), and say whether one was
    /// started: a NEW read of these bytes is in the slot, whether it still runs or
    /// has already answered (a stage still waiting refuses before any `codesign`, so
    /// its read can end before this returns). `false` when none could start: a
    /// fresh verdict stands, or the stage is no longer this one.
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

    /// A check reported the stage at `build` (its `UpdateStaged` wake, or a
    /// person's check): it may have confirmed the stage's signer. Recorded whether
    /// or not anything holds now ([`Self::roster_check_reported_since`]), so an
    /// answer read before it is never taken for the stage's state: one that lands
    /// before a hold exists, or during the hold's own re-read, is read again rather
    /// than held on (round eight, #503's second review). A lane held for it
    /// ([`crate::AutoApplyRosterWait`]) asks the stage again at its next poll
    /// instead of at its re-read's deadline. A check that confirmed nothing
    /// (offline, a lock it did not get) costs one cheap re-read, which renews the
    /// hold. The intent is due now too, so the poll the reconcile runs looks.
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

    /// WHAT THIS PROCESS KNOWS ABOUT THE STAGE `attempt` TARGETS WAITING FOR A
    /// CHECK TO CONFIRM ITS SIGNER, and when that was answered (round eight, #503's
    /// second review): the wait's words, when a pre-park read of exactly these
    /// bytes ended with it no older than a verdict's freshness and began after the
    /// last check that reported the stage; or, failing that, when the lane's hold
    /// for these bytes stands on such an answer (a launch's boot swap, a worker's
    /// pre-park check). Memory only: what the cold lane reads before it relaunches
    /// for any mode ([`Self::cold_lane_verdict_gate`]), since the successor's boot
    /// swap would wait the same way.
    ///
    /// Not a verdict on the bytes, and so not subject to the rule that a person's
    /// apply is not answered by a cached verdict: what it says is how long ago a
    /// check last ran, and every check that has reported since retires it.
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
        if deadline_ready {
            // A capture refusal recorded beside a busy engine reads that session
            // at the lane's next look, whatever the poll then decides (round
            // eight of the update audit, U43 second review).
            self.capture_refusal_take_baseline();
        }
        let phase = self.automatic_apply_phase(now);
        self.announce_apply_phase(phase);
        self.announce_restored_hold(now);
        // A same-image handoff counts as work: a real stage arriving while the QA
        // seam's handoff is in flight answers `Wait(WorkActive)` (intent kept, no
        // budget spent) instead of failing as "already in flight". Real handoffs
        // already set `active`/`Applying`.
        let work_active = self.native_updater_service.snapshot().active.is_some()
            || self.update_handoff_in_flight()
            || self.person_apply_is_held();
        let applying = self.native_updater_service.snapshot().phase == UpdaterPhase::Applying;
        // Durable facts are collected by workers and reduced by their wakes. A
        // timer callback is memory-only: it never reads ledger files or launches
        // PlistBuddy on the UI loop.
        if deadline_ready && !work_active && !applying {
            let Some(authoritative_intent) = self.auto_apply_intent else {
                // Asynchronous reconciliation retired or consumed the stage. Never
                // resurrect the copied intent into another wake loop — and a wait
                // on a person ends with it (round eight, the review of U38).
                self.settle_update_wait_on_person();
                return;
            };
            intent = authoritative_intent;
        }
        // The reconciliation above is this wake's one filesystem observation;
        // from here on reduce the process-owned snapshot only
        // (`staged_update_ready()` would re-read the ledger on the event loop).
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
                // posture that now holds. A person's wait was a wait for this
                // lane, so it ends here too — its standing row and the overdue
                // notice's exemption (round eight, the review of lead U38).
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
                // Work/publication/activity are ordering facts, not attempts:
                // keep the intent through any number of bounded transitions and
                // spend no retry budget.
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
        // A standing capture refusal holds the attempt until the refused
        // desk changes (or the backstop passes), so no successor is
        // launched into the same refusal. Intent kept, nothing spent,
        // re-probed on the refusal lane's cadence.
        let activation = updater.staged.as_ref().is_some_and(|staged| {
            staged.build == attempt_build && staged.is_installed_activation()
        });
        if self.capture_refusal_holds(attempt_build, intent.dmg_sha256, activation, now) {
            intent.retry_at = self.capture_refusal_reprobe_at(now);
            self.auto_apply_intent = Some(intent);
            return;
        }
        // A stage waiting for an update check to confirm its signer holds the
        // attempt (round eight, #503's review; [`crate::AutoApplyRosterWait`]):
        // intent kept, nothing spent, looked at again when the hold says.
        #[cfg(unix)]
        if let Some(retry_at) = self.roster_wait_holds(attempt_build, intent.dmg_sha256, now) {
            intent.retry_at = retry_at;
            self.auto_apply_intent = Some(intent);
            return;
        }
        intent.build = attempt_build;
        self.auto_apply_intent = None;
        // The mode carries the phase to the handoff: `Automatic` is the first
        // phase (a quiet moment at entry and at park); `AutomaticPastGrace` is
        // every later phase, whose gates read the ladder. Activity revokes both
        // mid-flight (`note_update_handoff_activity` is mode-blind, and
        // `exact_activity` is a conjunct of Commit in every lane).
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
        // Whether a person holds the lane as of this attempt: unsaved editor or
        // Settings work is the one blocker the lane cannot outwait, and the
        // overdue notice must not call that a failure
        // (`App::update_lands_by_itself`). Every attempt past preflight updates it.
        self.update_waits_on_person = match &outcome {
            UpdateOutcome::Blocked { reasons } => {
                Self::update_blocker_for_person(reasons).map(|_| intent.build)
            }
            _ => None,
        };
        // …and its standing row leaves with it (round eight, lead U38).
        if self.update_waits_on_person.is_none() {
            self.withdraw_update_waits_for_you();
        }
        let disposition = crate::native_update_auto_intent::finish(match &outcome {
            UpdateOutcome::Accepted => AttemptResult::Accepted,
            #[cfg(any(unix, test))]
            UpdateOutcome::InstalledNeedsRelaunch { .. } => AttemptResult::InstalledNeedsRelaunch,
            UpdateOutcome::Blocked { .. } => AttemptResult::Blocked,
            // The intent is retained (the refusal lane re-arms it below), so to
            // the reducer this is the not-now family, never the latching one.
            #[cfg(any(unix, test))]
            UpdateOutcome::CaptureRefused { .. } => AttemptResult::Blocked,
            UpdateOutcome::Failed { .. } => AttemptResult::Failed,
            UpdateOutcome::Deferred { .. } => unreachable!("returned above"),
        });
        match (disposition, outcome) {
            (AttemptDisposition::Complete, UpdateOutcome::Accepted) => {
                // A successful replacement never returns; Accepted means the
                // successor is launching (or a joined request's owner owns
                // completion). The landing is logged at Commit
                // (`finish_update_handoff`).
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
                // Every blocked attempt records its reason in durable status,
                // timer retries included, so UI silence never hides why a verified
                // stage is not running. Polls that merely wait returned above.
                let blocked = UpdateOutcome::Blocked {
                    reasons: reasons.clone(),
                };
                self.record_apply_outcome_in_ledger(&blocked);
                // A preflight block is a fact about this moment, never evidence
                // against the artifact: a spent budget slows the lane down but
                // neither ends it nor becomes a recurring intrusion.
                //
                // So the intent is retained rather than latched, and `attempts`
                // counts for the life of that intent:
                //   * past the budget the rate is one attempt per
                //     `PREFLIGHT_BLOCK_COOLDOWN`; the 5 s/15 s probes are the
                //     first twenty seconds only;
                //   * `attempts` is a per-(build, artifact) counter no lapse can
                //     reset, so the status pill fires on the first exhaustion
                //     only. `arm` answers `Keep` for a duplicate stage wake, so
                //     only a different artifact re-arms at zero;
                //   * nothing is permanent: fixing the blocker lands the update on
                //     the next cooldown.
                // Probes use `ClosePreflightVisibility::Quiet`, so a blocked
                // native reducer never switches tabs or opens Close Recovery.
                //
                // The cooldown is safe only because every `Blocked` reaching it
                // is transient:
                //   * close-preflight blockers (unsaved Settings text, dirty or
                //     mid-checkpoint documents, a restore still landing) are the
                //     user's live state; a failed checkpoint
                //     (`DocumentPhase::Blocked`) costs one probe per cooldown
                //     until the user acts;
                //   * `NotStaged` is ordering: a stage may publish next reduce;
                //   * a native-close reducer error is reported as `Failed` and
                //     takes the strict arm below;
                //   * `Disabled` and missing source provenance are unreachable
                //     here: a disabled ledger clears `snapshot.staged`, and
                //     `staged_from_status` refuses a stage without a 40-hex commit.
                // Hard failures arrive as `UpdateOutcome::Failed` and keep their
                // strict, converging budget.
                //
                // The retained intent cannot spin: `poll` answers
                // `Wait(Deadline)` until `retry_at`, and `about_to_wait` folds that
                // instant into `WaitUntil` (`fold_auto_apply_deadline`), so an idle
                // terminal wakes once per cooldown. A superseded, retired or
                // consumed stage clears the intent through
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
                // from a monotone `attempts` fires this once per artifact:
                // only `arm` resets it, for different bytes or a newer build.
                let first_exhaustion =
                    cooling_down && intent.attempts == MAX_AUTOMATIC_UPDATE_CYCLES;
                self.auto_apply_intent = Some(intent);
                // The words may not outlive the promise: admission refusals route
                // through this lane synchronously, and the gate's posture may now
                // say the lane is off, so recompute it (a no-op for an ordinary
                // close-preflight block). Unsaved editor work holding it is the
                // outcome row `note_update_blockers` raises.
                self.restate_staged_bar_posture(intent.build);
                if intent.attempts == 1 {
                    // The first block of an intent is always logged (the pill stays
                    // gated on `announce`): a preflight blocker is the one thing an
                    // operator can act on.
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
                    // Logged at warn once per artifact and at debug on later
                    // cooldowns (one probe a minute). The UI says it once, and only
                    // when it is the person's to clear (an unsaved editor); a lane
                    // re-probing on its own posts no row.
                    if first_exhaustion {
                        aterm_log::warn!("{message}");
                        self.note_update_blockers(&reasons, true);
                    } else {
                        aterm_log::debug!("{message}");
                        // A WAIT THAT BEGAN PAST THE BUDGET says itself too (round
                        // eight, the review of lead U38): the first exhaustion is
                        // once per artifact, and work saved once and unsaved again
                        // holds the same build anew, with the overdue notice off —
                        // without this raise, a wait with no notice at all (L5).
                        if self.update_waits_on_person.is_some() && !self.update_wait_said {
                            self.note_update_blockers(&reasons, true);
                        }
                    }
                }
            }
            #[cfg(any(unix, test))]
            (AttemptDisposition::Retry, UpdateOutcome::CaptureRefused { message }) => {
                // The fork lane's capture refused the desk: same lane as the
                // launched lane's completion. Recorded as an apply failure naming
                // the session (so `failing_applies` moves and `update status`
                // shows it), retried once the desk changes, never latched and
                // never charged to the physical budget.
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
                // A returned physical handoff can park/read/checkpoint sessions,
                // so it is retried rarely (twice per epoch), but an epoch always
                // ends. `retry_at: None` comes only from a structural
                // `PhysicalFailureSchedule::Converged`; the transient lane
                // converges into a quiet six-hourly re-sample.
                self.auto_apply_intent = None;
                // Transient, because this lane has no typed outcome to read: these
                // are submission-time failures (the admission classifier refusing,
                // a cold-lane `exec` that returned, a stale reducer) with no
                // candidate spawned, and a schedule is never derived from the
                // message string. Each is a fact about this process's current
                // state, which changes.
                // Which side of the swap, from the live stage (no candidate, so no
                // ticket): `poll` admitted this intent only against the exactly
                // matching stage.
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
                // said once, and `apply_retry_for` carries the block's own
                // remedy to every surface (never the ledger's latest reason,
                // which a later ordinary failure rewrites while the block
                // stands). The latch clears with the reason — the checker's
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
                    self.suspend_automatic_apply_for_person(
                        intent.build,
                        intent.dmg_sha256,
                        cached_refusal.as_deref().unwrap_or(""),
                        message,
                    );
                    return;
                }
                // …AND ONE THAT IS ONLY THE WAIT FOR AN UPDATE CHECK TO CONFIRM THE
                // STAGE'S SIGNER IS NO FAILURE AT ALL (round eight, #503's second
                // review). It lasts until a check reads the machine roster, which no
                // attempt does, so the transient schedule below spent the budget,
                // took the 600 s / 1800 s / 6 h latch and booked the `apply` streak
                // for a check that had not run yet. The lane holds for the check
                // instead, its intent kept and nothing charged; the hold books the
                // wait as the standing refusal. (A refused cold relaunch no longer
                // seeds the wait as a verdict, `App::seed_refused_cold_relaunch`;
                // this is the backstop for any other way it reaches the cache.)
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
                // A passing cached refusal is not the verifier's verdict: it rides
                // the transient schedule, and `needs_person` above is false for it.
                let shape = PhysicalFailureShape::cached_refusal_shape(cached_refusal.as_deref());
                // One helper for every lane: the budget, the latch, the schedule's
                // log line, its `update status` note and, at convergence, the
                // health notice are written by the same code here and on both
                // asynchronous completions.
                let schedule = self.latch_after_physical_failure(
                    intent.build,
                    intent.dmg_sha256,
                    activation,
                    shape,
                    &message,
                );
                // An admission refusal is synchronous, so the Staged bar may still
                // say "applies in place" while the Version-menu row says the
                // attempt did not start; restate the bar now.
                self.restate_staged_bar_posture(intent.build);
                self.surface_update_apply_outcome(
                    // The label names which schedule answered; the glass says
                    // nothing while a retry is scheduled and names the Version
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
                // The standing schedule note was written by that surfacing, after
                // the failure it booked (which clears any standing refusal); see
                // `App::apply_schedule_standing`.
            }
            (_, outcome) => {
                // A policy/outcome mismatch fails safe: no panic in the input loop,
                // no unbounded physical retry, and a latch that neither a
                // structural re-sample nor a newer release releases.
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

    /// The quiet download's words changed: only the Settings projection and the
    /// windows showing a Settings view repaint. The Version menu, palette, tab
    /// indicators and other windows read nothing the quiet lane changes, so with
    /// no Settings open this costs nothing.
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
        // A read that began before this process's own stage import describes a
        // disk without that stage (the read spans a codesign; the check's wake can
        // land first). Reducing it would retire the stage just imported, so it is
        // ignored; the next observation sees the stage.
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
        // Whether this copy can replace itself, the moment any read observed it —
        // the page says "not read yet" until then, never a guess either way.
        if let Some(installable) = installable
            && self.native_updater_service.note_installability(installable)
        {
            self.publish_native_update_state();
        }
        // When the last check completed, whoever ran it (Settings says "Checked 12 min ago").
        if self
            .native_updater_service
            .note_checked_at(durable.as_ref().and_then(|durable| durable.checked_at))
        {
            self.publish_native_update_state();
        }
        let build = self.native_updater_service.snapshot().current_build;
        // What a structural latch reads, taken from this observation before
        // anything below rewrites it: the boot sentinel's count for the installed
        // bundle (measured on the facts worker), and the verified download on
        // disk, read off the durable marker here because the activation import
        // below overwrites it with the bundle's identity. What is on disk now:
        // the verdict remembers the release it already offered. A count read
        // before the last apply attempt returned measures nothing; it may be one
        // launch low (facts read mid-attempt are replayed as `Deferred`).
        let trial_stale_before = self.native_installed_trial_stale_before;
        let trial_readable = trial_stale_before.is_none_or(|stale| observed_at >= stale);
        self.native_installed_trial = installed
            .as_ref()
            .filter(|_| trial_readable)
            .map(|installed| (installed.build, installed.trial_launches));
        // …and whether it could PROVE the installed bundle at all (round eight, #80):
        // a read whose verification did not finish (`spctl` on a captive portal)
        // observed nothing about it, and a look over it counts nothing.
        if trial_readable {
            self.native_installed_trial_read = self.last_native_update_reconcile_sequence;
            self.native_installed_trial_unproven = installed_unproven;
        }
        if let Some(durable) = durable.as_ref() {
            self.native_verified_download_on_disk =
                crate::native_updater_service::verified_download_build(build, durable).unwrap_or(0);
        }
        // The installed bundle outranks a staged download once it is newer than
        // this process: a downloaded stage in memory is retired first, so the
        // activation imports through the ordinary check/stage transitions
        // (`request_check` refuses while a stage is held).
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
                // The latch belongs to the update, not the download. A latch on the
                // build the bundle now is was minted by the candidate that
                // installed it, so it is re-keyed to the activation with its
                // deadline kept (clearing it would fire the confirming retry at
                // once). An older latch retires with the download; a strictly newer
                // one survives, as in the `InstalledNeedsRelaunch` arms.
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
                        // A retired build must not keep its latch: once its bytes
                        // are the installed bundle there is nothing left to refuse
                        // (`arm` also refuses a build no longer newer, but state
                        // must not say something false).
                        //
                        // A latch naming a strictly newer build is about another
                        // artifact and survives, as in the `Retired` arm of
                        // `reconcile_returned_native_apply_with_facts`: a newer
                        // attempt may have completed out of order.
                        //
                        // A latch on the installed build itself is re-keyed with its
                        // deadline, since those bytes are still the update on offer
                        // as the bundle's activation.
                        self.carry_manual_only_latch_across_swap(build, build, installed.as_ref());
                        // The installer may be another aterm or this process: an
                        // overlap child swaps the bundle and re-execs before writing
                        // a readiness proof, so a handoff rejected after ProofReady
                        // leaves the new bundle installed by our own candidate. Name
                        // the state and what happens next, by version where the
                        // bundle names one, without attributing the installer.
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
                        // The latch belongs to the exact retired artifact; keeping it
                        // could suppress a replacement whose build is lower than the
                        // withdrawn stage but newer than this process. A latch for
                        // other bytes survives.
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

        // The ledger's failure verdict reaches the snapshot on every reconcile
        // that holds a stage, exact or not (an activation never matches the
        // durable marker, and `request_check` refuses while a stage is held, so
        // `finish_check` cannot carry it).
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

        // Newness is the stager's test only: strictly newer than the running
        // image. `observed_stage_floor` is in-pass hysteresis against re-importing
        // a stage this pass just retired. The installed bundle gets no vote: its
        // plist can be replaced under a live process, and folding it in would make
        // a staged build the bundle already carries compare "not newer" forever.
        let stage_floor = build.max(observed_stage_floor);
        // The installed bundle is newer than this process: activate it. The bytes
        // are already at our path (the release cutter, a user dragging in a new
        // `.app`, a sibling aterm, or our own overlap child that swapped and then
        // failed to prove readiness). It is a staged update whose swap already
        // happened, so it is imported as an activation stage (the installed
        // bundle's sealed identity under `installed_activation_digest`) and rides
        // the ordinary stage → automatic apply → seamless handoff path; the
        // successor finds nothing to swap and adopts every window and shell. The
        // durable download is not used as the stage: with the bundle replaced, a
        // swap-apply's rollback-source proof would defer forever. Activation
        // outranks any durable stage while the bundle is newer.
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

    /// The reconcile's one look at a structural latch, after the stage settled,
    /// so a latch the bundle swap re-keyed to the activation in this pass is the
    /// one a newer release may supersede. A released re-sample is armed by the
    /// caller's `Reduced` arm; a supersede answers later through
    /// `finish_activation_supersede`.
    fn look_at_structural_latch_after_reconcile(&mut self) {
        let on_disk = self.native_verified_download_on_disk;
        let _released = self
            .look_at_structural_latch(std::time::Instant::now(), (on_disk > 0).then_some(on_disk));
    }

    /// THE AUTOMATIC LANE'S ANSWER TO A REFUSAL ONLY A PERSON CAN CLEAR (the
    /// synchronous arm of [`Self::try_pending_native_auto_apply`]): `refusal`, the
    /// cached pre-park verdict, says no lane can install `build` until a person
    /// changes the installed copy or its folder. A deadline-less latch
    /// ([`Self::block_native_auto_apply_environment`], keyed by what refused),
    /// the remedy said once on the row and in the log, and the attempt's
    /// failure booked (`message`).
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
            // Exact bytes only: this latch is about the installed copy refusing,
            // and the bundle moving is what releases it.
            activation: false,
            retry_at: None,
        });
    }

    /// The remedy of the environment block standing for `build`, or `None` when
    /// none stands ([`Self::auto_apply_environment_blocked`]). The one source of
    /// the person's next step while the block holds: the ledger's latest reason
    /// may be an ordinary failure booked since — the person's own retry, failing
    /// before it reached the refusal (round eight, finding 14's review).
    ///
    /// NOT A FOLDER THAT HAS ANSWERED (round eight, the review of finding 14's
    /// review): a person's retry whose pre-park check passed after the folder's
    /// block was taken ([`Self::install_location_answered_since`]) and whose
    /// successor then died comes back BEFORE the reconcile that releases the
    /// block on that pass — the worker sends the completion first — and its
    /// row, built from this remedy, sent the person to "Move aterm to a folder
    /// you can write to" about a folder that had just taken the write. The
    /// pass is read here as the reconcile will read it; the block itself goes
    /// with that reconcile, which also asks whether the updater may retry.
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

    /// A STANDING BLOCK FOLLOWS THE NEWEST REFUSAL ONLY A PERSON CAN CLEAR
    /// (round eight, the review of finding 14's review). The block is keyed by
    /// what refused when it was taken, and its cause decides both the remedy
    /// every surface names and the evidence that releases it
    /// ([`Self::release_repaired_auto_apply_environment`]). A person who put
    /// the folder right and retried can meet a refusal of ANOTHER cause next:
    /// the staged check asks the folder first and the installed copy after it
    /// (`preverify_installed_rollback_source_with`), so an install with both
    /// problems shows the folder's first. Kept on the folder's cause, the block
    /// named the folder's remedy beside "the installed copy could not be
    /// verified" for the rest of the process, and it could never be released:
    /// only a pass releases the folder's block, and none comes while the
    /// installed copy refuses.
    ///
    /// So `newest` — the verdict cache's entry — re-keys a standing block when
    /// it is a refusal of exactly the blocked artifact, published after the
    /// block's own key, that only a person can clear and names another cause.
    /// The re-keyed block counts from that refusal: the evidence that releases
    /// it must be newer than the refusal that now names it. A refusal of the
    /// same cause, a passing refusal (never cached) and a pass change nothing
    /// here.
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

    /// This is a memory-only reduction of the facts worker's complete signing
    /// policy check — or, for a FOLDER that refused, of the pre-park
    /// verification that asked it ([`Self::install_location_answered_since`]).
    /// Unknown/older evidence cannot rehabilitate a refused source.
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
        // What refused since the block was taken names it first
        // ([`Self::rekey_environment_block_to`]), so the evidence asked below
        // is that cause's. Never waits: a cache another thread holds re-keys
        // nothing this reconcile.
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
        // A FOLDER IS NOT A BUNDLE (round eight, finding 14). Every fact below is
        // about the installed COPY, and for a folder this user cannot write it is
        // always true: the copy is fine. Released by it, the block came off at
        // the next reconcile — opening Software Update is one — the intent
        // re-armed, the page said "It will try again by itself", and the same
        // refusal came round again, booking an apply failure each time.
        //
        // The folder's own evidence is a pre-park verification that asked it and
        // passed after the block was taken (round eight, finding 14's review):
        // with none, a person who opened the folder and retried — a retry that
        // passed the check, then failed for an ordinary reason (a capture
        // refusal, a successor that did not dial) — kept the block for the rest
        // of the process, the automatic lane never retried, and the standing
        // reason, no longer the folder's, sent the page to the signed-release
        // remedy for a copy and a folder that were both fine.
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
        // The pre-park verdict cache exists only where the seamless handoff reads it.
        #[cfg(unix)]
        {
            // A cache publication newer than this read may have seen another
            // change; never erase it with older evidence. A matching old denial is
            // invalidated, not turned into a fabricated pass.
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

    /// THE FOLDER TOOK THE SWAP'S WRITE SINCE IT REFUSED (round eight, finding
    /// 14's review): the verdict cache holds a PASS of exactly the `blocked`
    /// artifact — its build and digest — published after the block was taken.
    ///
    /// Only the STAGED lane's pre-park verification can pass that way, and it
    /// asks the folder before it can pass (`aterm_update`'s
    /// `install_location_check`, first in the rollback-source half of
    /// `preverify_staged_for_handoff`); both of its publishers — the arm-time read
    /// (`spawn_staged_handoff_preverification`) and the handoff worker
    /// (`PreverifyPublisher::publish_pass`, a person's retry included) — put the
    /// pass here with the moment it was published. An ACTIVATION's check reads
    /// the bundle under the executable and never asks the folder, so its pass is
    /// no evidence about one. (Off macOS the staged check asks nothing and
    /// passes, but there no refusal is a person's to clear —
    /// `aterm_update::refusal_needs_person` — so no folder block exists.) A
    /// refusal published since, of any kind, replaced the pass in the one slot:
    /// the newest verdict is the one read. Not bound by
    /// [`crate::HANDOFF_PREVERIFY_FRESHNESS`]: the pass says the folder was
    /// writable after it refused, which a person's change is; the attempt it
    /// re-arms verifies again when the pass is stale, and the swap's own step 5
    /// asks the folder once more under the apply lock. A pass that asked the
    /// folder before its refusal did and was published after the block (the two
    /// checks are serialized by the apply lock; only their publications can
    /// cross) costs one attempt, which the folder refuses and blocks again.
    ///
    /// Never waits: a cache another thread holds is no answer this reconcile,
    /// and the next reconcile asks again.
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
        // No verdict cache exists off unix, so nothing there says the folder
        // took the write — nor can a refusal only a person can clear be met
        // there.
        #[cfg(not(unix))]
        {
            let _ = blocked;
            false
        }
    }

    pub(crate) fn apply_native_update(&mut self, mode: ApplyMode) -> UpdateOutcome {
        // A PERSON'S PRESS ALREADY HELD for the candidate's pre-park read (round
        // eight of the update audit, U35's review): a second press joins it, and
        // runs with it once the read ends (`App::run_held_person_apply`).
        if !mode.is_automatic() && self.person_apply_is_held() {
            return UpdateOutcome::Accepted;
        }
        // A retire of this activation is running: its bundle is being taken off
        // the install path under the apply lock, and a candidate launched now
        // could come up as the replaced build. It takes seconds and the newer
        // build is the stage when it answers; until then any press waits. (The
        // latch already keeps the automatic lane off this build.)
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
        // A BUILD ON HOLD (`latched_activation_trial_spent`): its next start
        // is the one its boot trial rolls back, and no press re-arms that
        // trial. A person's press that reaches here — a control `update
        // apply`; the Version menu opens the details and Settings sends
        // nothing — is refused before anything launches, and says so. (The
        // latch keeps the automatic lane off it already.)
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
        // `Ok(false)` means a native app declined (the user's live state,
        // self-correcting); `Err` means the close reducer broke (an unknown
        // window, unhandled effects), which waiting never repairs. Keep that
        // distinction typed to the outcome, since a `Blocked` latch now cools
        // down and retries.
        let mut shutdown_error = None;
        // A background probe must not take over the screen: under
        // `ClosePreflightVisibility::Interactive` a `Blocked` reducer makes
        // `surface_native_close_recovery` switch tabs, move focus, re-front the
        // window and open Close Recovery, and the automatic lane re-probes on a
        // `PREFLIGHT_BLOCK_COOLDOWN` schedule. Quiet changes nothing about the
        // verdict. The person-initiated lane (Version menu, `Immediate`) keeps the
        // recovery surface, and the exhaustion pill points at that menu.
        let visibility = if mode.is_automatic() {
            crate::app_tabs::ClosePreflightVisibility::Quiet
        } else {
            crate::app_tabs::ClosePreflightVisibility::Interactive
        };
        // A draft journal that owes work (a failed append, a re-seat after a
        // rolled-back update) is retried by the update itself, so an unsaved draft
        // reads as "still being written" rather than holding the update.
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
                    // A PERSON'S PRESS HELD FOR THE CANDIDATE'S READ (round eight,
                    // U35's review): the ticket goes back, as for a deferral, and
                    // the press is held, its installing row up, to run again once
                    // the read ends (`App::run_held_person_apply`). Accepted: how
                    // it ends reaches the row, as any accepted install's does.
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
                    // An admission refusal: nothing was attempted, so nothing
                    // failed. The stage is re-armed like a deferral and the outcome
                    // takes the Blocked lane (the ledger's non-streak Refused slot
                    // and the disposition's cooldown), not the failure streak and
                    // physical budget of `Failed` below.
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
                    // The fork lane's park missed its budget: timing, like the
                    // launched lane's stand-down after its last rung, so the same
                    // spaced activity retry, never the physical budget. A person's
                    // apply is told; nothing is spent either way.
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
                    // The fork lane's capture refused the desk: the refusal lane, as
                    // on the launched lane. The automatic caller arms the retry;
                    // every caller records a failure. A person's press answers
                    // `Failed` (the launched lane's `Manual`), since no
                    // desk-fingerprint retry is armed for it.
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
                    // Submission failed before the worker touched disk: re-arm the
                    // exact authority directly. Physical failures always return
                    // through `finish_async_native_update_handoff` with worker facts.
                    Err(error) => self.abort_unstarted_native_apply(&attempt, error.into_message()),
                }
            }
            ApplyDecision::Blocked(reasons) => {
                self.publish_native_update_state();
                // The preflight ticket had to be consumed above (an abandoned
                // `pending_preflight` makes every later `begin_apply_preflight`
                // answer `Joined`), so the error is reported here, not by an early
                // return.
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

    /// Main-thread completion for any apply attempt that left this process
    /// alive, including the asynchronous overlap waiter. Disk is authoritative:
    /// the exact stage may be re-armed, a consumed/swapped stage becomes
    /// InstalledNeedsRelaunch, and a changed stage retires the old generation
    /// before importing the new one.
    ///
    /// `lane` is the typed classification from the
    /// [`crate::native_updater_service::ApplyMode`] and the worker's activity
    /// verdict. An activity-revoked automatic attempt spends bounded
    /// [`AutomaticRetryKind::ActivityRevoked`] budget; a person's attempt spends
    /// nothing (see [`HandoffFailureLane`]).
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

    /// Spend one physical-failure attempt for this exact artifact and say what
    /// happens next. Every lane books the physical budget here: the synchronous
    /// `(ManualOnly, Failed)` arm and the asynchronous
    /// `abort_reaped_native_apply_before_reconcile` and
    /// `reconcile_returned_native_apply_with_facts`.
    ///
    /// The counter is a lifetime count on `auto_apply_physical_retry`, which
    /// outlives both the latch (destroyed on lapse) and
    /// `AutoApplyIntent::attempts` (reset on re-arm), so the lane converges. The
    /// epoch and position within it derive from it
    /// ([`PHYSICAL_FAILURES_PER_EPOCH`]).
    ///
    /// Three answers:
    ///   * mid-epoch: retry in 600 s, then 1800 s;
    ///   * epoch spent: stand down [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`], then
    ///     start the next epoch's schedule;
    ///   * this shape's [`PhysicalFailureShape::lifetime_attempts`] spent:
    ///     [`PhysicalFailureSchedule::Converged`], told once
    ///     ([`App::announce_automatic_apply_stranded`]). For the machine-shaped
    ///     failures that is a quiet re-sample every
    ///     [`PHYSICAL_FAILURE_EPOCH_COOLDOWN`]; for a structural failure one
    ///     re-sample [`STRUCTURAL_RESAMPLE_AFTER`] out, remembered by a
    ///     [`crate::AutoApplyStructuralVerdict`].
    ///
    /// A structural failure of bytes a verdict already stands on (its
    /// re-sample, or an attempt a newer release earned) converges at once onto
    /// whatever the verdict still owes, and the count does not replenish under
    /// it: the `AttemptFails` step of `NativeUpdateStructuralLatch`.
    ///
    /// Keyed by (build, dmg), so a different artifact starts clean; half a day
    /// with no physical failure for these bytes
    /// ([`PHYSICAL_RETRY_BUDGET_REPLENISH`]) starts the schedule over.
    ///
    /// `shape` acts only through [`PhysicalFailureShape::lifetime_attempts`] and
    /// the structural early return; the counter is shared and shape-blind, so a
    /// structural failure on an artifact that spent its transient budget
    /// converges immediately. [`PhysicalFailureShape::Unexplained`] rides the
    /// epoch machinery (it may be about the machine), with fewer epochs.
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
        // The verdict stands: an attempt it earned failed the same way, so
        // re-latch at once onto what the verdict still owes (the re-sample's
        // deadline if still to come, else nothing).
        if let Some(verdict) = verdict {
            return PhysicalFailureSchedule::Converged {
                resample_at: verdict.resample_at,
            };
        }
        // `spent` counts physical failures before this one, so it is this
        // failure's 0-based lifetime index. Saturating arithmetic keeps a runaway
        // `u8` pinned at Converged instead of wrapping into a fresh budget.
        if spent >= shape.lifetime_attempts().saturating_sub(1) {
            if shape == PhysicalFailureShape::Structural {
                return self.mint_structural_verdict(build, dmg_sha256, activation, now);
            }
            return PhysicalFailureSchedule::converged(shape, now);
        }
        if shape == PhysicalFailureShape::Structural {
            // One confirming retry, then the lane is done with these bytes. The
            // epoch machinery is skipped for this shape only: once the candidate
            // has twice failed to become the successor, there is nothing about
            // the machine left to re-sample.
            //
            // Same first rung as the transient schedule; if that rung ever
            // disappears, converge (fail closed).
            return match automatic_retry_delay(0, AutomaticRetryKind::PhysicalFailure) {
                Some(delay) => PhysicalFailureSchedule::Retry(now + delay),
                None => self.mint_structural_verdict(build, dmg_sha256, activation, now),
            };
        }
        let within_epoch = spent % PHYSICAL_FAILURES_PER_EPOCH;
        match automatic_retry_delay(within_epoch, AutomaticRetryKind::PhysicalFailure) {
            Some(delay) => PhysicalFailureSchedule::Retry(now + delay),
            // This epoch is spent but the lane is not: stand down long enough for
            // the next epoch to be an independent sample, while the counter
            // survives (asserted against [`PHYSICAL_RETRY_BUDGET_REPLENISH`]).
            None => PhysicalFailureSchedule::StandDown(now + PHYSICAL_FAILURE_EPOCH_COOLDOWN),
        }
    }

    /// WHAT THIS LANE HAS BOOKED AGAINST `attempt`'s BYTES, for the cold lane to
    /// carry across its `exec` (round eight, #1's review; see
    /// [`crate::app_update_handoff::ColdLaneBudget`]): the physical-failure record
    /// and the structural verdict that cover them, raw — the relaunched process's own
    /// [`Self::spend_physical_failure_budget`] applies the replenish window and the
    /// verdict's rule to them, with the clock carried along, exactly as this process
    /// would have.
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

    /// A REFUSED COLD RELAUNCH, HANDED TO THIS PROCESS'S AUTOMATIC LANE (round
    /// eight, #1 and its review) — called once, by `main_entry`, on the `App` of the
    /// launch whose boot swap refused ([`crate::app_update_handoff::ColdRelaunch`]).
    ///
    /// The refusal goes in the verdict cache, and is also kept apart until an
    /// automatic read of its artifact takes it
    /// ([`Self::answer_with_refused_cold_relaunch`]), so that read finds it however
    /// late it comes. The outgoing process's physical-failure record and structural
    /// verdict for these bytes are put back with their clocks, so the booking that
    /// read leads to ([`Self::latch_after_physical_failure`], from the synchronous
    /// arm) counts this refusal after every earlier one: the second structural
    /// refusal converges, the transient schedule climbs to its stand-down and
    /// converges after its ninth, and a spent re-sample is spent.
    ///
    /// A WAIT FOR A ROSTER CONFIRMATION IS NO REFUSAL (round eight, #503's second
    /// review; [`crate::app_update_handoff::ColdRelaunchSeed::roster_wait`]): no
    /// verdict is cached and nothing is kept apart for an attempt to book. The lane
    /// HOLDS for the check instead ([`Self::hold_for_roster_confirmation`]: no
    /// intent armed here, nothing booked, since the boot swap booked the wait as the
    /// standing refusal), and the arm-time read the startup reconcile starts finds the
    /// stage as the check left it. The budget is put back all the same: it is the
    /// record of these bytes' earlier failures, which a later one counts after.
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
                // A download's verdict — the only kind a cold relaunch carries: it
                // binds its artifact only off the activation — offers no newer
                // release the install path, and its trial always has room.
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

    /// A STRUCTURAL convergence, and the verdict that remembers it (gap 14,
    /// 2026-09-26): ONE re-sample [`STRUCTURAL_RESAMPLE_AFTER`] out, and no newer
    /// release offered the install path yet.
    ///
    /// A verified release already on disk while the bytes failed is NOT spent
    /// here any more (round three). Gap 14 recorded it as spent because its
    /// answer to a newer release was one more attempt at the latched build, and
    /// the failures had just been those attempts. The answer is now to let the
    /// newer build take the activation's place, which no failure of the older
    /// bytes has tried — so the first look after convergence offers it.
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

    /// Spend, latch, and say so: the one sequence every physical handoff failure
    /// takes, on every lane (the synchronous `(ManualOnly, Failed)` arm,
    /// `abort_reaped_native_apply_before_reconcile` and
    /// `reconcile_returned_native_apply_with_facts`). A latch is never written
    /// without its log line, its `update status` note and, at convergence, the
    /// health notice.
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

    /// The schedule's log line, its standing note, and, when the lane just
    /// converged, the loud notice. Split from
    /// [`Self::latch_after_physical_failure`] so the words have one author.
    ///
    /// The note is queued on [`crate::App::apply_schedule_standing`], not written
    /// here: booking the failure in the ledger clears any standing refusal, so
    /// the note must land after it, which
    /// `record_apply_outcome_for_target_in_ledger` does on every lane.
    fn say_physical_failure_schedule(
        &mut self,
        build: u64,
        shape: PhysicalFailureShape,
        schedule: PhysicalFailureSchedule,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        let wait_secs = |at: std::time::Instant| at.saturating_duration_since(now).as_secs();
        // The lifetime index of this failure against the artifact's budget, not
        // the per-intent counter (which restarts at 1 on every lapse and re-arm).
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
        // The schedule, durably: `status.toml` and `update status` carry the
        // pause, not only this process's memory.
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
    ///
    /// The ledger's own health warning (`aterm_update::health_failing_title("apply")`)
    /// fires at [`aterm_update::PERSISTENT_AFTER`] (3) consecutive apply
    /// failures, but a structural failure converges after
    /// [`STRUCTURAL_FAILURE_LIFETIME_ATTEMPTS`] (2), so convergence raises the
    /// same warning itself through [`App::note_update_health`], plus the OS
    /// banner, once per build and verdict.
    ///
    /// One decision per stopped lane: the `Install now` press is the outcome
    /// row's (`react_to_update_apply_outcome`); the warning names the Version
    /// menu and carries the `Software Update` capsule. A lane that re-samples by
    /// itself raises no decision row.
    ///
    /// No restart prompt: nothing on the update lane asks for a restart
    /// (`update_words`' guards); the Version menu's apply is the in-place
    /// remedy (owner, 2026-08-30).
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
        // so a warning already up takes the new words in place, one folded is
        // followed by a record in them (`restate_update_health`, where this
        // lane's own words were said before; a log line where another
        // notice's were), and the stopped lane's own outcome row carries the
        // `Install now` press.
        let verdict = (build, resample_at.is_some());
        if self.auto_apply_stranded_announced == Some(verdict) {
            return;
        }
        self.auto_apply_stranded_announced = Some(verdict);
        // Plain words and the one install verb: no "apply", "build N" or
        // "handoff" on the warning; those belong in aterm.log.
        let cause = match shape {
            PhysicalFailureShape::Structural => "the new version couldn't take over",
            PhysicalFailureShape::Transient => "the moment to switch kept passing",
            #[cfg(unix)]
            PhysicalFailureShape::Unexplained => "the new version kept quitting",
        };
        // A structural verdict re-samples once, a day out; the machine-shaped
        // ones re-sample every cooldown.
        let next = match (shape, resample_at) {
            // A spent boot trial: the menu's start would roll it back too.
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
        // The OS notification a health announcement owes, through the door
        // `Wake::UpdateHealth` shares (it holds `desktop_alerts` and never posts
        // from a unit test).
        if announced {
            self.post_update_health_banner(crate::update_words::HealthKind::Install, title, &body);
        } else if said_before {
            // This lane's own words were said this launch, and promised what
            // no longer holds (a retry withdrawn): the warning takes the new
            // words, where it stands or as a record after it.
            self.restate_update_health(
                crate::update_words::HealthKind::of_updater_title(title),
                title,
                &body,
            );
        }
    }

    /// Carry the manual-only latch across a retirement of the stage it was
    /// minted on, now that the bundle under our executable is `installed_build`.
    ///
    /// * A latch on `installed_build` itself is the same logical update (our
    ///   candidate installed that bundle before failing), so it is re-keyed to
    ///   the bundle's activation identity with `retry_at` unchanged. The
    ///   environment block is the exception: it waits on the installed copy
    ///   changing, which just happened, so it is released.
    /// * A latch on a build no newer than the retired one retires with it.
    /// * A latch on a strictly newer build is about another artifact and
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
    /// automatic lane a spaced activity retry that is recorded, and for a
    /// person's apply a failure they are told about.
    ///
    /// Recorded because this is not the ladder's "not this instant": the
    /// readers parked and the screen froze before it missed. It is surfaced and
    /// booked as a refusal, like the launched lane's `ActivityRevoked`
    /// stand-down, so `update status` shows `apply_refusal=`. Never a failure: a
    /// missed stopwatch is not evidence against the candidate, and
    /// `failing_applies` does not move.
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

    /// Consume one activity-revoked overlap retry cycle for this artifact and
    /// re-arm the automatic intent at its spaced deadline. `None` only when the
    /// artifact identity is malformed. The counter lives on
    /// `auto_overlap_retry`, keyed by (build, dmg), so duplicate or reordered
    /// completions cannot restart the spacing and a different artifact starts
    /// at the first rung. The ladder anchor is untouched.
    #[cfg(any(unix, test))]
    fn arm_activity_revoked_overlap_retry(
        &mut self,
        attempt: &crate::native_updater_service::ApplyAttemptTicket,
    ) -> Option<std::time::Duration> {
        let dmg_sha256 = decode_dmg_sha256(attempt.target_dmg_sha256())?;
        let build = attempt.target_build();
        // Which side of the swap: a failed candidate leaves the bundle
        // installed, so the retry for the same update arrives as an activation;
        // `covers` folds both into one ladder.
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
        // A revocation with no anchor for this build (the attempt was armed
        // before this process learned the ladder): the ladder starts here.
        self.anchor_auto_apply_ladder(build, now);
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            retry_at: now + delay,
            attempts: cycles,
        });
        Some(delay)
    }

    /// The desk as a capture refusal sees it ([`crate::CaptureDesk`]): every live
    /// session by id, read from the pool with no engine lock (session count and
    /// ids for `TooManySessions`/`DuplicateLocalId`), and per session a hash of
    /// its rows, cols, alt screen and reported cwd (for content refusals), or
    /// `None` for a session whose engine is busy this instant. A busy engine
    /// costs only its own session's reading.
    ///
    /// Not where a parser's last read ended (round eight of the update audit,
    /// U43): it flips with every read that stops inside an escape sequence, so
    /// a streaming tab released every refusal at the next probe, and no capture
    /// refusal is about it — a parser mid-sequence is a park miss, never this
    /// lane, and one over a quiet PTY is carried.
    ///
    /// The refusal lane retries when this changes, spaced by
    /// [`capture_refusal_spacing`], not on a timer.
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

    /// The park's capture refused this artifact's handoff: record the refused
    /// desk and re-arm the automatic intent on the refusal lane's probe, never
    /// the activity spacing, the physical budget or a latch. The caller's
    /// surfacing records the failure, which moves `failing_applies` and puts the
    /// session in `update status`'s `apply_failure=`.
    #[cfg(any(unix, test))]
    pub(crate) fn arm_capture_refusal_retry(
        &mut self,
        build: u64,
        dmg_sha256: [u8; 32],
        activation: bool,
        detail: &str,
    ) {
        let now = std::time::Instant::now();
        // A refusal of the attempt a release admitted counts on: the spacing
        // grows while the same update keeps being refused.
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
        // The refusal lane owns the intent now; a lapsed physical latch for the
        // same bytes has nothing left to say about it.
        self.auto_apply_manual_only = None;
        self.anchor_auto_apply_ladder(build, now);
        let attempts = self
            .auto_apply_intent
            .filter(|intent| intent.build == build)
            .map_or(1, |intent| intent.attempts);
        self.auto_apply_intent = Some(crate::AutoApplyIntent {
            build,
            dmg_sha256,
            // The spacing, which nothing releases sooner — or, for a session an
            // engine kept from being read, the baseline probe, which reads it.
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

    /// When the lane next looks at a standing capture refusal: the end of its
    /// spacing once its baseline desk is complete, since nothing releases it
    /// sooner, and otherwise soon. A refusal recorded while an engine was busy
    /// has no reading of that session yet, and taking one is not held back by
    /// the spacing (round eight of the update audit, U43 review): a session
    /// first read when the spacing ends would absorb a change made to it during
    /// the spacing. It is looked for every [`CAPTURE_REFUSAL_BASELINE_PROBE`]
    /// during the refusal's first probe, then at the probe.
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

    /// Read the sessions a standing capture refusal's baseline is missing,
    /// whatever the poll then decides: the hold reads them too, but a poll that
    /// waits for activity to settle never reaches it, and a session first read
    /// once the activity ends would absorb what the person did to it meanwhile.
    /// A no-op once the baseline is complete.
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

    /// How much longer the standing capture refusal holds the next attempt at
    /// the soonest, however the desk moves: what is left of its spacing
    /// ([`capture_refusal_spacing`]). The words that record a refusal say it
    /// when it is more than a probe (`update_words::capture_refused_waiting`),
    /// so a person who closes tabs as told is not left watching nothing happen.
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

    /// Whether a standing capture refusal still holds the attempt the poll just
    /// admitted: same artifact, and either its spacing has not passed
    /// ([`capture_refusal_spacing`]) or the desk has not moved
    /// ([`crate::CaptureDesk::moved_to`]), backstop not reached. Releasing it
    /// marks the record released, which then holds nothing and only lets the
    /// next refusal of the same update count on. Every call first reads the
    /// sessions the baseline is missing, at the probe and not after the
    /// spacing, which holds back only the release; a session busy at this read
    /// says nothing about itself (the probe comes back), and a session closed
    /// or opened is seen whatever is busy.
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
            // A session busy when the refusal was recorded takes its first
            // readable state as its baseline, whatever the spacing, so a change
            // made to it during the spacing is still a change when it ends
            // (round eight of the update audit, U43 review). Never a release by
            // itself: what the refusal was about may be unchanged.
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

    /// A handoff worker has killed and reaped its child, but ordered disk
    /// reconciliation may still be queued. Re-arm the exact in-memory authority
    /// now so the UI never stays Applying while readers resume; the later
    /// reconcile wake retires or imports disk authority.
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
            // The `Rearmed` policy of `reconcile_returned_native_apply_with_facts`
            // (the lane every completion takes), with no facts to reduce.
            return self.settle_rearmed_apply_failure(attempt, message, lane, None);
        }
        UpdateOutcome::Failed { message }
    }

    /// The automatic lane's answer to a failed handoff whose stage stayed armed,
    /// shared by the reaped abort and a `Rearmed` reconcile. Every exit publishes
    /// the update state, then reduces the returned `facts`.
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
        // A refusal only a person can clear, of another cause than a standing
        // block's, that this attempt's own pre-park check met (the worker
        // caches its verdict before it sends the completion): the block names
        // it before any row about this attempt is built
        // ([`Self::rekey_environment_block_to`]) — a person's press included,
        // whose failure is surfaced before the reconcile that follows it.
        #[cfg(unix)]
        {
            let cache = std::sync::Arc::clone(&self.handoff_preverified);
            let newest = cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.rekey_environment_block_to(newest.as_ref());
        }
        // THE STAGE WAITS FOR A CHECK TO CONFIRM ITS SIGNER (round eight, #503's
        // review): the pre-park check's refusal lasts until a check reads the
        // machine roster, which no attempt does. Filed as a physical failure it
        // spent the transient budget, took the 600 s / 1800 s / 6 h latch the
        // confirming check never lifted, and booked the `apply` streak that raises
        // "auto-update is failing" — for a laptop that was offline. A deferral
        // instead: the automatic lane holds for the check (re-armed here when it
        // was the one that tried), nothing is charged, and the ledger keeps the
        // wait as its standing refusal.
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
        // A person's failure is not the background lane's business: the apply
        // authority is reduced (the artifact stays staged, the UI leaves
        // Applying) but no automatic budget is spent. See [`HandoffFailureLane`].
        if !lane.charges_the_automatic_lane() {
            publish(self);
            return UpdateOutcome::Failed { message };
        }
        // Activity-revoked: the stage is re-armed and the rollback was lossless,
        // so schedule one spaced re-attempt, never a latch.
        if lane == HandoffFailureLane::ActivityRevoked
            && let Some(delay) = self.arm_activity_revoked_overlap_retry(attempt)
        {
            publish(self);
            aterm_log::info!(
                "update apply: activity revoked the overlap; automatic retry in {delay:?}"
            );
            return UpdateOutcome::Deferred { reason: message };
        }
        // A capture refusal: recorded by the caller's surfacing, retried when the
        // desk changes, never latched.
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
        // A VERDICT ONLY A PERSON CAN CLEAR is no failure of the build's (round
        // eight, finding 14): the worker's pre-park check reached it — the
        // attempt outran the arm-time read — and keeps it where the synchronous
        // arm reads it ([`Self::suspend_automatic_apply_for_person`], by the
        // same typed fact). Filed `PreparationFailed`, it spent the structural
        // budget, and two of them stranded a healthy build as one that
        // "couldn't take over", while the folder this user cannot write, or the
        // installed copy, was what refused.
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
        // A PHYSICAL failure spends its converging budget and takes the manual
        // latch — logged and recorded (plan P1-1(a)): the latch used to be
        // written with no log line and no standing reason, and the 2026-09-22/23
        // incident log holds zero lines saying the lane had stood down for good,
        // across 45 hours stuck.
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
        // The Staged bar, if it is still up for this build, stands down with
        // the lane — not after the next unrelated reconcile.
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
                // The stage is re-armed on disk; the facts are reduced once the
                // lane's policy has published.
                Some(self.settle_rearmed_apply_failure(&attempt, message, lane, Some(facts)))
            }
            ReturnedApplyDisposition::InstalledNeedsRelaunch { build } => {
                self.auto_apply_intent = None;
                // Our attempt swapped the bundle (this process before the launch, or
                // a fork-lane child) and then failed. The failure
                // is charged like every returned one (the budget folds download and
                // activation into one count), and the latch is carried to the
                // activation it now guards, so the imported activation does not
                // retry immediately.
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
                // The reducer's own sentence for the same retirement, less its
                // "The install stopped" (the row and the log say that already).
                let changed = if attempt.is_installed_activation() {
                    "aterm on disk has since changed"
                } else {
                    "the download on disk has since changed"
                };
                Some(UpdateOutcome::Failed {
                    message: format!("{message}; {changed}"),
                })
            }
            // A delayed callback for attempt A must not perturb attempt B's UI: the
            // service is reducer-inert and `None` forbids logs, alerts, redraws or
            // "remains ready" text at the caller.
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
        // An unsaved draft is carried, not a blocker, once its journal holds it
        // durably and the image on disk replays to the editor's exact text
        // (`App::draft_carry`). Asked at the update's start and at Commit, where
        // the image read is the successor's republication of what it restored.
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
        // Whether the successor reopens what is open here: the seamless successor
        // restores every native tab from the handed-over layout, which carries
        // each Settings view's unsaved field drafts
        // (`App::capture_handoff_layout`). Where it does, Settings drafts and
        // failed-checkpoint documents ride the update; where it does not (a
        // headless successor, the Windows replace) they hold it. The cold unix
        // lane refuses what this token says rides
        // (`App::start_unix_update_handoff`).
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
            // A journal write on the worker now (any document's, a re-seat after
            // a rolled-back update included) could land after the successor read
            // its restore image or after Commit, so no update starts or commits
            // over one.
            blockers.push("Wait: a draft journal write is in flight".to_string());
        }
        if self.document_journal_landed_while_parked() {
            // At Commit: a journal write already on the worker at the park landed
            // afterwards, possibly after the successor read its restore image;
            // nothing can order the two, so nothing is vouched for.
            blockers.push(
                "Wait: a draft journal was written after the successor began restoring".to_string(),
            );
        }
        // A failed checkpoint is not a loss the successor causes: its unsaved
        // text is a dirty document's draft, which the tally above carries or
        // holds the update for. A reopening successor takes it (unfrozen, unsaved,
        // to save again); only a lane that reopens nothing waits.
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
    /// The original token authorizes preparation only; edits can happen while the
    /// child boots, so the outgoing process needs fresh evidence before exit.
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

    /// The close scope an update asks every native app with: `CarriedRelaunch`
    /// when this process's successor reopens its native tabs from the
    /// handed-over layout (which carries their unsaved view state), else
    /// `Relaunch`, under which an app holds what it cannot keep. Same predicate
    /// as the preflight's carried-draft count
    /// ([`Self::successor_restores_document_tabs`]).
    pub(crate) fn update_close_scope(&self) -> crate::native_app::CloseScope {
        if self.successor_restores_document_tabs() {
            crate::native_app::CloseScope::CarriedRelaunch
        } else {
            crate::native_app::CloseScope::Relaunch
        }
    }

    /// QA same-binary reexec through the production native-state preflight, so
    /// `ATERM_DEBUG_SEAMLESS_REEXEC` never bypasses dirty documents, checkpoint
    /// work, or restore/adoption state.
    pub(crate) fn apply_debug_seamless_update(&mut self) -> UpdateOutcome {
        // A QA seam is driven by a person at a keyboard, so a blocker is the
        // answer to something they just did: surface the recovery commands.
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
                // The keep-stale rung below is this tab's own previous title,
                // captured before any leaf is re-read.
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
                    // Nonblocking and keep-stale: this runs on the winit thread on
                    // every tab switch/focus change (`sync_window`), and the reader
                    // thread holds this mutex for a whole ingest slice.
                    //
                    // On contention only this tab's own previous title may be
                    // reused. `tab.presentation.title` is stable metadata read as
                    // the fallback rung by `tab_titles`, `refill_strip_titles`,
                    // `window_title_identity` and the `tabs` verb, and only the
                    // next structural sync corrects it. This rung is the raw OSC
                    // title (or `"aterm"`); `tab_title_cache` must not be consulted,
                    // since it can hold an operator `meta set title` or the
                    // abbreviated cwd. The next output wake republishes the true
                    // title either way.
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
                    // The shared leaf builder's mapping: a window sync must not
                    // blank indicators a status change just published.
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
                            // A native leaf is the out-of-band attention owner;
                            // it has no session and no classified status.
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

/// The failure sentence for a Settings ▸ Packages Check or install child run the lanes'
/// way ([`crate::run_pass_child`]), or `None` when it succeeded:
/// [`packages_child_failure`] over its exit and stderr, except for the one ending the
/// exit code cannot name, a pass on the cached index.
///
/// atpkg exits 1 with nothing on stderr for a pass that did not reach the signed index
/// and was not offline (its own, or one it queued behind), recording the cause only in
/// `status.toml`. Both read that record, as the lanes do
/// ([`crate::PassRun::recorded_cached_index_cause`], gated on exit 1), and say the cause.
/// It stays a failure: the index could not be asked, and `PackagesCommandOutcome` is
/// pinned to {none, success, failure} (see [`packages_child_failure`]).
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

/// The Check's sentence for a pass that ran on the cached index
/// ([`check_pass_failure`]): the recorded cause verbatim and what to do. It claims only
/// that the index was not reached, nothing about what the pass installed.
fn cached_index_check_message(cause: &str) -> String {
    format!(
        "The package index could not be reached ({cause}), so the pass ran on the last \
         verified copy of it \u{2014} try again later."
    )
}

/// The failure sentence for one finished `atpkg` child of the Packages worker, or
/// `None` when it succeeded. Pure over the exit status, argv and stderr, so it is
/// testable without a child.
///
/// Contention: the Check runs the window's own pass argv (`--wait-lock`), so its 75
/// means the half-hour wait ran out; the worker answers at once, before any spawn,
/// only when a person's typed verb holds the store. Other verbs fail fast. Refusals
/// are classified by code (75, `EX_TEMPFAIL`, is atpkg's contention code; 69 is
/// offline). The headline stays the operation's `failed_headline`:
/// `PackagesCommandOutcome` is pinned to {none, success, failure} by the aterm-spec
/// model (`models_native.rs` / `native_packages_conformance.rs`).
fn packages_child_failure(
    result: Result<std::process::ExitStatus, &std::io::Error>,
    verb: &[String],
    said: &str,
) -> Option<String> {
    match result {
        Ok(status) if status.success() => None,
        // Exit 2 = ran fine, installed nothing, and never will here (atpkg
        // `cmd_install_default_set`: the signed index pins no build for this
        // architecture). Neither success nor a retryable failure, so it gets its
        // own words.
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
        // Exit 75 = another atpkg pass holds the store lock (this window's
        // launch/loop pass, another window's, or a terminal `aterm pkg …`).
        // Transient; the page's status refresh shows the store it leaves behind.
        Ok(status) if status.code() == Some(i32::from(atpkg::lock::CONTENDED_EXIT)) => {
            // A fixed sentence: the child's own line names a peer (after a
            // self-update, this app's predecessor's pass) and a lock file; the
            // exit code says all there is to say.
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

/// Resolve admission once, then launch at most one worker. A completion's `true`
/// answer already owns the coalesced rerun and must not re-enter Request. The
/// injected operation is thread creation, not the machine read.
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
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
) -> Result<(), String> {
    let atpkg = crate::co_located_atpkg()
        .ok_or_else(|| "the package tool is not installed beside aterm".to_string())?;
    spawn_native_machine_read(proxy, atpkg)
        .map_err(|error| format!("could not start the machine read: {error}"))
}

/// The detached machine read thread: one `atpkg machine` child, its parsed record
/// posted back as [`Wake::NativeMachineStateFinished`]. Serves an admitted initial
/// request and a reserved completion rerun; `Err` is a thread-spawn failure.
fn spawn_native_machine_read(
    proxy: aterm_window::event_loop::EventLoopProxy<Wake>,
    atpkg: std::path::PathBuf,
) -> std::io::Result<()> {
    crate::qos::spawn_lane_thread(
        "aterm-machine-status",
        crate::qos::Role::Background,
        move || {
            // A host-settings probe is cosmetic to the terminal; it lands via
            // the proxy when done.
            let result = read_machine_state(&atpkg);
            let _ = proxy.send_event(Wake::NativeMachineStateFinished { result });
        },
    )
    .map(drop)
}

/// Run bare `atpkg machine` and parse its `machine-state:` line. The child gets the
/// launch-time apply's environment (`spawn_machine_settings_once`): the spawner pid
/// and the atpkg child PATH. Both streams are bounded; only the stdout state line is
/// the contract.
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
            // The line prints after atpkg's platform check, so its absence means
            // no state was measured, which the status must say, never "nothing to
            // apply".
            "atpkg machine printed no state".to_string()
        } else {
            format!("atpkg machine exited with {}", output.status)
        }
    })
}

/// A machine command has one deadline and bounded stdout/stderr. Nonblocking pipe
/// reads avoid a reader thread or a wait on a descendant's inherited stdout. Cleanup
/// spends only the reserved tail of the budget and reports an unconfirmed reap
/// rather than waiting indefinitely after a failed kill.
///
/// One `Overflow` and one wrapper for every target; `cfg` chooses only the body of
/// [`machine_command_output_bounded`], whose two arms share a signature, so a call
/// site cannot compile on Unix and break the Windows build.
fn machine_command_output(
    command: &mut aterm_uds::spawnfd::Command,
    operation: &str,
    limit: std::time::Duration,
) -> Result<std::process::Output, String> {
    machine_command_output_bounded(command, operation, limit, Overflow::Fail)
}

/// What a flood of output means for this child.
///
/// The 64 KiB cap bounds this process's memory. A read's record is one line, so a
/// flood means a broken reader and the child is killed. `atpkg machine apply` narrates
/// two lines per migrated directory and can legitimately exceed the cap; killing it
/// could land between a `rename` and its symlink, so its output is truncated instead
/// (the state is re-read afterwards).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Overflow {
    /// Stop with an error, and stop the child with it.
    Fail,
    /// Keep the first 64 KiB, go on draining so the child never blocks on a full
    /// pipe, and let it finish its work.
    Truncate,
}

#[cfg(test)]
thread_local! {
    /// The pid of the last child [`machine_command_output_bounded`] spawned on
    /// this thread, so a test names its child without the child's help.
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
                                // Keep the head, drop this chunk, keep reading, so
                                // the child's pipe never fills and it runs to
                                // completion.
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
            // Killed but not yet collected: dropping it hands it to the
            // process-wide reaper, so it never stays a zombie of the window.
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

/// The non-Unix arm, with the same signature as the Unix one (`Overflow` included).
/// There is no nonblocking-pipe machinery here and only macOS has machine settings,
/// so this refuses before spawning.
#[cfg(not(unix))]
fn machine_command_output_bounded(
    _command: &mut aterm_uds::spawnfd::Command,
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
    /// appstatus ledger and the status's "Last change").
    pub(crate) events: Vec<Wake>,
    /// The last verdict sentence after [`atpkg::cli::MACHINE_VERDICT_PREFIX`]:
    /// `applied — …`, `nothing changed — …` or `not applied — …`.
    pub(crate) verdict: Option<String>,
    /// [`atpkg::cli::MACHINE_NOT_APPLIED_PREFIX`]` <reason>`: the pass refused (a
    /// home that is not the account's), or `MACHINE_APPLY_FAILED_PREFIX`: a write
    /// failed. Either is a failure even when the command exits successfully.
    pub(crate) refusal: Option<String>,
    /// The apply's own parsed `machine-state:` record, carried on the completion
    /// rather than posted as an event, so only this verb confirms the status.
    pub(crate) record: Option<atpkg::machine::MachineState>,
}

/// Sort an `atpkg machine apply` stdout into marker events, the verdict sentence and
/// any refusal or failed write. Pure, so tests pin these shapes. The prefixes are
/// atpkg's constants, so a re-spelling on either side is a compile error, never a
/// refusal reported as success.
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
    // No record-missing fallback: the completion re-reads the machine whenever it
    // carries no record (`finish_native_packages`), and that one read takes the
    // expectation the change line opened.
    read
}

/// The exact argv of the `atpkg` process a [`PackagesRequest`] runs, the one table
/// the Settings verbs go through; pinned by `packages_argv_is_pinned_per_request`.
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

/// The verb a person pressed and waits on: every Packages request but the machine
/// apply, a local edit that finishes in well under a second.
pub(crate) fn person_verb(
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

/// How a person's verb row ends: Fault when the verb failed (its page says why),
/// Vanish for a child that never ran a pass (it stood aside at the lock with nothing
/// to claim), otherwise Complete with the verb's finished words.
pub(crate) fn person_echo(failed: bool, ran: bool) -> aterm_messages::EchoKind {
    if failed {
        aterm_messages::EchoKind::Fault
    } else if ran {
        aterm_messages::EchoKind::Complete
    } else {
        aterm_messages::EchoKind::Vanish
    }
}

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

/// Why this process refuses `request` right now for what it is, or `None`.
///
/// Under a private state root
/// ([`aterm_types::dirs::runs_under_private_state_root`]), Apply Now is refused: the
/// `[machine]` settings are the Mac's (Universal Control for the account's host). The
/// page's button applies nothing,
/// the launch's daily apply stands aside
/// ([`atpkg::machine::LaunchApply::PrivateStateRoot`]) and a pass carries no
/// `[machine]` edit; `aterm pkg machine apply` in a terminal is the one way left.
/// Other verbs pass: Check, Install and Uninstall act on the shared package prefix
/// whatever `[packages]` says (that switch is Automatic updates alone), and Check and
/// Install lead with [`crate::pass_lead`], so their pass carries no `[machine]` edit.
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
