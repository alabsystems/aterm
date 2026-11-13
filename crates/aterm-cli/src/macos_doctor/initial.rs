// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable first-launch plan.

use super::*;
use aterm_machines::init_secrets::Replaces;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{Plan, Subject};

mod check;
mod input_activation;
mod pending_inputs;
pub use check::{check_initial_setup, check_initial_setup_observed};
#[cfg(any(test, debug_assertions))]
pub use input_activation::input_setup_scope_at;
pub use input_activation::{
    InputServiceSet, InputSetupScope, activate_input_attachments, input_setup_scope,
    with_current_input_scope,
};
pub use pending_inputs::{
    initial_setup_pending_repositories, initial_setup_pending_workstation_revision,
};
mod background_observation;
#[cfg(any(test, debug_assertions))]
pub use background_observation::background_workstation_of_reply;
pub use background_observation::{
    BackgroundAnswer, ask_background_setup, check_background_workstation,
};
mod background_vault_authorization;
pub use background_vault_authorization::BackgroundVaultAuthorization;
mod observation;
pub use observation::{InitialSetupObservation, InitialSetupObserved};
mod authorization;
pub use authorization::InitialSetupAuthorization;
#[cfg(any(test, debug_assertions))]
pub use authorization::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
mod access;
mod observe;
pub use observe::{ObservationRead, observe_initial_setup};
pub(super) use observe::{changed_at_login, note_at_login, read_back};
mod scope;
#[cfg(any(test, debug_assertions))]
pub use scope::ProvisionalSetupWatchTest;
pub use scope::{
    InitialSetupUpgrade, SetupUpgrade, alab_mac, provisional_setup_widened,
    provisional_setup_widens, put_back_held_initial_setup,
};
mod park_watch;
pub use park_watch::{ParkWatch, Watches};
mod secrets_retry;
use secrets_retry::Pace;
mod pass_retry;
pub use pass_retry::{PassRetry, PassWorker, SCHEDULE as PASS_RETRY_SCHEDULE};
mod resume;
pub use resume::{
    ENTRY as SETUP_RESUME_ENTRY, LABEL as SETUP_RESUME_LABEL, SetupRecordsWatch, SetupResume,
    SetupWindowHold, end_initial_setup_resume, hold_initial_setup_window, initial_setup_resumable,
    initial_setup_window_open, keep_initial_setup_resume, wait_for_initial_setup_windows,
};
pub(super) mod kept;
mod read_back_record;
pub use read_back_record::{
    Fence, journal_fence, remember_private_read_back, remembered_private_read_back,
};
mod ledger;
pub use ledger::setup_ledger_environment;
pub use ledger::{DisplayHistory, LastRead, SetupHistoryStore};
mod revision;
mod set_aside;
pub(super) use set_aside::forget_at as forget_set_aside_at;
pub use set_aside::{
    SetAside, forget_set_aside_setup, set_aside_unreadable_setup,
    set_aside_unreadable_setup_observing,
};
#[cfg(any(test, debug_assertions))]
pub use set_aside::{
    set_aside_unreadable_setup_at, set_aside_unreadable_setup_at_observing,
    set_aside_unreadable_setup_at_observing_before_checkpoint,
    set_aside_unreadable_setup_at_observing_done_error,
};
#[cfg(test)]
mod completion_io_tests;
mod repositories;
#[cfg(test)]
mod vault_sync_tests;

#[cfg(test)]
pub(super) fn resume_native_accounts_after_init_for_test(
    root: &Path,
    account: &std::sync::Arc<crate::macos_doctor::accounts_visibility_tests::Account>,
) -> Result<Option<Report>, String> {
    tests::resume_native_accounts_after_init_for_test(root, account)
}

/// One independently retried part of a setup run. The discriminant is the bit
/// the progress record stores, so a stored bit keeps its meaning across versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Workstation = 0,
    Spotlight = 1,
    Permissions = 2,
    Secrets = 3,
    Repositories = 4,
    /// The Messages and FaceTime sign-outs and the Accessibility and Automation
    /// they need: irreversible, so they run last (System State design §3.5).
    Accounts = 5,
}

impl Stage {
    /// Run order: the sign-outs last, after Repositories.
    const ALL: [Self; 6] = [
        Self::Workstation,
        Self::Spotlight,
        Self::Permissions,
        Self::Secrets,
        Self::Repositories,
        Self::Accounts,
    ];

    /// Every stored bit a progress record may hold.
    const KNOWN: u16 = (1 << 6) - 1;

    const fn bit(self) -> u16 {
        1 << self as u8
    }

    fn selected(self, plan: &Plan) -> bool {
        match self {
            Self::Workstation => !preferences(plan.selection).is_empty(),
            Self::Spotlight => plan.selection.contains(Setting::Spotlight),
            Self::Permissions => plan.selection.contains(Setting::Permissions),
            Self::Secrets => plan.selection.contains(Setting::Secrets),
            Self::Repositories => plan.selection.contains(Setting::Repositories),
            Self::Accounts => {
                plan.selection.contains(Setting::Messages)
                    || plan.selection.contains(Setting::FaceTime)
            }
        }
    }

    const fn name(self) -> &'static str {
        self.row().name
    }

    /// The stage's row as System State reads it: its name and its settings.
    const fn row(self) -> aterm_types::macos_setup::StageRow {
        use aterm_types::macos_setup as stages;
        match self {
            Self::Workstation => stages::WORKSTATION_STAGE,
            Self::Spotlight => stages::SPOTLIGHT_STAGE,
            Self::Permissions => stages::PERMISSIONS_STAGE,
            Self::Secrets => stages::SECRETS_STAGE,
            Self::Repositories => stages::REPOSITORIES_STAGE,
            Self::Accounts => stages::ACCOUNTS_STAGE,
        }
    }

    /// The System State rows this stage changes under `plan` ([`Self::subjects`]): its settings
    /// the plan selects, and every grant.
    fn subjects_in(self, plan: &Plan) -> Vec<Subject> {
        self.subjects()
            .iter()
            .copied()
            .filter(|subject| match subject {
                Subject::Setting(setting) => plan.selection.contains(*setting),
                Subject::Access(_) => true,
                // A Sync row is no stage's subject; it rides the Secrets row only.
                Subject::Sync(_) => false,
            })
            .collect()
    }

    /// The System State rows a step of this stage that failed as a whole is said under (design
    /// §3.6), of `subjects`, the rows it changes in this pass ([`Self::subjects`]): the grants of
    /// Permissions, and of any other stage its settings' rows, never a grant it only needs (the
    /// sign-outs' Accessibility). [`Self::name`] is `aterm doctor`'s name for the stage.
    fn page_rows(self, subjects: &[Subject]) -> Vec<Subject> {
        subjects
            .iter()
            .copied()
            .filter(|subject| self == Self::Permissions || matches!(subject, Subject::Setting(_)))
            .collect()
    }

    /// The settings whose rows this stage changes, for System State's `running`.
    const fn settings(self) -> &'static [Setting] {
        self.row().settings
    }

    /// The System State rows this stage's work can change, read again once it ends.
    const fn subjects(self) -> &'static [Subject] {
        use aterm_types::macos_setup::Access;
        match self {
            Self::Workstation => &[
                Subject::Setting(Setting::CapsLock),
                Subject::Setting(Setting::NaturalScrolling),
                Subject::Setting(Setting::TrackpadScrolling),
                Subject::Setting(Setting::Desktop),
                Subject::Setting(Setting::Widgets),
                Subject::Setting(Setting::Dock),
            ],
            Self::Spotlight => &[Subject::Setting(Setting::Spotlight)],
            Self::Permissions => &[
                Subject::Access(Access::FullDiskAccess),
                Subject::Access(Access::DeveloperTools),
                Subject::Access(Access::Debugger),
            ],
            Self::Secrets => &[Subject::Setting(Setting::Secrets)],
            Self::Repositories => &[Subject::Setting(Setting::Repositories)],
            Self::Accounts => &[
                Subject::Setting(Setting::Messages),
                Subject::Setting(Setting::FaceTime),
                Subject::Access(Access::Accessibility),
            ],
        }
    }
}

/// The workstation preferences a plan selects, without the two sign-outs, which are the Accounts
/// stage's.
pub(super) fn preferences(selection: Selection) -> Selection {
    let mut preferences = selection.workstation();
    preferences.set(Setting::Messages, false);
    preferences.set(Setting::FaceTime, false);
    preferences
}

/// The sign-outs a plan selects.
pub(super) fn accounts(selection: Selection) -> Selection {
    let mut accounts = Selection::none();
    for setting in [Setting::Messages, Setting::FaceTime] {
        accounts.set(setting, selection.contains(setting));
    }
    accounts
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Progress {
    done: u16,
    /// Every selected stage was done at the run's last reading. A pass, explicit or not, sets it
    /// only through [`publish_completion`], which reads the network stages again first; an empty
    /// plan is completed as it is staged, and an older aterm's verified pass as it finished
    /// ([`keep_older_pass`]).
    completed: bool,
    admitted: bool,
    running: Option<Stage>,
    plan: [u8; 32],
    revision: u64,
    /// A person stopped this revision between stages (System State's Stop): it
    /// ends without cancelling the plan, and a later admission runs normally.
    stopped: bool,
    /// The earlier revision whose admission of the Permissions group this one
    /// continues ([`carried_access`]), or `None` when it is this revision's own.
    access: Option<u64>,
    admitted_vault: AdmittedVault,
    /// The preferences this revision's Workstation stage sets, where a person's choice
    /// turned only some groups to Set up after the stage had finished the rest
    /// ([`chosen_keeps`]): the groups that stayed are not set again, so a change the
    /// person made to them since stays. `None`: every preference the plan selects.
    only: Option<Selection>,
    /// The selection of the plan this progress was staged for, which [`Self::plan`]
    /// binds ([`Self::bind`]): what the next choice is measured from when a choice before
    /// it was cut short after it saved its plan, before its progress, so the saved plan
    /// moved ahead of the run ([`Self::staged`]). `None` in a record an older aterm wrote.
    selection: Option<Selection>,
    /// The revision, admitted or not, whose unfinished run this one carries on with no choice
    /// of its own: a Restore's ([`leave_restored_as_is`]), the run a bring-forward stages for
    /// the launch's offer ([`restored_forward`]) and that offer's own answer of it
    /// ([`InitialSetupAuthorization::persist_offered_observed`]); `None` otherwise. A person's
    /// Stop names the run the window registered, so one naming that revision stops this one
    /// too, even when its save lands after this record ([`Self::named_by`]).
    continues: Option<u64>,
}

/// The plan a run's progress was staged for, as a choice measures from it
/// ([`Progress::staged`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Staged {
    /// That plan's selection.
    selection: Selection,
    /// Whether that plan's vault is the choice's.
    vault_kept: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdmittedVault {
    #[default]
    Unknown,
    None,
    Configured([u8; 32]),
}

/// What this attempt can hand Stop after its write authority ends. A confirmed
/// progress publication reserves its revision; an uncertain write reserves no number.
#[derive(Clone, Copy, Default)]
enum StopReservation {
    #[default]
    None,
    Unknown,
    Known(u64),
}

impl StopReservation {
    fn target(self) -> Result<Option<u64>, String> {
        match self {
            Self::None => Ok(None),
            Self::Unknown => Err("the failed setup publication could not be identified".into()),
            Self::Known(revision) => Ok(Some(revision)),
        }
    }

    fn writing(&mut self) {
        if !matches!(self, Self::Known(_)) {
            *self = Self::Unknown;
        }
    }
}

impl AdmittedVault {
    fn of(configured: Result<Option<ConfiguredVault>, String>) -> Self {
        match configured {
            Ok(None) => Self::None,
            Ok(Some(vault)) => Self::Configured(vault.identity()),
            Err(_) => Self::Unknown,
        }
    }

    fn stands_in(self, plan: &Plan, configured: &ConfiguredVault) -> bool {
        let since = match self {
            Self::Unknown => false,
            Self::None => true,
            Self::Configured(identity) => !configured.continues_identity(identity),
        };
        since
            && matches!(
                aterm_machines::init_secrets::selected_vault(
                    &plan.vault_url,
                    &configured.home,
                    Some(&configured.config),
                ),
                Ok(None)
            )
    }

    fn record(self) -> Option<Value> {
        match self {
            Self::Unknown => None,
            Self::None => Some(Value::Null),
            Self::Configured(identity) => Some(aterm_secrets::b64::encode_padded(&identity).into()),
        }
    }
}

fn plan_digest(plan: &Plan) -> [u8; 32] {
    let mut bytes = Vec::with_capacity(2 + plan.vault_url.len());
    bytes.extend_from_slice(&plan.selection.bits().to_le_bytes());
    bytes.extend_from_slice(plan.vault_url.as_bytes());
    aterm_digest::Sha256::digest(&bytes)
}

impl Progress {
    fn for_plan(plan: &Plan) -> Self {
        let mut progress = Self {
            completed: plan.selection.is_empty(),
            ..Self::default()
        };
        progress.bind(plan);
        progress
    }

    /// Bind this progress to `plan`: its digest, and its selection ([`Self::selection`]).
    fn bind(&mut self, plan: &Plan) {
        self.plan = plan_digest(plan);
        self.selection = Some(plan.selection);
    }

    fn belongs_to(self, plan: &Plan) -> bool {
        self.plan == plan_digest(plan)
    }

    /// The plan this progress was staged for, as a choice of `plan` measures from it
    /// ([`chosen_keeps`], [`carried_access`]): the saved plan (`saved`) when the progress
    /// is its own. A choice cut short after it saved its plan, before its progress, left
    /// the saved plan ahead of the run, and the run is still the one this progress
    /// records: then the selection the record keeps ([`Self::selection`]). Its vault is
    /// `plan`'s when the binding says so. `None` for a record an older aterm wrote that
    /// belongs to no plan saved here.
    fn staged(self, saved: Option<&Plan>, plan: &Plan) -> Option<Staged> {
        let selection = match saved {
            Some(saved) if self.belongs_to(saved) => saved.selection,
            _ => self.selection?,
        };
        Some(Staged {
            selection,
            vault_kept: self.belongs_to(&Plan {
                selection,
                vault_url: plan.vault_url.clone(),
            }),
        })
    }

    fn next_for_plan(self, root: &Path, plan: &Plan) -> Result<Self, String> {
        Ok(Self {
            revision: revision::next(root, self.revision)?,
            ..Self::for_plan(plan)
        })
    }

    /// The revision whose access steps this run's Permissions stage reads, and
    /// the window's access lanes key their hand-overs on (System State design §6).
    const fn access_revision(self) -> u64 {
        match self.access {
            Some(access) => access,
            None => self.revision,
        }
    }

    /// Whether a person stopped this revision: saved in the record, or asked for
    /// in a `request` no pass has folded in yet ([`request_stop_at`]). A run
    /// whose admission and every stage finished is completed, not stopped: a Stop asked for
    /// during its last stage, or after it, came too late to stop anything.
    fn stopped_by(self, request: Option<StopRequest>) -> bool {
        self.stopped || (!(self.admitted && self.completed) && self.named_by(request))
    }

    /// Whether `request` names this revision, or the run it carries on ([`Self::continues`]).
    fn named_by(self, request: Option<StopRequest>) -> bool {
        request.is_some_and(|request| {
            request.names(self.revision) || self.continues.is_some_and(|run| request.names(run))
        })
    }
}

/// A person's Stop that no pass has folded into the progress record yet
/// (System State design §3.3). It is written the moment the Stop is asked for,
/// outside profile.lock, so a quit before the running stage ends still never
/// resumes the run the person turned away from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StopRequest {
    /// The Stop of this run revision.
    Revision(u64),
    /// A request that cannot be read stops whatever run is current: a Stop
    /// is never read as permission to go on.
    Unreadable,
}

impl StopRequest {
    const fn names(self, revision: u64) -> bool {
        match self {
            Self::Revision(stopped) => stopped == revision,
            Self::Unreadable => true,
        }
    }
}

const STOP_REQUEST: &str = "stop-request.json";
const STOP_REQUEST_LOCK: &str = "stop-request.lock";

/// Ask, durably and at once, for run `revision` to stop. The caller need not
/// hold profile.lock: only the lock's holder writes progress, so the request is
/// a file of its own, which every reader of the run's end honors and the next
/// pass folds in ([`fold_stop_request`]). Its short lock serializes publication
/// and removal; a stale process never replaces a newer revision's Stop.
pub(super) fn request_stop_at(root: &Path, revision: u64) -> Result<(), String> {
    request_stop_with_flush(root, revision, durable_name)
}

fn request_stop_with_flush(
    root: &Path,
    revision: u64,
    flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let _lock = HeldLock::take_named(root, STOP_REQUEST_LOCK)?;
    let path = root.join(STOP_REQUEST);
    match stop_request_at(root) {
        Some(StopRequest::Revision(current)) if current >= revision => {
            // An earlier failed save may have published this exact name.
            // Observing it alone cannot certify that a Stop was saved.
            flush_record_with(&path, flush)
        }
        Some(StopRequest::Unreadable) => Err(
            "the existing System State stop request cannot be read; it was left unchanged".into(),
        ),
        _ => write_json_with_flush(
            &path,
            &json!({"profile":PROFILE,"revision":revision}),
            flush,
        ),
    }
}

/// The Stop no pass has folded in yet. Reads only: no durability flush, since
/// honoring a request that was not flushed can only stop a run, never start one.
fn stop_request_at(root: &Path) -> Option<StopRequest> {
    let bytes = match super::read_record_bytes(&root.join(STOP_REQUEST)) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(StopRequest::Unreadable),
    };
    let request = aterm_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|value| value["profile"].as_str() == Some(PROFILE))
        .and_then(|value| value["revision"].as_u64())
        .map_or(StopRequest::Unreadable, StopRequest::Revision);
    Some(request)
}

/// Under profile.lock: fold a Stop no pass has taken into the run it names, so
/// the progress record says so for every reader (an older aterm's too), then
/// remove it. A request for any other revision is spent and only removed.
fn fold_stop_request(root: &Path) -> Result<(), String> {
    // A put-back cut short is finished first: its run, not the widened one, takes the Stop.
    scope::settle_put_back(root)?;
    let Some(request) = stop_request_at(root) else {
        return Ok(());
    };
    if let Some(mut progress) = progress(root)?
        && !progress.completed
        && (progress.named_by(Some(request)) || progress.stopped)
    {
        progress.stopped = true;
        progress.running = None;
        // An upgrade can derive this Stop from its predecessor until the physical next-run
        // record catches up: persist it before removing the request, or a later read could lose
        // the Stop.
        if progress_file(root)? != Some(progress) {
            save_progress(root, progress)?;
        }
    }
    settle_stop_request(root, request)
}

/// Remove the request the caller folded, unless a newer one replaced it since.
fn settle_stop_request(root: &Path, folded: StopRequest) -> Result<(), String> {
    settle_stop_request_with(root, folded, || Ok(()))
}

fn settle_stop_request_with(
    root: &Path,
    folded: StopRequest,
    before_remove: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let _lock = HeldLock::take_named(root, STOP_REQUEST_LOCK)?;
    settle_stop_request_locked(root, folded, before_remove)
}

/// Settle a folded request while the caller holds `stop-request.lock`.
fn settle_stop_request_locked(
    root: &Path,
    folded: StopRequest,
    before_remove: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if stop_request_at(root) != Some(folded) {
        return Ok(());
    }
    before_remove()?;
    match std::fs::remove_file(root.join(STOP_REQUEST)) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot settle the System State stop: {error}")),
    }
}

/// Ask the admitted run `revision` to stop at its next stage boundary (System State's Stop, or a
/// choice that replaces its plan). It is durable when this returns, so a relaunch reads the run
/// as stopped; it takes only the short Stop-record lock and probes nothing.
pub fn request_initial_setup_stop(revision: u64) -> Result<(), String> {
    ensure_person_scope()?;
    request_stop_at(&state_root()?, revision)
}

/// A person pressed Check again: lift every stage of the current run that aterm parked after it
/// stopped twice there or after it ran past the hang limit, so the run's next pass runs it again
/// (design §6.5), and have that pass start every other automatic try of the run afresh (the
/// vault's, the Mac settings'), as if none had been made ([`tries_afresh`]). Only the ledger's
/// short lock is taken, so a pass running now is not waited for. Nothing automatic calls this:
/// the page's own Check reads the rows and lifts nothing.
pub fn retry_parked_setup_stages() -> Result<(), String> {
    ensure_person_scope()?;
    ledger::retry_parked_at(&state_root()?).map(|_| ())
}

/// `worker`'s durable pace between its passes of run `revision` ([`PassRetry`]): what a worker
/// that starts (a launch, a relaunch after an update or a reboot) goes on from. Unreadable, or
/// out of a person's scope, it reads as no pass retried, so the next pass goes at once.
pub fn initial_setup_pass_retry(worker: PassWorker, revision: u64) -> PassRetry {
    ensure_person_scope()
        .and_then(|()| state_root())
        .map_or_else(
            |_| PassRetry::default(),
            |root| pass_retry::load(&root, worker, revision),
        )
}

/// Keep `worker`'s pace between its passes of run `revision` ([`PassRetry`]) on disk; a pace of
/// no try removes that worker's record alone.
///
/// # Errors
/// Out of a person's scope, or the record cannot be written or removed.
pub fn note_initial_setup_pass_retry(
    worker: PassWorker,
    revision: u64,
    retry: PassRetry,
) -> Result<(), String> {
    ensure_person_scope()?;
    pass_retry::save(&state_root()?, worker, revision, retry)
}

/// The pass of `revision` holding profile.lock found a person's Check again since the last one
/// ([`ledger::Ledger::take_afresh`]): every automatic try of the run starts afresh, so its next
/// try is now rather than at the end of its pause. What was set and read back stays settled.
///
/// # Errors
/// A record cannot be read, written or removed.
fn tries_afresh(root: &Path, revision: u64) -> Result<(), String> {
    secrets_retry::afresh(root, revision)?;
    super::mac_settings_afresh(root, revision)
}

/// aterm is quitting, or an update's handoff is about to write its Commit: the setup stages
/// running now are cut off by no crash, so the next launch resumes them without counting this
/// begin toward a park (design §6.5). Only the ledger's short lock is taken.
pub fn note_setup_interrupted() -> Result<(), String> {
    ensure_person_scope()?;
    ledger::interrupt_at(&state_root()?).map(|_| ())
}

/// The handoff [`note_setup_interrupted`] noted did not go (its Commit was not written, or
/// returned): the notes are taken back, so aterm stopping in one of those stages later counts
/// toward their park (design §6.5). Only the ledger's short lock is taken.
pub fn take_back_setup_interrupted() -> Result<(), String> {
    ensure_person_scope()?;
    ledger::refused_at(&state_root()?).map(|_| ())
}

/// The access revision a new revision of `plan` continues, when its prior run (its
/// `progress`, staged for the saved plan `previous` or, after a choice cut short once it
/// saved its plan, for the plan before it: [`Progress::staged`]) managed Permissions too:
/// a change to another group is not a new approval of the grants, so it hands nothing to
/// macOS again and a Not now stays answered (System State design §3.3, §6). Turning the
/// Permissions group off and on again is a new approval.
fn carried_access(previous: Option<&Plan>, progress: Option<Progress>, plan: &Plan) -> Option<u64> {
    let progress = progress?;
    let staged = progress.staged(previous, plan)?;
    (staged.selection.contains(Setting::Permissions)
        && plan.selection.contains(Setting::Permissions))
    .then(|| progress.access_revision())
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "initial setup run revision is exhausted".into())
}

/// What every reader of a saved setup record answers for one a newer aterm wrote: a setting
/// or a stage this build does not know, or a newer format ([`FORMAT`]). Such a record is
/// sound, so a set-aside leaves it as it is ([`set_aside`]), and this build changes nothing of
/// the setup it belongs to: each write reads the records first and is refused. System State
/// says so in one calm line ([`InitialSetupSnapshotError::Newer`]).
pub const NEWER_SETUP: &str = "a newer aterm saved this Mac's setup; this aterm leaves it as it is";

/// Whether `error`, from any read or write of the saved setup, is [`NEWER_SETUP`].
#[must_use]
pub fn setup_is_newer(error: &str) -> bool {
    error.contains(NEWER_SETUP)
}

/// The format of the setup records this build writes, in their `format` field (none: this
/// one). A field this build does not know, in a record of a format it knows, is one a newer
/// aterm lets an older build read around, and drop when it writes the record again; a newer
/// aterm that needs a field kept writes a newer format, which this build never writes over.
const FORMAT: u64 = 1;

/// `record` with this build's [`FORMAT`].
pub(super) fn stamped(mut record: Value) -> Value {
    if let Some(fields) = record.as_object_mut() {
        fields.insert("format".into(), Value::from(FORMAT));
    }
    record
}

/// Whether `record` is of a format this build reads: a newer one is [`NEWER_SETUP`].
pub(super) fn known_format(record: &Value) -> Result<(), String> {
    match record.get("format") {
        None => Ok(()),
        Some(format) => match format.as_u64() {
            Some(format) if format <= FORMAT => Ok(()),
            Some(_) => Err(NEWER_SETUP.into()),
            None => Err("invalid setup record format".into()),
        },
    }
}

/// Whether a record's `version` is the one this build writes: a later one is a newer aterm's
/// ([`NEWER_SETUP`]), anything else `invalid`.
fn known_version(version: &Value, invalid: &str) -> Result<(), String> {
    match version.as_u64() {
        Some(1) => Ok(()),
        Some(version) if version > 1 => Err(NEWER_SETUP.into()),
        _ => Err(invalid.into()),
    }
}

/// The bits of `field` within `known`: one past them is a newer aterm's ([`NEWER_SETUP`]);
/// anything but a whole number is `invalid`.
fn known_bits(field: &Value, known: u16, invalid: &str) -> Result<u16, String> {
    let bits = field.as_u64().ok_or_else(|| invalid.to_owned())?;
    if bits & !u64::from(known) != 0 {
        return Err(NEWER_SETUP.into());
    }
    u16::try_from(bits).map_err(|_| invalid.to_owned())
}

/// The selection `field` keeps, as [`known_bits`] reads it.
pub(super) fn known_selection(field: &Value, invalid: &str) -> Result<Selection, String> {
    Selection::from_bits(known_bits(field, Selection::all().bits(), invalid)?)
        .ok_or_else(|| invalid.to_owned())
}

fn progress(root: &Path) -> Result<Option<Progress>, String> {
    scope::upgrade_progress(root, progress_file(root)?)
}

/// The physical progress publication, before an interrupted upgrade's fallback.
fn progress_file(root: &Path) -> Result<Option<Progress>, String> {
    let path = root.join("initial-setup.json");
    let bytes = match super::read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid initial setup progress record")?;
    let progress = parse_progress(&value)?;
    flush_record_with(&path, durable_name)?;
    Ok(Some(progress))
}

fn parse_progress(value: &Value) -> Result<Progress, String> {
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized initial setup profile".into());
    }
    known_format(value)?;
    // More stages than this build runs, or one it does not know, are a newer aterm's.
    if value
        .get("stages")
        .and_then(Value::as_u64)
        .is_some_and(|stages| stages > Stage::ALL.len() as u64)
    {
        return Err(NEWER_SETUP.into());
    }
    let mut done = known_bits(
        &value["done"],
        Stage::KNOWN,
        "unrecognized initial setup progress",
    )?;
    // A record without `stages` (an older aterm's) ran the sign-outs inside the Workstation
    // stage: its finished Workstation finished them too.
    if value.get("stages").is_none() && done & Stage::Workstation.bit() != 0 {
        done |= Stage::Accounts.bit();
    }
    let stopped = match value.get("stopped") {
        None => false,
        Some(value) => value.as_bool().ok_or("invalid initial setup stop")?,
    };
    let completed = match value["state"].as_str() {
        Some("pending") => false,
        Some("completed") => true,
        _ => return Err("unrecognized initial setup state".into()),
    };
    let admitted = match value.get("admitted") {
        None => completed,
        Some(value) => value.as_bool().ok_or("invalid initial setup admission")?,
    };
    let running = match value.get("running") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let bits = known_bits(value, Stage::KNOWN, "unrecognized running setup stage")?;
            Some(
                Stage::ALL
                    .into_iter()
                    .find(|stage| bits == stage.bit())
                    .ok_or("unrecognized running setup stage")?,
            )
        }
    };
    let plan = match value.get("plan") {
        None => [0; 32],
        Some(value) => value
            .as_str()
            .and_then(aterm_secrets::b64::decode_padded)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or("invalid initial setup plan binding")?,
    };
    let revision = match value.get("revision") {
        None => 0,
        Some(value) => value.as_u64().ok_or("invalid initial setup run revision")?,
    };
    let access = match value.get("access_revision") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|access| *access <= revision)
                .ok_or("invalid initial setup access revision")?,
        ),
    };
    let admitted_vault = match value.get("admitted_vault") {
        None => AdmittedVault::Unknown,
        Some(Value::Null) => AdmittedVault::None,
        Some(value) => AdmittedVault::Configured(
            value
                .as_str()
                .and_then(aterm_secrets::b64::decode_padded)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or("invalid initial setup vault at admission")?,
        ),
    };
    let only = match value.get("workstation_only") {
        None => None,
        Some(value) => Some(known_selection(
            value,
            "invalid initial setup preference scope",
        )?),
    };
    let selection = match value.get("plan_selection") {
        None => None,
        Some(value) => Some(known_selection(
            value,
            "invalid initial setup plan selection",
        )?),
    };
    let continues = match value.get("continues_revision") {
        None => None,
        Some(value) => Some(
            value
                .as_u64()
                .filter(|run| *run < revision)
                .ok_or("invalid initial setup run carried on")?,
        ),
    };
    Ok(Progress {
        done,
        completed,
        admitted,
        running,
        plan,
        revision,
        stopped,
        access,
        admitted_vault,
        only,
        selection,
        continues,
    })
}

fn save_progress(root: &Path, progress: Progress) -> Result<(), String> {
    carry_older_person_access(root)?;
    revision::reserve(root, progress.revision)?;
    // The login job goes with the record ([`resume::follow`]): one that owes a pass is never
    // published without it, whatever stops aterm after.
    resume::follow(root, &progress, || {
        write_json(&root.join("initial-setup.json"), &progress_record(progress))
    })
}

fn save_progress_observing(
    root: &Path,
    progress: Progress,
    published: &mut (impl FnMut(u64) + ?Sized),
) -> Result<(), String> {
    save_progress_observing_with(root, progress, published, save_progress)
}

fn save_progress_observing_with(
    root: &Path,
    progress: Progress,
    published: &mut (impl FnMut(u64) + ?Sized),
    write: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<(), String> {
    // The caller holds profile.lock. A rename can land before its durability
    // error: only this exact canonical candidate is then evidence for Stop.
    // Reporting it neither admits a stage nor changes the original write result.
    let result = write(root, progress);
    if result.is_ok() || progress_candidate_is_visible(root, progress) {
        published(progress.revision);
    }
    result
}

fn progress_candidate_is_visible(root: &Path, progress: Progress) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let Ok(expected) = aterm_json::to_vec(&progress_record(progress)) else {
        return false;
    };
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(root.join("initial-setup.json"))
    else {
        return false;
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == expected.len() as u64)
    {
        return false;
    }
    let mut actual = Vec::with_capacity(expected.len());
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut actual)
        .is_ok()
        && actual == expected
}

#[cfg(any(test, debug_assertions))]
fn progress_publication_error(
    root: &Path,
    progress: Progress,
    after_rename: bool,
) -> Result<(), String> {
    if !after_rename {
        return Err("injected failure before Progress publication".into());
    }
    revision::reserve(root, progress.revision)?;
    write_json_with_flush(
        &root.join("initial-setup.json"),
        &progress_record(progress),
        |_, _| {
            Err(std::io::Error::other(
                "injected Progress namespace flush failure",
            ))
        },
    )
}

fn progress_record(progress: Progress) -> Value {
    let mut record = stamped(
        json!({"profile":PROFILE,"state":if progress.completed {"completed"} else {"pending"},PROGRESS_BY_PRESS:true,"admitted":progress.admitted,"done":progress.done,"running":progress.running.map(Stage::bit),"plan":aterm_secrets::b64::encode_padded(&progress.plan),"revision":progress.revision,"stages":Stage::ALL.len()}),
    );
    if let Some(fields) = record.as_object_mut() {
        if progress.stopped {
            fields.insert("stopped".into(), Value::Bool(true));
        }
        if progress.access_revision() != progress.revision {
            fields.insert(
                "access_revision".into(),
                Value::from(progress.access_revision()),
            );
        }
        if let Some(vault) = progress.admitted_vault.record() {
            fields.insert("admitted_vault".into(), vault);
        }
        if let Some(only) = progress.only {
            fields.insert("workstation_only".into(), Value::from(only.bits()));
        }
        if let Some(selection) = progress.selection {
            fields.insert("plan_selection".into(), Value::from(selection.bits()));
        }
        if let Some(run) = progress.continues {
            fields.insert("continues_revision".into(), Value::from(run));
        }
    }
    record
}

fn saved_plan(root: &Path) -> Result<Option<Plan>, String> {
    Ok(saved_plan_record(root)?.map(|(plan, _)| plan))
}

fn saved_plan_record(root: &Path) -> Result<Option<(Plan, bool)>, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match super::read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid automatic setup plan")?;
    let plan = parse_plan_record(&value)?;
    flush_record_with(&path, durable_name)?;
    Ok(Some(plan))
}

fn parse_plan_record(value: &Value) -> Result<(Plan, bool), String> {
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic setup profile".into());
    }
    known_format(value)?;
    let selection = known_selection(
        &value["selection"],
        "unrecognized automatic setup selection",
    )?;
    let plan = Plan {
        selection,
        vault_url: match value.get("vault_url") {
            None => aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
            Some(value) => value
                .as_str()
                .ok_or("invalid automatic setup vault source")?
                .into(),
        },
    };
    validate_plan(&plan)?;
    Ok((plan, value.get("vault_url").is_some()))
}

fn validate_plan(plan: &Plan) -> Result<(), String> {
    if !plan.selection.contains(Setting::Secrets) {
        return Ok(());
    }
    let home = Path::new("/");
    let (source, _) = aterm_secrets::transport::parse_source(&plan.vault_url, home)
        .map_err(|_| "the vault source must be an http(s) URL or an absolute or ~/ file path")?;
    aterm_secrets::transport::vault_name(&source)
        .map_err(|_| "the vault source must end in a valid vault name")?;
    Ok(())
}

pub fn validate_initial_setup_plan(plan: &Plan) -> Result<(), String> {
    validate_plan(plan)
}

/// Save `plan` as a person's ([`save_plan_as`]).
#[cfg(test)]
fn save_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    save_plan_as(root, plan, false)
}

/// Save `plan`; `provisional` marks one aterm chose by itself at first launch
/// (ALab scope, System State design §3.5), which a later launch may widen. The
/// field is written only when set: an absent field is a plan a person shaped.
fn save_plan_as(root: &Path, plan: &Plan, provisional: bool) -> Result<(), String> {
    validate_plan(plan)?;
    carry_older_person_access(root)?;
    scope::settle_put_back(root)?;
    let mut record = plan_record(plan, provisional);
    scope::keep_upgrade(root, plan, &mut record)?;
    write_json(&root.join("automatic-plan.json"), &record)
}

fn plan_record(plan: &Plan, provisional: bool) -> Value {
    let mut record = stamped(
        json!({"profile":PROFILE,"selection":plan.selection.bits(),"vault_url":plan.vault_url}),
    );
    if provisional && let Some(fields) = record.as_object_mut() {
        fields.insert("provisional".into(), Value::Bool(true));
    }
    record
}

/// Whether the saved plan is one aterm chose by itself ([`save_plan_as`]). A plan a Stop put
/// back is the person's whatever its record says: an earlier build saved it provisional
/// ([`scope::put_back_held_initial_setup`]), and the Stop declined the widen for good.
fn saved_plan_provisional(root: &Path) -> Result<bool, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match super::read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid automatic setup plan")?;
    if scope::put_back(&value) {
        return Ok(false);
    }
    match value.get("provisional") {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "invalid automatic setup plan origin".into()),
    }
}

/// `aterm doctor --apply`'s and a whole `aterm init`'s plan: the plan in force with every Mac
/// setting the pass sets (`on`) turned on, every other row as that plan had it, aterm's own
/// Permissions included (a person's Leave as is stays, System State design principle 4), and
/// with the person's Cancel standing aterm's permissions left as they are. On a Mac with no setup
/// records the plan starts from aterm's own for this Mac, the base plan every Mac syncs
/// ([`aterm_types::macos_setup::BASE_PLAN`]) with `on` (which on an ALab Mac holds the
/// repositories too), and is saved as aterm's own (provisional, `scope`), never as a person's
/// choice: nothing it leaves out reads as turned off. A provisional plan stays aterm's own, as it
/// only widens. A Restore withdraws no permission, so after one the plan keeps them; the run a
/// Restore ended stays admitted, at every write of this one too, so the explicit pass finishes it
/// ([`complete_explicit_workstation_plan`]) and its deferred Widgets change follows. A finished
/// run whose plan this one leaves as it is, with no Stop or Cancel on record, keeps its progress
/// as it is: finished, so a pass that ends before it completes reopens nothing. A run it widened
/// or reopened has its login job kept at once ([`resume::keep_after_explicit`]), so one the pass
/// leaves unfinished goes on with no window and no person. What it returns is that completion's
/// to read.
pub(super) fn save_explicit_workstation_plan(
    root: &Path,
    on: Selection,
) -> Result<ExplicitRun, String> {
    let run = save_explicit_workstation_plan_with(root, on, |_| Ok(()))?;
    // Best effort: the definition the save wrote starts the job at the next login all the same.
    let _ = resume::keep_after_explicit(root);
    Ok(run)
}

/// What names the run in force at `root`: its revision and the digest of the plan it runs, or
/// `None` with no run on record. `aterm init` reads it as its whole pass saves its plan and again
/// before it records that run done: a run Set up now in System State restarted meanwhile is not
/// init's to mark done.
///
/// # Errors
/// The run's record cannot be read.
pub(super) fn run_in_force(root: &Path) -> Result<Option<(u64, [u8; 32])>, String> {
    Ok(progress(root)?.map(|progress| (progress.revision, progress.plan)))
}

/// What [`save_explicit_workstation_plan`] found, for [`complete_explicit_workstation_plan`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ExplicitRun {
    /// No Cancel held the run the explicit apply took over, so the apply admits it at its last
    /// write: a run nobody had admitted yet (one a Restore brought forward for the launch's
    /// offer, or a choice whose admission never landed) is then the person's explicit run, and
    /// the next launch reruns nothing it did.
    admits: bool,
}

/// One write of [`save_explicit_workstation_plan`] after a person's Stop, a Restore or a Cancel,
/// in its order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExplicitWrite {
    /// The run's next revision, under the decision that ended it.
    Revision,
    /// The first-run decision ([`save_workstation_authorization`]).
    Decision,
    /// The explicit plan.
    Plan,
    /// The run's progress, bound to that plan.
    Progress,
}

/// [`save_explicit_workstation_plan`], asking `before` ahead of each write: an error ends it
/// there, as a process that ends between two writes does (a test's).
fn save_explicit_workstation_plan_with(
    root: &Path,
    on: Selection,
    mut before: impl FnMut(ExplicitWrite) -> Result<(), String>,
) -> Result<ExplicitRun, String> {
    // Admission can retire a Restore's baseline. Check its pending originals
    // before any record changes, for both doctor Apply and init's whole profile.
    let required = super::required_restore_originals(root)?;
    if !required.is_empty() {
        super::load_backup_preserving_restore(&root.join("original-preferences.json"), &required)?;
    }
    // A System State Stop asked for the run this revives is part of its record.
    fold_stop_request(root)?;
    let stop = stopped_at(root)?;
    let stopped = stop.is_some();
    let cancelled = stop == Some(Stop::Cancelled);
    let run = ExplicitRun { admits: !cancelled };
    // Every row the pass does not set stays as the page presents it, not as the bits on disk
    // say: a plan an older aterm saved, which predates the Permissions group, presents aterm's
    // own permissions on (System State design §3.5), and a Restore presents what it restored.
    let records = Records::read(root)?;
    let presented = records
        .presented_plan_record()
        .map(|(plan, _)| plan.selection);
    // aterm's own plan stays its own: the pass only widens it. A plan a person's Cancel or
    // Restore presents is theirs, so every group the pass does not set keeps their hold.
    let own = aterms_own(&records);
    let Some(mut progress) = progress(root)? else {
        // No run on record: the plan an older aterm saved with none, kept as it was presented,
        // or with none saved aterm's own for this Mac.
        let (mut plan, automatic) = match saved_plan(root)? {
            Some(mut saved) => {
                if let Some(selection) = presented {
                    saved.selection = selection;
                }
                (saved, own)
            }
            None => (
                Plan::from(aterm_types::macos_setup::BASE_PLAN.union(on)),
                true,
            ),
        };
        for setting in Setting::ALL {
            if setting.is_workstation() && on.contains(setting) {
                plan.selection.set(setting, true);
            }
        }
        if cancelled {
            plan.selection.set(Setting::Permissions, false);
        }
        return restart_explicit(root, &plan, automatic).map(|_| run);
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    // A person's Stop of this run holds it: an explicit pass starts no run of its own over it
    // (System State design principle 4); Set up now in System State goes on.
    if progress.stopped && progress.belongs_to(&plan) {
        return Err(format!(
            "{STOPPED_IN_SYSTEM_STATE}, so nothing was changed; Set up now there goes on"
        ));
    }
    let (previous, prior) = (plan.clone(), progress);
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(root, &plan)?;
    } else if stopped {
        progress.revision = revision::next(root, progress.revision)?;
        progress.stopped = false;
    }
    let admitted = progress.admitted;
    if let Some(selection) = presented {
        plan.selection = selection;
    }
    if stopped {
        // The new revision lands before the decision it reads under. A Cancel's run runs no more
        // until this apply admits it; a Restore withdrew no permission, so its run keeps its
        // admission and its Permissions approval wherever this process ends.
        progress.admitted = admitted && !cancelled;
        progress.access = carried_access(Some(&previous), Some(prior), &previous);
        before(ExplicitWrite::Revision)?;
        save_progress(root, progress)?;
    }
    // The rows this pass sets (`on`) are on; every other row stays as the plan had it, so a
    // group the person turned off stays off (System State design principle 4).
    for setting in Setting::ALL {
        if setting.is_workstation() && on.contains(setting) {
            plan.selection.set(setting, true);
        }
    }
    // aterm's own Permissions stay as the plan had them: a person's Leave as is holds, and a
    // Cancel leaves them as they are.
    if cancelled {
        plan.selection.set(Setting::Permissions, false);
    }
    progress.access = carried_access(Some(&previous), Some(prior), &plan);
    before(ExplicitWrite::Decision)?;
    seal_older_person_access(root)?;
    save_workstation_authorization(root, &plan)?;
    before(ExplicitWrite::Plan)?;
    save_plan_as(root, &plan, own)?;
    // A finished run this pass leaves its plan and its scope as they are stays finished: the
    // pass sets only rows that read otherwise, and its completion records the run done again.
    // So a pass that ends before it verifies (a Ctrl-C, a closed terminal, a row that did not
    // take) reopens no stage that only a later launch or press would run again, and owes no
    // sign-out again. A run limited to one group, or one an older run's Stop still names, is
    // widened by this pass, so it reopens below.
    if prior.completed
        && !stopped
        && prior.only.is_none()
        && prior.continues.is_none()
        && prior.belongs_to(&previous)
        && prior.belongs_to(&plan)
    {
        return Ok(run);
    }
    progress.bind(&plan);
    progress.done &= !(Stage::Workstation.bit() | Stage::Accounts.bit());
    progress.completed = false;
    progress.running = None;
    // The explicit profile selects every preference, superseding any earlier
    // group-only retry even if this process ends before its first OS change.
    progress.only = None;
    // A person's explicit apply is a run of its own: an older run's Stop, which no longer names
    // the run in force, holds it no more.
    progress.continues = None;
    progress.admitted = admitted;
    before(ExplicitWrite::Progress)?;
    save_progress(root, progress)?;
    Ok(run)
}

/// The explicit pass's last write, once it set every row it names: it records the Mac settings
/// stages it did, whether or not its run is admitted, and admits its run unless a Cancel held
/// the run it took over (`run`, [`ExplicitRun::admits`]). So a run the next launch's offer
/// admits (one a Restore brought forward, a choice whose admission never landed, or one an older
/// aterm's Cancel held, which the explicit plan's decision replaced) never resets the Dock or
/// signs out again after `aterm doctor --apply` did (System State design §3.4); a Cancel's run
/// still waits for that offer, and its plan leaves aterm's own permissions as they are. The run
/// is completed as a pass completes it ([`publish_completion`]): a vault or checkout stage an
/// earlier pass finished is read again first, and the rows of any that no longer reads ready
/// are returned for the caller's report, that stage left to the next pass.
///
/// This one reads this Mac's vault and checkouts (the production entry points' completion,
/// [`CompleteExplicit`]).
///
/// # Errors
/// The run's records cannot be read or written.
pub(super) fn complete_explicit_workstation_plan(
    root: &Path,
    run: ExplicitRun,
) -> Result<Vec<Row>, String> {
    let reopened = complete_explicit_workstation_plan_with(&mut LiveInitial::live(), root, run)?;
    // A stage its reread reopened is the login job's to run again, at once; a completed run
    // ends the job. Best effort, as for the save.
    let _ = resume::keep_after_explicit(root);
    Ok(reopened)
}

/// How an explicit pass completes the window's run ([`complete_explicit_workstation_plan`]):
/// with this Mac's own readings in production, and over a test's inert reader
/// ([`complete_explicit_workstation_plan_for_test`]) in tests, so no fixture reads this Mac.
pub(super) type CompleteExplicit = fn(&Path, ExplicitRun) -> Result<Vec<Row>, String>;

/// The completion of a seam whose pass never completes the window's run: it refuses, and reads
/// nothing of this Mac's.
///
/// # Errors
/// Always.
#[cfg(any(test, debug_assertions))]
pub(super) fn completes_nothing(_root: &Path, _run: ExplicitRun) -> Result<Vec<Row>, String> {
    Err("this pass completes no run of the window's".into())
}

/// [`complete_explicit_workstation_plan`] over an inert reader that reads every vault and
/// checkout ready and touches nothing of this Mac's.
///
/// # Errors
/// The run's records cannot be read or written.
#[cfg(test)]
pub(super) fn complete_explicit_workstation_plan_for_test(
    root: &Path,
    run: ExplicitRun,
) -> Result<Vec<Row>, String> {
    complete_explicit_workstation_plan_with(&mut tests::Fake::default(), root, run)
}

fn complete_explicit_workstation_plan_with(
    host: &mut dyn InitialHost,
    root: &Path,
    run: ExplicitRun,
) -> Result<Vec<Row>, String> {
    let Some(mut progress) = progress(root)? else {
        return Ok(Vec::new());
    };
    let plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        return Ok(Vec::new());
    }
    progress.admitted |= run.admits;
    // The explicit pass signs out in the same pass as its preferences.
    progress.done |= Stage::Workstation.bit() | Stage::Accounts.bit();
    progress.running = None;
    Ok(publish_completion(host, root, &plan, &mut progress)?
        .into_iter()
        .flat_map(|(_, rows)| rows)
        .collect())
}

/// Record whether the run is completed, as every pass does ([`Progress::completed`]): the vault
/// and checkout stages an earlier pass finished are read again first
/// ([`revalidate_completed_network_stages`]), so a done bit left from before a final person step
/// (a sign-out, an explicit pass) never completes a run whose vault, repository inventory or
/// credential route moved since, and the run is completed only while every selected stage is
/// still done after that reading. Returns the stages that reading reopened with their rows, for
/// the caller to report; each runs again, guarded as ever, at the next pass.
fn publish_completion(
    host: &mut dyn InitialHost,
    root: &Path,
    plan: &Plan,
    progress: &mut Progress,
) -> Result<Vec<(Stage, Vec<Row>)>, String> {
    let reopened = revalidate_completed_network_stages(host, root, plan, progress)?;
    progress.completed = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(plan) || progress.done & stage.bit() != 0);
    save_progress(root, *progress)?;
    Ok(reopened)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfiguredVault {
    config: aterm_secrets::client::Config,
    home: PathBuf,
}

impl ConfiguredVault {
    fn read(paths: &aterm_secrets::client::Paths) -> Result<Option<Self>, String> {
        Ok(aterm_secrets::client::config(paths)
            .map_err(|error| error.to_string())?
            .map(|config| Self {
                config,
                home: paths.home.clone(),
            }))
    }

    fn identity(&self) -> [u8; 32] {
        self.identity_as(self.config.signer)
    }

    fn identity_as(&self, signer: Option<[u8; 32]>) -> [u8; 32] {
        let source = match &self.config.source {
            aterm_secrets::transport::Source::Url(url) => format!("url\0{url}"),
            aterm_secrets::transport::Source::Path(path) => {
                format!("path\0{}", path.to_string_lossy())
            }
        };
        let mut bytes = format!(
            "aterm.initial-setup.vault-authority/1\0{source}\0{}\0",
            self.config.name
        )
        .into_bytes();
        match signer {
            Some(signer) => {
                bytes.push(1);
                bytes.extend_from_slice(&signer);
            }
            None => bytes.push(0),
        }
        aterm_digest::Sha256::digest(&bytes)
    }

    /// The exact admitted authority, or a predecessor the client's verified
    /// lineage still admits. Older source-only stamps cannot prove this.
    fn continues_identity(&self, identity: [u8; 32]) -> bool {
        self.identity() == identity
            || self.config.followed.iter().copied().any(|signer| {
                aterm_secrets::client::pin_admitted(&self.config, Some(signer))
                    && self.identity_as(Some(signer)) == identity
            })
    }

    fn left_in_place(&self, plan: &Plan) -> String {
        let joined = vault_label(&self.config.name, &self.config.source);
        let named = aterm_secrets::transport::parse_source(&plan.vault_url, &self.home)
            .ok()
            .and_then(|(source, _)| {
                let name = aterm_secrets::transport::vault_name(&source).ok()?;
                Some((source, name))
            });
        match named {
            Some((source, name)) if source == self.config.source && name == self.config.name => {
                format!(
                    "This Mac is joined to {joined} under another signer. Setup keeps it. To switch, type the vault with its #<signer> and press Join."
                )
            }
            named => format!(
                "This Mac is joined to {joined}. Setup keeps it. To switch to {}, type that vault and press Join.",
                named.map_or_else(
                    || "the vault setup names".into(),
                    |(source, name)| vault_label(&name, &source)
                )
            ),
        }
    }

    fn present(&self, mut plan: Plan) -> Plan {
        let names_this = matches!(
            aterm_machines::init_secrets::selected_vault(
                &plan.vault_url,
                &self.home,
                Some(&self.config),
            ),
            Ok(Some(_))
        );
        if !names_this {
            plan.vault_url = match &self.config.source {
                aterm_secrets::transport::Source::Url(url) => url.clone(),
                aterm_secrets::transport::Source::Path(path) => path.to_string_lossy().into_owned(),
            };
        }
        plan
    }
}

fn vault_label(name: &str, source: &aterm_secrets::transport::Source) -> String {
    let at = match source {
        aterm_secrets::transport::Source::Url(url) => aterm_secrets::transport::describe(url),
        aterm_secrets::transport::Source::Path(path) => path.display().to_string(),
    };
    format!("vault {name} ({at})")
}

fn live_configured_vault() -> Result<Option<ConfiguredVault>, String> {
    let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
    ConfiguredVault::read(&paths)
}

/// Whether `configured`, the vault this Mac is in, is the one `plan` names. One that cannot
/// be read is left to the vault item, which reads it again.
fn in_plan_vault(plan: &Plan, configured: Result<Option<ConfiguredVault>, String>) -> bool {
    match configured {
        Ok(Some(vault)) => matches!(
            aterm_machines::init_secrets::selected_vault(
                &plan.vault_url,
                &vault.home,
                Some(&vault.config),
            ),
            Ok(Some(_))
        ),
        Ok(None) => false,
        Err(_) => true,
    }
}

/// What the Secrets stage's `rows` heard of the ALab vault where they leave it off this network:
/// every row not in place is a vault nobody answered ([`UNREACHED`], read as
/// [`scope::VaultHere::Unreached`]) or one that answered that none is there ([`ABSENT`], read as
/// [`scope::VaultHere::Absent`] where no row went unanswered), by the rows' status alone.
/// `None` for any other rows.
fn off_network(rows: &[Row]) -> Option<scope::VaultHere> {
    let off = rows.iter().any(|row| !row_ready(row))
        && rows
            .iter()
            .all(|row| row_ready(row) || matches!(row.status.as_str(), UNREACHED | ABSENT));
    off.then(|| {
        if rows.iter().any(|row| row.status == UNREACHED) {
            scope::VaultHere::Unreached
        } else {
            scope::VaultHere::Absent
        }
    })
}

/// Whether the Secrets stage's `rows` leave the ALab vault off this network ([`off_network`]).
/// The Repositories stage then runs on this Mac's own GitHub credentials in the same pass instead
/// of waiting for a vault this network may never have, and waits for the vault's token only when
/// this Mac has none to sign in with; a join waiting for an admin, or a vault still syncing, keeps
/// them waiting for its token. An explicitly disabled swarm also leaves repositories on local
/// credentials.
fn vault_not_here(rows: &[Row]) -> bool {
    (!rows.is_empty()
        && rows
            .iter()
            .all(|row| row.detail == aterm_secrets::swarm::SWARM_OFF))
        || off_network(rows).is_some()
}

/// The Repositories row while they wait for the selected vault's token: [`WAITING`] while the
/// vault stage parked (`vault`), else blocked, which the run's retry tries again.
fn vault_token_wait(vault: SetupNext) -> Row {
    Row {
        name: Stage::Repositories.name().into(),
        status: if vault == SetupNext::Park {
            WAITING
        } else {
            "blocked"
        }
        .into(),
        detail: "waiting for the selected secrets vault to synchronize credentials".into(),
        elapsed_ms: None,
    }
}

/// A bootstrap row whose fallback look stood aside reads as the look's observation, by the
/// stand-aside's kind, with the look's words. A vault that did not answer it either
/// ([`aterm_converge::StoodAside::Offline`]) is [`UNREACHED`]; a server there holding no vault,
/// or another vault ([`aterm_converge::StoodAside::Absent`]), is [`ABSENT`]. Either parks the
/// pass, and the Secrets retry pause asks again by itself.
fn unanswered_join_rows(reports: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    let blocked = |report: &aterm_machines::model::ItemReport| {
        report.outcome == aterm_machines::model::Outcome::Blocked
    };
    let parked: Vec<Option<&'static str>> = reports
        .iter()
        .map(|report| match &report.stood_aside {
            Some(aterm_converge::StoodAside::Offline(_)) if blocked(report) => Some(UNREACHED),
            Some(aterm_converge::StoodAside::Absent(_)) if blocked(report) => Some(ABSENT),
            _ => None,
        })
        .collect();
    secrets_rows(reports)
        .into_iter()
        .zip(parked)
        .map(|(row, parked)| match parked {
            Some(status) => Row {
                status: status.into(),
                ..row
            },
            None => row,
        })
        .collect()
}
/// The plan in force as offered and shown: the `saved` one (or every group, before any is saved)
/// in the vault this Mac is `configured` for now. A vault configuration aterm cannot read (a
/// root-owned `source.toml` from a `sudo aterm secrets …`, a newer format) reads as no vault
/// here, as [`in_plan_vault`] leaves it to the vault item: the plan keeps the vault it names,
/// and the Secrets vault row says on its own that aterm cannot read this Mac's vault settings
/// ([`observe::VAULT_UNREAD`]). Permissions and every other group go ahead (System State design
/// §3.3).
fn offered_plan(
    saved: Option<(Plan, bool)>,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Plan {
    let plan = saved.map_or_else(Plan::default, |(plan, _)| plan);
    match configured {
        Ok(Some(vault)) => vault.present(plan),
        Ok(None) | Err(_) => plan,
    }
}

/// Whether the plan in force keeps this Mac's vault in sync (System State design §3.3, Secrets
/// vault): it has the Secrets vault at Set up (every group, before any plan is saved, as
/// [`automatic_plan`] reads it), no person's Stop holds its run, on record or asked for
/// ([`Records::stopped_run`]), and no older aterm's Cancel stands. A Restore holds nothing here:
/// it leaves credentials as they are. The launch's mend of the vault's sync process runs only
/// while this holds. Reads aterm's setup records only.
pub fn initial_setup_keeps_vault_sync() -> Result<bool, String> {
    keeps_vault_sync_at(&state_root()?)
}

pub(super) fn keeps_vault_sync_at(root: &Path) -> Result<bool, String> {
    let records = observation_at(root)
        .map_err(|error| error.to_string())?
        .records;
    Ok(keeps_vault_sync(&records))
}

/// Why a person's Stop holds every group of `aterm init` and `aterm doctor --apply` (System
/// State design principle 4): their words for it.
pub const STOPPED_IN_SYSTEM_STATE: &str = "you stopped setup in System State";

/// What the person's own choices in System State hold of `aterm init`'s and `aterm doctor
/// --apply`'s pass (System State design principle 4, init design §7): the settings the page
/// shows at Leave as is in a plan the person shaped, or that their Cancel or Restore presents
/// so, and their Stop of setup, on record or asked for. A plan aterm chose by itself holds
/// nothing, and neither does a Mac with no setup records; records that cannot be read hold
/// every setting, saying so. Reads aterm's setup records only.
#[must_use]
pub fn initial_setup_holds() -> aterm_machines::init::Holds {
    if aterm_types::dirs::runs_under_private_state_root() {
        return aterm_machines::init::Holds::default();
    }
    match state_root() {
        Ok(root) => holds_at(&root),
        Err(why) => unread_holds(&why),
    }
}

/// The folder [`initial_setup_holds`] reads its records from, where it reads any: a change in it
/// can change the holds.
#[must_use]
pub fn initial_setup_records() -> Option<PathBuf> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return None;
    }
    state_root().ok()
}

/// Whether background default-vault bootstrap still belongs to this Mac's current setup.
/// Reads setup records only, without taking their HeldLock or the vault lock.
/// # Errors
/// Current setup records cannot be read; callers then hold bootstrap.
pub fn automatic_vault_bootstrap_allowed() -> Result<bool, String> {
    automatic_vault_bootstrap_allowed_at(&state_root()?)
}

fn automatic_vault_bootstrap_allowed_at(root: &Path) -> Result<bool, String> {
    let records = Records::read(root)?;
    Ok(!holds(&records).holds(Setting::Secrets)
        && records
            .presented_plan_record()
            .is_none_or(|(plan, _)| aterm_secrets::is_alab_default_url(&plan.vault_url)))
}

/// [`initial_setup_holds`] at `root`.
pub(super) fn holds_at(root: &Path) -> aterm_machines::init::Holds {
    match observation_at(root) {
        Ok(observation) => holds(&observation.records),
        Err(why) => unread_holds(&why.to_string()),
    }
}

/// The settings this Mac's setup takes at `root`: the plan every reader presents
/// ([`Records::presented_plan_record`]), and where that plan is aterm's own (provisional, no
/// Cancel or Restore of the person's) what aterm sets up on this Mac by itself too, every setting
/// on an ALab Mac (`alab`), as the next look for the ALab signal widens it (`scope::upgrade_of`);
/// with no plan saved, or records that cannot be read, that scope alone (the base plan
/// elsewhere). A plan the person shaped is theirs alone. `aterm doctor --check` judges these and `aterm doctor --apply` sets these up, less
/// what the person's choices hold ([`holds_at`]).
pub(super) fn plan_in_scope(root: &Path, alab: impl FnOnce() -> bool) -> Selection {
    let scope = |alab: bool| {
        if alab {
            Selection::all()
        } else {
            aterm_types::macos_setup::BASE_PLAN
        }
    };
    let Ok(observation) = observation_at(root) else {
        return scope(alab());
    };
    let records = &observation.records;
    match records.presented_plan_record() {
        None => scope(alab()),
        // A run the person stopped, or a standing Restore, widens nothing, as the window's watch
        // does not (`scope::upgrade_of`).
        Some((plan, _))
            if aterms_own(records)
                && !records.stopped_run()
                && records.stop_request != Some(StopRequest::Unreadable)
                && records.restored.is_none() =>
        {
            plan.selection.union(scope(alab()))
        }
        Some((plan, _)) => plan.selection,
    }
}

/// Whether the plan `records` present is aterm's own, chosen by itself for this Mac's scope
/// (provisional), with no Cancel or Restore of the person's presenting it: no one's choice.
fn aterms_own(records: &Records) -> bool {
    records.provisional
        && records.cancelled.is_none()
        && !matches!(records.decision, Decision::Stopped(_))
}

/// What records that cannot be read hold: every group, saying why; records a newer aterm saved
/// ([`NEWER_SETUP`]) hold every group as that newer aterm's setup, which this one leaves alone.
fn unread_holds(why: &str) -> aterm_machines::init::Holds {
    if setup_is_newer(why) {
        return aterm_machines::init::Holds {
            newer: true,
            ..aterm_machines::init::Holds::default()
        };
    }
    aterm_machines::init::Holds {
        stopped: Some(format!("System State's records cannot be read now ({why})")),
        ..aterm_machines::init::Holds::default()
    }
}

fn holds(records: &Records) -> aterm_machines::init::Holds {
    let stopped_run = records.stopped_run();
    let stopped = (stopped_run || records.stop_request == Some(StopRequest::Unreadable))
        .then(|| STOPPED_IN_SYSTEM_STATE.to_owned());
    // A plan aterm chose by itself (its scope's) is no one's choice; a Cancel or a Restore of
    // the person's is, whatever plan it presents.
    let mut off = Selection::none();
    if let Some((plan, _)) = records
        .presented_plan_record()
        .filter(|_| !aterms_own(records))
    {
        for setting in Setting::ALL {
            off.set(setting, !plan.selection.contains(setting));
        }
    }
    aterm_machines::init::Holds {
        off,
        stopped,
        stopped_revision: records
            .progress
            .filter(|_| stopped_run)
            .map(|progress| progress.continues.unwrap_or(progress.revision)),
        newer: false,
    }
}

fn keeps_vault_sync(records: &Records) -> bool {
    let selection = records
        .presented_plan_record()
        .map_or_else(Selection::all, |(plan, _)| plan.selection);
    selection.contains(Setting::Secrets)
        && records.decision != Decision::Stopped(Stop::Cancelled)
        && records.stop_request != Some(StopRequest::Unreadable)
        && !records.stopped_run()
}

/// Whether a run System State admitted still owes its Secrets stage while the plan in force
/// keeps this Mac's vault in sync ([`initial_setup_keeps_vault_sync`]): the run is admitted, not
/// completed and not stopped, and its Secrets stage is not done. That stage joins the vault, so
/// the converge pass's `sync.peers` leaves the join to it (a run waiting on a person before its
/// Secrets stage included) and joins only once the stage ran without one. Reads aterm's setup
/// records only.
pub fn initial_setup_owes_secrets_stage() -> Result<bool, String> {
    owes_secrets_stage_at(&state_root()?)
}

fn owes_secrets_stage_at(root: &Path) -> Result<bool, String> {
    let records = observation_at(root)
        .map_err(|error| error.to_string())?
        .records;
    Ok(owes_secrets_stage(&records))
}

fn owes_secrets_stage(records: &Records) -> bool {
    keeps_vault_sync(records)
        && records.progress.as_ref().is_some_and(|progress| {
            progress.admitted
                && !progress.completed
                && !progress.stopped
                && progress.done & Stage::Secrets.bit() == 0
        })
}

/// Run a local subscriber repair only while the current setup keeps the vault in sync.
/// The profile lock orders the work against a new choice or Restore; a busy profile returns
/// `None` without waiting. `work` receives the revision read with the saved Stop policy,
/// so the caller can also refuse an in-process Stop whose save has not landed yet.
/// The callback must perform local service work only, never a vault or other network request.
pub fn with_initial_setup_vault_sync<T>(work: impl FnOnce(u64) -> T) -> Result<Option<T>, String> {
    ensure_person_scope()?;
    with_vault_sync_at(&state_root()?, work)
}

fn with_vault_sync_at<T>(root: &Path, work: impl FnOnce(u64) -> T) -> Result<Option<T>, String> {
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Ok(None);
    };
    let records = Records::read(root)?;
    let revision = plan_revision(
        records.progress,
        records.plan.as_ref().map(|(plan, _)| plan),
    )?;
    if !keeps_vault_sync(&records) {
        return Ok(None);
    }
    Ok(Some(work(revision)))
}

#[cfg(any(test, debug_assertions))]
pub fn with_initial_setup_vault_sync_at<T>(
    root: &Path,
    work: impl FnOnce(u64) -> T,
) -> Result<Option<T>, String> {
    with_vault_sync_at(root, work)
}

/// The plan in force: the saved one in the vault this Mac is configured for now
/// ([`offered_plan`]), reading only aterm's own configuration and setup records.
pub fn automatic_plan() -> Result<Plan, String> {
    let records = observation_at(&state_root()?)
        .map_err(|error| error.to_string())?
        .records;
    Ok(offered_plan(
        records.presented_plan_record(),
        live_configured_vault(),
    ))
}

pub fn restart_initial_setup(plan: &Plan) -> Result<u64, String> {
    restart_initial_setup_observed(plan).result
}

pub fn restart_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<u64> {
    observe_person_mutation(Err(super::SETTING_UP.into()), |root| {
        restart_reading(root, plan, live_configured_vault)
    })
}

/// One explicit restart, retaining ownership only while its final admission save needs recovery.
#[derive(Default)]
pub struct InitialSetupRestart {
    started: bool,
    /// The plan is a person's choice of groups (or a Join), never provisional.
    chosen: bool,
    /// The settings the page changed since it last took the plan in force
    /// ([`Self::changing`]); `None`: the plan as sent.
    changed: Option<Selection>,
    final_commit: Option<RestartCommit>,
    stop: StopReservation,
    /// Said once, the moment this press finds the profile lock another setup holds and starts
    /// waiting for it ([`Self::noting_wait`]).
    waits: Option<Box<dyn FnMut() + Send>>,
}

struct RestartCommit {
    plan: Plan,
    staged: Records,
    admitted: Records,
    publications: observation::Publications,
    progress: Progress,
}

impl RestartCommit {
    fn same_records(&self, current: &Records, expected: &Records) -> bool {
        let without_old_stop = |records: &Records| {
            let mut records = records.clone();
            // A manual restart replaces earlier runs. Their asynchronous Stop
            // writers can finish after this admission's own failed save, without
            // revoking its choice. Every other field and Stop stays exact.
            if matches!(records.stop_request,
                Some(StopRequest::Revision(revision)) if revision < self.progress.revision)
            {
                records.stop_request = None;
            }
            records
        };
        without_old_stop(current) == without_old_stop(expected)
    }
}

impl InitialSetupRestart {
    /// The exact revision this attempt reserved, for Stop only, never admission or replay.
    pub fn stop_target(&self) -> Result<Option<u64>, String> {
        self.stop.target()
    }

    /// A restart of a plan a person (or a control client) shaped on System
    /// State, by a group choice or a Join: the plan is theirs from now on, even
    /// where it equals the provisional plan aterm chose by itself, so no later
    /// launch widens it (design §3.5). [`Self::default`] is Set up now, or the
    /// window resuming the plan in force, which leaves a provisional plan so.
    pub fn chosen() -> Self {
        Self {
            chosen: true,
            ..Self::default()
        }
    }

    /// A restart that takes only `changed` from the plan it is sent, onto the plan in force as
    /// it admits it (System State design §3.3): the settings of the groups the person pressed
    /// since the page last took the plan in force, a group pressed and pressed back included.
    /// A page left open while a Restore, `aterm doctor` or another window changed the plan
    /// then never sets again a group someone else turned to Leave as is, and never drops a
    /// press of its own. An empty `changed` runs the plan in force as it stands (Set up now);
    /// `None` takes the plan as sent.
    #[must_use]
    pub const fn changing(mut self, changed: Option<Selection>) -> Self {
        self.changed = changed;
        self
    }

    pub const fn pending(&self) -> bool {
        self.final_commit.is_some()
    }

    /// Say `waits` the moment this press finds the profile lock another setup holds and starts
    /// waiting for it ([`Self::persist_observed`]): System State shows the press waiting from
    /// then, never a run going (design §3.3).
    #[must_use]
    pub fn noting_wait(mut self, waits: impl FnMut() + Send + 'static) -> Self {
        self.waits = Some(Box::new(waits));
        self
    }

    /// A test's stand-in for this press finding the profile lock another setup holds: it says
    /// so as [`Self::persist_observed`] does ([`Self::noting_wait`]).
    #[cfg(any(test, debug_assertions))]
    pub fn finds_lock_held_for_test(&mut self) {
        if let Some(waits) = self.waits.as_mut() {
            waits();
        }
    }

    /// Admit the person's `plan` as the next run, waiting for the profile lock while another
    /// holds it, at most [`CHOICE_LOCK_WAIT`] ([`observe_person_choice`]), and saying so the
    /// moment it starts waiting ([`Self::noting_wait`]).
    pub fn persist_observed(
        &mut self,
        plan: &Plan,
        vault_typed: bool,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_person(|root, before| {
            self.persist_at(
                root,
                before,
                choice_lock_wait(),
                plan,
                vault_typed,
                live_configured_vault,
            )
        })
    }

    /// [`Self::persist_observed`] in `root`, `wait` waiting for the profile lock while another
    /// holds it ([`observe_choice_at`]) after this press says it waits ([`noting_first`]), and
    /// `read` reading which vault this Mac is in.
    fn persist_at(
        &mut self,
        root: &Path,
        before: Option<InitialSetupObservation>,
        wait: impl FnMut() -> bool,
        plan: &Plan,
        vault_typed: bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    ) -> InitialSetupObserved<Option<u64>> {
        let mut waits = self.waits.take();
        let noting = || {
            if let Some(waits) = waits.as_mut() {
                waits();
            }
        };
        let observed = observe_choice_at(root, before, noting_first(noting, wait), |root| {
            let admitted =
                self.persist_with(root, plan, vault_typed, read, save_progress, |root| {
                    observation_at(root).map_err(|error| error.to_string())
                });
            if matches!(admitted, Ok(Some(_))) {
                // A person chose: the note that aterm set its records aside goes. One left only
                // keeps that calm note up.
                let _ = set_aside::forget_at(root);
                // Set up now sets every key again, and a group turned on sets its own: the keys
                // that changed since aterm set them are forgotten there (design §6.6).
                let turned_on = if self.chosen {
                    Some(self.changed.map_or(plan.selection, |changed| {
                        filtered(changed, |setting| plan.selection.contains(setting))
                    }))
                } else {
                    None
                };
                // One that cannot be written leaves those rows reading Changed since aterm set
                // it, which says so; the admission stands.
                let _ = kept::forget_at(root, turned_on);
            }
            admitted
        });
        self.waits = waits;
        observed
    }

    fn persist_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        vault_typed: bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
        commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        if self.pending() {
            if !owns_publications(root, &self.final_commit.as_ref().unwrap().publications)? {
                self.final_commit = None;
                return Ok(None);
            }
            let current = observe(root)?;
            let owned = self.final_commit.as_ref().unwrap();
            if owned.plan != *plan
                || current.publications() != owned.publications
                || (!owned.same_records(&current.records, &owned.staged)
                    && !owned.same_records(&current.records, &owned.admitted))
            {
                self.final_commit = None;
                return Ok(None);
            }
            if owned.same_records(&current.records, &owned.admitted) {
                let revision = owned.progress.revision;
                self.final_commit = None;
                return Ok(Some(revision));
            }
        } else {
            if self.started {
                return Ok(None);
            }
            // The plan in force as every reader presents it: a Restore cut short before its
            // plan landed is the plan its record names ([`Records::restore_presentation`]).
            let records = observation_at(root)
                .map_err(|error| error.to_string())?
                .records;
            let in_force = cancelled_in_force(&records, self.changed)
                .or_else(|| records.presented_plan_record().map(|(plan, _)| plan));
            let chosen = onto_plan_in_force(in_force.as_ref(), plan, self.changed, vault_typed);
            // A vault configuration aterm cannot read (a root-owned or newer-format
            // `source.toml`) never refuses a group choice: it reads as no vault, as the startup's
            // offer reads it ([`InitialSetupAuthorization::offer_reading`]). The admission then
            // records no vault, never the older record's Unknown, so the Secrets step reads the
            // configuration again before it does anything, waits while it cannot, and never
            // replaces a vault it did not read ([`secrets_try`], [`AdmittedVault::stands_in`]).
            // Only a Join is refused: the vault it replaces is the one it cannot read, and as no
            // vault it would never join the one the person typed.
            let configured = match read() {
                Err(why) if vault_typed && Stage::Secrets.selected(&chosen) => {
                    return Err(format!("{VAULT_CONFIG_UNREAD}: {why}"));
                }
                configured => configured.or(Ok(None)),
            };
            let admitted_plan = match &configured {
                Ok(Some(vault)) if !vault_typed => shown_in_vault(
                    chosen,
                    &observation_at(root)
                        .map_err(|error| error.to_string())?
                        .records,
                    vault,
                ),
                _ => chosen,
            };
            let before = observation::publications(root)?;
            self.started = true;
            let previous_stop = self.stop;
            self.stop.writing();
            let mut proposed = None;
            let staged = stage_restart_with(
                root,
                &admitted_plan,
                Restart {
                    chosen: self.chosen,
                    joins: self.chosen && vault_typed,
                    automatic: false,
                },
                AdmittedVault::of(configured),
                |root, progress| {
                    proposed = Some(progress);
                    save_progress(root, progress)?;
                    self.stop = StopReservation::Known(progress.revision);
                    Ok(())
                },
                |_| Ok(()),
            );
            let mut next = match staged {
                Ok(next) => next,
                Err(why) => {
                    if !matches!(self.stop, StopReservation::Known(_)) {
                        match observation::publications(root) {
                            Ok(after) if after == before => self.stop = previous_stop,
                            Ok(after) => {
                                if let Some(proposed) = proposed
                                    && let Ok(current) = observation_at(root)
                                    && current.publications() == after
                                    && current.records.plan.as_ref().is_some_and(
                                        |(plan, modern)| *modern && *plan == admitted_plan,
                                    )
                                    && current.records.progress == Some(proposed)
                                {
                                    self.stop = StopReservation::Known(proposed.revision);
                                }
                            }
                            Err(_) => {}
                        }
                    }
                    return Err(why);
                }
            };
            let staged = observe(root)?;
            if !staged
                .records
                .plan
                .as_ref()
                .is_some_and(|(saved, modern)| *modern && *saved == admitted_plan)
                || staged.records.progress != Some(next)
                || !matches!(
                    staged.records.decision,
                    Decision::Pending | Decision::Applied(true)
                )
            {
                return Err("the staged setup restart changed before admission".into());
            }
            next.admitted = true;
            let mut admitted = staged.records.clone();
            admitted.progress = Some(next);
            self.final_commit = Some(RestartCommit {
                plan: plan.clone(),
                staged: staged.records.clone(),
                admitted,
                publications: staged.publications(),
                progress: next,
            });
        }

        let next = self.final_commit.as_ref().unwrap().progress;
        match commit(root, next) {
            Ok(()) => {
                self.final_commit = None;
                Ok(Some(next.revision))
            }
            Err(why) => {
                match observation::publications(root) {
                    Ok(publications) => {
                        self.final_commit.as_mut().unwrap().publications = publications;
                    }
                    Err(_) => {
                        self.final_commit = None;
                    }
                }
                Err(why)
            }
        }
    }
}

/// `plan` as a choice that changed only `changed` makes it of the plan in force (`in_force`):
/// each setting `changed` names as `plan` has it, every other as the plan in force has it, and
/// the in-force vault unless Join explicitly typed another ([`InitialSetupRestart::changing`]).
/// With no plan in force, or no `changed`, `plan` as it is.
/// While a person's Cancel of the first-launch review stands ([`Records::cancelled`]), the plan a
/// press is taken onto: the plan the review showed for Set up now (no group `changed`), which
/// is how a person who cancelled sets up after all (design §3.3); every group at Leave as is for
/// a group choice or a Join, so it sets up the groups pressed and nothing else, saved plan or
/// none. `None` with no such Cancel.
fn cancelled_in_force(records: &Records, changed: Option<Selection>) -> Option<Plan> {
    let reviewed = records.cancelled.as_ref()?;
    if changed.is_some_and(|changed| changed.is_empty()) {
        return Some(reviewed.clone());
    }
    Some(Plan {
        selection: Selection::none(),
        vault_url: records.plan.as_ref().map_or_else(
            || reviewed.vault_url.clone(),
            |(plan, _)| plan.vault_url.clone(),
        ),
    })
}

fn onto_plan_in_force(
    in_force: Option<&Plan>,
    plan: &Plan,
    changed: Option<Selection>,
    vault_typed: bool,
) -> Plan {
    let (Some(in_force), Some(changed)) = (in_force, changed) else {
        return plan.clone();
    };
    let mut chosen = plan.clone();
    if !vault_typed {
        chosen.vault_url = in_force.vault_url.clone();
    }
    for setting in Setting::ALL {
        if !changed.contains(setting) {
            chosen
                .selection
                .set(setting, in_force.selection.contains(setting));
        }
    }
    chosen
}

fn owns_publications(root: &Path, expected: &observation::Publications) -> Result<bool, String> {
    let stop = stopped_at(root);
    if matches!(stop, Ok(Some(_))) {
        return Ok(false);
    }
    if observation::publications(root)? != *expected {
        return Ok(false);
    }
    stop?;
    Ok(true)
}

#[cfg(test)]
fn restart_at(root: &Path, plan: &Plan) -> Result<u64, String> {
    restart_reading(root, plan, || Ok(None))
}

fn restart_reading(
    root: &Path,
    plan: &Plan,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<u64, String> {
    let configured = read()?;
    restart_in_vault(root, plan, AdmittedVault::of(Ok(configured)))
}

fn restart_in_vault(root: &Path, plan: &Plan, vault: AdmittedVault) -> Result<u64, String> {
    restart_with(root, plan, Restart::default(), vault, save_progress)
}

/// [`save_explicit_workstation_plan`]'s run where none is on record: `plan` admitted at once,
/// saved as aterm's own (provisional) where `automatic` says it is.
fn restart_explicit(root: &Path, plan: &Plan, automatic: bool) -> Result<u64, String> {
    restart_with(
        root,
        plan,
        Restart {
            automatic,
            ..Restart::default()
        },
        AdmittedVault::Unknown,
        save_progress,
    )
}

fn restart_with(
    root: &Path,
    plan: &Plan,
    restart: Restart,
    vault: AdmittedVault,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<u64, String> {
    let mut next = stage_restart(root, plan, restart, vault)?;
    next.admitted = true;
    commit(root, next)?;
    Ok(next.revision)
}

/// Who restarts a run ([`stage_restart`]).
#[derive(Clone, Copy, Debug, Default)]
struct Restart {
    /// Keep completed settings across a choice, Join or automatic widening; otherwise
    /// Set up now, or a resume of the plan in force, applies its whole selection.
    chosen: bool,
    /// A Join: the vault typed in System State, whose steps run again.
    joins: bool,
    /// The plan is aterm's own for this Mac, which nobody chose (an explicit pass on a Mac with
    /// no setup records, [`save_explicit_workstation_plan`]): saved provisional.
    automatic: bool,
}

/// Stage `plan` as the next revision, kept with the vault this Mac was in (`vault`).
fn stage_restart(
    root: &Path,
    plan: &Plan,
    restart: Restart,
    vault: AdmittedVault,
) -> Result<Progress, String> {
    stage_restart_with(root, plan, restart, vault, save_progress, |_| Ok(()))
}

/// One write of [`stage_restart`], in its order. Wherever the process ends between two of
/// them, the next launch offers the plan saved and admits its answer, measured from the run
/// the choice left ([`Progress::staged`], [`InitialSetupAuthorization`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestartWrite {
    /// The plan.
    Plan,
    /// The next revision's progress, bound to that plan and not admitted yet.
    Progress,
    /// The retry records the restart resets.
    Reset,
    /// The first-run decision ([`save_admitted_decision`]).
    Decision,
}

/// [`stage_restart`], publishing the run's progress with `publish_progress` and asking `before`
/// ahead of each write: an error ends it there, as a process that ends between two writes does
/// (a test's).
fn stage_restart_with(
    root: &Path,
    plan: &Plan,
    restart: Restart,
    vault: AdmittedVault,
    publish_progress: impl FnOnce(&Path, Progress) -> Result<(), String>,
    mut before: impl FnMut(RestartWrite) -> Result<(), String>,
) -> Result<Progress, String> {
    let Restart {
        chosen, automatic, ..
    } = restart;
    validate_plan(plan)?;
    // A Stop asked for the run this replaces ends it on record first: should
    // this restart fail part-way, that run is still not resumed.
    fold_stop_request(root)?;
    let prior = progress(root)?;
    let previous = saved_plan_record(root)?;
    let mut next = prior.unwrap_or_default().next_for_plan(root, plan)?;
    next.access = carried_access(previous.as_ref().map(|(plan, _)| plan), prior, plan);
    next.admitted_vault = vault;
    // An unreadable decision is rewritten below, as before.
    let (decision, restored) = decision_record(root).unwrap_or((Decision::Missing, None));
    let waiting = restored.is_some() && restore_waits(root);
    let still_restored;
    (next.done, next.only, still_restored) = admitted_keeps(
        restart,
        previous.as_ref(),
        prior.or_else(|| older_run(previous.as_ref(), decision)),
        restored,
        waiting,
        plan,
    );
    // Only a person's choice makes a provisional plan theirs (`InitialSetupRestart::chosen`).
    let provisional = !chosen
        && (automatic
            || (previous
                .as_ref()
                .is_some_and(|(saved, modern)| *modern && saved == plan)
                && saved_plan_provisional(root)?));
    before(RestartWrite::Plan)?;
    save_plan_as(root, plan, provisional)?;
    before(RestartWrite::Progress)?;
    publish_progress(root, next)?;
    before(RestartWrite::Reset)?;
    for (record, what) in [
        ("automatic-attempt.json", "the automatic retry cooldown"),
        (HELD_RECORD, "the held automatic setup items"),
        (secrets_retry::RECORD, "the secrets retry pause"),
    ]
    .into_iter()
    .chain(pass_retry::PassWorker::ALL.map(|worker| (worker.record(), "the setup retry pause")))
    {
        match std::fs::remove_file(root.join(record)) {
            Ok(()) => durable_directory(root).map_err(|error| error.to_string())?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("cannot reset {what}: {error}")),
        }
    }
    before(RestartWrite::Decision)?;
    save_admitted_decision(
        root,
        still_restored,
        admitted_applied(plan, still_restored, next.done),
    )?;
    Ok(next)
}

/// What an admission of `plan` keeps of the run it replaces ([`chosen_keeps`]), and what of a
/// Restore's record (`restored`) still stands after it: one path for a choice on System State
/// ([`stage_restart`]) and for a startup offer's answer, whatever plan it admits
/// ([`InitialSetupAuthorization`]).
///
/// Restore put the Mac preferences back, and only the person sets them again: Set up now, or the
/// window resuming the plan in force (`restart.chosen` false), sets every one the plan selects; a
/// choice or a Join sets what the restored record queues ([`Restored::queued_after`]): the groups
/// a choice turned on since the Restore that no pass has set yet, never a group left as it was or
/// turned to Leave as is (a Join, or a plan presented in a vault joined since, turns none on).
/// The record, not the run's progress, holds
/// that queue and the plan it was measured from, so nothing staged before the Restore runs after
/// it, and a choice cut short before its record landed is measured again from the plan before
/// it. Nothing restored is recorded applied until a pass sets it (the pass writes that itself,
/// [`record_applied`]).
///
/// The record survives every admission while it stands for something ([`Restored::stands`]): a
/// preference the Restore put back that no pass has set since, or a put-back that waits for Full
/// Disk Access (`waiting`), which the grant then puts back whatever choice, Set up now or Join
/// came between, unless that admission queued its group to be set. Whether it owes a preference
/// the plan selects ([`Restored::owes`]) decides only whether the decision reads applied
/// ([`admitted_applied`]).
fn admitted_keeps(
    restart: Restart,
    previous: Option<&(Plan, bool)>,
    prior: Option<Progress>,
    restored: Option<Restored>,
    waiting: bool,
    plan: &Plan,
) -> (u16, Option<Selection>, Option<Restored>) {
    let (mut done, only) = chosen_keeps(restart, previous, prior, plan);
    let Some(restored) = restored else {
        return (done, only, None);
    };
    done &= !Stage::Workstation.bit();
    let still = |queued: Selection| {
        Some(Restored {
            queued: Some(queued),
            selection: Some(plan.selection),
            run: None,
            held: false,
            ..restored
        })
        .filter(|restored| restored.stands(waiting))
    };
    if !restart.chosen {
        // The Workstation stage sets every preference the plan selects: queued, so a put-back
        // waiting for Full Disk Access leaves those to it.
        return (done, None, still(preferences(plan.selection)));
    }
    let saved = previous.map(|(saved, _)| saved);
    let queued = restored.queued_after(prior, saved, plan);
    if queued.is_empty() {
        done |= Stage::Workstation.bit();
    }
    (
        done,
        Some(queued).filter(|queued| !queued.is_empty()),
        still(queued),
    )
}

/// Whether an admission of `plan` that keeps `done` and what of a Restore still stands
/// (`restored`, [`admitted_keeps`]) reads applied: every Mac settings stage the plan selects is
/// kept finished ([`mac_settings_kept`]), and no preference it selects stands restored or
/// queued ([`Restored::owes`]).
fn admitted_applied(plan: &Plan, restored: Option<Restored>, done: u16) -> bool {
    mac_settings_kept(plan, done) && !restored.is_some_and(|restored| restored.owes(plan.selection))
}

/// The first-run decision an admission writes once [`admitted_keeps`] decided it: applied or
/// pending as [`admitted_applied`] says, keeping what of a Restore still stands (`restored`).
fn save_admitted_decision(
    root: &Path,
    restored: Option<Restored>,
    applied: bool,
) -> Result<(), String> {
    match restored {
        Some(restored) => save_restored_decision(
            root,
            if applied {
                RestoredState::Applied
            } else {
                RestoredState::Pending
            },
            restored,
        ),
        None => save_workstation_decision(root, applied),
    }
}

/// Whether a Restore's put-back waits for Full Disk Access (`restore-waiting.json`); one that
/// cannot be read is taken to wait, so its record is kept.
fn restore_waits(root: &Path) -> bool {
    restore_waiting(root).map_or(true, |waiting| !waiting.is_empty())
}

/// The preferences `plan` selects that `before` did not: what a choice turned on.
fn turned_on(before: Selection, plan: &Plan) -> Selection {
    let (before, now) = (preferences(before), preferences(plan.selection));
    let mut added = Selection::none();
    for setting in Setting::ALL {
        added.set(setting, now.contains(setting) && !before.contains(setting));
    }
    added
}

/// The one pass an older aterm made of its plan (`previous`, a record with no vault field), which
/// kept no progress record, as a choice measures from it ([`chosen_keeps`]): its preferences set
/// once its first-run `decision` reads applied, and its Messages and FaceTime sign-outs once that
/// says they were verified too. So a choice, or the launch's offer, over such a setup never resets
/// the Dock or signs out again. `None` for a modern plan, or a pass that set nothing.
fn older_run(previous: Option<&(Plan, bool)>, decision: Decision) -> Option<Progress> {
    let (plan, modern) = previous?;
    let Decision::Applied(verified) = decision else {
        return None;
    };
    if *modern {
        return None;
    }
    let mut run = Progress::for_plan(plan);
    run.done = [(Stage::Workstation, true), (Stage::Accounts, verified)]
        .into_iter()
        .filter(|(stage, set)| *set && stage.selected(plan))
        .fold(0, |done, (stage, _)| done | stage.bit());
    Some(run)
}

/// What a person's choice keeps of the run it replaces (System State design §3.3): the done bits
/// of each stage whose groups and vault it left as they were, and the preferences the Workstation
/// stage still sets ([`Progress::only`]). A choice therefore never resets the Dock or signs
/// Messages and FaceTime out again for another group. A Join or a new vault runs Secrets and
/// GitHub repositories (whose token the vault brings) again; Set up now keeps nothing. The
/// Workstation stage holds two groups, so it keeps progress per preference: a group turned to
/// Leave as is keeps a finished stage finished, and one turned to Set up is the only one it sets
/// again.
/// What changed is measured from the plan the run's progress was staged for ([`Progress::staged`]):
/// the saved plan, unless a choice cut short after saving its plan never ran, when the next choice
/// is measured from the run it left and still runs only what it changed. An older aterm's plan,
/// which kept no progress record, is measured from the one pass that aterm made of it
/// ([`older_run`]).
fn chosen_keeps(
    restart: Restart,
    previous: Option<&(Plan, bool)>,
    prior: Option<Progress>,
    plan: &Plan,
) -> (u16, Option<Selection>) {
    let (true, Some((saved, _)), Some(prior)) = (restart.chosen, previous, prior) else {
        return (0, None);
    };
    let Some(previous) = prior.staged(Some(saved), plan) else {
        return (0, None);
    };
    let vault_again = restart.joins || !previous.vault_kept;
    let kept = Stage::ALL
        .into_iter()
        .filter(|stage| {
            stage.settings().iter().all(|setting| {
                previous.selection.contains(*setting) == plan.selection.contains(*setting)
            })
        })
        .filter(|stage| !(vault_again && matches!(stage, Stage::Secrets | Stage::Repositories)))
        .fold(0, |kept, stage| kept | (prior.done & stage.bit()));
    let now = preferences(plan.selection);
    let added = turned_on(previous.selection, plan);
    let within = |scope: Selection| {
        let mut within = Selection::none();
        for setting in Setting::ALL {
            within.set(setting, now.contains(setting) && scope.contains(setting));
        }
        within
    };
    let scope = if prior.done & Stage::Workstation.bit() != 0 {
        // Finished: what the choice turned on is all there is to set.
        Some(added)
    } else {
        // Not finished yet: what it had still to set, and what the choice turned on.
        prior.only.map(|only| {
            let mut grown = within(only);
            for setting in Setting::ALL {
                if added.contains(setting) {
                    grown.set(setting, true);
                }
            }
            grown
        })
    };
    // An empty scope owes nothing: a group turned to Set up and back before its pass
    // leaves the stage finished, never waiting on a pass with nothing to set.
    let finished = scope.is_some_and(|scope| scope.is_empty());
    (
        kept | if finished {
            Stage::Workstation.bit()
        } else {
            0
        },
        scope.filter(|scope| !scope.is_empty()),
    )
}

/// Whether every Mac settings stage `plan` selects is in `done`: the preferences and the
/// sign-outs a Join kept finished, or none selected.
fn mac_settings_kept(plan: &Plan, done: u16) -> bool {
    [Stage::Workstation, Stage::Accounts]
        .into_iter()
        .all(|stage| !stage.selected(plan) || done & stage.bit() != 0)
}

fn save_workstation_authorization(root: &Path, plan: &Plan) -> Result<(), String> {
    save_workstation_decision(root, plan.selection.workstation().is_empty())
}

/// What a first-run record that keeps a Restore's fields ([`Restored`]) says of the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestoredState {
    /// The Restore itself, whose decision ends the run.
    Restored,
    /// A choice since owes Mac settings, or a pass that may set some has begun.
    Pending,
    /// Nothing the plan selects is owed, and the record still stands for something
    /// ([`Restored::stands`]).
    Applied,
}

/// The first-run decision while a Restore's record stands ([`Restored`]): the Restore itself,
/// or pending or applied after a choice or a pass since.
fn save_restored_decision(
    root: &Path,
    state: RestoredState,
    restored: Restored,
) -> Result<(), String> {
    write_json(&root.join("first-run.json"), &restored.record(state))
}

/// The first-run record a Restore writes: every Mac preference it puts back stands
/// restored, nothing is queued to set again (a choice staged before it, admitted or not,
/// runs no more), and the next choice is measured from the plan in force now. An older aterm's
/// verified setup first gets the run that keeps its permissions ([`keep_older_pass`]).
pub(super) fn save_restore(root: &Path) -> Result<(), String> {
    keep_older_pass(root)?;
    let selection = authorized_selection(root).ok().flatten();
    save_restored_decision(
        root,
        RestoredState::Restored,
        Restored {
            held: held_restore(root)?,
            ..Restored::at_restore(selection)
        },
    )
}

/// Whether the Restore's decision in force holds aterm's own permissions ([`Restored::held`]),
/// which a Restore pressed again keeps, as it keeps a person's Cancel of the first-launch review
/// ([`Records::cancelled`]): a Restore after it never brings the review back.
fn held_restore(root: &Path) -> Result<bool, String> {
    let (_, restored, cancelled) = decision_record_reviewed(root)?;
    Ok(restored.is_some_and(|restored| restored.held) || cancelled.is_some())
}

/// Before a Restore's first write, under profile.lock: an older aterm's verified setup keeps no
/// run here, and only its first-run record allows aterm's own permissions (approved by default,
/// System State design §3.5, §6), which the Restore's decision replaces. So the one pass that
/// aterm made of its plan ([`older_run`]: its preferences, and its verified sign-outs) is saved
/// first as an admitted, finished run of that plan, the run a launch's offer would have admitted.
/// The Restore then keeps it as it keeps any admitted run ([`leave_restored_as_is`]): aterm's
/// permissions stay allowed and shown, and a choice after it never resets the Dock or signs
/// Messages and FaceTime out again. One progress write, after its run number's reservation
/// ([`revision::reserve`]), which leaves the records whole either way. Any
/// other setup is left as it is: a run here carries its own admission, and an older setup that
/// was pending or unverified gets its permissions through the launch's offer, as before.
fn keep_older_pass(root: &Path) -> Result<(), String> {
    if progress(root)?.is_some() {
        return Ok(());
    }
    let decision = decision_at(root)?;
    if decision != Decision::Applied(true) {
        return Ok(());
    }
    let previous = saved_plan_record(root)?;
    let Some(mut run) = older_run(previous.as_ref(), decision) else {
        return Ok(());
    };
    let plan = previous.map(|(plan, _)| plan).unwrap_or_default();
    run.revision = revision::next(root, 0)?;
    run.admitted = true;
    run.completed = Stage::ALL
        .into_iter()
        .all(|stage| !stage.selected(&plan) || run.done & stage.bit() != 0);
    save_progress(root, run)
}

/// Once a Restore has put the Mac preferences back, the groups it put back (Keyboard &
/// Trackpad, Desktop & Dock) are left as is (System State design §3.4): the person asked for
/// them as they were, so the page asks nothing of them, and one press of Set up on one sets
/// that group alone. The plan is saved as a person's choice is ([`stage_restart`]), under
/// profile.lock, in an order that leaves the records whole wherever the process ends: a Stop
/// asked for the run it replaces is folded in first; then the Restore's record, against the
/// plan it stands for now, which every reader presents as the plan in force until the plan
/// lands ([`Records::restore_presentation`]); then the plan, a person's and never
/// provisional, even where the Restore left it as it was (aterm's own plan of Permissions alone,
/// say), so no later launch widens it over the groups the Restore put back; then the run's
/// progress, a new revision bound to that plan (its digest and
/// `plan_selection`) that keeps what each other stage finished ([`chosen_keeps`]), the
/// Permissions approval it continues ([`carried_access`]), and the run's admission and a
/// person's Stop of it, even one saved after this write ([`Progress::continues`]); last, when the
/// plan was written, the Restore's record again, naming that run ([`Restored::run`]). So a choice
/// made after any of those writes is measured from, and
/// applied onto, the plan the page shows, and the Restore never withdraws aterm's
/// permissions: the access steps go on by themselves as before. The Restore's own decision
/// ends the run, so no pass runs until a choice, Set up now or a Join admits one. With no
/// saved plan there is none to change.
///
/// The run's admission is that of the run the progress records, whichever plan it was staged
/// for ([`Progress::staged`]): a plan saved ahead of it (by a choice, or by this Restore, cut
/// short before its progress) never ran. The progress is written whenever it is not bound to the
/// Restore's plan, also when the plan is left as it was, so a Restore pressed again, or the
/// next launch ([`bring_forward_restored_setup`]), finishes one cut short before its progress.
/// The Restore's record names that run's revision ([`Restored::run`]), so a choice made after the
/// Restore and cut short before its own decision is told from a Restore cut short: its first
/// write names it only when the plan is already the Restore's, so a plan a choice before the
/// Restore saved ahead of its run never reads as one saved since.
///
/// A plan an older aterm saved predates the Permissions group, so it is left as every reader
/// presents it, with aterm's own permissions ([`Records::present`]). A Restore that leaves no
/// admitted run to carry them (one pressed while the first offer waited for its answer, or over
/// an older setup that was pending or unverified) brings the setup forward under the same lock
/// ([`bring_forward_restored_with`]), on System State and in `aterm doctor --restore` alike: the
/// run that sets them waits for the startup's offer, which System State makes again at once.
pub(super) fn leave_restored_as_is(root: &Path) -> Result<(), String> {
    leave_restored_as_is_with(root, |_| Ok(()))?;
    bring_forward_restored_with(root, |_| Ok(())).map(|_| ())
}

/// One write of [`leave_restored_as_is`] or [`bring_forward_restored_with`], each in its own
/// order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RestoreWrite {
    /// The Restore's record, still its own decision, naming the run a bring-forward stages
    /// ([`Restored::run`]).
    Run,
    /// The Restore's record, measured from the plan with the restored groups left as is.
    Record,
    /// That plan.
    Plan,
    /// The run's progress, bound to that plan.
    Progress,
}

/// [`leave_restored_as_is`], asking `before` ahead of each write: an error ends it there, as a
/// process that ends between two writes does (a test's).
fn leave_restored_as_is_with(
    root: &Path,
    before: impl FnMut(RestoreWrite) -> Result<(), String>,
) -> Result<(), String> {
    leave_restored_as_is_observing_with(root, before, |_| {})
}

fn leave_restored_as_is_observing_with(
    root: &Path,
    before: impl FnMut(RestoreWrite) -> Result<(), String>,
    published: impl FnMut(u64),
) -> Result<(), String> {
    leave_restored_as_is_observing_using(root, before, published, save_progress)
}

fn leave_restored_as_is_observing_using(
    root: &Path,
    mut before: impl FnMut(RestoreWrite) -> Result<(), String>,
    mut published: impl FnMut(u64),
    mut write: impl FnMut(&Path, Progress) -> Result<(), String>,
) -> Result<(), String> {
    fold_stop_request(root)?;
    let Some(previous) = saved_plan_record(root)? else {
        return Ok(());
    };
    let prior = progress(root)?;
    let mut plan = previous.0.clone();
    if !previous.1 {
        // An older aterm's plan, as every reader presents it ([`Records::present`]): a Restore's
        // decision stands now, so no Cancel holds aterm's permissions.
        plan.selection.set(Setting::Permissions, true);
    }
    let restored = preferences(Selection::all());
    for setting in Setting::ALL {
        if restored.contains(setting) {
            plan.selection.set(setting, false);
        }
    }
    // The run this Restore leaves in force: one bound to its plan goes on, any other is carried
    // on as the next revision below.
    let carried = prior.is_some_and(|prior| !prior.belongs_to(&plan));
    let run = match prior {
        Some(prior) if !carried => Some(prior.revision),
        // One this Restore named and reserved before it was cut short goes on.
        Some(prior) => Some(
            match decision_record(root)?.1.and_then(|restored| {
                named_run(restored, prior.revision, revision::floor(root).ok()?)
            }) {
                Some(run) => run,
                None => revision::next(root, prior.revision)?,
            },
        ),
        None => None,
    };
    // The plan aterm chose by itself, left as it was, is now a person's; its run goes on.
    let plan_write = plan != previous.0 || saved_plan_provisional(root)?;
    let held = held_restore(root)?;
    let record = |run| Restored {
        run,
        held,
        ..Restored::at_restore(Some(plan.selection))
    };
    // The record names its run only once the plan is the Restore's: a plan a choice before it
    // saved ahead of its run is never read as one saved since ([`staged_since`]).
    before(RestoreWrite::Record)?;
    let named = run.filter(|_| !plan_write);
    if carried && let Some(run) = named {
        // A carried run's number is reserved before a record names it: its progress comes
        // after this write, and no later run may take that number meanwhile.
        revision::reserve(root, run)?;
    }
    save_restored_decision(root, RestoredState::Restored, record(named))?;
    if plan_write {
        before(RestoreWrite::Plan)?;
        save_plan_as(root, &plan, false)?;
    }
    // A run bound to the plan goes on as it is; one staged for the plan before it is bound to
    // this one, and so is one a Restore cut short before this write left behind.
    if let Some(prior) = prior.filter(|prior| !prior.belongs_to(&plan)) {
        carry_restored_run(&previous, prior, &plan, run, &mut before, &mut |next| {
            save_progress_observing_with(root, next, &mut published, &mut write)
        })?;
    }
    if plan_write && run.is_some() {
        before(RestoreWrite::Run)?;
        save_restored_decision(root, RestoredState::Restored, record(run))?;
    }
    Ok(())
}

/// [`leave_restored_as_is_with`]'s progress: the run `prior` the Restore ends, carried on as
/// `revision`, the run the Restore's record names, bound to the Restore's `plan` over the plan
/// saved before it (`previous`).
fn carry_restored_run(
    previous: &(Plan, bool),
    prior: Progress,
    plan: &Plan,
    revision: Option<u64>,
    before: &mut impl FnMut(RestoreWrite) -> Result<(), String>,
    publish_progress: &mut impl FnMut(Progress) -> Result<(), String>,
) -> Result<(), String> {
    let mut next = Progress {
        revision: revision.ok_or("a carried run has no revision")?,
        ..Progress::for_plan(plan)
    };
    next.access = carried_access(Some(&previous.0), Some(prior), plan);
    next.admitted_vault = prior.admitted_vault;
    // The run the Restore ends stays admitted, and stopped if a person stopped it: the
    // Permissions approval it continues stands, as a person's Stop does (design §6). A Stop of
    // the unfinished run it carries on, admitted or not, saved after this record, stops it too.
    // The admitted run is the one the progress records, also when a plan was saved ahead of it.
    next.admitted = prior.admitted;
    next.stopped = prior.stopped;
    next.continues =
        (!(prior.admitted && prior.completed)).then(|| prior.continues.unwrap_or(prior.revision));
    (next.done, next.only) = chosen_keeps(
        Restart {
            chosen: true,
            joins: false,
            automatic: false,
        },
        Some(previous),
        Some(prior),
        plan,
    );
    before(RestoreWrite::Progress)?;
    publish_progress(next)
}

/// Bring forward a setup a Restore left with no admitted run to carry aterm's own permissions,
/// which a Restore never withdraws (System State design §3.4): only a person's Stop, or an older
/// aterm's Cancel, holds them. Startup calls it once the person is resolved, before it binds the
/// records, and with nothing to bring forward it writes nothing; [`BroughtForward::Run`] names
/// the run it wrote or found waiting for startup. A Restore brings the setup forward itself as it ends
/// ([`leave_restored_as_is`]), so this finds work only where a Restore was cut short, an older
/// aterm restored, or a choice since was cut short.
///
/// - A Restore cut short before one of its writes (its record, its plan or its progress, say
///   after it saved its plan, which left the admitted run behind that plan) is finished
///   ([`leave_restored_as_is`]): the groups it put back are at Leave as is, the run keeps its
///   admission and its Permissions approval, and nothing runs.
/// - A Restore that ended setup before any run was admitted (an older aterm's, whose
///   `first-run.json` says `restored` with no progress record; one before the first launch's
///   offer was answered; one over a staged run whose admission never landed, or one an
///   `aterm doctor --apply` cut short left unadmitted; or one over an older aterm's setup that was
///   pending or unverified) has no run to carry them. This stages the
///   run a choice over the Restore stages ([`stage_restart`]), unadmitted, for the launch's offer
///   to admit as it admits any staged choice ([`InitialSetupAuthorization`]): Full Disk Access,
///   Developer Tools and the debugger then go on by themselves. The Mac
///   preference groups stay where the Restore left them, at Leave as is, and every other stage
///   the plan selects is recorded finished, so that run runs Permissions alone: no sign-out, no
///   Spotlight, no Dock.
/// - A choice made after the Restore and cut short before its decision is handed to the
///   launch's offer as it stands ([`staged_since`]).
///
/// While another pass holds profile.lock it waits for it, at most [`CHOICE_LOCK_WAIT`], and then
/// answers [`BroughtForward::Busy`], never "nothing to bring forward": the startup asks again
/// once that pass lets go, so the permissions get their run in this launch.
pub fn bring_forward_restored_setup() -> Result<BroughtForward, String> {
    bring_forward_restored_setup_observing(|_| {})
}

/// Report an exact checkpoint as soon as it is saved, even if a later record
/// write fails. The callback runs under profile.lock; callers collect the target
/// and hand it to their Stop owner after this function releases that lock.
pub fn bring_forward_restored_setup_observing(
    published: impl FnMut(u64),
) -> Result<BroughtForward, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(BroughtForward::Nothing);
    }
    match person_scope() {
        PersonScope::Refused(_) => return Ok(BroughtForward::Nothing),
        PersonScope::Unresolved(why) => return Err(why),
        PersonScope::Person => {}
    }
    let deadline = std::time::Instant::now() + CHOICE_LOCK_WAIT;
    bring_forward_waiting_observing(
        &state_root()?,
        || {
            let more = std::time::Instant::now() < deadline;
            if more {
                std::thread::sleep(CHOICE_LOCK_POLL);
            }
            more
        },
        published,
    )
}

/// What [`bring_forward_restored_setup`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BroughtForward {
    /// Nothing to bring forward, or nothing this process may change.
    Nothing,
    /// A revision it wrote, or found waiting for the startup's offer: the run it or the Restore
    /// staged for that offer, the choice it handed to it, or the run a Restore cut short left
    /// once its writes were finished. The startup runs (System State starts it again after a
    /// Restore).
    Run(u64),
    /// Another pass held profile.lock past the wait: the records may change under it, so it is
    /// asked again once that pass lets go.
    Busy,
}

/// [`bring_forward_restored_setup`] at `root`. It reads first, so a Mac with nothing to bring
/// forward gets no lock file and no write. While another holds profile.lock, `wait` waits a
/// moment and says whether to ask for it again; once it says no, the answer is
/// [`BroughtForward::Busy`].
#[cfg(test)]
fn bring_forward_waiting(
    root: &Path,
    wait: impl FnMut() -> bool,
) -> Result<BroughtForward, String> {
    bring_forward_waiting_observing(root, wait, |_| {})
}

fn bring_forward_waiting_observing(
    root: &Path,
    mut wait: impl FnMut() -> bool,
    mut published: impl FnMut(u64),
) -> Result<BroughtForward, String> {
    loop {
        let records = Records::read(root)?;
        if restored_forward(&records, revision::floor(root)?).is_none() {
            return Ok(offer_waits(&records).map_or(BroughtForward::Nothing, BroughtForward::Run));
        }
        if let Some(_lock) = HeldLock::try_take(root)? {
            return Ok(
                bring_forward_restored_observing_with(root, |_| Ok(()), &mut published)?
                    .map_or(BroughtForward::Nothing, BroughtForward::Run),
            );
        }
        if !wait() {
            return Ok(BroughtForward::Busy);
        }
    }
}

/// The run a Restore left for the startup's offer, which still waits for it: the Restore's
/// record, pending, over a run nobody admitted (or a plan saved ahead of its run) that no person
/// stopped. The Restore wrote it itself ([`leave_restored_as_is`]), so System State's startup
/// offers it at once ([`BroughtForward::Run`]); nothing is written.
fn offer_waits(records: &Records) -> Option<u64> {
    records.restored?;
    if records.decision != Decision::Pending {
        return None;
    }
    let progress = records.progress?;
    let (saved, _) = records.plan.as_ref()?;
    let admitted = progress.admitted && progress.belongs_to(saved);
    (!admitted && !progress.stopped_by(records.stop_request)).then_some(progress.revision)
}

/// [`bring_forward_waiting`] for a test that holds no lock: the revision of the run that waits
/// for the offer, if any.
#[cfg(test)]
fn bring_forward_restored_at(root: &Path) -> Result<Option<u64>, String> {
    Ok(match bring_forward_waiting(root, || false)? {
        BroughtForward::Run(revision) => Some(revision),
        BroughtForward::Nothing => None,
        BroughtForward::Busy => return Err("profile.lock is held".into()),
    })
}

/// What [`bring_forward_restored_with`] does for `records` ([`bring_forward_restored_setup`]).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Forward {
    /// Hand a choice staged since the Restore, cut short before its decision, to the launch's
    /// offer ([`staged_since`]): the Restore's record, kept whole, now pending, so the offer
    /// admits the plan the person chose, measured from the Restore's plan, and runs what the
    /// choice turned on. The revision is the staged run's.
    Offer(u64),
    /// Finish a Restore cut short before one of its writes ([`leave_restored_as_is`]): the plan
    /// in force still selects a group it put back, or is still aterm's provisional one, or the
    /// admitted run is not bound to the plan saved ahead of it, or the record does not name its
    /// run yet.
    Finish,
    /// Stage the plan, its progress and the Restore's record measured from that plan, for the
    /// launch's offer to admit.
    Stage(Plan, Progress, Restored),
}

/// What [`bring_forward_restored_with`] does for `records`; `None` when no Restore's decision
/// stands, when its writes are whole and its admitted run carries the permissions itself, when a
/// person stopped the run it left unadmitted, or when that run is a person's plan that leaves
/// Permissions as is. A Restore cut short is finished whatever its run: a Stop of an admitted run
/// stays with it.
///
/// A staged plan is the one every reader presents as in force
/// ([`Records::presented_plan_record`]), or none, with the preferences Restore puts back at
/// Leave as is and Permissions selected; the groups the person had otherwise stay as that plan
/// has them, at Leave as is where there is none. Its progress, with every stage the plan selects
/// but Permissions finished, is the revision after the last run number reserved
/// (`revision_floor`, [`revision::floor`]; 1 on a Mac with none, as the first run's is) or after
/// the run the records keep, whichever is later, carrying that run's Permissions approval
/// ([`carried_access`]). Each input is
/// one this writes again unchanged, so a launch that finds the writes cut short anywhere
/// finishes them: a progress already staged so is kept as it is.
///
/// A choice made after the Restore whose writes were cut short before its decision
/// ([`staged_since`]) is the person's: it goes to the offer, never back to the Restore's plan,
/// unless the person stopped it.
fn restored_forward(records: &Records, revision_floor: u64) -> Option<Forward> {
    let restored = records
        .restored
        .filter(|_| records.decision == Decision::Stopped(Stop::Restored))?;
    // A person's Stop or Cancel that aterm kept as it set its records aside holds everything.
    if restored.held {
        return None;
    }
    if let Some((revision, held)) = staged_since(records, restored) {
        return (!held).then_some(Forward::Offer(revision));
    }
    if let Some(progress) = records.progress.filter(|progress| progress.admitted) {
        let (saved, _) = records.plan.as_ref()?;
        let whole = progress.belongs_to(saved)
            && preferences(saved.selection).is_empty()
            && !records.provisional
            && restored.run.is_some();
        return (!whole).then_some(Forward::Finish);
    }
    if records
        .progress
        .is_some_and(|progress| progress.stopped_by(records.stop_request))
    {
        return None;
    }
    let presented = records.presented_plan_record();
    // A person's plan that leaves Permissions as is keeps them so; an older aterm's plan, or
    // none, predates the group.
    if records.progress.is_some()
        && presented
            .as_ref()
            .is_some_and(|(plan, modern)| *modern && !plan.selection.contains(Setting::Permissions))
    {
        return None;
    }
    let mut plan = presented.map_or_else(|| Plan::from(Selection::none()), |(plan, _)| plan);
    let put_back = preferences(Selection::all());
    plan.selection = filtered(plan.selection, |setting| !put_back.contains(setting));
    plan.selection.set(Setting::Permissions, true);
    let staged_at = |revision: u64, access: Option<u64>, continues: Option<u64>| {
        let mut staged = Progress {
            revision,
            access,
            continues,
            ..Progress::for_plan(&plan)
        };
        staged.done = Stage::ALL
            .into_iter()
            .filter(|stage| *stage != Stage::Permissions && stage.selected(&plan))
            .fold(0, |done, stage| done | stage.bit());
        staged
    };
    // A bring-forward cut short after its record named the run it stages goes on with that run.
    let next = |after: u64| {
        named_run(restored, after, revision_floor)
            .map_or_else(|| next_revision(after.max(revision_floor)).ok(), Some)
    };
    let staged = match records.progress {
        None => staged_at(next(0)?, None, None),
        Some(progress)
            if progress == staged_at(progress.revision, progress.access, progress.continues) =>
        {
            progress
        }
        // The run it stages carries on the one the records keep, which nobody admitted: a Stop
        // of that run, saved after this, holds it too ([`Progress::continues`]).
        Some(progress) => staged_at(
            next(progress.revision)?,
            carried_access(
                records.plan.as_ref().map(|(saved, _)| saved),
                Some(progress),
                &plan,
            ),
            Some(progress.continues.unwrap_or(progress.revision)),
        ),
    };
    let restored = Restored {
        queued: Some(Selection::none()),
        selection: Some(plan.selection),
        ..restored
    };
    Some(Forward::Stage(plan, staged, restored))
}

/// The run a Restore's record names ([`Restored::run`]) after `after`, the run the records keep,
/// while no progress publishes it yet and it is still the last number reserved (`floor`): a
/// Restore or a bring-forward cut short after its record named that run goes on with it, as an
/// owned retry keeps its reserved number, so one finished later writes what one never cut short
/// writes.
fn named_run(restored: Restored, after: u64, floor: u64) -> Option<u64> {
    restored.run.filter(|run| *run > after && *run == floor)
}

/// A choice (or Set up now, or a Join) made after the Restore whose writes ([`RestartWrite`]) were
/// cut short before its decision, which left the Restore's own decision standing: the revision it
/// staged, and whether a person stopped it. Told from a Restore cut short by the run the
/// Restore's record names ([`Restored::run`]), which no write of the Restore, or of a
/// bring-forward, leaves behind a plan or run of its own:
///
/// - after its progress: a run newer than the Restore's, bound to the plan saved, that nobody
///   admitted;
/// - after its plan, before its progress: the Restore's own run still, with a plan saved ahead
///   of it.
///
/// A run admitted since (an `aterm doctor --apply` cut short) is not a choice the offer makes.
fn staged_since(records: &Records, restored: Restored) -> Option<(u64, bool)> {
    let run = restored.run?;
    let progress = records.progress?;
    let (saved, _) = records.plan.as_ref()?;
    if progress.revision > run && !progress.admitted && progress.belongs_to(saved) {
        return Some((progress.revision, progress.stopped_by(records.stop_request)));
    }
    (progress.revision == run && !progress.belongs_to(saved)).then_some((run, false))
}

/// [`bring_forward_restored_at`]'s writes, under profile.lock, asking `before` ahead of each:
/// an error ends it there, as a process that ends between two writes does (a test's). A Stop
/// asked for is folded in first. A Restore cut short is finished in its own order
/// ([`leave_restored_as_is`]). A staged run's order keeps every prefix a Restore's own decision
/// that nothing runs from: first the Restore's record naming the run it stages
/// ([`Restored::run`]), so none of the writes after it reads as a choice staged since
/// ([`staged_since`]); then the plan, a person's and never provisional (the Restore's record
/// presents it until then); then the staged progress; last the Restore's record, pending now,
/// which lets the launch's offer admit the run. A choice staged since goes to the offer with one
/// write, the Restore's record kept whole and pending.
fn bring_forward_restored_with(
    root: &Path,
    before: impl FnMut(RestoreWrite) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    bring_forward_restored_observing_with(root, before, |_| {})
}

fn bring_forward_restored_observing_with(
    root: &Path,
    before: impl FnMut(RestoreWrite) -> Result<(), String>,
    published: impl FnMut(u64),
) -> Result<Option<u64>, String> {
    bring_forward_restored_observing_using(root, before, published, save_progress)
}

fn bring_forward_restored_observing_using(
    root: &Path,
    mut before: impl FnMut(RestoreWrite) -> Result<(), String>,
    mut published: impl FnMut(u64),
    mut write: impl FnMut(&Path, Progress) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    fold_stop_request(root)?;
    let records = Records::read(root)?;
    let (plan, staged, restored) = match restored_forward(&records, revision::floor(root)?) {
        None => return Ok(None),
        Some(Forward::Offer(revision)) => {
            let Some(restored) = records.restored else {
                return Ok(None);
            };
            before(RestoreWrite::Record)?;
            save_restored_decision(
                root,
                RestoredState::Pending,
                Restored {
                    run: None,
                    ..restored
                },
            )?;
            return Ok(Some(revision));
        }
        Some(Forward::Finish) => {
            leave_restored_as_is_observing_using(root, before, &mut published, &mut write)?;
            return revision_at(root).map(Some);
        }
        Some(Forward::Stage(plan, staged, restored)) => (plan, staged, restored),
    };
    if records.restored.and_then(|restored| restored.run) != Some(staged.revision) {
        before(RestoreWrite::Run)?;
        // Reserved before the record names it, so no later run takes its number.
        revision::reserve(root, staged.revision)?;
        save_restored_decision(
            root,
            RestoredState::Restored,
            Restored {
                run: Some(staged.revision),
                ..records.restored.unwrap_or(restored)
            },
        )?;
    }
    if records.plan.as_ref() != Some(&(plan.clone(), true)) || records.provisional {
        before(RestoreWrite::Plan)?;
        save_plan_as(root, &plan, false)?;
    }
    if records.progress != Some(staged) {
        before(RestoreWrite::Progress)?;
        save_progress_observing_with(root, staged, &mut published, &mut write)?;
    } else {
        published(staged.revision);
    }
    before(RestoreWrite::Record)?;
    save_restored_decision(
        root,
        RestoredState::Pending,
        Restored {
            run: None,
            ..restored
        },
    )?;
    Ok(Some(staged.revision))
}

/// The settings of `selection` that `keep` keeps.
fn filtered(selection: Selection, keep: impl Fn(Setting) -> bool) -> Selection {
    let mut kept = Selection::none();
    for setting in Setting::ALL {
        kept.set(setting, selection.contains(setting) && keep(setting));
    }
    kept
}

/// What the first-run record keeps while Mac preferences stand restored (System State
/// design §3.4, Restore): which preferences stand restored, which a choice since has
/// queued to set again, and the plan the queue was measured from, in one record written
/// whole, so no write cut short leaves them out of step with each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Restored {
    /// The Mac preferences Restore put back that no pass has set since. A pass takes out
    /// those it set ([`record_applied`]); while one the plan selects is left, nothing
    /// reads applied. A group turned to Leave as is stays in, but counts only while the
    /// plan selects it again, and a group turned back on is queued, so its pass takes it
    /// out.
    left: Selection,
    /// The preferences a choice since the Restore turned to Set up that no pass has set
    /// yet: what the Workstation stage sets ([`Progress::only`]). `None` in a pending
    /// record an older aterm wrote, which kept them in the run's progress
    /// ([`Self::queue`]).
    queued: Option<Selection>,
    /// The plan's selection the record was written against: what the next choice turned
    /// on is measured from it, never from a plan a choice cut short saved before its
    /// record. `None` in an older record: the saved plan.
    selection: Option<Selection>,
    /// While the Restore's own decision stands: the revision of the run the Restore's writes
    /// leave in force ([`leave_restored_as_is`]), or of the one bringing it forward stages
    /// ([`bring_forward_restored_with`]). A run newer than it that nobody admitted, or a plan
    /// saved ahead of it, is then a choice staged since the Restore whose last write, its
    /// decision, never landed ([`staged_since`]). `None` in the record a Restore writes before
    /// it puts anything back ([`save_restore`]), in one an older aterm wrote, and once a choice
    /// replaced the Restore's decision.
    run: Option<u64>,
    /// A person's Stop or Cancel held aterm's own permissions as aterm set its unreadable
    /// records aside ([`set_aside`]): the Restore's decision stands stopped for them too, so no
    /// bring-forward stages a run over it ([`restored_forward`]) and no reader presents the plan
    /// it names ([`Records::restore_presentation`]). A later Restore keeps it; a choice, Set up
    /// now or a Join, which admits a run of its own, ends it ([`admitted_keeps`]).
    held: bool,
}

impl Restored {
    /// Restore's record ([`save_restore`]), against the plan's `selection`, if any.
    fn at_restore(selection: Option<Selection>) -> Self {
        Self {
            left: preferences(Selection::all()),
            queued: Some(Selection::none()),
            selection,
            run: None,
            held: false,
        }
    }

    /// The record's restored fields, with `stopped` for a Restore's own. A `"restored":
    /// true` (or a Restore's, with none) from an older aterm stands for every preference.
    fn read(value: &Value, stopped: bool) -> Result<Self, String> {
        let bits = |name: &str| match value.get(name) {
            None => Ok(None),
            Some(field) => {
                known_selection(field, &format!("invalid first-run {name} preferences")).map(Some)
            }
        };
        let left = match value.get("restored") {
            None | Some(Value::Bool(true)) => preferences(Selection::all()),
            Some(_) => bits("restored")?.unwrap_or_else(Selection::none),
        };
        let queued = match bits("queued")? {
            None if stopped => Some(Selection::none()),
            queued => queued,
        };
        let run = match value.get("run") {
            None => None,
            Some(run) => Some(run.as_u64().ok_or("invalid first-run restored run")?),
        };
        let held = match value.get("held") {
            None => false,
            Some(held) => held.as_bool().ok_or("invalid first-run restored hold")?,
        };
        Ok(Self {
            left,
            queued,
            selection: bits("selection")?,
            run,
            held,
        })
    }

    fn record(self, state: RestoredState) -> Value {
        let mut record = stamped(json!({"profile":PROFILE,"state":match state {
            RestoredState::Restored => "restored",
            RestoredState::Pending => "pending",
            RestoredState::Applied => "applied",
        },"restored":self.left.bits()}));
        if let Some(fields) = record.as_object_mut() {
            if state == RestoredState::Applied {
                fields.insert("accounts_verified".into(), Value::Bool(true));
            }
            if let Some(queued) = self.queued {
                fields.insert("queued".into(), Value::from(queued.bits()));
            }
            if let Some(selection) = self.selection {
                fields.insert("selection".into(), Value::from(selection.bits()));
            }
            if let Some(run) = self.run {
                fields.insert("run".into(), Value::from(run));
            }
            if self.held {
                fields.insert("held".into(), Value::Bool(true));
            }
        }
        record
    }

    /// The queue: the record's, or for an older pending record, the scope the run's
    /// unfinished Workstation stage of the saved plan had still to set.
    fn queue(self, progress: Option<Progress>, saved: Option<&Plan>) -> Selection {
        self.queued.unwrap_or_else(|| match (progress, saved) {
            (Some(progress), Some(saved))
                if progress.belongs_to(saved) && progress.done & Stage::Workstation.bit() == 0 =>
            {
                progress.only.unwrap_or_else(Selection::none)
            }
            _ => Selection::none(),
        })
    }

    /// What a choice of `plan` leaves queued: what the queue held that `plan` still
    /// selects, and the preferences `plan` turned on since the plan the record was written
    /// against.
    fn queued_after(self, prior: Option<Progress>, saved: Option<&Plan>, plan: &Plan) -> Selection {
        let before = self
            .selection
            .or_else(|| saved.map(|saved| saved.selection))
            .unwrap_or(plan.selection);
        let now = preferences(plan.selection);
        let (queued, added) = (self.queue(prior, saved), turned_on(before, plan));
        filtered(now, |setting| {
            queued.contains(setting) || added.contains(setting)
        })
    }

    /// The record after a pass that set `set`: none of it stands restored or queued.
    fn after_pass(self, set: Selection, progress: Option<Progress>, saved: Option<&Plan>) -> Self {
        let queued = self.queue(progress, saved);
        Self {
            left: filtered(self.left, |setting| !set.contains(setting)),
            queued: Some(filtered(queued, |setting| !set.contains(setting))),
            ..self
        }
    }

    /// Whether the record still stands for something (System State design §3.4): a preference
    /// the Restore put back that no pass has set since ([`Self::left`]), or a put-back that
    /// waits for Full Disk Access (`waiting`). While it does, every admission keeps it
    /// ([`admitted_keeps`]), and so does a pass that needs nothing more ([`record_applied`]).
    const fn stands(self, waiting: bool) -> bool {
        !self.left.is_empty() || waiting
    }

    /// Whether a preference `selection` selects stands restored or queued: the Mac
    /// settings it selects are not all applied.
    fn owes(self, selection: Selection) -> bool {
        let planned = preferences(selection);
        let queued = self.queued.unwrap_or_else(Selection::none);
        Setting::ALL.into_iter().any(|setting| {
            planned.contains(setting) && (self.left.contains(setting) || queued.contains(setting))
        })
    }
}

/// The first-run decision an authorization starts from: `applied` for Mac settings with
/// nothing left to do, else pending. It replaces what a Restore kept ([`Restored`]), so it
/// is written as a plan is authorized, never by a pass: by a restart once it has decided
/// what of a Restore still stands ([`stage_restart`]), by the explicit profile, and by an
/// admission. A pass marks the decision pending with [`record_pending`], which keeps it.
fn save_workstation_decision(root: &Path, applied: bool) -> Result<(), String> {
    write_json(
        &root.join("first-run.json"),
        &stamped(if applied {
            json!({"profile":PROFILE,"state":"applied","accounts_verified":true})
        } else {
            json!({"profile":PROFILE,"state":"pending"})
        }),
    )
}

/// Tests' authorized plan: `plan` saved and admitted as a run of its own, as an older aterm's
/// first-launch authorization admitted one. Nothing in aterm authorizes a plan so now: a
/// choice, Set up now or a Join restarts one ([`stage_restart`]), and the startup's offer is
/// admitted by [`InitialSetupAuthorization`], each keeping what a Restore keeps
/// ([`admitted_keeps`]), which this does not.
#[cfg(test)]
fn authorize_at(root: &Path, plan: &Plan) -> Result<Option<u64>, String> {
    let admitted = authorize_reading(root, plan, || Ok(None));
    // These tests stand for a person's admission, whose press asked for macOS's grants.
    if matches!(admitted, Ok(Some(_))) {
        note_person_asked_at(root)?;
    }
    admitted
}

#[cfg(test)]
fn authorize_reading(
    root: &Path,
    plan: &Plan,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<u64>, String> {
    authorize_with(root, plan, AdmittedVault::of(read()), save_progress)
}

#[cfg(test)]
fn authorize_with(
    root: &Path,
    plan: &Plan,
    vault: AdmittedVault,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<Option<u64>, String> {
    if finished_at(root)? {
        return Ok(None);
    }
    validate_plan(plan)?;
    let previous = saved_plan(root)?;
    let prior_progress = progress(root)?;
    let revision = revision::next(root, prior_progress.unwrap_or_default().revision)?;
    let mut current = if previous.as_ref() == Some(plan)
        && prior_progress.is_some_and(|progress| progress.belongs_to(plan))
    {
        prior_progress.unwrap_or_else(|| Progress::for_plan(plan))
    } else {
        Progress::for_plan(plan)
    };
    current.completed = plan.selection.is_empty();
    current.admitted = false;
    current.running = None;
    current.continues = None;
    current.access = carried_access(previous.as_ref(), prior_progress, plan);
    current.revision = revision;
    current.admitted_vault = vault;
    save_plan(root, plan)?;
    save_progress(root, current)?;
    if current.done & Stage::Workstation.bit() == 0 {
        save_workstation_authorization(root, plan)?;
    }
    current.admitted = true;
    commit(root, current)?;
    Ok(Some(current.revision))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    Cancelled,
    Restored,
}

/// Whether an older aterm's Cancel stands at `root`: its decision holds every group until a
/// switch, Set up now or a Join, and an explicit pass that sets nothing leaves it standing.
pub(super) fn cancelled_at(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)? == Some(Stop::Cancelled))
}

fn stopped_at(root: &Path) -> Result<Option<Stop>, String> {
    Ok(match decision_at(root)? {
        Decision::Stopped(stop) => Some(stop),
        _ => None,
    })
}

pub(super) fn stopped(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)?.is_some() || Records::read(root)?.stopped_run())
}

pub(super) fn finished_at(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)?.is_some() || completed_at(root)?)
}

fn completed_at(root: &Path) -> Result<bool, String> {
    match progress(root)? {
        Some(progress) => Ok(saved_plan(root)?.is_some_and(|plan| {
            progress.belongs_to(&plan)
                && (progress.stopped_by(stop_request_at(root))
                    || (progress.admitted && progress.completed))
        })),
        None => first_run_complete(root),
    }
}

pub fn initial_setup_finished() -> Result<bool, String> {
    finished_at(&state_root()?)
}

pub fn initial_setup_revision() -> Result<u64, String> {
    revision_at(&state_root()?)
}

pub(super) fn revision_at(root: &Path) -> Result<u64, String> {
    let Some(progress) = progress(root)? else {
        return Ok(0);
    };
    plan_revision(Some(progress), saved_plan(root)?.as_ref())
}

/// Check again's look at the ALab vault (System State design §3.3): the Secrets vault row then
/// reads what this look heard at the ALab vault's address ([`scope::look_again`]), not a look
/// kept from long ago. The look is `aterm_secrets`' probe of the vault, made only while the root
/// LAN broker answers ([`access::lan_unavailable`]), as the Secrets stage waits for it; with none
/// it leaves the last look standing. It waits for the look on the caller's thread.
pub fn look_for_the_vault_again() {
    look_again_when(
        ensure_person_scope().is_ok(),
        access::lan_unavailable,
        scope::look_again,
    );
}

/// [`look_for_the_vault_again`] of what it reads: `look` runs only in a person's scope (`scoped`)
/// and once `broker` says the root LAN broker answers (`None`).
fn look_again_when(scoped: bool, broker: impl FnOnce() -> Option<String>, look: impl FnOnce()) {
    if scoped && broker().is_none() {
        look();
    }
}

/// The revision whose access steps the current run's Permissions stage reads
/// (`aterm_types::macos_setup::access_record`), zero with no modern run.
pub fn initial_setup_access_revision() -> Result<u64, String> {
    access_revision_at(&state_root()?)
}

pub(super) fn access_revision_at(root: &Path) -> Result<u64, String> {
    let Some(progress) = progress(root)? else {
        return Ok(0);
    };
    plan_revision(Some(progress), saved_plan(root)?.as_ref())?;
    Ok(progress.access_revision())
}

fn plan_revision(progress: Option<Progress>, plan: Option<&Plan>) -> Result<u64, String> {
    let Some(progress) = progress else {
        return Ok(0);
    };
    let plan = plan.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(plan) {
        return Err("the initial setup plan changed before its progress reset completed".into());
    }
    Ok(progress.revision)
}

/// The revision a startup's review binds ([`InitialSetupAuthorization`]): the run's, also when
/// its progress does not belong to the saved plan. A choice cut short after it saved its plan,
/// before its progress ([`RestartWrite`]), left the plan ahead of the run; no pass runs either
/// ([`revision_at`] refuses them), and the next launch offers the plan saved and measures the
/// answer from the run that progress records ([`Progress::staged`]), so setup resumes.
fn review_revision(records: &Records) -> Result<u64, String> {
    match (records.progress, &records.plan) {
        (None, _) => Ok(0),
        (Some(_), None) => Err("the initial setup plan is missing".into()),
        (Some(progress), Some(_)) => Ok(progress.revision),
    }
}

/// [`review_revision`] of the records now: what a startup compares with the revision its review
/// bound, to learn whether another process changed them since.
pub fn initial_setup_review_revision() -> Result<u64, String> {
    let records = observation_at(&state_root()?)
        .map_err(|error| error.to_string())?
        .records;
    review_revision(&records)
}

/// A coherent observation of a durably admitted modern setup run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialSetupAdmission {
    pub plan: Plan,
    pub revision: u64,
    pub completed: bool,
}

pub fn initial_setup_admission() -> Result<Option<InitialSetupAdmission>, String> {
    admission_at(&state_root()?)
}

fn admission_at(root: &Path) -> Result<Option<InitialSetupAdmission>, String> {
    if stopped_at(root)?.is_some() {
        return Ok(None);
    }
    let Records {
        plan: Some((plan, true)),
        progress: Some(progress),
        decision: Decision::Pending | Decision::Applied(true),
        stop_request,
        ..
    } = observation_at(root)
        .map_err(|error| error.to_string())?
        .records
    else {
        return Ok(None);
    };
    Ok(
        (progress.admitted && progress.belongs_to(&plan)).then_some(InitialSetupAdmission {
            plan,
            revision: progress.revision,
            // A run a person stopped has nothing left to resume, whether its
            // pass reached a stage boundary to save that or not.
            completed: progress.completed || progress.stopped_by(stop_request),
        }),
    )
}

/// One observation of the saved plan, its progress and the permission decision.
#[derive(Debug)]
pub struct InitialSetupSnapshot {
    pub plan: Plan,
    pub finished: bool,
    pub rows: Vec<Row>,
    pub revision: u64,
    /// This exact saved plan has matching admitted progress and has not been stopped.
    /// Independent of the carried Permissions approval and whether Permissions is selected.
    pub run_allowed: bool,
    /// The revision whose admission of the Permissions group this run continues:
    /// the window's access lanes key their once-per-approval steps on it, so a
    /// change to another group hands nothing to macOS again (design §6).
    pub access_revision: u64,
    pub permissions_allowed: bool,
    /// Whether System State's access steps run, wait for an admission still to come, or are held
    /// by a person's choice ([`PermissionSteps`]); `permissions_allowed` is its `Allowed`.
    pub access_steps: PermissionSteps,
    /// The plan in force is one aterm chose by itself (design §3.5), which a look that finds
    /// the ALab signal widens: System State then sends every group press, even one that
    /// leaves the plan as it is, so the plan becomes the person's and is never widened.
    pub provisional: bool,
    /// A Cancel an older aterm's first-launch review recorded stands ([`save_older_cancel`]):
    /// every group reads Leave as is and nothing runs until a group's switch turns it on.
    pub cancelled: bool,
    /// The plan in force is the one aterm set up afresh after it set aside a saved setup it
    /// could not read ([`set_aside_unreadable_setup`]), and no person has chosen since: System
    /// State says so in one calm note (design §3.5).
    pub recovered: bool,
    /// aterm saved this Mac's preferences before it changed them (`original-preferences.json`
    /// beside the records), so Restore previous settings has something to put back
    /// ([`restorable_at`]).
    pub restorable: bool,
    /// This Mac holds setup records of any run: a saved plan, a run's progress or a decision.
    /// Before aterm.app's first launch (aterm installed from a terminal and the app never
    /// opened) there are none, and the rows are the default plan's, nothing running them.
    pub begun: bool,
    pub observation: InitialSetupObservation,
}

/// Whether Restore previous settings has a saved record to put back at `root`: the preferences
/// a pass saved before it changed them. Before any pass changed a preference there is none, and
/// System State offers no Restore.
fn restorable_at(root: &Path) -> bool {
    std::fs::symlink_metadata(root.join("original-preferences.json"))
        .is_ok_and(|meta| meta.is_file())
}

/// A concurrent progress publication is expected; a record read failure is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitialSetupSnapshotError {
    Updating,
    Read(String),
    /// A newer aterm saved this Mac's setup ([`NEWER_SETUP`]): this build leaves it as it is,
    /// and reading it again changes nothing until that aterm, or a person's choice on it, does.
    Newer,
}

impl InitialSetupSnapshotError {
    /// A read that failed for `why`: [`Self::Newer`] for a newer aterm's records.
    fn read(why: String) -> Self {
        if setup_is_newer(&why) {
            Self::Newer
        } else {
            Self::Read(why)
        }
    }

    /// A read of the records at `root` that failed for `why`: [`Self::Newer`] also where the
    /// record that failed is damaged and another beside it is a newer aterm's
    /// ([`set_aside::holds_newer`]). That setup is the newer aterm's, which no set-aside touches,
    /// so the page says so in its calm line and reads nothing again by itself, rather than say it
    /// reads again and try a set-aside that is refused.
    pub(super) fn read_at(root: &Path, why: String) -> Self {
        match Self::read(why) {
            Self::Read(_) if set_aside::holds_newer(root) => Self::Newer,
            error => error,
        }
    }
}

impl std::fmt::Display for InitialSetupSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Updating => formatter.write_str("initial setup records are changing"),
            Self::Read(why) => formatter.write_str(why),
            Self::Newer => formatter.write_str(NEWER_SETUP),
        }
    }
}

impl std::error::Error for InitialSetupSnapshotError {}

pub fn initial_setup_observation() -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(&state_root().map_err(InitialSetupSnapshotError::read)?)
}

#[cfg(any(test, debug_assertions))]
pub fn initial_setup_observation_at(
    root: &Path,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(root)
}

pub(super) fn observation_at(
    root: &Path,
) -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation::at(root)
}

pub(super) fn observe_person_mutation<T>(
    busy: Result<T, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    observe_person(|root, before| observe_mutation_at(root, before, busy, operation))
}

/// A person's mutation of the records at the account's state root, past its scope check:
/// `mutate` gets the root and the records observed before it.
pub(super) fn observe_person<T>(
    mutate: impl FnOnce(&Path, Option<InitialSetupObservation>) -> InitialSetupObserved<T>,
) -> InitialSetupObserved<T> {
    let root = state_root();
    let before = root
        .as_ref()
        .ok()
        .and_then(|root| observation_at(root).ok());
    if let Err(why) = ensure_person_scope() {
        return InitialSetupObserved::new(Err(why), before);
    }
    match root {
        Ok(root) => mutate(&root, before),
        Err(why) => InitialSetupObserved::new(Err(why), before),
    }
}

pub(super) fn observe_mutation_at<T>(
    root: &Path,
    before: Option<InitialSetupObservation>,
    busy: Result<T, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let _lock = match HeldLock::try_take(root) {
        Ok(Some(lock)) => lock,
        Ok(None) => return InitialSetupObserved::new(busy, before),
        Err(why) => return InitialSetupObserved::new(Err(why), before),
    };
    let result = operation(root);
    InitialSetupObserved::new(result, observation_at(root).ok())
}

/// How long a person's press (a choice, the answer to the startup's offer, a Restore) waits for
/// the profile lock another holder has ([`observe_person_choice`]). The ALab watch's upgrade
/// holds it for one short write, so the press waits for it and is then admitted, never dropped.
/// A longer holder (`aterm doctor` in a terminal, a setup another aterm runs) outlasts it, and
/// the press comes back refused in the person's words ([`SETUP_BUSY`]), which System State
/// queues until that setup finishes (design §3.3, §3.4).
const CHOICE_LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How often a choice waiting for the profile lock asks for it again.
const CHOICE_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(25);

/// Why a person's press (a choice, the answer to the startup's offer, a Restore) did not get
/// the records: another setup held them past [`CHOICE_LOCK_WAIT`]. System State queues the
/// press then and applies it once that setup finishes (design §3.3, §3.4); `aterm doctor` says
/// it as it is.
pub const SETUP_BUSY: &str = "Another setup is running. Try again when it finishes.";

/// [`observe_person_mutation`] for a person's choice, or the answer to the startup's offer:
/// while another holds the profile lock it waits for it, at most [`CHOICE_LOCK_WAIT`], rather
/// than drop the press.
pub(super) fn observe_person_choice<T>(
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    observe_person(|root, before| observe_choice_at(root, before, choice_lock_wait(), operation))
}

/// `wait` ([`observe_choice_at`]'s), saying `first` once, before its first wait: the press
/// found the lock held, and waits from this moment.
pub(super) fn noting_first(
    mut first: impl FnMut(),
    mut wait: impl FnMut() -> bool,
) -> impl FnMut() -> bool {
    let mut said = false;
    move || {
        if !std::mem::replace(&mut said, true) {
            first();
        }
        wait()
    }
}

/// The wait a person's press gives the profile lock another holds: it asks again every
/// [`CHOICE_LOCK_POLL`] until [`CHOICE_LOCK_WAIT`] has passed ([`observe_choice_at`]).
pub(super) fn choice_lock_wait() -> impl FnMut() -> bool {
    let deadline = std::time::Instant::now() + CHOICE_LOCK_WAIT;
    move || {
        let more = std::time::Instant::now() < deadline;
        if more {
            std::thread::sleep(CHOICE_LOCK_POLL);
        }
        more
    }
}

/// [`observe_mutation_at`] for a person's press: while another holds the profile lock, `wait`
/// waits a moment and says whether to ask for it again; once it says no, the press is refused
/// in the person's words ([`SETUP_BUSY`]).
pub(super) fn observe_choice_at<T>(
    root: &Path,
    before: Option<InitialSetupObservation>,
    mut wait: impl FnMut() -> bool,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let _lock = loop {
        match HeldLock::try_take(root) {
            Ok(Some(lock)) => break lock,
            Ok(None) if wait() => {}
            Ok(None) => return InitialSetupObserved::new(Err(SETUP_BUSY.into()), before),
            Err(why) => return InitialSetupObserved::new(Err(why), before),
        }
    };
    let result = operation(root);
    InitialSetupObserved::new(result, observation_at(root).ok())
}

pub(super) fn observe_read_only<T>(
    root: Result<PathBuf, String>,
    operation: impl FnOnce(&Path) -> Result<T, String>,
) -> InitialSetupObserved<T> {
    let root = match root {
        Ok(root) => root,
        Err(why) => return InitialSetupObserved::new(Err(why), None),
    };
    let before = observation_at(&root).ok();
    let idle_before = HeldLock::profile_active(&root).is_ok_and(|active| !active);
    let result = operation(&root);
    let after = observation_at(&root).ok();
    let idle_after = HeldLock::profile_active(&root).is_ok_and(|active| !active);
    let observation =
        before.filter(|before| idle_before && idle_after && Some(before) == after.as_ref());
    InitialSetupObserved::new(result, observation)
}

/// Whether another setup holds this Mac's setup records now (profile.lock): a press that would
/// wait for it waits quietly, reading nothing, until it lets go. A state folder that does not
/// resolve, or a lock that cannot be read, holds nothing: the press then meets what it meets.
#[must_use]
pub fn setup_records_held() -> bool {
    state_root().is_ok_and(|root| HeldLock::profile_active(&root).unwrap_or(false))
}

/// aterm's macOS grants as `aterm init`'s Permissions phase lists them, read as System State's
/// Access rows read them (without a prompt): Full Disk Access, Developer Tools, debugger
/// access, and Accessibility for a run that signs out (`sign_out`).
pub(super) fn init_grants(
    sign_out: bool,
) -> Vec<(
    aterm_types::macos_setup::Access,
    aterm_machines::init::Grant,
)> {
    use aterm_types::macos_setup::Access;
    let mut selection = Selection::none();
    selection.set(Setting::Permissions, true);
    selection.set(Setting::Messages, sign_out);
    // A person's own `aterm init` lists every grant left to them, as it always did.
    let grants = access::Grants {
        acquiring: true,
        ..access::read(selection, &[])
    };
    let app = i32::try_from(std::process::id())
        .ok()
        .and_then(aterm_containment::responsible_app)
        .map_or_else(|| "aterm".to_owned(), |app| app.display_name);
    [
        Access::FullDiskAccess,
        Access::DeveloperTools,
        Access::Debugger,
        Access::Accessibility,
    ]
    .into_iter()
    .filter(|access| sign_out || *access != Access::Accessibility)
    .map(|access| {
        let row = access::observation(access, &grants, selection);
        // Developer mode whose password is not coming by itself says so, and the person's
        // routes, in its note: that is the row's why, never its detail.
        let why = if row.note.as_deref() == Some(aterm_types::macos_setup::DEBUGGER_NOT_ASKED) {
            row.note
        } else {
            row.detail.or(row.note)
        };
        (access, init_grant(access, row.standing, why, &app))
    })
    .collect()
}

/// One grant as `aterm init` lists it ([`init_grants`]): `standing` as System State reads it,
/// `why` its detail (developer mode's note where its password is not coming by itself,
/// [`aterm_types::macos_setup::DEBUGGER_NOT_ASKED`]). Full Disk Access and Accessibility are read for the app macOS holds
/// responsible for this process (`app`: aterm in aterm's tab, Terminal under Terminal), which
/// is the one this run's items need and the one to switch on; Developer Tools is read from
/// aterm's own entry, and the debugger is this Mac's.
fn init_grant(
    access: aterm_types::macos_setup::Access,
    standing: aterm_types::macos_setup::Standing,
    why: Option<String>,
    app: &str,
) -> aterm_machines::init::Grant {
    use aterm_machines::init::Grant;
    use aterm_types::macos_setup::{Access, Standing};
    match standing {
        Standing::InPlace => Grant::On,
        Standing::NeedsYou | Standing::Differs => Grant::NeedsYou(match access {
            // aterm does not ask by itself again (a person declined, or the record cannot be
            // read): the row's own words say so, and name the person's routes.
            Access::Debugger
                if why.as_deref() == Some(aterm_types::macos_setup::DEBUGGER_NOT_ASKED) =>
            {
                aterm_types::macos_setup::DEBUGGER_NOT_ASKED.to_owned()
            }
            Access::Debugger => "setup's one Mac password, which aterm asks for by itself as \
                                 setup begins, turns it on (`aterm init --only base.clt` asks \
                                 for it now)"
                .to_owned(),
            Access::FullDiskAccess | Access::Accessibility => format!(
                "System Settings \u{25b8} Privacy & Security \u{25b8} {}: switch on {app}",
                access.name()
            ),
            _ => format!(
                "System Settings \u{25b8} Privacy & Security \u{25b8} {}: switch on aterm",
                access.name()
            ),
        }),
        Standing::NotNeeded => Grant::NotNeeded(why.unwrap_or_else(|| "not needed".into())),
        _ => Grant::Unread(why.unwrap_or_else(|| "Not readable here".into())),
    }
}

pub fn initial_setup_snapshot() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::read)?;
    snapshot_in_vault(&root, live_configured_vault())
}

/// [`initial_setup_snapshot`] for a reader that writes nothing (`aterm status`): a stage a live
/// process has run past the hang limit reads as running past it, and is left for System State's
/// own reading to park ([`ledger::read_marking_hung`]).
pub fn initial_setup_snapshot_read_only() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError>
{
    let root = state_root().map_err(InitialSetupSnapshotError::read)?;
    snapshot_in_vault_reading(&root, live_configured_vault(), Hung::Leave)
}

/// What a reading does with a stage a live process has run past the hang limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Hung {
    /// Park it in the ledger, a mark that outlives the process ([`ledger::read_marking_hung`]).
    Mark,
    /// Leave the ledger as it is: the reading writes nothing.
    Leave,
}

fn snapshot_in_vault(
    root: &Path,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_in_vault_reading(root, configured, Hung::Mark)
}

/// [`snapshot_in_vault`], doing with a hung stage what `hung` says.
fn snapshot_in_vault_reading(
    root: &Path,
    configured: Result<Option<ConfiguredVault>, String>,
    hung: Hung,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let saved =
        saved_plan_record(root).map_err(|why| InitialSetupSnapshotError::read_at(root, why))?;
    let fallback = offered_plan(saved, configured.clone());
    let mut snapshot = match hung {
        Hung::Mark => snapshot_at(root, fallback)?,
        Hung::Leave => snapshot_at_reading(root, fallback, ledger::Live::now, hung)?,
    };
    if let Ok(Some(vault)) = configured {
        snapshot.plan = shown_in_vault(snapshot.plan, &snapshot.observation.records, &vault);
    }
    Ok(snapshot)
}

fn shown_in_vault(plan: Plan, records: &Records, configured: &ConfiguredVault) -> Plan {
    if joining_saved_vault(records, configured) {
        plan
    } else {
        configured.present(plan)
    }
}

fn joining_saved_vault(records: &Records, configured: &ConfiguredVault) -> bool {
    let Some((plan, true)) = &records.plan else {
        return false;
    };
    plan.selection.contains(Setting::Secrets)
        && !matches!(records.decision, Decision::Stopped(_))
        && records.progress.is_some_and(|progress| {
            progress.belongs_to(plan)
                && progress.admitted
                && !progress.completed
                && progress.done & Stage::Secrets.bit() == 0
                && !progress.admitted_vault.stands_in(plan, configured)
        })
}

#[cfg(any(test, debug_assertions))]
pub fn initial_setup_snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_at(root, fallback)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Missing,
    Pending,
    Applied(bool),
    Stopped(Stop),
}

fn decision_at(root: &Path) -> Result<Decision, String> {
    decision_record(root).map(|(decision, _)| decision)
}

/// Record the profile applied after a pass over `set` (the selection it set) that needs
/// nothing more, unless the plan still owes Mac settings outside `set`:
///
/// - a preference the authorized plan selects stands restored, or queued to set again,
///   and `set` leaves it out ([`Restored`]):
///   the record keeps the rest, less what `set` set, so once every group the plan
///   selects has been set again since a Restore, piece by piece or by Set up now, the
///   profile reads applied;
/// - or the run's progress has a Mac settings stage that `set` has no part in and has
///   not finished: the sign-outs after a preferences pass that failed, or the preferences
///   before the sign-outs' pass.
///
/// A pass checkpoints each verified preference in its remaining scope before the decision,
/// so another preference's failure or a later bookkeeping failure cannot repeat its effects.
/// A stage finishes only once every selected setting is verified. A failed state read changes
/// no completion record.
pub(super) fn record_applied(root: &Path, set: Selection) -> Result<(), String> {
    record_applied_with(root, set, decision_record)
}

/// A partial preference cohort only checkpoints a coherent, admitted modern run. An older
/// selection-only decision records the whole verified pass, never per-setting completion.
pub(super) fn record_verified_preferences(root: &Path, set: Selection) -> Result<(), String> {
    if admission_at(root)?.is_some_and(|admission| !admission.completed) {
        record_applied(root, set)?;
    }
    Ok(())
}

fn record_applied_with(
    root: &Path,
    set: Selection,
    read: impl FnOnce(&Path) -> Result<(Decision, Option<Restored>), String>,
) -> Result<(), String> {
    let planned = authorized_selection(root)?.unwrap_or_default();
    let (mut progress, plan) = (progress(root)?, saved_plan(root)?);
    let restored = match read(root)? {
        (decision, Some(restored)) => Some((
            decision == Decision::Stopped(Stop::Restored),
            restored.after_pass(set, progress, plan.as_ref()),
        )),
        (_, None) => None,
    };
    if let Some((mut verified, plan)) = progress.zip(plan.as_ref())
        && verified.belongs_to(plan)
    {
        for stage in [Stage::Workstation, Stage::Accounts] {
            let required = filtered(plan.selection, |setting| {
                stage.settings().contains(&setting)
                    && (stage != Stage::Workstation
                        || verified.only.is_none_or(|only| only.contains(setting)))
            });
            if stage == Stage::Workstation && verified.done & stage.bit() == 0 {
                let remaining = filtered(required, |setting| !set.contains(setting));
                if remaining != required {
                    verified.only = Some(remaining).filter(|only| !only.is_empty());
                }
            }
            if !required.is_empty()
                && Setting::ALL
                    .into_iter()
                    .all(|setting| !required.contains(setting) || set.contains(setting))
            {
                verified.done |= stage.bit();
            }
        }
        if Some(verified) != progress {
            save_progress(root, verified)?;
            progress = Some(verified);
        }
    }
    let stage_owed = progress.zip(plan.as_ref()).is_some_and(|(progress, plan)| {
        progress.belongs_to(plan) && !mac_settings_kept(plan, progress.done)
    });
    if !stage_owed && !restored.is_some_and(|(_, restored)| restored.owes(planned)) {
        // Applied, keeping a Restore's record while it still stands for something.
        return match restored
            .map(|(_, restored)| restored)
            .filter(|restored| restored.stands(restore_waits(root)))
        {
            Some(restored) => save_restored_decision(root, RestoredState::Applied, restored),
            None => write_json(
                &root.join("first-run.json"),
                &stamped(
                    json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
                ),
            ),
        };
    }
    match restored {
        // What this pass set stands restored no more.
        Some((stopped, restored)) => save_restored_decision(
            root,
            if stopped {
                RestoredState::Restored
            } else {
                RestoredState::Pending
            },
            restored,
        ),
        None => Ok(()),
    }
}

/// Mark the first-run decision pending as a Mac settings pass begins, before it sets
/// anything; [`record_applied`] records what the pass set once it needs nothing more.
/// What a Restore keeps ([`Restored`]) stays whole: which preferences stand restored, which
/// are queued, and the plan they were measured from. So a pass that sets one group (a
/// choice's scope, or the sign-outs), or one cut short, leaves every other group restored
/// or queued, and the profile never reads applied for a group no pass set again. Every
/// pass marks pending through this, the real one and a test's stand-in alike. An unreadable
/// decision is preserved and stops the pass before it changes preferences.
pub(super) fn record_pending(root: &Path) -> Result<(), String> {
    record_pending_with(root, decision_record)
}

fn record_pending_with(
    root: &Path,
    read: impl FnOnce(&Path) -> Result<(Decision, Option<Restored>), String>,
) -> Result<(), String> {
    match read(root)? {
        // A pass runs only for an admitted run, and none is admitted while a set-aside's hold
        // stands but by a person's choice, Set up now or a Join, which ends it.
        (_, Some(restored)) => save_restored_decision(
            root,
            RestoredState::Pending,
            Restored {
                held: false,
                ..restored
            },
        ),
        (_, None) => write_json(
            &root.join("first-run.json"),
            &stamped(json!({ "profile": PROFILE, "state": "pending" })),
        ),
    }
}

/// The first-run decision, and what of a Restore it keeps while Mac preferences stand
/// restored ([`Restored`]): a Restore, or a choice since that has not had a pass set
/// them all again.
fn decision_record(root: &Path) -> Result<(Decision, Option<Restored>), String> {
    decision_record_reviewed(root).map(|(decision, restored, _)| (decision, restored))
}

/// [`decision_record`], with the plan the first-launch review showed when a person's Cancel of
/// it is the decision ([`Records::cancelled`]).
fn decision_record_reviewed(root: &Path) -> Result<DecisionRecord, String> {
    let path = root.join("first-run.json");
    let bytes = match super::read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Decision::Missing, None, None));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let decision = parse_decision_reviewed(&bytes)?;
    flush_record_with(&path, durable_name)?;
    Ok(decision)
}

/// The first-run decision, what of a Restore it keeps, and the plan a cancelled review showed.
type DecisionRecord = (Decision, Option<Restored>, Option<Plan>);

/// The first-run decision in `bytes`, as [`decision_record`] reads it from its file.
fn parse_decision(bytes: &[u8]) -> Result<(Decision, Option<Restored>), String> {
    parse_decision_reviewed(bytes).map(|(decision, restored, _)| (decision, restored))
}

/// [`parse_decision`], with the plan a cancelled review showed ([`decision_record_reviewed`]).
/// A Cancel an older aterm saved names none: it keeps its older meaning, the saved plan as it
/// stands with aterm's own permissions held.
fn parse_decision_reviewed(bytes: &[u8]) -> Result<DecisionRecord, String> {
    let value: Value = aterm_json::from_slice(bytes).map_err(|_| "invalid first-run decision")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized first-run profile".into());
    }
    known_format(&value)?;
    let decision = match value["state"].as_str() {
        Some("pending") => Decision::Pending,
        Some("applied") => Decision::Applied(value["accounts_verified"].as_bool() == Some(true)),
        Some("cancelled") => Decision::Stopped(Stop::Cancelled),
        Some("restored") => Decision::Stopped(Stop::Restored),
        _ => return Err("unrecognized first-run decision".into()),
    };
    let restored = match decision {
        Decision::Stopped(Stop::Restored) => Some(Restored::read(&value, true)?),
        Decision::Pending | Decision::Applied(_)
            if value
                .get("restored")
                .is_some_and(|restored| *restored != Value::Bool(false)) =>
        {
            Some(Restored::read(&value, false)?)
        }
        _ => None,
    };
    let reviewed = match (decision, value.get("reviewed")) {
        (Decision::Stopped(Stop::Cancelled), Some(reviewed)) => {
            Some(parse_plan_record(reviewed)?.0)
        }
        // A Cancel an older aterm's review recorded over what a Restore brought forward: the
        // Restore's record, held by it, naming the plan that review showed.
        (Decision::Stopped(Stop::Restored), Some(reviewed))
            if restored.is_some_and(|restored| restored.held) =>
        {
            Some(parse_plan_record(reviewed)?.0)
        }
        _ => None,
    };
    Ok((decision, restored, reviewed))
}

/// The Cancel an older aterm's first-launch review recorded (System State design revision 3,
/// §3.3), which this aterm still reads ([`Records::cancelled`]) and never writes: the first-run
/// decision, cancelled, naming the plan the review showed (`reviewed`); or, where a Restore's
/// record stood (the review offered what a Restore brought forward), that record held
/// ([`Restored::held`]), naming `reviewed` too. Every group then reads Leave as is, aterm's own
/// permissions with them, and a group's switch, a Join or `aterm doctor --apply` replaces it.
#[cfg(any(test, debug_assertions))]
pub(super) fn save_older_cancel(root: &Path, reviewed: &Plan) -> Result<(), String> {
    validate_plan(reviewed)?;
    let record = match decision_record_reviewed(root)?.1 {
        Some(restored) => {
            let mut record = Restored {
                held: true,
                ..restored
            }
            .record(RestoredState::Restored);
            record
                .as_object_mut()
                .ok_or("the Restore's record is not an object")?
                .insert("reviewed".into(), plan_record(reviewed, false));
            record
        }
        None => stamped(json!({
            "profile": PROFILE,
            "state": "cancelled",
            "reviewed": plan_record(reviewed, false),
        })),
    };
    write_json(&root.join("first-run.json"), &record)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Records {
    plan: Option<(Plan, bool)>,
    progress: Option<Progress>,
    decision: Decision,
    /// What of a Restore the decision keeps while Mac preferences stand restored
    /// ([`decision_record`]).
    restored: Option<Restored>,
    /// The saved plan is one aterm chose by itself ([`save_plan_as`]).
    provisional: bool,
    /// A person's Stop no pass has folded in yet ([`request_stop_at`]).
    stop_request: Option<StopRequest>,
    /// The plan an older aterm's first-launch review showed, when a person's Cancel of it is the
    /// decision ([`save_older_cancel`]): every reader presents every group at Leave as is while
    /// it stands ([`Self::present`]).
    cancelled: Option<Plan>,
    /// Historical explicit permission request on this Mac. Kept to read older setup records;
    /// current acquisition follows the admitted Permissions selection automatically.
    person_access: bool,
}

impl Records {
    fn read(root: &Path) -> Result<Self, String> {
        let (decision, restored, cancelled) = decision_record_reviewed(root)?;
        Ok(Self {
            plan: saved_plan_record(root)?,
            progress: progress(root)?,
            decision,
            restored,
            provisional: saved_plan_provisional(root)?,
            stop_request: stop_request_at(root),
            cancelled,
            person_access: false,
        }
        .with_person_access(root))
    }

    /// `self` with [`Self::person_access`] read from `root` ([`PersonAccess`]).
    fn with_person_access(mut self, root: &Path) -> Self {
        self.person_access = match person_access_at(root) {
            PersonAccess::Asked => true,
            PersonAccess::NotAsked => false,
            PersonAccess::Older => older_rule_allows(&self),
        };
        self
    }

    fn presented_plan_record(&self) -> Option<(Plan, bool)> {
        self.plan
            .clone()
            .map(|(plan, modern)| (self.present(plan), modern))
    }

    /// The plan every reader presents as in force for the saved `plan`: what a Restore's decision
    /// names ([`Self::restore_presentation`]), with aterm's own permissions in a plan an older
    /// aterm saved. That plan predates the Permissions group, so nobody turned it off: they are
    /// approved by default as on every Mac (System State design §3.5, §6), and a choice of another
    /// group keeps them. Only an older aterm's Cancel holds them. A person's Cancel of the
    /// first-launch review leaves every group as it is, aterm's own permissions with them
    /// ([`Self::cancelled`]): nobody approved any of it.
    fn present(&self, plan: Plan) -> Plan {
        // A Restore's decision held by a person's Cancel (one pressed after it, or a Cancel of
        // the review a Restore brought forward) holds every group as it is too.
        let held = self.decision == Decision::Stopped(Stop::Restored)
            && self.restored.is_some_and(|restored| restored.held);
        if self.cancelled.is_some() || held {
            return Plan {
                selection: Selection::none(),
                ..plan
            };
        }
        let mut plan = self.restore_presentation(plan);
        if self.plan.as_ref().is_some_and(|(_, modern)| !modern)
            && self.decision != Decision::Stopped(Stop::Cancelled)
        {
            plan.selection.set(Setting::Permissions, true);
        }
        plan
    }

    /// Restore's final decision can publish its Leave-as-is selection before the
    /// plan rename. Present that durable choice while the old admitted run still
    /// owns the physical plan (or no run exists). A later choice's mismatched or
    /// unadmitted progress takes precedence: its plan is staged, not restored.
    /// An early Restore decision still names the original selection, and legacy
    /// records without a captured selection keep their existing presentation.
    fn restore_presentation(&self, mut plan: Plan) -> Plan {
        if self.decision == Decision::Stopped(Stop::Restored)
            && let Some(selection) = self
                .restored
                .filter(|restored| !restored.held)
                .and_then(|restored| restored.selection)
            && self.progress.is_none_or(|progress| {
                progress.admitted
                    && self
                        .plan
                        .as_ref()
                        .is_some_and(|(saved, _)| progress.belongs_to(saved))
            })
        {
            plan.selection = selection;
        }
        plan
    }

    /// Whether a person stopped the staged or admitted run of these records, on record
    /// or by a request no pass has folded in yet.
    fn stopped_run(&self) -> bool {
        self.progress.is_some_and(|progress| {
            progress.stopped_by(self.stop_request)
                && self
                    .plan
                    .as_ref()
                    .is_some_and(|(plan, _)| progress.belongs_to(plan))
        })
    }
}

/// What the deferred preference pass may still set, read while its caller holds
/// profile.lock. An interrupted modern admission gives no authority and keeps
/// its owed work for confirmation; legacy selection-only setup keeps its scope.
/// A Restore leaves a preference out until a later choice queues or sets it.
pub(super) fn withheld_selection(root: &Path) -> Result<Option<Selection>, String> {
    let records = observation_at(root)
        .map_err(|error| error.to_string())?
        .records;
    if matches!(records.decision, Decision::Stopped(_)) || records.stopped_run() {
        return Ok(None);
    }
    let saved = records.plan.as_ref().map(|(plan, _)| plan);
    if (records.progress.is_some() || records.plan.as_ref().is_some_and(|(_, modern)| *modern))
        && (!matches!(
            records.decision,
            Decision::Pending | Decision::Applied(true)
        ) || !records.progress.is_some_and(|progress| {
            progress.admitted && saved.is_some_and(|plan| progress.belongs_to(plan))
        }))
    {
        return Ok(None);
    }
    let selected = saved.map_or_else(Selection::all, |plan| plan.selection);
    Ok(Some(match records.restored {
        Some(restored) => {
            let queued = restored.queue(records.progress, saved);
            filtered(selected, |setting| {
                !restored.left.contains(setting) || queued.contains(setting)
            })
        }
        None => selected,
    }))
}

/// What a follow-up does with a key a Restore could not put back for want of Full Disk Access
/// ([`restore_owed`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RestoreOwed {
    /// Put it back: its row still stands restored.
    PutBack,
    /// Keep it for a later follow-up: a choice since queued its row to be set again and no
    /// pass has, or the records cannot be read now.
    Wait,
    /// Let it go: a pass set its row again since, or the plan in force sets it.
    LetGo,
}

/// What a follow-up does with a key of row `setting` (`None`: a key no row of this build
/// writes) that a Restore could not put back for want of Full Disk Access: put it back while the
/// first-run record keeps the row restored ([`Restored`]) and nothing queues it to be set again.
///
/// An admission or a pass keeps that record while it stands for something, a put-back waiting
/// for the grant included ([`Restored::stands`]); an earlier build dropped it once the plan in
/// force selected no preference that stood restored. With no record, a row that plan leaves at
/// Leave as is still stands as the Restore left it, since a pass that sets it needs the grant
/// and its own follow-up lets the key go: so it is put back, as after a Join, a choice of another
/// group or the run a restored setup brought forward admits (design §3.4). One the plan selects
/// is set by its run; one a person's Cancel ended is let go.
pub(super) fn restore_owed(root: &Path, setting: Option<Setting>) -> RestoreOwed {
    let Ok(records) = Records::read(root) else {
        return RestoreOwed::Wait;
    };
    let Some(restored) = records.restored else {
        let left_as_is = records.decision != Decision::Stopped(Stop::Cancelled)
            && setting
                .zip(records.plan.as_ref())
                .is_some_and(|(setting, (plan, _))| !plan.selection.contains(setting));
        return if left_as_is {
            RestoreOwed::PutBack
        } else {
            RestoreOwed::LetGo
        };
    };
    let Some(setting) = setting else {
        return RestoreOwed::PutBack;
    };
    let saved = records.plan.as_ref().map(|(plan, _)| plan);
    if restored.queue(records.progress, saved).contains(setting) {
        RestoreOwed::Wait
    } else if restored.left.contains(setting) {
        RestoreOwed::PutBack
    } else {
        RestoreOwed::LetGo
    }
}

fn snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_at_when(root, fallback, ledger::Live::now)
}

/// [`snapshot_at`], reading the clock and the boot as `live` says.
fn snapshot_at_when(
    root: &Path,
    fallback: Plan,
    live: impl FnOnce(bool) -> ledger::Live,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_at_reading(root, fallback, live, Hung::Mark)
}

/// [`snapshot_at_when`], doing with a hung stage what `hung` says.
fn snapshot_at_reading(
    root: &Path,
    fallback: Plan,
    live: impl FnOnce(bool) -> ledger::Live,
    hung: Hung,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let observation = observation_at(root)?;
    let active = HeldLock::profile_active(root).map_err(InitialSetupSnapshotError::read)?;
    // The latest hold's mark: a stage is alive only while the lock is held by the hold that
    // began it.
    let live = ledger::Live {
        holder: HeldLock::profile_holder(root),
        ..live(active)
    };
    let raw_plan = observation
        .records
        .plan
        .as_ref()
        .map_or(&fallback, |(plan, _)| plan);
    let progress = observation
        .records
        .progress
        .filter(|progress| progress.belongs_to(raw_plan));
    let ledger = match hung {
        Hung::Mark => ledger::read_marking_hung(root, progress, live),
        Hung::Leave => ledger::read(root, progress),
    };
    Ok(InitialSetupSnapshot {
        recovered: set_aside::shown(root),
        restorable: restorable_at(root),
        ..snapshot_from_observation(fallback, live, &ledger, observation)
    })
}

#[cfg(test)]
fn snapshot_with(
    fallback: Plan,
    activity: impl FnOnce() -> Result<bool, String>,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    // As `observation::at` takes them: an observation that met a write is taken again.
    let records = observation::settled(observation::READS, &mut |_| {}, || {
        let records = read().map_err(InitialSetupSnapshotError::read)?;
        if records != read().map_err(InitialSetupSnapshotError::read)? {
            return Err(InitialSetupSnapshotError::Updating);
        }
        Ok(records)
    })?;
    let active = activity().map_err(InitialSetupSnapshotError::read)?;
    Ok(snapshot_from_observation(
        fallback,
        ledger::Live {
            active,
            ..ledger::Live::default()
        },
        &ledger::Ledger::default(),
        observation::from_records(records),
    ))
}

/// Whether System State's access steps (the window's Permissions lanes) run for the setup records
/// in force (design §6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PermissionSteps {
    /// They run: a person pressed for them on this Mac ([`note_person_asked_for_access`]), with an
    /// admitted run whose plan selects Permissions, the run a Restore ended (a Restore never
    /// withdraws aterm's own permissions), or an older aterm's verified setup.
    Allowed,
    /// Not yet: no plan or no admitted run yet. They may still come with nobody's choice.
    NotYet,
    /// Held until a person chooses again: their Cancel or Stop holds them (a Restore's record a
    /// Cancel holds among them), the admitted run's plan leaves Permissions out, or no person
    /// pressed for them: the grants only a person can give are offered, and an automatic setup
    /// decision opens none of their pages (setup's one early elevation is its own lane).
    Off,
}

/// The record of whether a person pressed for macOS's grants on this Mac
/// ([`Records::person_access`]).
const PERSON_ACCESS: &str = "person-access.json";

/// The progress record's mark that this build wrote it, and so reads macOS's grants as a
/// person's press ([`PERSON_ACCESS`]). A record without it is an older aterm's.
const PROGRESS_BY_PRESS: &str = "access_by_press";

/// Whether a person pressed for macOS's grants on this Mac, as [`PERSON_ACCESS`] says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PersonAccess {
    /// No record, under an older aterm's progress record (or none): that aterm read its admitted
    /// Permissions group as the authority (it struck the group from every automatic answer, so
    /// an admitted one was a person's), and its rule stands ([`older_rule_allows`]) until this
    /// build first writes the records, which carries an approval over as [`Self::Asked`]
    /// ([`carry_older_person_access`]): an update stops no step a person already approved.
    Older,
    /// A person pressed for them on this Mac ([`note_person_asked_for_access`]).
    Asked,
    /// Nobody pressed for them on this Mac: this build wrote the records and no person pressed,
    /// the record is damaged, or it names another Mac (a home migrated or restored here carries
    /// it, and no one pressed on this one).
    NotAsked,
}

const MAX_SETUP_RECORD_BYTES: u64 = 64 * 1024;

fn person_access_at(root: &Path) -> PersonAccess {
    person_access_at_with_host(root, aterm_containment::consent::host_uuid().as_deref())
}

fn person_access_at_with_host(root: &Path, host: Option<&str>) -> PersonAccess {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    match std::fs::symlink_metadata(root) {
        Ok(meta) if meta.file_type().is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return PersonAccess::Older,
        Ok(_) | Err(_) => return PersonAccess::NotAsked,
    }
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(root.join(PERSON_ACCESS))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return if older_progress_without_press(root) {
                PersonAccess::Older
            } else {
                PersonAccess::NotAsked
            };
        }
        Err(_) => return PersonAccess::NotAsked,
    };
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return PersonAccess::NotAsked;
    };
    let euid = aterm_machines::port::Live::new().euid();
    let Ok(metadata) = file.metadata() else {
        return PersonAccess::NotAsked;
    };
    if !metadata.file_type().is_file()
        || metadata.nlink() != 1
        || metadata.uid() != euid
        || metadata.mode() & 0o6077 != 0
        || metadata.len() > MAX_SETUP_RECORD_BYTES
    {
        return PersonAccess::NotAsked;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(MAX_SETUP_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_SETUP_RECORD_BYTES as usize
    {
        return PersonAccess::NotAsked;
    }
    let asked = aterm_json::from_slice::<Value>(&bytes).is_ok_and(|value| {
        value["profile"].as_str() == Some(PROFILE)
            && value["asked"].as_bool() == Some(true)
            && value["host"].as_str() == Some(host)
    });
    if asked {
        PersonAccess::Asked
    } else {
        PersonAccess::NotAsked
    }
}

/// Whether `root` has either no progress record yet or a regular progress record an older aterm
/// wrote without [`PROGRESS_BY_PRESS`]. A marked, unreadable, symlinked or special file never
/// qualifies for [`PersonAccess::Older`].
fn older_progress_without_press(root: &Path) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(root.join("initial-setup.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(_) => return false,
    };
    let euid = aterm_machines::port::Live::new().euid();
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.file_type().is_file()
        || metadata.uid() != euid
        || metadata.len() > MAX_SETUP_RECORD_BYTES
    {
        return false;
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    if file
        .take(MAX_SETUP_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_SETUP_RECORD_BYTES as usize
    {
        return false;
    }
    aterm_json::from_slice::<Value>(&bytes)
        .is_ok_and(|value| value.get(PROGRESS_BY_PRESS).is_none())
}

/// Whether an older aterm's rule allows the access steps for `records`: its Permissions group,
/// admitted, with no press asked for.
fn older_rule_allows(records: &Records) -> bool {
    let older = Records {
        person_access: true,
        ..records.clone()
    };
    steps_of(&older) == PermissionSteps::Allowed
}

/// Before this build first writes a plan or a progress record over an older aterm's
/// ([`PersonAccess::Older`]): an approval that aterm's rule gave carries over as a person's
/// ([`PersonAccess::Asked`]), so it outlives the records that showed it. Nothing is written
/// otherwise: this build's progress record then reads as nobody's press.
fn carry_older_person_access(root: &Path) -> Result<(), String> {
    if person_access_at(root) != PersonAccess::Older {
        return Ok(());
    }
    // Unreadable records carry nothing over: their reader refuses them on its own.
    if Records::read(root).is_ok_and(|records| records.person_access) {
        note_person_access_at(root, true)?;
    }
    Ok(())
}

/// Before an explicit pass rewrites `first-run.json` ahead of its plan and progress writes:
/// durably record whatever [`PersonAccess::Older`] answered before the decision changes, so an
/// older unverified (`Applied(false)`) progress record cannot be promoted by a pending decision.
fn seal_older_person_access(root: &Path) -> Result<(), String> {
    if person_access_at(root) != PersonAccess::Older {
        return Ok(());
    }
    if let Ok(records) = Records::read(root) {
        note_person_access_at(root, records.person_access)?;
    }
    Ok(())
}

/// A person's press on System State asked for macOS's grants: durably say so, for this Mac's
/// hardware UUID, so the window's access steps may put macOS's prompts and pages on screen from
/// now on, a Quit & Reopen's next launch included. Only the window calls this, once it read the
/// press as a person's.
///
/// # Errors
///
/// The state root cannot be resolved, or the record cannot be written.
pub fn note_person_asked_for_access() -> Result<(), String> {
    note_person_asked_at(&state_root()?)
}

fn note_person_asked_at(root: &Path) -> Result<(), String> {
    note_person_access_at(root, true)
}

/// Write [`PERSON_ACCESS`]: `asked` for this Mac's hardware UUID.
fn note_person_access_at(root: &Path, asked: bool) -> Result<(), String> {
    note_person_access_at_with_host(
        root,
        asked,
        aterm_containment::consent::host_uuid().as_deref(),
    )
}

fn note_person_access_at_with_host(
    root: &Path,
    asked: bool,
    host: Option<&str>,
) -> Result<(), String> {
    let Some(host) = host.map(str::trim).filter(|host| !host.is_empty()) else {
        return Err("this Mac's hardware UUID could not be read".into());
    };
    write_json_with_flush(
        &root.join(PERSON_ACCESS),
        &json!({"profile":PROFILE,"asked":asked,"host":host}),
        durable_name,
    )
}

/// [`PermissionSteps`] of `records`, with `progress` their run's progress when it belongs to the
/// saved plan and `plan` the plan every reader presents as in force ([`Records::present`]).
/// `Allowed` is exactly the snapshot's `permissions_allowed`.
fn permission_steps(records: &Records, progress: Option<Progress>, plan: &Plan) -> PermissionSteps {
    let cancelled = records.decision == Decision::Stopped(Stop::Cancelled);
    let run_stopped = progress.is_some_and(|progress| progress.stopped_by(records.stop_request));
    let admitted = records.plan.is_some()
        && progress.is_some_and(|progress| progress.admitted)
        && matches!(
            records.decision,
            Decision::Pending | Decision::Applied(true) | Decision::Stopped(Stop::Restored)
        );
    let selected = plan.selection.contains(Setting::Permissions);
    // A Restore puts back Mac preferences and never withdraws aterm's own permissions: the
    // run it ends stays admitted ([`leave_restored_as_is`]), so the access steps go on by
    // themselves. Only a person's Cancel or Stop holds them.
    let allowed = !cancelled
        && !run_stopped
        && match records.progress {
            Some(_) => admitted && selected,
            None => {
                !records.plan.as_ref().is_some_and(|(_, modern)| *modern)
                    && records.decision == Decision::Applied(true)
            }
        };
    let held_restore = records.decision == Decision::Stopped(Stop::Restored)
        && records.restored.is_some_and(|restored| restored.held);
    if allowed {
        PermissionSteps::Allowed
    } else if cancelled
        || run_stopped
        || held_restore
        || (records.progress.is_some() && admitted && !selected)
    {
        PermissionSteps::Off
    } else {
        PermissionSteps::NotYet
    }
}

fn permission_steps_at(root: &Path) -> Result<PermissionSteps, String> {
    let records = observation_at(root)
        .map_err(|error| error.to_string())?
        .records;
    Ok(steps_of(&records))
}

/// [`PermissionSteps`] of `records` alone ([`permission_steps_at`]).
fn steps_of(records: &Records) -> PermissionSteps {
    // With no saved plan the fallback never decides the answer: the progress branch needs a
    // saved plan, and the legacy branch reads only the decision.
    let raw_plan = records
        .plan
        .as_ref()
        .map_or_else(Plan::default, |(plan, _)| plan.clone());
    let progress = records
        .progress
        .filter(|progress| progress.belongs_to(&raw_plan));
    let plan = records.present(raw_plan);
    permission_steps(records, progress, &plan)
}

fn snapshot_from_observation(
    fallback: Plan,
    live: ledger::Live,
    ledger: &ledger::Ledger,
    observation: InitialSetupObservation,
) -> InitialSetupSnapshot {
    let records = &observation.records;
    let raw_plan = records
        .plan
        .as_ref()
        .map_or(fallback, |(plan, _)| plan.clone());
    let progress = records
        .progress
        .filter(|progress| progress.belongs_to(&raw_plan));
    let plan = records.present(raw_plan);
    let stopped = match records.decision {
        Decision::Stopped(stop) => Some(stop),
        _ => None,
    };
    let completed = match records.progress {
        Some(_) => progress.is_some_and(|progress| progress.admitted && progress.completed),
        None => records.decision == Decision::Applied(true),
    };
    // A person's Stop ends this revision's work, its access steps included,
    // from the moment it is asked for.
    let run_stopped = progress.is_some_and(|progress| progress.stopped_by(records.stop_request));
    let finished = stopped.is_some() || completed || run_stopped;
    let run_allowed = records.plan.is_some()
        && progress.is_some_and(|progress| progress.admitted)
        && !run_stopped
        && matches!(
            records.decision,
            Decision::Pending | Decision::Applied(true) | Decision::Stopped(Stop::Restored)
        );
    let access_steps = permission_steps(records, progress, &plan);
    let permissions_allowed = access_steps == PermissionSteps::Allowed;
    let rows = Stage::ALL
        .iter()
        .filter(|stage| stage.selected(&plan))
        .map(|stage| {
            let completed = progress.is_some_and(|progress| progress.done & stage.bit() != 0);
            // Restored preferences a choice since left alone are no run's to apply.
            let restoring = *stage == Stage::Workstation
                && (stopped == Some(Stop::Restored)
                    || (completed
                        && records
                            .restored
                            .is_some_and(|restored| restored.owes(plan.selection))));
            // What the run's ledger says of the stage: running (past the hang limit too),
            // resumed after aterm stopped, or parked until its pause comes due or Check again
            // (design §6.5).
            let verdict = progress.map_or(ledger::Verdict::Idle, |progress| {
                ledger.verdict(*stage, progress.done, progress.running, live)
            });
            Row {
                name: stage.name().into(),
                status: if restoring {
                    "stopped"
                } else if completed {
                    "applied"
                } else if finished {
                    "stopped"
                } else {
                    match verdict {
                        ledger::Verdict::Parked(_) => WAITING,
                        ledger::Verdict::Running
                        | ledger::Verdict::Overdue
                        | ledger::Verdict::Resuming { running: true } => "running",
                        ledger::Verdict::Resuming { running: false } | ledger::Verdict::Idle => {
                            "pending"
                        }
                    }
                }
                .into(),
                detail: if restoring {
                    "Restore requested; see results.".into()
                } else if finished && !completed {
                    "Not applied by this run.".into()
                } else if completed {
                    String::new()
                } else {
                    match verdict {
                        ledger::Verdict::Parked(park) => ledger::parked_words(*stage, park),
                        ledger::Verdict::Overdue => ledger::overdue_words(*stage),
                        ledger::Verdict::Resuming { .. } => ledger::RESUMING.into(),
                        ledger::Verdict::Running | ledger::Verdict::Idle => String::new(),
                    }
                },
                elapsed_ms: None,
            }
        })
        .collect();
    InitialSetupSnapshot {
        plan,
        finished,
        rows,
        revision: progress.map_or(0, |progress| progress.revision),
        run_allowed,
        access_revision: progress.map_or(0, Progress::access_revision),
        permissions_allowed,
        access_steps,
        provisional: records.plan.is_some() && records.provisional && records.cancelled.is_none(),
        cancelled: records.cancelled.is_some(),
        recovered: false,
        restorable: false,
        begun: records.plan.is_some()
            || records.progress.is_some()
            || records.decision != Decision::Missing,
        observation,
    }
}

#[cfg(test)]
fn status_at(root: &Path, fallback: Plan) -> Result<(Plan, bool, Vec<Row>), String> {
    let snapshot = snapshot_at(root, fallback).map_err(|error| error.to_string())?;
    Ok((snapshot.plan, snapshot.finished, snapshot.rows))
}

trait InitialHost {
    /// The automatic swarm master switch, read before the setup lane reaches the vault.
    fn swarm_enabled(&mut self) -> Result<bool, String> {
        Ok(true)
    }
    /// A legacy plan's one pass: its original workstation rows, sign-outs included.
    fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String>;
    /// The Workstation stage: the plan's preferences without the sign-outs, or `only` these
    /// ([`Progress::only`]).
    fn workstation(
        &mut self,
        root: &Path,
        only: Option<Selection>,
    ) -> Result<Option<Report>, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    /// What the Spotlight stage this host ran last read of its row ([`StageEvent::readback`]),
    /// taken once: none from a stage that read none. The Mac settings and sign-out stages'
    /// readings come on their pass's page ([`PassPage::readback`]).
    fn spotlight_readback(&mut self) -> WorkstationReadback {
        WorkstationReadback::EMPTY
    }
    /// The Permissions stage of `revision`, complete as `access::stage_rows` says (System State
    /// design §6).
    fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String>;
    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String>;
    fn secrets_pace(&mut self) -> Option<Pace>;
    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String>;
    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    /// Read current repository inputs, credential routing and required checkout readiness
    /// without changing them. A prior completed stage remains current only while this is ready.
    fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String>;
    fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String>;
    /// The reason the failed part of the Repositories stage, as [`Self::repositories`] last ran
    /// it, gives where the step that stopped decided one
    /// ([`aterm_machines::init_observe::failure_note`]): System State's row says it in place of
    /// the stage's own line.
    fn repositories_failure(&mut self) -> Option<&'static str> {
        None
    }
    /// Whether the Repositories stage, as [`Self::repositories`] last ran it, stopped because this
    /// Mac has no GitHub token to sign in with ([`aterm_machines::init_observe::Wait::Token`]).
    fn repositories_want_token(&mut self) -> bool {
        false
    }
    /// The Messages and FaceTime sign-outs, with the Accessibility they need. Each reads this
    /// login's session right before its own clicks, and one that cannot take them waits.
    fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String>;
    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String>;
    /// The settings [`Self::withheld`] would change now (`super::owed_with_full_disk_access`):
    /// the pass marks them busy while it runs, as it marks a stage's, so a reading taken
    /// meanwhile never stands, and reads them again once it ends.
    fn withheld_owed(&mut self, _root: &Path) -> Vec<Setting> {
        Vec::new()
    }
    /// This boot of the Mac, which the run's ledger stamps each begin with
    /// ([`ledger::boot_session`]).
    fn boot_session(&mut self) -> Option<u128> {
        ledger::boot_session()
    }
    /// The running software, without probing the machine or changing setup's admission.
    fn software_epoch(&mut self) -> Option<ledger::SoftwareEpoch> {
        ledger::SoftwareEpoch::current()
    }
    /// Now, in seconds since the epoch, which the run's ledger stamps each begin with.
    fn now_secs(&mut self) -> u64 {
        ledger::now_secs()
    }
    /// Concurrent Secrets lane that starts beside [`Stage::Workstation`] once the root LAN broker
    /// answers, when this host supports parallel execution.
    fn secrets_lane(&mut self) -> Option<Box<dyn SecretsLane>> {
        None
    }
}

/// Self-contained Secrets stage runner spawned in a lane beside [`Stage::Workstation`].
trait SecretsLane: Send {
    fn swarm_enabled(&mut self) -> Result<bool, String> {
        Ok(true)
    }
    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String>;
    fn secrets_pace(&mut self) -> Option<Pace>;
    /// Why the root LAN broker the lane's join reaches the LAN through does not answer now, in
    /// its own words ([`access::lan_unavailable`]); `None` once it answers. The lane waits for
    /// it, never for a person.
    fn lan_unavailable(&mut self) -> Option<String>;
    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String>;
    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    fn now_secs(&mut self) -> u64 {
        ledger::now_secs()
    }
}

/// One stage of a setup pass starting or ending, for System State: the settings
/// it changes (the page's busy rows) and the rows to read again once it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageEvent<'a> {
    /// The admitted run whose stage owns this event.
    pub revision: u64,
    /// The settings whose rows the stage changes.
    pub settings: &'a [Setting],
    /// The System State rows the stage's work can change.
    pub subjects: &'a [Subject],
    /// Whether the stage is starting (`true`) or has ended (`false`).
    pub running: bool,
    /// At a stage's end, what its own run read of its rows: each a complete item cohort's native
    /// reading, dated from before that read ([`WorkstationReadback`]), never an operation's
    /// completion stamp. System State takes each as its row's reading now while the stage's
    /// revision, choice and touch fences hold (design §3.5). Empty at a start, from a stage that
    /// read none, and from one stopped.
    pub readback: &'a WorkstationReadback,
}

/// What a setup pass asks between its stages.
#[derive(Clone, Copy)]
pub struct SetupHooks {
    /// Whether a person asked the run of this revision to stop (System State's
    /// Stop). Asked before every stage, and at the pass's end.
    pub stop: fn(u64) -> bool,
    /// A stage started or ended.
    pub stage: for<'a> fn(StageEvent<'a>),
    /// Whether the pass gives way at this stage boundary: the login job's, once a window runs
    /// setup or an update replaced its build. Asked before every stage. Unlike a Stop it ends
    /// nothing of the run: the stage does not begin, the pass ends at once with what it left to
    /// try again ([`SetupNext::Retry`]), and the next pass (the window's) goes on from there.
    pub gives_way: fn() -> bool,
}

impl SetupHooks {
    /// A pass nobody stops or watches.
    pub const NONE: Self = Self {
        stop: |_| false,
        stage: |_| {},
        gives_way: || false,
    };
}

/// What a pass left for the next one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupNext {
    /// Nothing: every selected stage finished, or a person stopped the run.
    Done,
    /// Something changes by itself (a service starting, the network): try again.
    Retry,
    /// Nothing changes until a person acts or a local vault/network fact changes.
    /// Release the setup claim and wait instead of running another pass.
    Park,
}

impl SetupNext {
    /// The later of two stages' needs: any retry wins over parking.
    fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Retry, _) | (_, Self::Retry) => Self::Retry,
            (Self::Park, _) | (_, Self::Park) => Self::Park,
            (Self::Done, Self::Done) => Self::Done,
        }
    }

    /// What one stage's rows leave: done when every row is ready ([`row_ready`]), a park when
    /// every row left waits on a person ([`WAITING`]), on a vault that did not answer
    /// ([`UNREACHED`]) or answered that none is there ([`ABSENT`]), or on what an engine put off
    /// ([`super::DEFERRED`]).
    fn of(rows: &[Row]) -> Self {
        if rows_ready(rows) {
            Self::Done
        } else if !rows.is_empty()
            && rows.iter().all(|row| {
                row_ready(row)
                    || matches!(
                        row.status.as_str(),
                        WAITING | UNREACHED | ABSENT | super::DEFERRED
                    )
            })
        {
            Self::Park
        } else {
            Self::Retry
        }
    }
}

/// A row's status when only a person moves it: the pass parks instead of retrying it
/// ([`SetupNext::Park`]), and that person's act (an approval, a grant) wakes it.
pub(super) const WAITING: &str = "waiting";

/// A row's status when the ALab vault's address answered that it holds none, or another vault
/// answered there ([`scope::NO_VAULT`]): the pass parks, and the Secrets retry pause asks again
/// by itself on its slow pace ([`secrets_retry::retry_due`]), since a vault set up there, or
/// another network, moves it without a person.
pub(super) const ABSENT: &str = "absent";

/// A row's status when the ALab vault did not answer ([`scope::NOT_REACHED`]): the
/// pass parks, and the Secrets retry pause wakes it to ask again
/// ([`secrets_retry::retry_due`]), since a network, the LAN broker or the vault
/// coming back moves it without a person.
pub(super) const UNREACHED: &str = "unreached";

/// Git availability and the admitted executable behind its selected command.
#[derive(Clone, Debug, PartialEq, Eq)]
enum RepositoryGit {
    Missing,
    Installed(PathBuf),
}

impl RepositoryGit {
    fn read(sys: &dyn aterm_machines::port::System) -> Self {
        aterm_machines::items::github::installed_git_identity(sys)
            .map_or(Self::Missing, Self::Installed)
    }
}
/// Local facts that can wake a parked setup run. This is an observation, never
/// authorization to run its stages.
#[derive(Clone, PartialEq, Eq)]
pub struct InitialSetupParkMark {
    vault: Option<aterm_secrets::client::Status>,
    network_run: Option<u64>,
    materialization_run: Option<u64>,
    materialization: Option<aterm_secrets::client::MaterializationMark>,
    subscriber_run: Option<u64>,
    subscriber: Option<bool>,
    /// The unfinished Secrets join whose broker readiness is observed.
    join_run: Option<u64>,
    /// The network this Mac's traffic leaves on ([`aterm_secrets::network::default_route`]), for
    /// selected Secrets or Repositories while setup remains pending: an address beside it
    /// (AirDrop, a VPN's `utun`, a temporary IPv6 address) is no move, so it wakes nothing.
    network: Option<String>,
    /// The pending run whose Repositories stage is selected: a changed Git can invalidate
    /// completed checkout evidence too ([`InitialSetupParkMark::facts_changed_since`]).
    git_run: Option<u64>,
    /// The selected Git's admitted build, read only for [`Self::git_run`].
    git: Option<RepositoryGit>,
    /// The selected repository inputs and pinned helper, without running either Git or helper.
    repository_inputs: Option<aterm_machines::init_github::RepositoryInputMark>,
    /// An admitted pending Workstation stage whose local console can become available.
    workstation_run: Option<u64>,
    console: Option<bool>,
    /// A still-owed, unheld Caps setting whose native service set can become available.
    keyboard_run: Option<u64>,
    keyboards: Option<InputServiceSet>,
    /// Positive native readability, never a verified mapping or an automatic-write receipt.
    readable_keyboards: InputServiceSet,
    retry_due: Option<u64>,
    mac_settings_retry_due: Option<u64>,
    /// The latest pause of a parked, unfinished stage that is over ([`ledger::Ledger::park_due`]).
    stage_retry_due: Option<u64>,
    /// The earliest Secrets, Mac settings or parked stage's retry not due yet
    /// ([`InitialSetupParkMark::next_due`]).
    retry_ahead: Option<u64>,
    /// Broker readiness for an unfinished Secrets join, observed without prompts.
    broker_unready: Option<bool>,
    /// An admitted, unfinished Accounts or Permissions stage
    /// ([`InitialSetupParkMark::waits_on_settings`]).
    in_settings: bool,
}

impl std::fmt::Debug for InitialSetupParkMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialSetupParkMark")
            .finish_non_exhaustive()
    }
}

impl InitialSetupParkMark {
    /// Whether a still-relevant local fact changed during this run's last
    /// pass. Unknown readings, retired scopes and retry-deadline bookkeeping
    /// supply no new fact. This comparison never authorizes a stage.
    pub fn facts_changed_since(&self, before: &Self) -> bool {
        fn changed<T: PartialEq>(current: &Option<T>, before: &Option<T>) -> bool {
            current
                .as_ref()
                .is_some_and(|current| before.as_ref() != Some(current))
        }
        if self.workstation_run.is_some()
            && self.workstation_run == before.workstation_run
            && changed(&self.console, &before.console)
        {
            return true;
        }
        if self.keyboard_run.is_some()
            && self.keyboard_run == before.keyboard_run
            && (changed(&self.keyboards, &before.keyboards)
                || self
                    .readable_keyboards
                    .gained_from(&before.readable_keyboards))
        {
            return true;
        }
        if self.network_run.is_none() || self.network_run != before.network_run {
            return false;
        }
        changed(&self.vault, &before.vault)
            || changed(&self.network, &before.network)
            || (self.materialization_run.is_some()
                && self.materialization_run == before.materialization_run
                && changed(&self.materialization, &before.materialization))
            || (self.subscriber_run.is_some()
                && self.subscriber_run == before.subscriber_run
                && (changed(&self.subscriber, &before.subscriber)
                    || changed(&self.broker_unready, &before.broker_unready)))
            // Activation can replace Git behind the same stable shim; losing it also changes
            // whether a completed repository stage can still be verified.
            || (self.git_run.is_some()
                && self.git_run == before.git_run
                && (changed(&self.repository_inputs, &before.repository_inputs)
                    || match (&self.git, &before.git) {
                        (Some(now), Some(old)) => now != old,
                        _ => false,
                    }))
    }

    /// Read broker readiness only for an unfinished Secrets stage.
    fn with_lan_broker(mut self, unready: impl FnOnce() -> bool) -> Self {
        self.broker_unready = self.join_run.map(|_| unready());
        self
    }

    /// Read the selected Git while this pending run still selects Repositories.
    fn with_git(mut self, read: impl FnOnce() -> Option<RepositoryGit>) -> Self {
        self.git = self.git_run.and_then(|_| read());
        self
    }

    fn with_repository_inputs(
        mut self,
        read: impl FnOnce() -> Option<aterm_machines::init_github::RepositoryInputMark>,
    ) -> Self {
        self.repository_inputs = self.git_run.and_then(|_| read());
        self
    }

    fn with_console(mut self, read: impl FnOnce() -> Option<bool>) -> Self {
        self.console = self.workstation_run.and_then(|_| read());
        self
    }

    fn with_keyboard_services(mut self, sys: &dyn aterm_machines::port::System) -> Self {
        if self.keyboard_run.is_some() && self.console == Some(true) {
            match sys
                .keyboard_service_observations()
                .and_then(|rows| InputServiceSet::observed(&rows))
            {
                Ok((present, readable)) => {
                    self.keyboards = Some(present);
                    self.readable_keyboards = readable;
                }
                Err(_) => {
                    // A failed property read establishes no positive readiness. Preserve
                    // independent membership observations without probing preferences.
                    self.keyboards = sys
                        .keyboard_service_ids()
                        .ok()
                        .and_then(|ids| InputServiceSet::new(ids).ok());
                }
            }
        }
        self
    }

    /// A contended or unreadable local state gives no new fact. Keep the
    /// last known materialization, service or Git state only within its same run.
    pub fn retain_known_from(&mut self, previous: &Self) {
        if self.keyboard_run.is_some()
            && self.keyboard_run == previous.keyboard_run
            && self.keyboards.is_none()
        {
            self.keyboards = previous.keyboards.clone();
        }
        if self.keyboard_run.is_some()
            && self.keyboard_run == previous.keyboard_run
            && let Some(present) = &self.keyboards
        {
            self.readable_keyboards
                .retain_present_from(&previous.readable_keyboards, present);
        }
        if self.workstation_run.is_some()
            && self.workstation_run == previous.workstation_run
            && self.console.is_none()
        {
            self.console = previous.console;
        }
        if self.materialization_run.is_some()
            && self.materialization_run == previous.materialization_run
            && self.materialization.is_none()
        {
            self.materialization = previous.materialization;
        }
        if self.subscriber_run.is_some()
            && self.subscriber_run == previous.subscriber_run
            && self.subscriber.is_none()
        {
            self.subscriber = previous.subscriber;
        }
        if self.git_run.is_some() && self.git_run == previous.git_run && self.git.is_none() {
            self.git = previous.git.clone();
        }
        if self.git_run.is_some()
            && self.git_run == previous.git_run
            && self.repository_inputs.is_none()
        {
            self.repository_inputs = previous.repository_inputs.clone();
        }
    }

    /// A retryable unfinished vault try is due. A live worker consumes each
    /// deadline once, so a pass that cannot reach Secrets cannot re-arm it.
    pub fn retry_due(&self) -> Option<u64> {
        self.retry_due
    }

    /// A selected, unfinished Mac settings stage has its next automatic try
    /// due. Its token is consumed independently of a Secrets deadline.
    pub fn mac_settings_retry_due(&self) -> Option<u64> {
        self.mac_settings_retry_due
    }

    /// An unfinished stage aterm parked after it stopped there, or after it ran past the hang
    /// limit, has waited out its pause: its next pass begins it again. Its token is consumed
    /// independently of the others.
    pub fn stage_retry_due(&self) -> Option<u64> {
        self.stage_retry_due
    }

    /// Whether the run waits on an admitted, unfinished Accounts or Permissions stage: what moves
    /// it is something a person does in System Settings or macOS, so one of aterm's windows
    /// gaining focus ends its park. No other park listens for focus (System State design §3.5).
    pub fn waits_on_settings(&self) -> bool {
        self.in_settings
    }

    /// The earliest automatic retry this run has ahead, in seconds since the epoch: the park's
    /// one timer. `None`: no retry is ahead, so only a wake or a watched fact ends the park.
    pub fn next_due(&self) -> Option<u64> {
        self.retry_ahead
    }

    /// An absent broker can become ready without any watched file changing.
    #[must_use]
    pub fn waits_on_lan_broker(&self) -> bool {
        self.broker_unready == Some(true)
    }

    /// The facts [`Self::facts_changed_since`] reads that can move while the run is parked: the
    /// secrets folder and network while a pending run selects Secrets or Repositories, and Git
    /// activation while it selects Repositories. The existing GUI input keeper observes a
    /// pending Caps setting's native service set; it needs no additional watch or timer here.
    pub fn watches(&self) -> Watches {
        Watches {
            secrets: self.network_run.is_some(),
            network: self.network_run.is_some(),
            git: self.network_run.is_some() && self.git_run.is_some(),
        }
    }
}

/// What a parked run waits on that moves without the window: this login's vault
/// state and, while an admitted pending run selects Secrets or Repositories, this Mac's network.
/// Selected Secrets also observes subscriber state; only an unfinished join keeps its existing
/// retry deadline and first-use door. A selected, unfinished Mac settings stage wakes for its second
/// automatic try. It also says whether an Accounts or Permissions stage is left,
/// which a person moves in System Settings or macOS. No network request or
/// permission prompt is made here.
pub fn initial_setup_park_mark() -> InitialSetupParkMark {
    let sys = aterm_machines::port::Live::new();
    initial_setup_park_mark_at(
        state_root().ok().as_deref(),
        aterm_secrets::client::Paths::live().ok().as_ref(),
        secrets_retry::Reads::now(&secrets_retry::Live),
        aterm_secrets::network::default_route,
    )
    .with_lan_broker(|| access::lan_unavailable().is_some())
    .with_git(|| Some(RepositoryGit::read(&sys)))
    .with_repository_inputs(|| aterm_machines::init_github::repository_input_mark(&sys).ok())
    .with_console(|| Some(!aterm_machines::console::remote(&sys)))
    .with_keyboard_services(&sys)
}

fn initial_setup_park_mark_at(
    root: Option<&Path>,
    paths: Option<&aterm_secrets::client::Paths>,
    now: u64,
    network: impl FnOnce() -> Option<String>,
) -> InitialSetupParkMark {
    let vault = paths.and_then(|paths| aterm_secrets::client::status(paths).ok());
    initial_setup_park_mark_reading(
        root,
        vault,
        now,
        network,
        || {
            paths.and_then(|paths| {
                aterm_secrets::client::materialization_mark(paths)
                    .ok()
                    .flatten()
            })
        },
        || {
            paths.and_then(|paths| {
                aterm_secrets::service::state(&paths.home, aterm_secrets::service::Job::Subscriber)
                    .ok()
                    .map(secrets_retry::subscriber_running)
            })
        },
    )
}

#[cfg(test)]
fn initial_setup_park_mark_with(
    root: Option<&Path>,
    vault: Option<aterm_secrets::client::Status>,
    now: u64,
    network: impl FnOnce() -> Option<String>,
) -> InitialSetupParkMark {
    initial_setup_park_mark_reading(root, vault, now, network, || None, || None)
}

fn initial_setup_park_mark_reading(
    root: Option<&Path>,
    mut vault: Option<aterm_secrets::client::Status>,
    now: u64,
    network: impl FnOnce() -> Option<String>,
    materialization: impl FnOnce() -> Option<aterm_secrets::client::MaterializationMark>,
    subscriber: impl FnOnce() -> Option<bool>,
) -> InitialSetupParkMark {
    if let Some(aterm_secrets::client::Status {
        membership: aterm_secrets::client::Membership::In { heard, .. },
        ..
    }) = &mut vault
    {
        // A subscriber's heartbeat alone changes no setup answer.
        *heard = 0;
    }
    let mut workstation_run = None;
    let mut keyboard_run = None;
    let pending = root.and_then(|root| {
        let observation = observation_at(root).ok()?;
        let records = &observation.records;
        workstation_run = pending_inputs::revision(records);
        keyboard_run = pending_inputs::caps_revision(root, records);
        let (plan, true) = records.plan.as_ref()? else {
            return None;
        };
        let progress = records.progress?;
        (progress.admitted
            && progress.belongs_to(plan)
            && !progress.completed
            && !progress.stopped_by(records.stop_request)
            && Stage::ALL
                .iter()
                .any(|stage| stage.selected(plan) && progress.done & stage.bit() == 0)
            && matches!(
                records.decision,
                Decision::Pending | Decision::Applied(true)
            ))
        .then_some((
            root,
            progress,
            Stage::Secrets.selected(plan),
            Stage::Repositories.selected(plan),
            (progress.done & Stage::Workstation.bit() == 0
                && Stage::Workstation.settings().iter().any(|setting| {
                    plan.selection.contains(*setting)
                        && progress.only.is_none_or(|only| only.contains(*setting))
                }))
                || (Stage::Accounts.selected(plan) && progress.done & Stage::Accounts.bit() == 0),
            [Stage::Accounts, Stage::Permissions]
                .iter()
                .any(|stage| stage.selected(plan) && progress.done & stage.bit() == 0),
        ))
    });
    let unfinished_secrets = pending.filter(|(_, progress, secrets, _, _, _)| {
        *secrets && progress.done & Stage::Secrets.bit() == 0
    });
    let selected_secrets = pending.filter(|(_, _, secrets, _, _, _)| *secrets);
    let selected_network =
        pending.filter(|(_, _, secrets, repositories, _, _)| *secrets || *repositories);
    let materialization_run = selected_network.map(|(_, progress, _, _, _, _)| progress.revision);
    let stage_dues = pending.map_or_else(Vec::new, |(root, progress, _, _, _, _)| {
        ledger::park_dues(root, progress, now)
    });
    InitialSetupParkMark {
        vault,
        network_run: selected_network.map(|(_, progress, _, _, _, _)| progress.revision),
        materialization_run,
        materialization: materialization_run.and_then(|_| materialization()),
        subscriber_run: selected_secrets.map(|(_, progress, _, _, _, _)| progress.revision),
        subscriber: selected_secrets.and_then(|_| subscriber()),
        join_run: unfinished_secrets.map(|(_, progress, _, _, _, _)| progress.revision),
        network: selected_network.and_then(|_| network()),
        git_run: pending
            .filter(|(_, _, _, repositories, _, _)| *repositories)
            .map(|(_, progress, _, _, _, _)| progress.revision),
        git: None,
        repository_inputs: None,
        workstation_run,
        console: None,
        keyboard_run,
        keyboards: None,
        readable_keyboards: InputServiceSet::default(),
        retry_due: unfinished_secrets.and_then(|(root, progress, _, _, _, _)| {
            secrets_retry::retry_due(root, progress.revision, now)
        }),
        mac_settings_retry_due: pending
            .filter(|(_, _, _, _, mac_settings, _)| *mac_settings)
            .and_then(|(root, progress, _, _, _, _)| {
                super::mac_settings_retry_due(
                    root,
                    progress.revision,
                    &aterm_stamp::build_id(),
                    now,
                )
                .ok()
                .flatten()
            }),
        stage_retry_due: stage_dues.iter().copied().filter(|due| *due <= now).max(),
        retry_ahead: unfinished_secrets
            .and_then(|(root, progress, _, _, _, _)| {
                secrets_retry::retry_ahead(root, progress.revision, now)
            })
            .into_iter()
            .chain(stage_dues.iter().copied().filter(|due| *due > now))
            .chain(
                pending
                    .filter(|(_, _, _, _, mac_settings, _)| *mac_settings)
                    .and_then(|(root, progress, _, _, _, _)| {
                        super::mac_settings_retry_ahead(
                            root,
                            progress.revision,
                            &aterm_stamp::build_id(),
                            now,
                        )
                        .ok()
                        .flatten()
                    }),
            )
            .min(),
        in_settings: pending.is_some_and(|(_, _, _, _, _, in_settings)| in_settings),
        broker_unready: None,
    }
}

#[cfg(test)]
#[path = "initial_parked_secrets_route_tests.rs"]
mod parked_secrets_route_tests;

struct LiveInitial<R> {
    reads: R,
    /// The reason the last Repositories stage's failed part gives
    /// ([`InitialHost::repositories_failure`]).
    repositories_failure: Option<&'static str>,
    /// Whether the last Repositories stage stopped for want of a GitHub token
    /// ([`InitialHost::repositories_want_token`]).
    token_wanted: bool,
    /// What the last Spotlight stage read of its row ([`InitialHost::spotlight_readback`]).
    spotlight_readback: WorkstationReadback,
    /// Actual native empty-origin observations live only through this setup pass's readback.
    repository_readback: Option<std::sync::Arc<aterm_machines::init_github::RepositoryReadback>>,
}

impl LiveInitial<secrets_retry::Live> {
    const fn live() -> Self {
        Self {
            reads: secrets_retry::Live,
            repositories_failure: None,
            token_wanted: false,
            spotlight_readback: WorkstationReadback::EMPTY,
            repository_readback: None,
        }
    }
}

impl<R> LiveInitial<R> {
    fn context(&self) -> Result<aterm_machines::items::Ctx, String> {
        aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new())
    }
}

fn secrets_rows(rows: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    item_rows(rows)
        .into_iter()
        .map(|row| Row {
            name: Stage::Secrets.name().into(),
            ..row
        })
        .collect()
}

/// A setup row's status for one engine result. A row only a person moves (a
/// switch in System Settings, a dialog) is [`WAITING`]: no pass retries it.
pub(super) fn row_status(row: &aterm_machines::model::ItemReport) -> &'static str {
    use aterm_converge::StoodAside;
    use aterm_machines::model::Outcome;
    if matches!(row.outcome, Outcome::Ok | Outcome::Changed)
        && aterm_machines::init_observe::standing(row)
            == aterm_types::macos_setup::Standing::Differs
    {
        return "drift";
    }
    match row.outcome {
        Outcome::Ok | Outcome::NotApplicable => "ok",
        Outcome::Changed => "applied",
        Outcome::Planned => "drift",
        Outcome::Person => WAITING,
        // Waits on a person, or on an admin Mac approving this Mac's join: no pass moves it.
        Outcome::Blocked
            if matches!(
                row.stood_aside,
                Some(StoodAside::Permission(_) | StoodAside::Waiting(_))
            ) =>
        {
            WAITING
        }
        Outcome::Blocked => "blocked",
        // Put off for the console, an idle Mac or the next launch: no pass a minute later moves it.
        Outcome::Deferred => super::DEFERRED,
        Outcome::Unknown => "unverified",
        Outcome::Failed => "error",
    }
}

fn item_rows(rows: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    rows.into_iter()
        .map(|row| Row {
            name: row.title.clone(),
            status: row_status(&row).into(),
            detail: row
                .why
                .or_else(|| match (row.from, row.to) {
                    (Some(from), Some(to)) => Some(format!("{from} → {to}")),
                    (None, Some(to)) => Some(to),
                    (Some(from), None) => Some(from),
                    (None, None) => None,
                })
                .unwrap_or_else(|| "verified".into()),
            elapsed_ms: row.elapsed_ms,
        })
        .collect()
}

impl<R: secrets_retry::Reads> InitialHost for LiveInitial<R> {
    fn swarm_enabled(&mut self) -> Result<bool, String> {
        aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string())
    }
    fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
        apply_on_first_run_at(root)
    }

    fn workstation(
        &mut self,
        root: &Path,
        only: Option<Selection>,
    ) -> Result<Option<Report>, String> {
        apply_preferences_at(root, only)
    }

    fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String> {
        apply_accounts_at(root)
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        // Dated from before the run reads anything (design §3.5).
        let at = aterm_machines::port::System::now(&sys);
        let rows = aterm_machines::init::run_initial_spotlight(
            &sys,
            &self.context()?,
            aterm_machines::model::Mode::Apply,
        )?;
        let tagged: Vec<_> = rows.iter().map(|row| (Setting::Spotlight, row)).collect();
        let mut spotlight = Selection::none();
        spotlight.set(Setting::Spotlight, true);
        self.spotlight_readback = WorkstationReadback(read_back(&tagged, spotlight, at));
        Ok(item_rows(rows))
    }

    fn spotlight_readback(&mut self) -> WorkstationReadback {
        self.spotlight_readback.take()
    }

    fn permissions(&mut self, _plan: &Plan, revision: u64) -> Result<Vec<Row>, String> {
        Ok(access::stage(revision))
    }

    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        run_live_secrets(plan, replaces, password)
    }

    fn secrets_lane(&mut self) -> Option<Box<dyn SecretsLane>> {
        self.reads
            .boxed_clone()
            .map(|reads| Box::new(LiveSecretsLane { reads }) as Box<dyn SecretsLane>)
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        Some(secrets_retry::pace(&self.reads))
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        live_configured_vault()
    }

    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        check::current_vault_rows(plan)
    }

    fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        self.repositories_failure = None;
        self.token_wanted = false;
        self.repository_readback = None;
        let start = initial_setup_holds();
        let ran = repositories::run_reading_guarded(
            &sys,
            &self.context()?,
            vault,
            aterm_machines::model::Mode::Apply,
            move || {
                let now = initial_setup_holds();
                if now.later(&start, Setting::Repositories) {
                    Err(now.why(Setting::Repositories))
                } else {
                    Ok(true)
                }
            },
        )?;
        self.repositories_failure = ran.failure;
        self.token_wanted = ran.token;
        self.repository_readback = ran.readback;
        Ok(ran.rows)
    }

    fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        repositories::run(
            &sys,
            &self.context()?,
            vault,
            aterm_machines::model::Mode::Plan,
            self.repository_readback.clone(),
        )
    }

    fn repositories_want_token(&mut self) -> bool {
        self.token_wanted
    }

    fn repositories_failure(&mut self) -> Option<&'static str> {
        self.repositories_failure
    }

    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
        super::follow_up_locked_at(root)
    }

    fn withheld_owed(&mut self, root: &Path) -> Vec<Setting> {
        super::owed_with_full_disk_access(root, super::full_disk_access)
    }
}

struct LiveSecretsLane {
    reads: Box<dyn secrets_retry::Reads + Send>,
}

impl SecretsLane for LiveSecretsLane {
    fn swarm_enabled(&mut self) -> Result<bool, String> {
        aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string())
    }
    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        run_live_secrets(plan, replaces, password)
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        Some(secrets_retry::pace(self.reads.as_ref()))
    }

    fn lan_unavailable(&mut self) -> Option<String> {
        access::lan_unavailable()
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        live_configured_vault()
    }

    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        check::current_vault_rows(plan)
    }
}

/// The Secrets stage's join, live. It runs once the lane found the root LAN broker answering
/// ([`access::wait_for_lan_broker`]); its LAN uses are `aterm_secrets`' and `aterm_machines`',
/// and it holds no macOS dialog slot for them.
fn run_live_secrets(
    plan: &Plan,
    replaces: Replaces,
    password: &mut Option<SecretBytes>,
) -> Result<Vec<Row>, String> {
    if let Some(rows) =
        automatic_secrets_rows(aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string()))
    {
        return Ok(rows);
    }
    let sys = aterm_machines::port::Live::new();
    let ctx = aterm_machines::init::initial_setup_context(&sys)?;
    if let Some(rows) =
        automatic_secrets_rows(aterm_secrets::swarm::enabled_live().map_err(|why| why.to_string()))
    {
        return Ok(rows);
    }
    let ops = aterm_machines::init_secrets::unattended(&ctx)?;
    let root = state_root()?;
    let expected_revision = revision_at(&root)?;
    let start = initial_setup_holds();
    let current = || {
        let now = initial_setup_holds();
        if now.later(&start, Setting::Secrets) {
            return Err(now.why(Setting::Secrets));
        }
        if revision_at(&root)? != expected_revision
            || saved_plan(&root)?.as_ref().is_some_and(|now| now != plan)
        {
            return Err("the selected setup plan changed while vault bootstrap ran".into());
        }
        Ok(())
    };
    aterm_machines::init::run_initial_secrets_guarded(
        &sys,
        &ctx,
        ops,
        &plan.vault_url,
        replaces,
        password,
        APPROVAL_WAIT,
        &current,
    )
    .map(unanswered_join_rows)
}

const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

/// Automatic setup leaves explicit manual joins alone. A disabled or unreadable master
/// switch reaches neither vault discovery nor the dialog/join path.
fn automatic_secrets_rows(enabled: Result<bool, String>) -> Option<Vec<Row>> {
    match enabled {
        Ok(true) => None,
        Ok(false) => Some(vec![readiness_row(
            Stage::Secrets.name(),
            true,
            aterm_secrets::swarm::SWARM_OFF.into(),
        )]),
        Err(why) => Some(vec![Row {
            name: Stage::Secrets.name().into(),
            status: "error".into(),
            detail: aterm_secrets::swarm::unreadable(&why),
            elapsed_ms: None,
        }]),
    }
}

fn readiness_row(name: &str, ready: bool, detail: String) -> Row {
    Row {
        name: name.into(),
        status: if ready { "ok" } else { "blocked" }.into(),
        detail,
        elapsed_ms: None,
    }
}

fn rows_ready(rows: &[Row]) -> bool {
    !rows.is_empty() && rows.iter().all(row_ready)
}

/// A row with nothing left to do: in place, set, back as saved and loading at the next login
/// ([`super::LOGIN`]), which no pass can hurry, a grant left to someone else or unreadable
/// here ([`super::OFFERED`], [`super::UNREAD`]), which no pass can take a step on, or a setting
/// changed since aterm set it ([`super::CHANGED`]), which no pass of setup's sets again.
fn row_ready(row: &Row) -> bool {
    matches!(
        row.status.as_str(),
        "ok" | "applied" | super::LOGIN | super::OFFERED | super::UNREAD | super::CHANGED
    )
}

enum SecretsTry {
    LeaveInPlace(ConfiguredVault),
    RunItem(Replaces),
}

/// What opens a Secrets stage row whose vault configuration aterm could not read
/// ([`secrets_try`]), and a Join refused for it ([`InitialSetupRestart::persist_observed`]):
/// the log and `aterm doctor` keep it; the page says it once, under the Secrets vault row,
/// whose reading reads the same file ([`observe::VAULT_UNREAD`]), and for the refused Join in
/// one calm line ([`vault_config_unread`]).
const VAULT_CONFIG_UNREAD: &str = "cannot read which vault this Mac is in";

/// Whether `error`, a person's press that was refused, is a Join refused because aterm cannot
/// read which vault this Mac is in: the vault a Join replaces is the one it cannot read, so it
/// changed nothing, and pressing it again meets the same file until `aterm secrets status`'s
/// reason is fixed. System State says so in one calm line, never a failure, and keeps the typed
/// vault for a Join once the settings read.
#[must_use]
pub fn vault_config_unread(error: &str) -> bool {
    error.starts_with(VAULT_CONFIG_UNREAD)
}

/// The Secrets vault row's calm note while aterm cannot read this Mac's vault settings
/// ([`observe::VAULT_UNREAD`]): System State reads it to say, while it stands, that the launch's
/// Join went unjoined ([`InitialSetupAuthorization::join_unread`]).
pub const VAULT_SETTINGS_UNREAD: &str = observe::VAULT_UNREAD;

/// The page's words for the Secrets stage's `rows` ([`secrets_retry::page`]), or none where
/// aterm could not read which vault this Mac is in: the Secrets vault row's own calm note says
/// that, and the stage tries again on its pause, so a second line would only repeat it.
fn secrets_page(root: &Path, revision: u64, rows: &[Row]) -> PassPage {
    if rows
        .iter()
        .any(|row| row.detail.starts_with(VAULT_CONFIG_UNREAD))
    {
        return PassPage::default();
    }
    secrets_retry::page(root, revision, rows)
}

fn secrets_try(
    host: &mut dyn InitialHost,
    plan: &Plan,
    admitted: AdmittedVault,
) -> Result<SecretsTry, String> {
    secrets_try_with(|| host.configured_vault(), plan, admitted)
}

fn secrets_try_with(
    configured_vault: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    plan: &Plan,
    admitted: AdmittedVault,
) -> Result<SecretsTry, String> {
    if admitted == AdmittedVault::Unknown {
        return Ok(SecretsTry::RunItem(Replaces::Any));
    }
    let configured =
        configured_vault().map_err(|error| format!("{VAULT_CONFIG_UNREAD}: {error}"))?;
    Ok(match configured {
        Some(vault) if admitted.stands_in(plan, &vault) => SecretsTry::LeaveInPlace(vault),
        configured => SecretsTry::RunItem(Replaces::Read(configured.map(|vault| vault.config))),
    })
}

enum SecretsLaneFinish {
    NotRun,
    Ran {
        ready: bool,
        rows: Vec<Row>,
        page: Option<PassPage>,
    },
}

/// A stage's running fence ([`SetupHooks::stage`]): open from [`Self::open`] until it drops, so
/// a stage that ends early with an error is read as ended too, as one that finishes is. While its
/// thread unwinds from a panic it calls no hook (as [`ledger::Begun`] records no end): the pass's
/// caller retires what a panic cut off.
struct StageFence<'a> {
    stage: for<'b> fn(StageEvent<'b>),
    stop: fn(u64) -> bool,
    revision: u64,
    settings: &'a [Setting],
    subjects: &'a [Subject],
    /// What the stage read of its rows, which its end carries ([`StageEvent::readback`]).
    readback: WorkstationReadback,
}

impl<'a> StageFence<'a> {
    fn open(
        hooks: SetupHooks,
        revision: u64,
        settings: &'a [Setting],
        subjects: &'a [Subject],
    ) -> Self {
        (hooks.stage)(StageEvent {
            revision,
            settings,
            subjects,
            running: true,
            readback: &WorkstationReadback::EMPTY,
        });
        Self {
            stage: hooks.stage,
            stop: hooks.stop,
            revision,
            settings,
            subjects,
            readback: WorkstationReadback::EMPTY,
        }
    }
}

impl Drop for StageFence<'_> {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            if (self.stop)(self.revision) {
                self.readback = WorkstationReadback::EMPTY;
            }
            let mut selected = Selection::none();
            for setting in self.settings {
                selected.set(*setting, true);
            }
            self.readback.retain(selected);
            (self.stage)(StageEvent {
                revision: self.revision,
                settings: self.settings,
                subjects: self.subjects,
                running: false,
                readback: &self.readback,
            });
        }
    }
}

struct WakeLaneOnDrop<'a>(&'a std::sync::atomic::AtomicBool);

impl Drop for WakeLaneOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
        access::wake_lane_waiters();
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "lane runner binds the scoped pass revision, ledger context, and hooks explicitly"
)]
fn run_secrets_lane(
    lane: &mut dyn SecretsLane,
    root: &Path,
    plan: &Plan,
    revision: u64,
    admitted_vault: AdmittedVault,
    initial_done: u16,
    boot: Option<u128>,
    software: Option<ledger::SoftwareEpoch>,
    password: &std::sync::Mutex<Option<SecretBytes>>,
    hooks: SetupHooks,
    reached_secrets: &std::sync::atomic::AtomicBool,
    stop_lane: &std::sync::atomic::AtomicBool,
) -> Result<SecretsLaneFinish, String> {
    use std::sync::atomic::Ordering;
    if let Some(rows) = automatic_secrets_rows(lane.swarm_enabled()) {
        return Ok(SecretsLaneFinish::Ran {
            ready: rows_ready(&rows),
            rows,
            page: None,
        });
    }
    if plan.vault_url != aterm_types::macos_setup::DEFAULT_VAULT_URL
        || in_plan_vault(plan, lane.configured_vault())
    {
        return Ok(SecretsLaneFinish::NotRun);
    }
    let broker = access::wait_for_lan_broker(
        || lane.lan_unavailable(),
        || {
            stop_lane.load(Ordering::Acquire)
                || reached_secrets.load(Ordering::Acquire)
                || (hooks.stop)(revision)
                || stopped_at(root).ok().flatten().is_some()
        },
    );
    if !broker
        || stop_lane.load(Ordering::Acquire)
        || (hooks.stop)(revision)
        || (hooks.gives_way)()
        || stopped_at(root)?.is_some()
    {
        return Ok(SecretsLaneFinish::NotRun);
    }
    if let Some(rows) = automatic_secrets_rows(lane.swarm_enabled()) {
        return Ok(SecretsLaneFinish::Ran {
            ready: rows_ready(&rows),
            rows,
            page: None,
        });
    }
    let paced = lane.secrets_pace();
    if paced
        .as_ref()
        .and_then(|pace| secrets_retry::paused(root, revision, pace))
        .is_some()
    {
        return Ok(SecretsLaneFinish::NotRun);
    }
    let stamp = ledger::Stamp {
        boot,
        at: lane.now_secs(),
        awake: ledger::awake_secs(),
        run: HeldLock::profile_holder(root),
        software,
    };
    // Not while aterm quits ([`ledger::quitting`]): the pass's own begin ends the pass then.
    let admitted = ledger::update(root, revision, |ledger| {
        ledger.begin(Stage::Secrets, initial_done, stamp, root)
    })?;
    if admitted != ledger::Admit::Run {
        return Ok(SecretsLaneFinish::NotRun);
    }
    let begun = ledger::Begun::new(root, revision, Stage::Secrets);
    let settings = [Setting::Secrets];
    let subjects = [Subject::Setting(Setting::Secrets)];
    let fence = StageFence::open(hooks, revision, &settings, &subjects);
    let started = std::time::Instant::now();
    let result = match secrets_try_with(|| lane.configured_vault(), plan, admitted_vault) {
        Ok(SecretsTry::LeaveInPlace(vault)) => {
            let mut rows = vec![readiness_row(
                Stage::Secrets.name(),
                true,
                vault.left_in_place(plan),
            )];
            match lane.current_vault(&vault.present(plan.clone())) {
                Ok(read) => rows.extend(read),
                Err(detail) => rows.push(Row {
                    name: Stage::Secrets.name().into(),
                    status: "unverified".into(),
                    detail,
                    elapsed_ms: None,
                }),
            }
            Ok(rows)
        }
        Ok(SecretsTry::RunItem(replaces)) => {
            let mut held = password
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lane.secrets(plan, replaces, &mut held)
        }
        Err(detail) => Ok(vec![Row {
            name: Stage::Secrets.name().into(),
            status: "unverified".into(),
            detail,
            elapsed_ms: None,
        }]),
    };
    let mut page;
    let (ready, mut rows) = match result {
        Ok(rows) => {
            page = items_page(Stage::Secrets, &rows);
            (rows_ready(&rows), rows)
        }
        Err(detail) => {
            let row = Row {
                name: Stage::Secrets.name().into(),
                status: "error".into(),
                detail,
                elapsed_ms: None,
            };
            page = Some(step_page(&row, Stage::Secrets.page_rows(&subjects)));
            (false, vec![row])
        }
    };
    let elapsed = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    for row in &mut rows {
        row.elapsed_ms.get_or_insert(elapsed);
    }
    begun.ended()?;
    if let Some(before) = paced.as_ref()
        && let Some(mut after) = lane.secrets_pace()
    {
        after.retain_known_from(before);
        secrets_retry::record(root, revision, &after, ready, &mut rows)?;
    }
    page = page.or_else(|| Some(secrets_page(root, revision, &rows)));
    if ready || rows.iter().all(|row| row.status != "error") {
        page = Some(secrets_page(root, revision, &rows));
    }
    drop(fence);
    Ok(SecretsLaneFinish::Ran { ready, rows, page })
}

/// Stop the pass's Secrets lane, if it runs (it stops at its next boundary), and wait for it to
/// end: the password it did not spend stays in the pass's hold for it
/// ([`execute_pass_target`]).
fn stop_secrets_lane(
    stop_lane: &std::sync::atomic::AtomicBool,
    lane: Option<
        std::io::Result<std::thread::ScopedJoinHandle<'_, Result<SecretsLaneFinish, String>>>,
    >,
) {
    stop_lane.store(true, std::sync::atomic::Ordering::Release);
    access::wake_lane_waiters();
    if let Some(Ok(handle)) = lane {
        let _ = handle.join();
    }
}

#[cfg(test)]
fn execute_initial(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    Ok(execute_pass(host, root, password, SetupHooks::NONE)?.map(|(report, _)| report))
}

fn current_setup_vault_plan(
    host: &mut dyn InitialHost,
    plan: &Plan,
    admitted_vault: AdmittedVault,
) -> Result<Plan, String> {
    secrets_try(host, plan, admitted_vault).map(|read| match read {
        SecretsTry::LeaveInPlace(vault) => vault.present(plan.clone()),
        SecretsTry::RunItem(_) => plan.clone(),
    })
}

/// Network stages can finish while another selected stage stays pending across launches.
/// Their old done bits cannot complete that later pass after local vault or checkout state
/// changes. Read them without effects; an unfinished reading reopens only its ordinary stage
/// and is returned for a pass that is about to publish completion.
fn revalidate_completed_network_stages(
    host: &mut dyn InitialHost,
    root: &Path,
    plan: &Plan,
    progress: &mut Progress,
) -> Result<Vec<(Stage, Vec<Row>)>, String> {
    let before = progress.done;
    let completed = [Stage::Secrets, Stage::Repositories];
    if !completed
        .iter()
        .any(|stage| stage.selected(plan) && before & stage.bit() != 0)
    {
        return Ok(Vec::new());
    }
    let sharing = host.swarm_enabled();
    let uses_vault = Stage::Secrets.selected(plan) && sharing == Ok(true);
    let mut current_vault = None;
    let mut reopened = Vec::new();
    for stage in completed {
        if !stage.selected(plan) || progress.done & stage.bit() == 0 {
            continue;
        }
        // A pending Repositories stage already reads Secrets immediately before its own
        // effects. Keep that boundary's observation rather than repeat it ahead of the stage.
        if stage == Stage::Secrets
            && uses_vault
            && Stage::Repositories.selected(plan)
            && before & Stage::Repositories.bit() == 0
        {
            continue;
        }
        let reading = if stage == Stage::Secrets {
            match automatic_secrets_rows(sharing.clone()) {
                Some(rows) => Ok(rows),
                None => {
                    let selected = current_vault.get_or_insert_with(|| {
                        current_setup_vault_plan(host, plan, progress.admitted_vault)
                    });
                    match selected {
                        Ok(selected) => host.current_vault(selected),
                        Err(why) => Err(why.clone()),
                    }
                }
            }
        } else {
            let selected = if uses_vault {
                Some(current_vault.get_or_insert_with(|| {
                    current_setup_vault_plan(host, plan, progress.admitted_vault)
                }))
            } else {
                None
            };
            match selected {
                Some(Err(why)) => Err(why.clone()),
                selected => host.current_repositories(selected.and_then(|plan| plan.as_ref().ok())),
            }
        };
        if !reading.as_ref().is_ok_and(|rows| rows_ready(rows)) {
            progress.done &= !stage.bit();
            let mut rows = reading.unwrap_or_else(|detail| {
                vec![Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail,
                    elapsed_ms: None,
                }]
            });
            if rows.is_empty() {
                rows.push(Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail: "current readiness was not returned".into(),
                    elapsed_ms: None,
                });
            }
            reopened.push((stage, rows));
        }
    }
    if progress.done != before {
        save_progress(root, *progress)?;
    }
    Ok(reopened)
}

/// [`execute_pass_target`] over every stage the plan selects, as a window's pass.
#[cfg(test)]
fn execute_pass(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
) -> Result<Option<(Report, SetupNext)>, String> {
    execute_pass_target(host, root, password, hooks, PassTarget::All)
}

/// Which stages a pass may run.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PassTarget {
    /// Every stage the plan selects: a window's pass, or `aterm init`'s.
    All,
    /// The login job's pass, with no window and no person ([`run_background_setup_pass`]): every
    /// stage that runs unattended (design §6.4), never a person stage, which waits for a window
    /// at this Mac.
    Background,
}

impl PassTarget {
    fn includes(self, stage: Stage) -> bool {
        self == Self::All || !matches!(stage, Stage::Permissions | Stage::Accounts)
    }
}

/// One pass over the stages `target` includes that the plan selects and are not done, in order,
/// asking `hooks` and the durable stop request before each and at the end whether a person
/// stopped this revision ([`stop_at_boundary`]). A Stop ends the revision without cancelling its
/// plan.
fn execute_pass_target(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
    target: PassTarget,
) -> Result<Option<(Report, SetupNext)>, String> {
    if target != PassTarget::All && progress(root)?.is_none() {
        // An older aterm's run with no progress record is a window's to carry on.
        return Ok(None);
    }
    fold_stop_request(root)?;
    if finished_at(root)? {
        return Ok(None);
    }
    let Some(plan) = saved_plan(root)? else {
        return Ok(None);
    };
    let Some(mut progress) = progress(root)? else {
        if target != PassTarget::All {
            return Ok(None);
        }
        return Ok(host.legacy(root)?.map(|report| {
            let next = if !report.complete() {
                SetupNext::of(&report.rows)
            } else {
                SetupNext::Done
            };
            (report, next)
        }));
    };
    if !progress.belongs_to(&plan) || !progress.admitted {
        return Ok(None);
    }
    // Holding profile.lock, so whatever is recorded running was cut off when aterm stopped: the
    // ledger resumes it once or parks it (design §6.5), and nothing runs until a stage begins.
    // A person's Check again since the last pass starts every automatic try of the run afresh:
    // the flag is taken only once those tries are cleared, so an error or an end between the
    // two leaves it for the next pass to clear them again, never a press lost.
    let now = ledger::now_secs();
    let boot = host.boot_session();
    let afresh = ledger::update(root, progress.revision, |ledger| {
        ledger.recover(progress.done, progress.running, boot, now);
        ledger.afresh
    })?;
    if afresh {
        tries_afresh(root, progress.revision)?;
        ledger::update(root, progress.revision, ledger::Ledger::take_afresh)?;
    }
    if progress.running.take().is_some() {
        save_progress(root, progress)?;
    }
    revalidate_completed_network_stages(host, root, &plan, &mut progress)?;
    let software = host.software_epoch();
    let mut report = Report {
        action: Action::Apply,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
        restore: None,
        page: None,
    };
    let mut next = SetupNext::Done;
    // What the vault stage left in this pass, for the repositories waiting on it.
    let mut vault = SetupNext::Retry;
    // The vault stage left the ALab vault absent from this network or unanswered on it
    // ([`vault_not_here`]): the repositories then run on this Mac's own credentials.
    let mut not_here = false;
    // The rows of `report` a stage's own page stands for (`Report::page`).
    let mut pages: Vec<(std::ops::Range<usize>, PassPage)> = Vec::new();
    // Each stage's rows its producer did not time carry the stage's time.
    let mut clock = RowClock::start();
    let secrets_lane = (Stage::Secrets.selected(&plan)
        && progress.done & Stage::Secrets.bit() == 0
        && Stage::Workstation.selected(&plan)
        && progress.done & Stage::Workstation.bit() == 0)
        .then(|| host.secrets_lane())
        .flatten();
    let reached_secrets = std::sync::atomic::AtomicBool::new(false);
    let stop_lane = std::sync::atomic::AtomicBool::new(false);
    let initial_done = progress.done;
    let pass_revision = progress.revision;
    let pass_admitted_vault = progress.admitted_vault;
    // The typed join password, held for the Secrets lane while it runs beside the stages: what
    // the lane leaves unspent goes back to `password` however the scope ends (its Secrets stage,
    // a Stop, aterm quitting, an error, the lane failing to start or panicking).
    let lane_password = std::sync::Mutex::new(if secrets_lane.is_some() {
        password.take()
    } else {
        None
    });
    // The scope ends the pass early with what it leaves: a Stop, or aterm quitting.
    let cut = std::thread::scope(|scope| -> Result<Option<SetupNext>, String> {
        let _wake_on_drop = WakeLaneOnDrop(&stop_lane);
        let mut lane_handle = secrets_lane.map(|mut lane| {
            let lane_password = &lane_password;
            let lane_plan = &plan;
            let reached_ref = &reached_secrets;
            let stop_ref = &stop_lane;
            std::thread::Builder::new()
                .name("aterm-setup-secrets-lane".into())
                .spawn_scoped(scope, move || {
                    run_secrets_lane(
                        lane.as_mut(),
                        root,
                        lane_plan,
                        pass_revision,
                        pass_admitted_vault,
                        initial_done,
                        boot,
                        software,
                        lane_password,
                        hooks,
                        reached_ref,
                        stop_ref,
                    )
                })
        });
        for stage in Stage::ALL {
            clock.lap(&mut report.rows);
            if !stage.selected(&plan) || progress.done & stage.bit() != 0 {
                continue;
            }
            if !target.includes(stage) {
                // A person stage the login job leaves: it waits for a window at this Mac.
                next = next.and(SetupNext::Park);
                continue;
            }
            if stage == Stage::Secrets {
                reached_secrets.store(true, std::sync::atomic::Ordering::Release);
                access::wake_lane_waiters();
                if let Some(spawned) = lane_handle.take() {
                    let finished = spawned
                        .map_err(|why| format!("secrets setup lane did not start: {why}"))?
                        .join()
                        .map_err(|_| "secrets setup lane panicked".to_owned())??;
                    *password = lane_password
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take();
                    match finished {
                        SecretsLaneFinish::NotRun => {}
                        SecretsLaneFinish::Ran { ready, rows, page } => {
                            let left = SetupNext::of(&rows);
                            if ready {
                                progress.done |= Stage::Secrets.bit();
                                save_progress(root, progress)?;
                            }
                            vault = left;
                            not_here = vault_not_here(&rows);
                            next = next.and(left);
                            let at = report.rows.len();
                            report.rows.extend(rows);
                            if let Some(page) = page {
                                pages.push((at..report.rows.len(), page));
                            }
                            clock.lap(&mut report.rows);
                            continue;
                        }
                    }
                }
            }
            if stop_at_boundary(root, &mut progress, hooks)? {
                stop_secrets_lane(&stop_lane, lane_handle.take());
                report.rows.push(Row {
                    name: "System State".into(),
                    status: "stopped".into(),
                    detail: "stopped before this step; nothing more runs until the plan changes"
                        .into(),
                    elapsed_ms: None,
                });
                clock.lap(&mut report.rows);
                return Ok(Some(SetupNext::Done));
            }
            if (hooks.gives_way)() {
                stop_secrets_lane(&stop_lane, lane_handle.take());
                clock.lap(&mut report.rows);
                return Ok(Some(SetupNext::Retry));
            }
            let mut repository_vault = None;
            if stage == Stage::Repositories
                && Stage::Secrets.selected(&plan)
                && host.swarm_enabled().unwrap_or(false)
            {
                if progress.done & Stage::Secrets.bit() != 0 {
                    let current = current_setup_vault_plan(host, &plan, progress.admitted_vault)
                        .and_then(|selected| {
                            let current = host.current_vault(&selected);
                            repository_vault = Some(selected);
                            current
                        });
                    if !current.as_ref().is_ok_and(|rows| rows_ready(rows)) {
                        progress.done &= !Stage::Secrets.bit();
                        progress.running = None;
                        save_progress(root, progress)?;
                        let rows = current.unwrap_or_else(|detail| {
                            vec![Row {
                                name: Stage::Secrets.name().into(),
                                status: "unverified".into(),
                                detail,
                                elapsed_ms: None,
                            }]
                        });
                        let at = report.rows.len();
                        pages.push((
                            at..at + rows.len(),
                            secrets_page(root, progress.revision, &rows),
                        ));
                        report.rows.extend(rows);
                    }
                }
                if progress.done & Stage::Secrets.bit() == 0 && !not_here {
                    let rows = vec![vault_token_wait(vault)];
                    next = next.and(SetupNext::of(&rows));
                    report.rows.extend(rows);
                    continue;
                }
            }
            let suppressed_secrets = (stage == Stage::Secrets)
                .then(|| automatic_secrets_rows(host.swarm_enabled()))
                .flatten();
            let paced = if stage == Stage::Secrets && suppressed_secrets.is_none() {
                host.secrets_pace()
            } else {
                None
            };
            if let Some(rows) = paced
                .as_ref()
                .and_then(|pace| secrets_retry::paused(root, progress.revision, pace))
            {
                vault = SetupNext::of(&rows);
                not_here = vault_not_here(&rows);
                next = next.and(vault);
                let at = report.rows.len();
                pages.push((
                    at..at + rows.len(),
                    secrets_page(root, progress.revision, &rows),
                ));
                report.rows.extend(rows);
                continue;
            }
            let settings: Vec<Setting> = stage
                .settings()
                .iter()
                .copied()
                .filter(|setting| {
                    plan.selection.contains(*setting)
                        && (stage != Stage::Workstation
                            || progress.only.is_none_or(|only| only.contains(*setting)))
                })
                .collect();
            let subjects: Vec<Subject> = stage
                .subjects()
                .iter()
                .copied()
                .filter(|subject| match subject {
                    Subject::Setting(setting) => settings.contains(setting),
                    Subject::Access(_) => true,
                    // A Sync row is no stage's subject; it rides the Secrets row only.
                    Subject::Sync(_) => false,
                })
                .collect();
            // A stage aterm stopped in twice, or one that ran past the hang limit, waits for Check
            // again while the others go on.
            let stamp = ledger::Stamp {
                boot,
                at: host.now_secs(),
                awake: ledger::awake_secs(),
                run: HeldLock::profile_holder(root),
                software,
            };
            let admitted = ledger::update(root, progress.revision, |ledger| {
                ledger.begin(stage, progress.done, stamp, root)
            })?;
            match admitted {
                ledger::Admit::Run => {}
                ledger::Admit::Done => continue,
                // aterm is quitting, or an update's Commit is about to be written: the pass ends
                // here, so the quit cuts off only stages its note names, and goes on at the next
                // launch, or at this one's next pass should the handoff be refused.
                ledger::Admit::Quitting => {
                    stop_secrets_lane(&stop_lane, lane_handle.take());
                    return Ok(Some(SetupNext::Retry));
                }
                ledger::Admit::Parked(park) => {
                    let rows = vec![Row {
                        name: stage.name().into(),
                        status: WAITING.into(),
                        detail: ledger::parked_words(stage, park),
                        elapsed_ms: None,
                    }];
                    next = next.and(SetupNext::of(&rows));
                    if stage == Stage::Secrets {
                        vault = SetupNext::Park;
                    }
                    report.rows.extend(rows);
                    continue;
                }
            }
            let begun = ledger::Begun::new(root, progress.revision, stage);
            progress.running = Some(stage);
            save_progress(root, progress)?;
            let mut fence = StageFence::open(hooks, progress.revision, &settings, &subjects);
            // The page's words for the stage's rows, where its report has them.
            let mut page = None;
            let result = match stage {
                Stage::Workstation => match host.workstation(root, progress.only) {
                    Ok(Some(result)) => {
                        if let Some(page) = result.page.as_ref() {
                            fence.readback = page.readback.clone();
                        }
                        page = result.page;
                        Ok(result.rows)
                    }
                    Ok(None) if first_run_complete(root)? => Ok(vec![readiness_row(
                        stage.name(),
                        true,
                        "selected Mac settings already verified".into(),
                    )]),
                    Ok(None) => Ok(vec![readiness_row(
                        stage.name(),
                        false,
                        "waiting for the next automatic Mac settings retry".into(),
                    )]),
                    Err(error) => Err(error),
                },
                Stage::Spotlight => {
                    let rows = host.spotlight();
                    fence.readback = host.spotlight_readback();
                    rows
                }
                Stage::Permissions => host.permissions(&plan, progress.access_revision()),
                Stage::Secrets => match suppressed_secrets {
                    Some(rows) => Ok(rows),
                    None => match secrets_try(host, &plan, progress.admitted_vault) {
                        Ok(SecretsTry::LeaveInPlace(vault)) => {
                            let mut rows = vec![readiness_row(
                                stage.name(),
                                true,
                                vault.left_in_place(&plan),
                            )];
                            match host.current_vault(&vault.present(plan.clone())) {
                                Ok(read) => rows.extend(read),
                                Err(detail) => rows.push(Row {
                                    name: stage.name().into(),
                                    status: "unverified".into(),
                                    detail,
                                    elapsed_ms: None,
                                }),
                            }
                            Ok(rows)
                        }
                        Ok(SecretsTry::RunItem(replaces)) => {
                            host.secrets(&plan, replaces, password)
                        }
                        Err(detail) => Ok(vec![Row {
                            name: stage.name().into(),
                            status: "unverified".into(),
                            detail,
                            elapsed_ms: None,
                        }]),
                    },
                },
                Stage::Repositories => host.repositories(repository_vault.as_ref()),
                Stage::Accounts => match host.accounts(root) {
                    Ok(Some(result)) => {
                        if let Some(page) = result.page.as_ref() {
                            fence.readback = page.readback.clone();
                        }
                        page = result.page;
                        Ok(result.rows)
                    }
                    Ok(None) => Ok(vec![readiness_row(
                        stage.name(),
                        false,
                        "waiting for the next automatic sign-out retry".into(),
                    )]),
                    Err(error) => Err(error),
                },
            };
            let (ready, mut rows) = match result {
                Ok(rows) => {
                    page = page.or_else(|| items_page(stage, &rows));
                    (rows_ready(&rows), rows)
                }
                Err(detail) => {
                    // `aterm doctor` names the stage; the page names the rows it covers.
                    let row = Row {
                        name: stage.name().into(),
                        status: "error".into(),
                        detail,
                        elapsed_ms: None,
                    };
                    page = Some(step_page(&row, stage.page_rows(&subjects)));
                    (false, vec![row])
                }
            };
            if stage == Stage::Repositories {
                // Run on this Mac's own credentials while the vault is not on this network
                // ([`vault_not_here`]): with no token of its own to sign in with, they wait for the
                // vault's as they would have, rather than ask the person for one the vault keeps.
                let waits_for_vault = not_here && host.repositories_want_token();
                if waits_for_vault {
                    rows = vec![vault_token_wait(vault)];
                    page = items_page(stage, &rows);
                }
                // A failed part's reason, where its step decided one, is the row's words on the page.
                if !waits_for_vault
                    && let Some(reason) = host.repositories_failure()
                    && let Some(page) = page.as_mut()
                {
                    page.failed_because(Setting::Repositories, reason);
                }
            }
            let left = SetupNext::of(&rows);
            if ready {
                progress.done |= stage.bit();
            }
            if matches!(stage, Stage::Workstation | Stage::Accounts) {
                // The inner pass can checkpoint verified effects before a later
                // summary or held-item write fails. Keep that exact run's evidence
                // when publishing this stage's result, including its error result.
                let recorded = self::progress(root)?
                    .filter(|recorded| {
                        recorded.revision == progress.revision && recorded.belongs_to(&plan)
                    })
                    .ok_or("setup progress changed while the Mac settings step ran")?;
                progress.done |= recorded.done & (Stage::Workstation.bit() | Stage::Accounts.bit());
                progress.only = recorded.only;
            }
            if stage == Stage::Secrets {
                vault = left;
                not_here = vault_not_here(&rows);
            }
            next = next.and(left);
            progress.running = None;
            save_progress(root, progress)?;
            begun.ended()?;
            if let Some(before) = paced.as_ref()
                && let Some(mut after) = host.secrets_pace()
            {
                after.retain_known_from(before);
                secrets_retry::record(root, progress.revision, &after, ready, &mut rows)?;
            }
            if stage == Stage::Secrets {
                // The vault's rows carry the retry pause's words, which the page says its own way.
                page = Some(secrets_page(root, progress.revision, &rows));
            }
            let at = report.rows.len();
            report.rows.extend(rows);
            if let Some(page) = page {
                pages.push((at..report.rows.len(), page));
            }
            drop(fence);
        }
        Ok(None)
    });
    if let Some(unspent) = lane_password
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
    {
        *password = Some(unspent);
    }
    if let Some(cut) = cut? {
        return Ok(Some((paged(report, pages), cut)));
    }
    clock.lap(&mut report.rows);
    let stages_finished = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0);
    // The follow-up can change preferences too, so a Stop holds before it, even one whose
    // durable save has not run yet; a run whose selected stages finished is completed instead.
    if !stages_finished && stop_at_boundary(root, &mut progress, hooks)? {
        report.rows.push(Row {
            name: "System State".into(),
            status: "stopped".into(),
            detail: "stopped before deferred settings; nothing more runs until the plan changes"
                .into(),
            elapsed_ms: None,
        });
        clock.lap(&mut report.rows);
        return Ok(Some((paged(report, pages), SetupNext::Done)));
    }
    // The follow-up changes its rows as a stage does: fenced from readings the same way.
    let owed = host.withheld_owed(root);
    let owed_subjects: Vec<Subject> = owed.iter().copied().map(Subject::Setting).collect();
    let followed = {
        let _fence = (!owed.is_empty())
            .then(|| StageFence::open(hooks, progress.revision, &owed, &owed_subjects));
        host.withheld(root)
    };
    match followed {
        Ok(Some(follow_up)) => {
            let at = report.rows.len();
            report.rows.extend(follow_up.rows);
            let page = if let Some(restore) = follow_up.restore {
                // The pass keeps the put-back's outcome separately from its
                // raw keys and reader rows, which belong only in CLI output.
                // Even an empty page stands for that raw range.
                report.restore = Some(restore);
                Some(follow_up.page.unwrap_or_default())
            } else {
                follow_up.page
            };
            if let Some(page) = page {
                pages.push((at..report.rows.len(), page));
            }
        }
        Ok(None) => {}
        Err(detail) => {
            let row = Row {
                name: "Settings waiting for Full Disk Access".into(),
                status: "unverified".into(),
                detail,
                elapsed_ms: None,
            };
            let at = report.rows.len();
            pages.push((
                at..at + 1,
                step_page(&row, owed.iter().copied().map(Subject::Setting)),
            ));
            report.rows.push(row);
        }
    }
    if stages_finished {
        // A final person step can outlast the vault, repository inventory or credential
        // route observed at this pass's start, so completion reads them again
        // ([`publish_completion`]); completed sign-outs stay recorded while only stale network
        // stages reopen for their ordinary guarded repair on the next pass.
        let reopened = publish_completion(host, root, &plan, &mut progress)?;
        if progress.completed {
            next = SetupNext::Done;
        }
        for (stage, rows) in reopened {
            next = next.and(SetupNext::of(&rows));
            let at = report.rows.len();
            let page = if stage == Stage::Secrets {
                Some(secrets_page(root, progress.revision, &rows))
            } else {
                items_page(stage, &rows)
            };
            if let Some(page) = page {
                pages.push((at..at + rows.len(), page));
            }
            report.rows.extend(rows);
        }
    }
    // The pass's end is a boundary too: a Stop asked for during the last stage of a run that
    // finished is spent, and one asked for while work is left stops the run here.
    let stopped = stop_at_boundary(root, &mut progress, hooks)?;
    if stopped {
        report.rows.push(Row {
            name: "System State".into(),
            status: "stopped".into(),
            detail: "stopped after this pass; nothing more runs until the plan changes".into(),
            elapsed_ms: None,
        });
        next = SetupNext::Done;
    }
    clock.lap(&mut report.rows);
    Ok(Some((paged(report, pages), next)))
}

/// `report` with its page (`Report::page`): each range of its rows a stage's own page stands
/// for in that page's place, its other rows as they are, in the pass's order.
fn paged(mut report: Report, pages: Vec<(std::ops::Range<usize>, PassPage)>) -> Report {
    let mut page = PassPage::default();
    let mut pages = pages.into_iter().peekable();
    let mut at = 0;
    while at < report.rows.len() {
        match pages.next_if(|(range, _)| range.start <= at) {
            Some((range, part)) => {
                page.merge(part);
                at = at.max(range.end);
            }
            None => {
                page.rows.push(report.rows[at].clone());
                at += 1;
            }
        }
    }
    // A stage whose page stands for no row still says what it did.
    for (_, part) in pages {
        page.merge(part);
    }
    report.page = Some(page);
    report
}

/// The page of a stage whose rows are its items' own lines (Rust in Spotlight's, GitHub
/// repositories'), which name what the item changes: the page names the stage's one row
/// instead, and says a change that failed or could not be read back in its own words
/// (design §3.6). `None` for a stage whose rows are the page's already.
fn items_page(stage: Stage, rows: &[Row]) -> Option<PassPage> {
    let setting = match stage {
        Stage::Spotlight => Setting::Spotlight,
        Stage::Repositories => Setting::Repositories,
        _ => return None,
    };
    let mut page = PassPage::default();
    for row in rows {
        let how = match row.status.as_str() {
            "error" => Some(Unfinished::Failed),
            "unverified" => Some(Unfinished::Unconfirmed),
            _ => None,
        };
        match how {
            // A stage's own line (`Spotlight and Rust documentation`) is said under the page's
            // row (`Rust in Spotlight`); a step that failed or could not be confirmed the page
            // says in its own words, and `aterm doctor` keeps the stage's.
            _ if row.name == stage.name() => page.rows.push(Row {
                name: setting.name().into(),
                detail: if how.is_some() {
                    String::new()
                } else {
                    row.detail.clone()
                },
                ..row.clone()
            }),
            Some(how) => page.outcome.unfinished(setting, how),
            None => {}
        }
    }
    Some(page)
}

/// At a stage boundary of a pass holding profile.lock: whether a person stopped
/// the run (`hooks`, or the durable request), saving it in the run's record if
/// so. The request is spent once read here, folded into a stopped run or too
/// late for a completed one; it is left only for a run it does not name.
fn stop_at_boundary(
    root: &Path,
    progress: &mut Progress,
    hooks: SetupHooks,
) -> Result<bool, String> {
    let request = stop_request_at(root);
    let stopped =
        !progress.completed && ((hooks.stop)(progress.revision) || progress.stopped_by(request));
    if stopped {
        progress.stopped = true;
        progress.running = None;
        save_progress(root, *progress)?;
    }
    if let Some(request) = request
        && (stopped || progress.completed)
    {
        settle_stop_request(root, request)?;
    }
    Ok(stopped)
}

pub fn apply_initial_setup(
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    apply_initial_setup_observed(expected_revision, password).result
}

pub fn apply_initial_setup_observed(
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
) -> InitialSetupObserved<Option<Report>> {
    let pass = run_initial_setup_pass(expected_revision, password, SetupHooks::NONE);
    InitialSetupObserved::new(
        pass.result.map(|pass| pass.map(|(report, _)| report)),
        pass.observation,
    )
}

/// One pass of the admitted run `expected_revision`, asking `hooks` between its
/// stages, and what it left for the next pass.
pub fn run_initial_setup_pass(
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
) -> InitialSetupObserved<Option<(Report, SetupNext)>> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return InitialSetupObserved::new(Ok(None), None);
    }
    observe_person_mutation(Ok(None), |root| {
        execute_initial_for_revision(
            &mut LiveInitial::live(),
            root,
            expected_revision,
            password,
            hooks,
        )
    })
}

/// One pass of the admitted run `expected_revision` by the login job, with no window and no
/// person (`resume`, design §6.4): the stages that run unattended (the Mac settings, Spotlight,
/// Secrets, Repositories, and the settings Full Disk Access let go), asking `hooks` between them,
/// and never a person stage (Permissions, the sign-outs), which waits for a window at this Mac, so
/// what it leaves parks. It holds no typed password.
pub fn run_background_setup_pass(
    expected_revision: u64,
    hooks: SetupHooks,
) -> InitialSetupObserved<Option<(Report, SetupNext)>> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return InitialSetupObserved::new(Ok(None), None);
    }
    observe_person_mutation(Ok(None), |root| {
        execute_initial_for_revision_target(
            &mut LiveInitial::live(),
            root,
            expected_revision,
            &mut None,
            hooks,
            PassTarget::Background,
        )
    })
}

fn execute_initial_for_revision(
    host: &mut dyn InitialHost,
    root: &Path,
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
) -> Result<Option<(Report, SetupNext)>, String> {
    execute_initial_for_revision_target(
        host,
        root,
        expected_revision,
        password,
        hooks,
        PassTarget::All,
    )
}

fn execute_initial_for_revision_target(
    host: &mut dyn InitialHost,
    root: &Path,
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
    target: PassTarget,
) -> Result<Option<(Report, SetupNext)>, String> {
    if revision_at(root)? != expected_revision {
        return Ok(None);
    }
    // System State's vault row waits for a vault nobody answered only while a run holds a join
    // password the person typed for a try ([`scope::join_typed`]): this one notes whether it holds
    // one as the pass begins, and as it ends, once the Secrets stage or its lane handed it back or
    // a try spent it.
    scope::holding_join_password(root, expected_revision, password.is_some());
    let pass = execute_pass_target(host, root, password, hooks, target);
    scope::holding_join_password(root, expected_revision, password.is_some());
    pass
}

#[cfg(test)]
mod tests;
