// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable first-launch plan.

use super::*;
use aterm_machines::init_secrets::Replaces;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{Plan, Subject};

mod check;
pub use check::{check_initial_setup, check_initial_setup_observed};
mod observation;
pub use observation::{InitialSetupObservation, InitialSetupObserved};
mod authorization;
pub use authorization::InitialSetupAuthorization;
#[cfg(any(test, debug_assertions))]
pub use authorization::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};
mod access;
mod observe;
pub use observe::observe_initial_setup;
pub(super) use observe::{changed_at_login, note_at_login};
mod scope;
#[cfg(any(test, debug_assertions))]
pub use scope::ProvisionalSetupWatchTest;
pub use scope::{
    InitialSetupUpgrade, SetupUpgrade, alab_mac, decline_provisional_setup_widen,
    provisional_setup_widened, provisional_setup_widens, put_back_held_initial_setup,
};
mod park_watch;
pub use park_watch::{ParkWatch, Watches};
mod secrets_retry;
use secrets_retry::Pace;
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
        match self {
            Self::Workstation => "Selected Mac settings",
            Self::Spotlight => "Spotlight and Rust documentation",
            Self::Permissions => "aterm permissions",
            Self::Secrets => aterm_types::macos_setup::SECRETS_VAULT_ROW,
            Self::Repositories => "GitHub repositories",
            Self::Accounts => "Messages and FaceTime sign-out",
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
        match self {
            Self::Workstation => &[
                Setting::CapsLock,
                Setting::NaturalScrolling,
                Setting::TrackpadScrolling,
                Setting::Desktop,
                Setting::Widgets,
                Setting::Dock,
            ],
            Self::Spotlight => &[Setting::Spotlight],
            Self::Permissions => &[Setting::Permissions],
            Self::Secrets => &[Setting::Secrets],
            Self::Repositories => &[Setting::Repositories],
            Self::Accounts => &[Setting::Messages, Setting::FaceTime],
        }
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
                Subject::Access(Access::LocalNetwork),
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
    let bytes = match std::fs::read(root.join(STOP_REQUEST)) {
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
    let bytes = match std::fs::read(&path) {
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
    revision::reserve(root, progress.revision)?;
    write_json(&root.join("initial-setup.json"), &progress_record(progress))
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
        json!({"profile":PROFILE,"state":if progress.completed {"completed"} else {"pending"},"admitted":progress.admitted,"done":progress.done,"running":progress.running.map(Stage::bit),"plan":aterm_secrets::b64::encode_padded(&progress.plan),"revision":progress.revision,"stages":Stage::ALL.len()}),
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
    let bytes = match std::fs::read(&path) {
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

fn save_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    save_plan_as(root, plan, false)
}

/// Save `plan`; `provisional` marks one aterm chose by itself at first launch
/// (ALab scope, System State design §3.5), which a later launch may widen. The
/// field is written only when set: an absent field is a plan a person shaped.
fn save_plan_as(root: &Path, plan: &Plan, provisional: bool) -> Result<(), String> {
    validate_plan(plan)?;
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
    let bytes = match std::fs::read(&path) {
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

/// `aterm doctor --apply`'s plan: every workstation row, and aterm's own permissions too
/// (approved by default, System State design §6) unless the person's Cancel stands, which an
/// explicit apply supersedes only for the rows it names. A Restore withdraws no permission, so
/// after one the plan keeps them; the run a Restore ended stays admitted, at every write of this
/// one too, so the explicit pass finishes it ([`complete_explicit_workstation_plan`]) and its
/// deferred Widgets change follows. What it returns is that completion's to read.
pub(super) fn save_explicit_workstation_plan(root: &Path) -> Result<ExplicitRun, String> {
    save_explicit_workstation_plan_with(root, |_| Ok(()))
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
    let Some(mut progress) = progress(root)? else {
        let mut plan = Plan::from(Selection::all().workstation());
        plan.selection.set(Setting::Permissions, !cancelled);
        return restart_in_vault(root, &plan, AdmittedVault::Unknown).map(|_| run);
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    let (previous, prior) = (plan.clone(), progress);
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(root, &plan)?;
    } else if stopped || progress.stopped {
        progress.revision = revision::next(root, progress.revision)?;
        progress.stopped = false;
    }
    let admitted = progress.admitted;
    if stopped {
        // The new revision lands before the decision it reads under. A Cancel's run runs no more
        // until this apply admits it; a Restore withdrew no permission, so its run keeps its
        // admission and its Permissions approval wherever this process ends.
        progress.admitted = admitted && !cancelled;
        progress.access = carried_access(Some(&previous), Some(prior), &previous);
        before(ExplicitWrite::Revision)?;
        save_progress(root, progress)?;
    }
    for setting in Setting::ALL {
        if setting.is_workstation() {
            plan.selection.set(setting, true);
        }
    }
    plan.selection.set(Setting::Permissions, !cancelled);
    progress.access = carried_access(Some(&previous), Some(prior), &plan);
    before(ExplicitWrite::Decision)?;
    save_workstation_authorization(root, &plan)?;
    before(ExplicitWrite::Plan)?;
    save_plan(root, &plan)?;
    progress.bind(&plan);
    progress.done &= !(Stage::Workstation.bit() | Stage::Accounts.bit());
    progress.completed = false;
    progress.running = None;
    // The explicit profile selects every preference, superseding any earlier
    // group-only retry even if this process ends before its first OS change.
    progress.only = None;
    // A person's explicit apply is a run of its own: an older run's Stop holds it no more.
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
/// still waits for that offer, and its plan leaves aterm's own permissions as they are.
pub(super) fn complete_explicit_workstation_plan(
    root: &Path,
    run: ExplicitRun,
) -> Result<(), String> {
    let Some(mut progress) = progress(root)? else {
        return Ok(());
    };
    let plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        return Ok(());
    }
    progress.admitted |= run.admits;
    // The explicit pass signs out in the same pass as its preferences.
    progress.done |= Stage::Workstation.bit() | Stage::Accounts.bit();
    progress.running = None;
    progress.completed = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0);
    save_progress(root, progress)
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

/// Whether this process let a probe of the ALab vault nobody answered through to the join, to
/// wait for macOS's Local Network answer ([`unjoined_alab_row`]).
static UNANSWERED_PROBE_JOINED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The row System State's Secrets stage stops at for a plan that names the ALab vault this Mac
/// has not joined, from what the probe of its address heard (`here`) and what a LAN look last
/// heard of macOS's Local Network switch (`lan`, [`access::last_answer`]); `None` lets the join
/// go ahead (design §3.3).
///
/// A server that holds no ALab vault leaves the row waiting for a change of network, the next
/// launch, a new plan or Check again. A probe nobody answered is asked again after the Secrets
/// retry pause, except the first in this process (`joined`, set here) while no look has heard
/// how the switch stands: macOS may be holding the probe's lookup until the person answers its
/// question, so that join goes ahead and its own look waits for the answer (`init_secrets`'
/// patient look, spent once per process too), and the Mac joins within seconds of Allow, not a
/// retry pause later. Once a look heard the switch, waiting for the person cannot help: the
/// network or the vault failed with it on, or the person turned it off. The record keeps an
/// answer from earlier launches, and an asked question from one long answered, so only the
/// first probe of a process waits for it.
fn unjoined_alab_row(
    here: scope::VaultHere,
    lan: Option<access::LanAnswer>,
    joined: &std::sync::atomic::AtomicBool,
) -> Option<Row> {
    use access::LanAnswer;
    let status = match here {
        scope::VaultHere::Absent => WAITING,
        scope::VaultHere::Unreached => {
            let heard = matches!(lan, Some(LanAnswer::Reached | LanAnswer::Refused));
            if !heard && !joined.swap(true, std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            UNREACHED
        }
        scope::VaultHere::Found | scope::VaultHere::Unasked => return None,
    };
    Some(Row {
        name: Stage::Secrets.name().into(),
        status: status.into(),
        detail: here.note()?.into(),
        elapsed_ms: None,
    })
}

/// [`secrets_rows`] of the join a probe nobody answered let through ([`unjoined_alab_row`]): a
/// row its look stood aside on reads as the probe's own row would have for what the look heard,
/// with the look's words. A vault that did not answer it either is [`UNREACHED`], so the pass
/// parks and the Secrets retry pause asks again; a server there holding no vault is [`WAITING`]
/// for a change ([`aterm_machines::init_secrets::NO_VAULT_NOW`]).
fn unanswered_join_rows(reports: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    let parked: Vec<Option<&'static str>> = reports
        .iter()
        .map(|report| match &report.stood_aside {
            Some(aterm_converge::StoodAside::Offline(why))
                if report.outcome == aterm_machines::model::Outcome::Blocked =>
            {
                Some(
                    if why.starts_with(aterm_machines::init_secrets::NO_VAULT_NOW) {
                        WAITING
                    } else {
                        UNREACHED
                    },
                )
            }
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

/// What the join a probe nobody answered let through ([`unjoined_alab_row`]) heard of macOS's
/// Local Network switch, kept for the Local Network row and later launches
/// ([`access::lan_answered`]) as the probe would have kept it. A join made, waiting for an admin
/// Mac, or failing after the vault answered (its subscriber not installed), and a server there
/// holding no vault are a reach; macOS refusing the vault's `.local` name is the person's Don't
/// Allow; a look nothing answered, or a join that failed before the vault answered, says nothing
/// of the switch.
/// The rows of a join a probe nobody answered let through, after `keep` is handed what its look
/// heard of Local Network ([`join_lan_answer`], [`unanswered_join_rows`]).
fn unanswered_join(
    reports: Vec<aterm_machines::model::ItemReport>,
    keep: impl FnOnce(access::LanAnswer),
) -> Vec<Row> {
    if let Some(answer) = join_lan_answer(&reports) {
        keep(answer);
    }
    unanswered_join_rows(reports)
}

fn join_lan_answer(reports: &[aterm_machines::model::ItemReport]) -> Option<access::LanAnswer> {
    use access::LanAnswer::{Reached, Refused};
    use aterm_converge::StoodAside;
    use aterm_machines::model::Outcome;
    reports
        .iter()
        .find_map(|report| match (report.outcome, &report.stood_aside) {
            (Outcome::Changed, None) | (Outcome::Blocked, Some(StoodAside::Waiting(_))) => {
                Some(Reached)
            }
            (Outcome::Failed, None)
                if report.why.as_deref().is_some_and(|why| {
                    why.starts_with(aterm_machines::init_secrets::VAULT_ANSWERED_BUT)
                }) =>
            {
                Some(Reached)
            }
            (Outcome::Blocked, Some(StoodAside::Offline(why))) => {
                if why.starts_with(aterm_machines::init_secrets::NO_VAULT_NOW) {
                    Some(Reached)
                } else {
                    aterm_secrets::client::refused_by_local_network(&aterm_secrets::Error::new(
                        why.as_str(),
                    ))
                    .then_some(Refused)
                }
            }
            _ => None,
        })
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

fn keeps_vault_sync_at(root: &Path) -> Result<bool, String> {
    let records = observation_at(root)
        .map_err(|error| error.to_string())?
        .records;
    Ok(keeps_vault_sync(&records))
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
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| restart_reading(root, plan, live_configured_vault),
    )
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
    restart_with(root, plan, vault, save_progress)
}

fn restart_with(
    root: &Path,
    plan: &Plan,
    vault: AdmittedVault,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<u64, String> {
    let mut next = stage_restart(root, plan, Restart::default(), vault)?;
    next.admitted = true;
    commit(root, next)?;
    Ok(next.revision)
}

/// Who restarts a run ([`stage_restart`]).
#[derive(Clone, Copy, Debug, Default)]
struct Restart {
    /// A person's choice of groups or a Join ([`InitialSetupRestart::chosen`]), which is
    /// never provisional; otherwise Set up now, or a resume of the plan in force.
    chosen: bool,
    /// A Join: the vault typed in System State, whose steps run again.
    joins: bool,
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
    let Restart { chosen, .. } = restart;
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
        && previous
            .as_ref()
            .is_some_and(|(saved, modern)| *modern && saved == plan)
        && saved_plan_provisional(root)?;
    before(RestartWrite::Plan)?;
    save_plan_as(root, plan, provisional)?;
    before(RestartWrite::Progress)?;
    publish_progress(root, next)?;
    before(RestartWrite::Reset)?;
    for (record, what) in [
        ("automatic-attempt.json", "the automatic retry cooldown"),
        (HELD_RECORD, "the held automatic setup items"),
        (secrets_retry::RECORD, "the secrets retry pause"),
    ] {
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
///   Developer Tools, the debugger and Local Network then go on by themselves. The Mac
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
    authorize_reading(root, plan, || Ok(None))
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

/// The window recorded an access step (`aterm_types::macos_setup::access_record`): Local
/// Network's says what the window heard of the switch, now, which outranks any older answer kept
/// from a look ([`access::local_network_step`]). The step itself is never a question asked again.
pub fn initial_setup_access_step(
    access: aterm_types::macos_setup::Access,
    outcome: aterm_types::macos_setup::AccessOutcome,
) {
    if access == aterm_types::macos_setup::Access::LocalNetwork {
        access::local_network_step(outcome);
    }
}

/// Check again's look at the LAN (System State design §3.3): the Local Network row then reads
/// what macOS answers now, not an answer kept from long ago. It looks only once macOS has asked
/// aterm already ([`access::lan_asked_before`]), so it never puts the question on screen, and
/// it waits for the look on the caller's thread.
pub fn probe_local_network_again() {
    if ensure_person_scope().is_ok() && access::lan_asked_before() {
        scope::probe_lan();
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
    /// The plan in force is one aterm chose by itself (design §3.5), which a look that finds
    /// the ALab signal widens: System State then sends every group press, even one that
    /// leaves the plan as it is, so the plan becomes the person's and is never widened.
    pub provisional: bool,
    /// A person's Cancel of the first-launch review stands ([`save_cancelled_decision`]): every
    /// group reads Leave as is and nothing runs, and System State says so in one calm line
    /// beside Set up now, which applies the plan the review showed (design §3.3).
    pub cancelled: bool,
    /// The plan in force is the one aterm set up afresh after it set aside a saved setup it
    /// could not read ([`set_aside_unreadable_setup`]), and no person has chosen since: System
    /// State says so in one calm note (design §3.5).
    pub recovered: bool,
    pub observation: InitialSetupObservation,
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
    let grants = access::read(selection, &[]);
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
        let why = row.detail.or(row.note);
        (access, init_grant(access, row.standing, why, &app))
    })
    .collect()
}

/// One grant as `aterm init` lists it ([`init_grants`]): `standing` as System State reads it,
/// `why` its detail. Full Disk Access and Accessibility are read for the app macOS holds
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
            Access::Debugger => "aterm.app's Permissions turns it on with one administrator \
                                 password (`aterm help permissions`)"
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
    initial_setup_snapshot_offering(None)
}

/// [`initial_setup_snapshot`], presenting `offered`, the plan this launch's review showed
/// before a person closed it unanswered, while no plan is saved: System State then shows the
/// plan the review showed, never the whole profile (design §3.3).
pub fn initial_setup_snapshot_offering(
    offered: Option<&Plan>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::read)?;
    snapshot_in_vault(&root, live_configured_vault(), offered)
}

fn snapshot_in_vault(
    root: &Path,
    configured: Result<Option<ConfiguredVault>, String>,
    offered: Option<&Plan>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let saved =
        saved_plan_record(root).map_err(|why| InitialSetupSnapshotError::read_at(root, why))?;
    let fallback = match (saved, offered) {
        (None, Some(offered)) => offered_plan(Some((offered.clone(), true)), configured.clone()),
        (saved, _) => offered_plan(saved, configured.clone()),
    };
    let mut snapshot = snapshot_at(root, fallback)?;
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
/// A pass checkpoints each selected Mac stage it fully verified before the decision,
/// so a later bookkeeping failure cannot repeat its effects. A partial stage receives
/// no completion credit. A failed state read changes no completion record.
pub(super) fn record_applied(root: &Path, set: Selection) -> Result<(), String> {
    record_applied_with(root, set, decision_record)
}

fn record_applied_with(
    root: &Path,
    set: Selection,
    read: impl FnOnce(&Path) -> Result<(Decision, Option<Restored>), String>,
) -> Result<(), String> {
    let planned = authorized_selection(root)?.unwrap_or_default();
    let (progress, plan) = (progress(root)?, saved_plan(root)?);
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
        }
    }
    let stage_owed = progress.zip(plan.as_ref()).is_some_and(|(progress, plan)| {
        progress.belongs_to(plan)
            && [
                (
                    Stage::Workstation,
                    preferences as fn(Selection) -> Selection,
                ),
                (Stage::Accounts, accounts),
            ]
            .into_iter()
            .any(|(stage, part)| {
                stage.selected(plan) && part(set).is_empty() && progress.done & stage.bit() == 0
            })
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
    let bytes = match std::fs::read(&path) {
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
        // A Cancel of the review a Restore brought forward ([`save_cancelled_restore`]): the
        // Restore's record, held by it.
        (Decision::Stopped(Stop::Restored), Some(reviewed))
            if restored.is_some_and(|restored| restored.held) =>
        {
            Some(parse_plan_record(reviewed)?.0)
        }
        _ => None,
    };
    Ok((decision, restored, reviewed))
}

/// A person's Cancel of the first-launch review (System State design §3.3): the first-run
/// decision, cancelled, naming the plan the review showed (`reviewed`). One write, so the
/// records are whole whenever a process ends. While it stands every group reads Leave as is,
/// aterm's own permissions with them, nothing runs and no launch offers the review again; Set
/// up now applies the plan it names, and a group choice, a Join or `aterm doctor --apply`
/// replaces it ([`Records::cancelled`]).
pub(super) fn save_cancelled_decision(root: &Path, reviewed: &Plan) -> Result<(), String> {
    validate_plan(reviewed)?;
    write_json(
        &root.join("first-run.json"),
        &stamped(json!({
            "profile": PROFILE,
            "state": "cancelled",
            "reviewed": plan_record(reviewed, false),
        })),
    )
}

/// A person's Cancel of the review that offered what a Restore brought forward (System State
/// design §3.3, §3.4): the Restore's record held ([`Restored::held`]), naming the plan the
/// review showed (`reviewed`), in one write. It reads and behaves as any other Cancel
/// ([`save_cancelled_decision`], [`Records::cancelled`]): every group at Leave as is, aterm's own
/// permissions held, nothing runs and no launch offers the review again, and Set up now applies
/// the plan it names. The held record keeps what the Restore put back: nothing automatic sets
/// those groups again, and a put-back still waiting for Full Disk Access is still put back. An
/// older aterm reads past `reviewed` (`format` 1) and keeps the held Restore as it was.
fn save_cancelled_restore(root: &Path, restored: Restored, reviewed: &Plan) -> Result<(), String> {
    validate_plan(reviewed)?;
    let mut record = Restored {
        held: true,
        ..restored
    }
    .record(RestoredState::Restored);
    record
        .as_object_mut()
        .ok_or("the Restore's record is not an object")?
        .insert("reviewed".into(), plan_record(reviewed, false));
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
    /// The plan the first-launch review showed, when a person's Cancel of it is the decision
    /// ([`save_cancelled_decision`]): every reader presents every group at Leave as is while it
    /// stands ([`Self::present`]), and Set up now applies this plan.
    cancelled: Option<Plan>,
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
        })
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
    let observation = observation_at(root)?;
    let active = HeldLock::profile_active(root).map_err(InitialSetupSnapshotError::read)?;
    Ok(InitialSetupSnapshot {
        recovered: set_aside::shown(root),
        ..snapshot_from_observation(fallback, active, observation)
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
        active,
        observation::from_records(records),
    ))
}

fn snapshot_from_observation(
    fallback: Plan,
    active: bool,
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
    // A Restore puts back Mac preferences and never withdraws aterm's own permissions: the
    // run it ends stays admitted ([`leave_restored_as_is`]), so the access steps go on by
    // themselves. Only a person's Cancel or Stop holds them.
    let permissions_allowed = stopped != Some(Stop::Cancelled)
        && !run_stopped
        && match records.progress {
            Some(_) => {
                records.plan.is_some()
                    && progress.is_some_and(|progress| progress.admitted)
                    && matches!(
                        records.decision,
                        Decision::Pending
                            | Decision::Applied(true)
                            | Decision::Stopped(Stop::Restored)
                    )
                    && plan.selection.contains(Setting::Permissions)
            }
            None => {
                !records.plan.as_ref().is_some_and(|(_, modern)| *modern)
                    && records.decision == Decision::Applied(true)
            }
        };
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
            let recorded_running =
                progress.is_some_and(|progress| progress.running == Some(*stage));
            let interrupted = !finished && !completed && recorded_running && !active;
            Row {
                name: stage.name().into(),
                status: if restoring {
                    "stopped"
                } else if completed {
                    "applied"
                } else if finished {
                    "stopped"
                } else if recorded_running && active {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                detail: if restoring {
                    "Restore requested; see results.".into()
                } else if finished && !completed {
                    "Not applied by this run.".into()
                } else if interrupted {
                    "Not running; Set up now to retry.".into()
                } else {
                    String::new()
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
        provisional: records.plan.is_some() && records.provisional && records.cancelled.is_none(),
        cancelled: records.cancelled.is_some(),
        recovered: false,
        observation,
    }
}

#[cfg(test)]
fn status_at(root: &Path, fallback: Plan) -> Result<(Plan, bool, Vec<Row>), String> {
    let snapshot = snapshot_at(root, fallback).map_err(|error| error.to_string())?;
    Ok((snapshot.plan, snapshot.finished, snapshot.rows))
}

trait InitialHost {
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
    fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String>;
    /// Whether the Repositories stage, as [`Self::repositories`] last ran it, stopped for want
    /// of Apple's Command Line Tools ([`aterm_machines::init_observe::Wait::CommandLineTools`]):
    /// the pass then asks the window to install them ([`SetupHooks::developer_tools`]).
    fn repositories_want_developer_tools(&mut self) -> bool {
        false
    }
    /// The reason the failed part of the Repositories stage, as [`Self::repositories`] last ran
    /// it, gives where the step that stopped decided one
    /// ([`aterm_machines::init_observe::failure_note`]): System State's row says it in place of
    /// the stage's own line.
    fn repositories_failure(&mut self) -> Option<&'static str> {
        None
    }
    /// The Messages and FaceTime sign-outs, with the Accessibility they need.
    /// `on_screen` is whether System State is the visible view of a focused window
    /// ([`SetupHooks::on_screen`]): without it a sign-out that still has work waits.
    fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String>;
    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String>;
    /// The settings [`Self::withheld`] would change now (`super::owed_with_full_disk_access`):
    /// the pass marks them busy while it runs, as it marks a stage's, so a reading taken
    /// meanwhile never stands, and reads them again once it ends.
    fn withheld_owed(&mut self, _root: &Path) -> Vec<Setting> {
        Vec::new()
    }
}

/// One stage of a setup pass starting or ending, for System State: the settings
/// it changes (the page's busy rows) and the rows to read again once it ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageEvent<'a> {
    /// The settings whose rows the stage changes.
    pub settings: &'a [Setting],
    /// The System State rows the stage's work can change.
    pub subjects: &'a [Subject],
    /// Whether the stage is starting (`true`) or has ended (`false`).
    pub running: bool,
}

/// What a setup pass asks between its stages.
#[derive(Clone, Copy)]
pub struct SetupHooks {
    /// Whether a person asked the run of this revision to stop (System State's
    /// Stop). Asked before every stage, and at the pass's end.
    pub stop: fn(u64) -> bool,
    /// A stage started or ended.
    pub stage: for<'a> fn(StageEvent<'a>),
    /// Whether System State is the visible view of a focused window now
    /// (`aterm_types::macos_setup::system_state_front`). The Accounts stage may raise macOS's
    /// Accessibility and Automation prompts, so it runs only then (design §6); otherwise it
    /// parks, and the window wakes the run when the page comes to the front.
    pub on_screen: fn() -> bool,
    /// The exact run revision and whether its Repositories stage waits on Apple's Command Line
    /// Tools, which only a root install with the person's password adds: `true`, at the end of
    /// an unstopped pass whose stage waits on them, and the window starts that install in a
    /// terminal tab of its own (`aterm init --only base.clt`), once per launch, never in the
    /// middle of a pass; `false` withdraws a want the window still holds, said as soon as the
    /// stage ends without waiting on them (so a pass that fails after it leaves no want), or
    /// when a Stop ends a pass whose stage waits on them. Once a pass that ran the stage, unless
    /// it fails after asking. The run's park wakes once the tools are there:
    /// a park's later sample differs from its first, and an install that finished during the
    /// pass is a fact [`InitialSetupParkMark::facts_changed_since`] reads.
    pub developer_tools: fn(u64, bool),
}

impl SetupHooks {
    /// A pass nobody stops or watches, started by a person where they are (a
    /// terminal), so nothing in it waits for a page. Its person installs the Command Line
    /// Tools by `aterm init`, which its row's remedy names.
    pub const NONE: Self = Self {
        stop: |_| false,
        stage: |_| {},
        on_screen: || true,
        developer_tools: |_, _| {},
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

    /// What one stage's rows leave: done when all are in place, a park when every
    /// row left waits on a person or an absent vault ([`WAITING`]), on a vault
    /// that did not answer ([`UNREACHED`]) or on what an engine put off ([`super::DEFERRED`]).
    fn of(rows: &[Row]) -> Self {
        if rows_ready(rows) {
            Self::Done
        } else if !rows.is_empty()
            && rows.iter().all(|row| {
                row_ready(row)
                    || matches!(row.status.as_str(), WAITING | UNREACHED | super::DEFERRED)
            })
        {
            Self::Park
        } else {
            Self::Retry
        }
    }
}

/// A row's status when only a person, or a vault this network does not have, moves
/// it: the pass parks instead of retrying it ([`SetupNext::Park`]).
pub(super) const WAITING: &str = "waiting";

/// A row's status when the ALab vault did not answer ([`scope::NOT_REACHED`]): the
/// pass parks, and the Secrets retry pause wakes it to ask again
/// ([`secrets_retry::retry_due`]), since a Local Network grant, a network or the
/// vault coming back moves it without a person.
pub(super) const UNREACHED: &str = "unreached";

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
    interfaces: Option<Vec<aterm_secrets::network::InterfaceAddress>>,
    /// The run whose Repositories stage is unfinished: the Command Line Tools arriving may end
    /// its wait ([`InitialSetupParkMark::facts_changed_since`]).
    developer_tools_run: Option<u64>,
    /// Whether the git every clone runs is installed, read for [`Self::developer_tools_run`]
    /// ([`InitialSetupParkMark::with_developer_tools`]).
    developer_tools: Option<bool>,
    retry_due: Option<u64>,
    mac_settings_retry_due: Option<u64>,
    /// The earliest Secrets or Mac settings retry not due yet ([`InitialSetupParkMark::next_due`]).
    retry_ahead: Option<u64>,
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
        if self.network_run.is_none() || self.network_run != before.network_run {
            return false;
        }
        changed(&self.vault, &before.vault)
            || changed(&self.interfaces, &before.interfaces)
            || (self.materialization_run.is_some()
                && self.materialization_run == before.materialization_run
                && changed(&self.materialization, &before.materialization))
            || (self.subscriber_run.is_some()
                && self.subscriber_run == before.subscriber_run
                && changed(&self.subscriber, &before.subscriber))
            // The tools' install (the tab the window opened, or a person's own `aterm init`)
            // finishing during the pass ends the Repositories stage's wait on them.
            || (self.developer_tools_run.is_some()
                && self.developer_tools_run == before.developer_tools_run
                && self.developer_tools == Some(true)
                && before.developer_tools == Some(false))
    }

    /// The mark with whether the Command Line Tools are installed (`installed`), read only
    /// for a run whose Repositories stage is unfinished.
    fn with_developer_tools(mut self, installed: impl FnOnce() -> Option<bool>) -> Self {
        self.developer_tools = self.developer_tools_run.and_then(|_| installed());
        self
    }

    /// A contended or unreadable local state gives no new fact. Keep the
    /// last known materialization or service state only within its same run.
    pub fn retain_known_from(&mut self, previous: &Self) {
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
    }

    /// A retryable unfinished vault try is due. A live worker consumes each
    /// deadline once, so a pass that cannot reach Secrets cannot re-arm it.
    pub fn retry_due(&self) -> Option<u64> {
        self.retry_due
    }

    /// A selected, unfinished Mac settings stage has a second automatic try
    /// due. Its token is consumed independently of a Secrets deadline.
    pub fn mac_settings_retry_due(&self) -> Option<u64> {
        self.mac_settings_retry_due
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

    /// The facts [`Self::facts_changed_since`] reads that can move while the run is parked: the
    /// secrets folder and the network for unfinished Secrets or Repositories, and the Command Line
    /// Tools for Repositories that wait on them. A run that waits on a person alone watches none.
    pub fn watches(&self) -> Watches {
        Watches {
            secrets: self.network_run.is_some(),
            network: self.network_run.is_some(),
            developer_tools: self.network_run.is_some()
                && self.developer_tools_run.is_some()
                && self.developer_tools != Some(true),
        }
    }
}

/// What a parked run waits on that moves without the window: this login's vault
/// state and, for admitted unfinished Secrets or Repositories, local interfaces.
/// Unfinished Secrets also observes subscriber state and an existing retry
/// deadline. A selected, unfinished Mac settings stage wakes for its second
/// automatic try. It also says whether an Accounts or Permissions stage is left,
/// which a person moves in System Settings or macOS. No network request or
/// permission prompt is made here.
pub fn initial_setup_park_mark() -> InitialSetupParkMark {
    initial_setup_park_mark_at(
        state_root().ok().as_deref(),
        aterm_secrets::client::Paths::live().ok().as_ref(),
        secrets_retry::Reads::now(&secrets_retry::Live),
        || aterm_secrets::network::interfaces().ok(),
    )
    .with_developer_tools(|| {
        Some(aterm_machines::init_github::git_installed(
            &aterm_machines::port::Live::new(),
        ))
    })
}

fn initial_setup_park_mark_at(
    root: Option<&Path>,
    paths: Option<&aterm_secrets::client::Paths>,
    now: u64,
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
) -> InitialSetupParkMark {
    let vault = paths.and_then(|paths| aterm_secrets::client::status(paths).ok());
    initial_setup_park_mark_reading(
        root,
        vault,
        now,
        interfaces,
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
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
) -> InitialSetupParkMark {
    initial_setup_park_mark_reading(root, vault, now, interfaces, || None, || None)
}

fn initial_setup_park_mark_reading(
    root: Option<&Path>,
    mut vault: Option<aterm_secrets::client::Status>,
    now: u64,
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
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
    let pending = root.and_then(|root| {
        let observation = observation_at(root).ok()?;
        let records = &observation.records;
        let (plan, true) = records.plan.as_ref()? else {
            return None;
        };
        let progress = records.progress?;
        (progress.admitted
            && progress.belongs_to(plan)
            && !progress.completed
            && !progress.stopped_by(records.stop_request)
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
    let unfinished_network = pending.filter(|(_, progress, secrets, repositories, _, _)| {
        (*secrets && progress.done & Stage::Secrets.bit() == 0)
            || (*repositories && progress.done & Stage::Repositories.bit() == 0)
    });
    let materialization_run = pending
        .filter(|(_, progress, secrets, repositories, _, _)| {
            *secrets
                && (progress.done & Stage::Secrets.bit() == 0
                    || (*repositories && progress.done & Stage::Repositories.bit() == 0))
        })
        .map(|(_, progress, _, _, _, _)| progress.revision);
    InitialSetupParkMark {
        vault,
        network_run: unfinished_network.map(|(_, progress, _, _, _, _)| progress.revision),
        materialization_run,
        materialization: materialization_run.and_then(|_| materialization()),
        subscriber_run: unfinished_secrets.map(|(_, progress, _, _, _, _)| progress.revision),
        subscriber: unfinished_secrets.and_then(|_| subscriber()),
        interfaces: unfinished_network.and_then(|_| interfaces()),
        developer_tools_run: pending
            .filter(|(_, progress, _, repositories, _, _)| {
                *repositories && progress.done & Stage::Repositories.bit() == 0
            })
            .map(|(_, progress, _, _, _, _)| progress.revision),
        developer_tools: None,
        retry_due: unfinished_secrets.and_then(|(root, progress, _, _, _, _)| {
            secrets_retry::retry_due(root, progress.revision, now)
        }),
        mac_settings_retry_due: pending
            .filter(|(_, _, _, _, mac_settings, _)| *mac_settings)
            .and_then(|(root, progress, _, _, _, _)| {
                super::mac_settings_retry_due(
                    root,
                    progress.revision,
                    &aterm_types::version::build_id(),
                    now,
                )
                .ok()
                .flatten()
            }),
        retry_ahead: unfinished_secrets
            .and_then(|(root, progress, _, _, _, _)| {
                secrets_retry::retry_ahead(root, progress.revision, now)
            })
            .into_iter()
            .chain(
                pending
                    .filter(|(_, _, _, _, mac_settings, _)| *mac_settings)
                    .and_then(|(root, progress, _, _, _, _)| {
                        super::mac_settings_retry_ahead(
                            root,
                            progress.revision,
                            &aterm_types::version::build_id(),
                            now,
                        )
                        .ok()
                        .flatten()
                    }),
            )
            .min(),
        in_settings: pending.is_some_and(|(_, _, _, _, _, in_settings)| in_settings),
    }
}

#[cfg(test)]
mod parked_secrets_route_tests {
    use super::*;
    use aterm_secrets::client::{Membership, Status};
    use aterm_secrets::network::InterfaceAddress;
    use std::cell::Cell;
    use std::net::{IpAddr, Ipv4Addr};

    fn plan(setting: Setting) -> Plan {
        let mut selection = Selection::none();
        selection.set(setting, true);
        Plan::from(selection)
    }

    fn routes(last: u8) -> Option<Vec<InterfaceAddress>> {
        Some(vec![InterfaceAddress {
            name: "en0".into(),
            address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, last)),
            netmask: Some(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 0))),
            flags: 1,
        }])
    }

    fn vault(membership: Membership) -> Option<Status> {
        Some(Status {
            config: None,
            machine: None,
            membership,
            names: Vec::new(),
        })
    }

    #[test]
    fn a_workstation_retry_deadline_wakes_after_the_worker_has_parked() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = plan(Setting::Dock);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut doctor = super::super::tests::NeverVerifies::dock();
        let started = 1_800_000_000;
        let read = |after| {
            initial_setup_park_mark_with(Some(root.path()), None, started + after, || {
                panic!("a Dock-only run must not inspect network interfaces")
            })
        };
        // The actual worker permits ten consecutive retry passes, with nine
        // minute-long backoffs. Its last pass is before this item's second try.
        for minute in 0..10 {
            let report = doctor
                .pass_at(root.path(), plan.selection, started + minute * 60)
                .unwrap();
            assert_eq!(SetupNext::of(&report.rows), SetupNext::Retry);
            assert_eq!(doctor.relaunches(), 1, "minute {minute}: still held");
        }
        let parked = read(540);
        assert_eq!(parked.mac_settings_retry_due(), None);
        assert_eq!(parked, read(599), "no early retry or changed park fact");
        assert_ne!(
            parked,
            read(600),
            "the second allowed doctor attempt must wake a parked worker"
        );
        assert_eq!(read(600).mac_settings_retry_due(), Some(started + 600));
        assert!(!read(600).facts_changed_since(&parked));
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        doctor
            .pass_at(root.path(), plan.selection, started + 600)
            .unwrap();
        assert_eq!(doctor.relaunches(), 2);
        let spent = read(600);
        assert_eq!(spent.mac_settings_retry_due(), None);
        assert_eq!(spent, read(1_200), "no third attempt is armed");
        doctor
            .pass_at(root.path(), plan.selection, started + 1_200)
            .unwrap();
        assert_eq!(doctor.relaunches(), 2, "the per-build cap stays spent");
    }

    /// A park waits on what a person does in System Settings or macOS only while an admitted
    /// Accounts or Permissions stage is left: only such a park ends when one of aterm's windows
    /// gains focus. A Mac settings stage, or one of those two finished, does not listen.
    #[test]
    fn only_a_left_accounts_or_permissions_stage_waits_on_settings() {
        for (setting, waits) in [
            (Setting::Permissions, true),
            (Setting::Messages, true),
            (Setting::Dock, false),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &plan(setting)).unwrap().unwrap();
            let read = || {
                initial_setup_park_mark_with(Some(root.path()), None, 1_000, || None)
                    .waits_on_settings()
            };
            assert_eq!(read(), waits, "{setting:?}");
            let pending = progress(root.path()).unwrap().unwrap();
            let stage = match setting {
                Setting::Permissions => Stage::Permissions,
                Setting::Messages => Stage::Accounts,
                _ => Stage::Workstation,
            };
            save_progress(
                root.path(),
                Progress {
                    done: stage.bit(),
                    ..pending
                },
            )
            .unwrap();
            assert!(!read(), "{setting:?}: finished");
        }
        assert!(!initial_setup_park_mark_with(None, None, 1_000, || None).waits_on_settings());
    }

    #[test]
    fn a_mac_settings_deadline_requires_a_selected_unfinished_admitted_stage() {
        for setting in [Setting::Dock, Setting::Messages] {
            let root = aterm_tempfile::tempdir().unwrap();
            let selected = plan(setting);
            authorize_at(root.path(), &selected).unwrap().unwrap();
            let mut doctor = if setting == Setting::Dock {
                super::super::tests::NeverVerifies::dock()
            } else {
                super::super::tests::NeverVerifies::accounts()
            };
            doctor
                .pass_at(root.path(), selected.selection, 1_000)
                .unwrap();
            let read = || {
                initial_setup_park_mark_with(Some(root.path()), None, 1_600, || {
                    panic!("no selected Secrets stage")
                })
                .mac_settings_retry_due()
            };
            assert_eq!(read(), Some(1_600), "{setting:?}");
            let pending = progress(root.path()).unwrap().unwrap();
            let stage = if setting == Setting::Dock {
                Stage::Workstation
            } else {
                Stage::Accounts
            };
            for blocked in [
                Progress {
                    done: stage.bit(),
                    ..pending
                },
                Progress {
                    completed: true,
                    ..pending
                },
                Progress {
                    stopped: true,
                    ..pending
                },
                Progress {
                    admitted: false,
                    ..pending
                },
                Progress {
                    revision: pending.revision + 1,
                    ..pending
                },
            ] {
                save_progress(root.path(), blocked).unwrap();
                assert_eq!(read(), None, "{setting:?}: {blocked:?}");
            }
            if setting == Setting::Dock {
                for only in [Selection::none(), plan(Setting::Widgets).selection] {
                    save_progress(
                        root.path(),
                        Progress {
                            only: Some(only),
                            ..pending
                        },
                    )
                    .unwrap();
                    assert_eq!(read(), None, "the choice left this scope alone");
                }
            }
            save_progress(root.path(), pending).unwrap();
            save_plan(root.path(), &plan(Setting::Permissions)).unwrap();
            assert_eq!(read(), None, "mismatched plan");
            save_progress(
                root.path(),
                Progress {
                    plan: plan_digest(&plan(Setting::Permissions)),
                    ..pending
                },
            )
            .unwrap();
            assert_eq!(read(), None, "no selected Mac settings stage");
            save_plan(root.path(), &selected).unwrap();
            save_progress(root.path(), pending).unwrap();
            request_stop_at(root.path(), pending.revision).unwrap();
            assert_eq!(read(), None, "a requested Stop holds before the fold");
        }
    }

    #[test]
    fn pending_secrets_wakes_for_routes_while_membership_stays_unconfigured() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &plan(Setting::Secrets)).unwrap();
        let read = |network| {
            initial_setup_park_mark_with(
                Some(root.path()),
                vault(Membership::Unconfigured),
                1_000,
                || routes(network),
            )
        };
        let before = read(1);
        assert_eq!(before, read(1), "an unchanged network stays parked");
        assert_ne!(before, read(2), "a new route wakes the unfinished vault");
        let admitted = progress(root.path()).unwrap().unwrap();
        assert!(admitted.admitted && !admitted.completed && admitted.done == 0);
    }

    #[test]
    fn routes_are_not_sampled_without_a_coherent_pending_network_stage() {
        for state in [
            "missing",
            "permissions",
            "staged",
            "done",
            "completed",
            "stopped",
            "cancelled",
            "restored",
            "unreadable",
            "mismatched",
            "missing decision",
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            if state != "missing" {
                let selected = plan(if state == "permissions" {
                    Setting::Permissions
                } else {
                    Setting::Secrets
                });
                authorize_at(root.path(), &selected).unwrap();
                let mut saved = progress(root.path()).unwrap().unwrap();
                match state {
                    "staged" => saved.admitted = false,
                    "done" => saved.done |= Stage::Secrets.bit(),
                    "completed" => saved.completed = true,
                    "stopped" => saved.stopped = true,
                    "mismatched" => saved.plan = [0; 32],
                    _ => {}
                }
                save_progress(root.path(), saved).unwrap();
                match state {
                    "cancelled" | "restored" => write_json(
                        &root.path().join("first-run.json"),
                        &json!({"profile":PROFILE,"state":state}),
                    )
                    .unwrap(),
                    "missing decision" => {
                        std::fs::remove_file(root.path().join("first-run.json")).unwrap();
                    }
                    "unreadable" => {
                        std::fs::write(root.path().join("initial-setup.json"), b"not json")
                            .unwrap();
                    }
                    _ => {}
                }
            }
            let calls = Cell::new(0);
            let read = |network| {
                initial_setup_park_mark_with(
                    Some(root.path()),
                    vault(Membership::Unconfigured),
                    1_000,
                    || {
                        calls.set(calls.get() + 1);
                        routes(network)
                    },
                )
            };
            assert_eq!(read(1), read(2), "{state} must stay parked");
            assert_eq!(calls.get(), 0, "{state} sampled routes");
        }
        let calls = Cell::new(0);
        initial_setup_park_mark_with(None, vault(Membership::Unconfigured), 1_000, || {
            calls.set(calls.get() + 1);
            routes(1)
        });
        assert_eq!(calls.get(), 0, "an unknown root sampled routes");
        let missing = aterm_tempfile::tempdir().unwrap().path().join("absent");
        initial_setup_park_mark_with(
            Some(&missing),
            vault(Membership::Unconfigured),
            1_000,
            || panic!("no admitted run"),
        );
        assert!(!missing.exists(), "observing must not create a state root");
    }

    #[test]
    fn membership_changes_still_wake_without_a_selected_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let read = |membership| {
            initial_setup_park_mark_with(Some(root.path()), vault(membership), 1_000, || {
                panic!("no plan")
            })
        };
        let before = read(Membership::Pending);
        let after = read(Membership::In { seq: 1, heard: 10 });
        assert_ne!(before, after);
        assert_eq!(after, read(Membership::In { seq: 1, heard: 20 }));
        assert_ne!(after, read(Membership::In { seq: 2, heard: 20 }));
        assert_ne!(
            read(Membership::Revoked { seq: 2 }),
            read(Membership::Revoked { seq: 3 })
        );
    }

    /// The Command Line Tools arriving during a pass is a fact that wakes the run's park on
    /// its Repositories stage (the tab the window opened for their install, or a person's own
    /// `aterm init`), whatever else the mark reads, and a park already sampling sees them
    /// change the mark. Their going away, a reading taken for no unfinished Repositories stage,
    /// and a finished stage are no such fact.
    #[test]
    fn the_command_line_tools_arriving_wakes_a_run_parked_on_its_repositories() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut selected = plan(Setting::Secrets);
        selected.selection.set(Setting::Repositories, true);
        authorize_at(root.path(), &selected).unwrap();
        let read = |installed: Option<bool>| {
            initial_setup_park_mark_reading(
                Some(root.path()),
                vault(Membership::Pending),
                1_000,
                || routes(1),
                || None,
                || Some(false),
            )
            .with_developer_tools(|| installed)
        };
        let missing = read(Some(false));
        assert!(!read(Some(false)).facts_changed_since(&missing));
        assert!(
            read(Some(true)).facts_changed_since(&missing),
            "the tools' install ends the wait"
        );
        // A park already sampling ends on any later sample that differs from its first.
        assert_ne!(
            read(Some(true)),
            missing,
            "the tools' install changes the sample"
        );
        assert!(
            !missing.facts_changed_since(&read(Some(true))),
            "the tools going away is no fact"
        );
        assert!(!read(None).facts_changed_since(&missing));
        assert!(!read(Some(true)).facts_changed_since(&read(None)));

        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.done |= Stage::Secrets.bit() | Stage::Repositories.bit();
        save_progress(root.path(), saved).unwrap();
        let finished = read(Some(true));
        assert_eq!(
            finished.developer_tools, None,
            "read for no unfinished stage"
        );
        assert!(!finished.facts_changed_since(&missing));

        // A plan without the repositories never reads the tools.
        let other = aterm_tempfile::tempdir().unwrap();
        authorize_at(other.path(), &plan(Setting::Secrets)).unwrap();
        let unselected = |installed: Option<bool>| {
            initial_setup_park_mark_reading(
                Some(other.path()),
                vault(Membership::Pending),
                1_000,
                || routes(1),
                || None,
                || Some(false),
            )
            .with_developer_tools(|| installed)
        };
        assert_eq!(unselected(Some(true)).developer_tools, None);
        assert!(!unselected(Some(true)).facts_changed_since(&unselected(Some(false))));
    }

    #[test]
    fn late_pass_facts_compare_only_known_inputs_still_owed_by_the_same_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut selected = plan(Setting::Secrets);
        selected.selection.set(Setting::Repositories, true);
        authorize_at(root.path(), &selected).unwrap();
        let read = |network: Option<u8>, subscriber, vault| {
            initial_setup_park_mark_reading(
                Some(root.path()),
                vault,
                1_000,
                || network.and_then(routes),
                || None,
                || subscriber,
            )
        };
        let before = read(Some(1), Some(false), vault(Membership::Pending));
        assert!(
            !read(Some(1), Some(false), vault(Membership::Pending)).facts_changed_since(&before)
        );
        assert!(
            read(Some(2), Some(false), vault(Membership::Pending)).facts_changed_since(&before)
        );
        assert!(read(Some(1), Some(true), vault(Membership::Pending)).facts_changed_since(&before));
        assert!(
            read(
                Some(1),
                Some(false),
                vault(Membership::In { seq: 1, heard: 10 })
            )
            .facts_changed_since(&before)
        );
        let unknown = read(None, None, None);
        assert!(!unknown.facts_changed_since(&before));
        assert!(before.facts_changed_since(&unknown), "newly known inputs");

        // Secrets completed within the pass. Retiring its subscriber input
        // supplies no new fact while Repositories still uses this network.
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.done |= Stage::Secrets.bit();
        save_progress(root.path(), saved).unwrap();
        let after_secrets = read(Some(1), Some(true), vault(Membership::Pending));
        assert!(!after_secrets.facts_changed_since(&before));

        saved.done |= Stage::Repositories.bit();
        saved.completed = true;
        save_progress(root.path(), saved).unwrap();
        assert!(
            !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
                .facts_changed_since(&before),
            "completion retires this run's inputs"
        );
        let next = restart_at(root.path(), &selected).unwrap();
        assert!(
            !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
                .facts_changed_since(&before),
            "a newer run cannot reuse this run's comparison"
        );
        request_stop_at(root.path(), next).unwrap();
        assert!(
            !read(Some(2), Some(true), vault(Membership::Revoked { seq: 2 }))
                .facts_changed_since(&before),
            "Stop retires this run's inputs"
        );
    }

    #[test]
    fn pending_secrets_wakes_at_its_existing_retry_deadline_but_waiting_and_held_do_not() {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &plan(Setting::Secrets))
            .unwrap()
            .unwrap();
        let read = |now| {
            initial_setup_park_mark_with(
                Some(root.path()),
                vault(Membership::Unconfigured),
                now,
                || routes(1),
            )
        };
        let pace = |now| Pace {
            build: "test-build".into(),
            marks: Some("same-network".into()),
            inputs: None,
            materialization: None,
            now,
        };
        let rows = |status: &str| {
            vec![Row {
                name: Stage::Secrets.name().into(),
                status: status.into(),
                detail: "injected vault result".into(),
                elapsed_ms: None,
            }]
        };
        assert_eq!(read(1_000).retry_due(), None, "no previous attempt");
        secrets_retry::record(
            root.path(),
            revision,
            &pace(1_000),
            false,
            &mut rows("blocked"),
        )
        .unwrap();
        let paused = read(1_059);
        assert_eq!(paused.retry_due(), None);
        let due = read(1_060);
        assert_ne!(paused, due, "the same route becomes retryable when due");
        assert_eq!(due.retry_due(), Some(1_060));
        assert!(
            !due.facts_changed_since(&paused),
            "a deadline has its own consumed token, not a changed local input"
        );
        assert_eq!(
            read(1_061).retry_due(),
            Some(1_060),
            "one token until an actual try advances it"
        );

        secrets_retry::record(
            root.path(),
            revision,
            &pace(1_060),
            false,
            &mut rows(WAITING),
        )
        .unwrap();
        assert!(
            !read(2_000).facts_changed_since(&due),
            "consuming the deadline is not a local input change"
        );
        assert_eq!(
            read(2_000).retry_due(),
            None,
            "a person or approval has no timer"
        );
        secrets_retry::record(
            root.path(),
            revision,
            &pace(2_000),
            false,
            &mut rows("error"),
        )
        .unwrap();
        secrets_retry::record(
            root.path(),
            revision,
            &pace(3_000),
            false,
            &mut rows("error"),
        )
        .unwrap();
        assert_eq!(
            read(4_000).retry_due(),
            None,
            "two failed changes stay held"
        );
    }
}

struct LiveInitial<R> {
    reads: R,
    /// Whether the last Repositories stage stopped for want of the Command Line Tools
    /// ([`InitialHost::repositories_want_developer_tools`]).
    developer_tools_wanted: bool,
    /// The reason the last Repositories stage's failed part gives
    /// ([`InitialHost::repositories_failure`]).
    repositories_failure: Option<&'static str>,
}

impl LiveInitial<secrets_retry::Live> {
    const fn live() -> Self {
        Self {
            reads: secrets_retry::Live,
            developer_tools_wanted: false,
            repositories_failure: None,
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

    fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String> {
        apply_accounts_at(root, on_screen)
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        aterm_machines::init::run_initial_spotlight(
            &sys,
            &self.context()?,
            aterm_machines::model::Mode::Apply,
        )
        .map(item_rows)
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
        let sys = aterm_machines::port::Live::new();
        let ctx = self.context()?;
        // A join's first request to the LAN can raise macOS's Local Network alert: only one
        // macOS dialog this process started is up at a time (System State design §6).
        let Some(_dialog) = aterm_types::macos_setup::MacosDialog::when_none_up(
            aterm_types::macos_setup::DIALOG_WAIT,
        ) else {
            return Ok(vec![readiness_row(
                Stage::Secrets.name(),
                false,
                "another macOS dialog is still up; the join waits for it".into(),
            )]);
        };
        // The ALab vault's own address, not joined yet: the probe's answer decides whether the
        // join goes ahead ([`unjoined_alab_row`]).
        let mut unanswered = false;
        if plan.vault_url == aterm_types::macos_setup::DEFAULT_VAULT_URL
            && !in_plan_vault(plan, live_configured_vault())
        {
            let here = scope::probe_vault();
            if let Some(row) =
                unjoined_alab_row(here, access::last_answer(), &UNANSWERED_PROBE_JOINED)
            {
                return Ok(vec![row]);
            }
            unanswered = here == scope::VaultHere::Unreached;
        }
        let ops = aterm_machines::init_secrets::unattended(&ctx)?;
        aterm_machines::init::run_initial_secrets(
            &sys,
            &ctx,
            ops,
            &plan.vault_url,
            replaces,
            password,
            APPROVAL_WAIT,
        )
        .map(|reports| {
            if !unanswered {
                return secrets_rows(reports);
            }
            unanswered_join(reports, access::lan_answered)
        })
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
        self.developer_tools_wanted = false;
        self.repositories_failure = None;
        let (rows, wanted, failure) = repositories::run_reading(
            &sys,
            &self.context()?,
            vault,
            aterm_machines::model::Mode::Apply,
        )?;
        self.developer_tools_wanted = wanted;
        self.repositories_failure = failure;
        Ok(rows)
    }

    fn repositories_want_developer_tools(&mut self) -> bool {
        self.developer_tools_wanted
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

const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

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

/// A row with nothing left to do: in place, set, or back as saved and loading at the next login
/// ([`super::LOGIN`]), which no pass can hurry.
fn row_ready(row: &Row) -> bool {
    matches!(row.status.as_str(), "ok" | "applied" | super::LOGIN)
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
fn secrets_page(root: &Path, rows: &[Row]) -> PassPage {
    if rows
        .iter()
        .any(|row| row.detail.starts_with(VAULT_CONFIG_UNREAD))
    {
        return PassPage::default();
    }
    secrets_retry::page(root, rows)
}

fn secrets_try(
    host: &mut dyn InitialHost,
    plan: &Plan,
    admitted: AdmittedVault,
) -> Result<SecretsTry, String> {
    if admitted == AdmittedVault::Unknown {
        return Ok(SecretsTry::RunItem(Replaces::Any));
    }
    let configured = host
        .configured_vault()
        .map_err(|error| format!("{VAULT_CONFIG_UNREAD}: {error}"))?;
    Ok(match configured {
        Some(vault) if admitted.stands_in(plan, &vault) => SecretsTry::LeaveInPlace(vault),
        configured => SecretsTry::RunItem(Replaces::Read(configured.map(|vault| vault.config))),
    })
}

#[cfg(test)]
fn execute_initial(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
) -> Result<Option<Report>, String> {
    Ok(execute_pass(host, root, password, SetupHooks::NONE)?.map(|(report, _)| report))
}

/// One pass over the selected stages that are not done, in order, asking `hooks` and the durable
/// stop request before each and at the end whether a person stopped this revision
/// ([`stop_at_boundary`]). A Stop ends the revision without cancelling its plan.
fn execute_pass(
    host: &mut dyn InitialHost,
    root: &Path,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
) -> Result<Option<(Report, SetupNext)>, String> {
    fold_stop_request(root)?;
    if finished_at(root)? {
        return Ok(None);
    }
    let Some(plan) = saved_plan(root)? else {
        return Ok(None);
    };
    let Some(mut progress) = progress(root)? else {
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
    // The rows of `report` a stage's own page stands for (`Report::page`).
    let mut pages: Vec<(std::ops::Range<usize>, PassPage)> = Vec::new();
    // Each stage's rows its producer did not time carry the stage's time.
    let mut clock = RowClock::start();
    // Whether this pass's Repositories stage, if it ran, waits on the Command Line Tools: the
    // window hears an ask once the pass is over, a withdrawal at once
    // ([`SetupHooks::developer_tools`]).
    let mut developer_tools = None;
    for stage in Stage::ALL {
        clock.lap(&mut report.rows);
        if !stage.selected(&plan) || progress.done & stage.bit() != 0 {
            continue;
        }
        if stop_at_boundary(root, &mut progress, hooks)? {
            report.rows.push(Row {
                name: "System State".into(),
                status: "stopped".into(),
                detail: "stopped before this step; nothing more runs until the plan changes".into(),
                elapsed_ms: None,
            });
            clock.lap(&mut report.rows);
            if developer_tools == Some(true) {
                (hooks.developer_tools)(progress.revision, false);
            }
            return Ok(Some((paged(report, pages), SetupNext::Done)));
        }
        let mut repository_vault = None;
        if stage == Stage::Repositories && Stage::Secrets.selected(&plan) {
            if progress.done & Stage::Secrets.bit() != 0 {
                let current = secrets_try(host, &plan, progress.admitted_vault).and_then(|read| {
                    let selected = match read {
                        SecretsTry::LeaveInPlace(vault) => vault.present(plan.clone()),
                        SecretsTry::RunItem(_) => plan.clone(),
                    };
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
                    pages.push((at..at + rows.len(), secrets_page(root, &rows)));
                    report.rows.extend(rows);
                }
            }
            if progress.done & Stage::Secrets.bit() == 0 {
                let rows = vec![Row {
                    name: stage.name().into(),
                    status: if vault == SetupNext::Park {
                        WAITING
                    } else {
                        "blocked"
                    }
                    .into(),
                    detail: "waiting for the selected secrets vault to synchronize credentials"
                        .into(),
                    elapsed_ms: None,
                }];
                next = next.and(SetupNext::of(&rows));
                report.rows.extend(rows);
                continue;
            }
        }
        let paced = if stage == Stage::Secrets {
            host.secrets_pace()
        } else {
            None
        };
        if let Some(rows) = paced
            .as_ref()
            .and_then(|pace| secrets_retry::paused(root, progress.revision, pace))
        {
            vault = SetupNext::of(&rows);
            next = next.and(vault);
            let at = report.rows.len();
            pages.push((at..at + rows.len(), secrets_page(root, &rows)));
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
            })
            .collect();
        progress.running = Some(stage);
        save_progress(root, progress)?;
        (hooks.stage)(StageEvent {
            settings: &settings,
            subjects: &subjects,
            running: true,
        });
        // The page's words for the stage's rows, where its report has them.
        let mut page = None;
        let result = match stage {
            Stage::Workstation => match host.workstation(root, progress.only) {
                Ok(Some(result)) => {
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
            Stage::Spotlight => host.spotlight(),
            Stage::Permissions => host.permissions(&plan, progress.access_revision()),
            Stage::Secrets => match secrets_try(host, &plan, progress.admitted_vault) {
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
                Ok(SecretsTry::RunItem(replaces)) => host.secrets(&plan, replaces, password),
                Err(detail) => Ok(vec![Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail,
                    elapsed_ms: None,
                }]),
            },
            Stage::Repositories => host.repositories(repository_vault.as_ref()),
            Stage::Accounts => match host.accounts(root, (hooks.on_screen)()) {
                Ok(Some(result)) => {
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
            // A failed part's reason, where its step decided one, is the row's words on the page.
            if let Some(reason) = host.repositories_failure()
                && let Some(page) = page.as_mut()
            {
                page.failed_because(Setting::Repositories, reason);
            }
            let wanted = host.repositories_want_developer_tools();
            // A stage that no longer waits on the tools withdraws at once, so a pass that fails
            // after it leaves no want standing; an ask waits for the pass's end.
            if !wanted {
                (hooks.developer_tools)(progress.revision, false);
            }
            developer_tools = Some(wanted);
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
        }
        if stage == Stage::Secrets {
            vault = left;
        }
        next = next.and(left);
        progress.running = None;
        save_progress(root, progress)?;
        if let Some(before) = paced.as_ref()
            && let Some(mut after) = host.secrets_pace()
        {
            after.retain_known_from(before);
            secrets_retry::record(root, progress.revision, &after, ready, &mut rows)?;
        }
        if stage == Stage::Secrets {
            // The vault's rows carry the retry pause's words, which the page says its own way.
            page = Some(secrets_page(root, &rows));
        }
        let at = report.rows.len();
        report.rows.extend(rows);
        if let Some(page) = page {
            pages.push((at..report.rows.len(), page));
        }
        (hooks.stage)(StageEvent {
            settings: &settings,
            subjects: &subjects,
            running: false,
        });
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
        if developer_tools == Some(true) {
            (hooks.developer_tools)(progress.revision, false);
        }
        return Ok(Some((paged(report, pages), SetupNext::Done)));
    }
    // The follow-up changes its rows as a stage does: fenced from readings the same way.
    let owed = host.withheld_owed(root);
    let owed_subjects: Vec<Subject> = owed.iter().copied().map(Subject::Setting).collect();
    let fence = |running| {
        if !owed.is_empty() {
            (hooks.stage)(StageEvent {
                settings: &owed,
                subjects: &owed_subjects,
                running,
            });
        }
    };
    fence(true);
    let followed = host.withheld(root);
    fence(false);
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
        progress.completed = true;
        save_progress(root, progress)?;
        next = SetupNext::Done;
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
    // Only now, with nothing of this pass left to run (no sign-out to drive Messages over the
    // password prompt), and never for a stopped run.
    if developer_tools == Some(true) {
        (hooks.developer_tools)(progress.revision, !stopped);
    }
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

fn execute_initial_for_revision(
    host: &mut dyn InitialHost,
    root: &Path,
    expected_revision: u64,
    password: &mut Option<SecretBytes>,
    hooks: SetupHooks,
) -> Result<Option<(Report, SetupNext)>, String> {
    if revision_at(root)? != expected_revision {
        return Ok(None);
    }
    execute_pass(host, root, password, hooks)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod deferred_mac_deadline_tests;
    mod embedded_restore_tests;

    type StageReading = (bool, Vec<Setting>, Vec<Subject>);

    #[derive(Default)]
    struct Fake {
        calls: Vec<Stage>,
        pending: Option<Stage>,
        /// A stage whose row waits on a person ([`WAITING`]).
        waiting: Option<Stage>,
        failure: Option<Stage>,
        password_seen: bool,
        observe: Option<PathBuf>,
        vault_checks: usize,
        vault_blocked: bool,
        vault_error: bool,
        /// A sign-out still has work, so the Accounts stage needs System State on screen.
        accounts_pending: bool,
        /// The access revision the last Permissions stage read.
        access_read: Option<u64>,
        /// While this stage runs, a person stops the run (System State's Stop)
        /// and the process ends: [`Self::relaunch`] is what the next launch reads.
        stop_during: Option<(Stage, PathBuf)>,
        relaunch: Option<Relaunch>,
        vault_url: Option<String>,
        pace: Option<Pace>,
        configured: Option<ConfiguredVault>,
        configured_unreadable: bool,
        checked_url: Option<String>,
        native_repositories: Option<(aterm_machines::port::Fake, aterm_machines::items::Ctx)>,
        /// The live Accounts stage adapter over an inert account machine.
        native_accounts: Option<(aterm_machines::port::Fake, NativeHost)>,
        native_accounts_now: Option<std::time::SystemTime>,
        /// Whether the last native Repositories stage stopped for want of the Command Line
        /// Tools, read as the live host reads it ([`repositories::want_developer_tools`]).
        developer_tools_wanted: bool,
        /// The reason its failed part gave ([`repositories::failure_note`]).
        repositories_failure: Option<&'static str>,
        /// What each Workstation stage was given to set ([`Progress::only`]).
        only: Vec<Option<Selection>>,
    }

    /// What a launch reads of a run whose process ended mid-stage.
    #[derive(Debug)]
    struct Relaunch {
        finished: bool,
        admission: Option<InitialSetupAdmission>,
        snapshot_finished: bool,
    }

    impl Fake {
        fn stage(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
            self.calls.push(stage);
            if let Some((during, root)) = &self.stop_during
                && *during == stage
            {
                let revision = progress(root).unwrap().unwrap().revision;
                request_stop_at(root, revision).unwrap();
                self.relaunch = Some(Relaunch {
                    finished: finished_at(root).unwrap(),
                    admission: admission_at(root).unwrap(),
                    snapshot_finished: status_at(root, Plan::default()).unwrap().1,
                });
            }
            if let Some(root) = &self.observe {
                let (_, finished, rows) = status_at(root, Plan::default()).unwrap();
                assert!(!finished);
                assert!(
                    rows.iter()
                        .any(|row| row.name == stage.name() && row.status == "running")
                );
            }
            if self.failure == Some(stage) {
                return Err("offline; retry later".into());
            }
            if self.waiting == Some(stage) {
                return Ok(vec![Row {
                    name: stage.name().into(),
                    status: WAITING.into(),
                    detail: "injected wait for a person".into(),
                    elapsed_ms: None,
                }]);
            }
            Ok(vec![readiness_row(
                stage.name(),
                self.pending != Some(stage),
                "injected stage outcome".into(),
            )])
        }
    }

    impl InitialHost for Fake {
        fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.workstation(root, None)
        }
        fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String> {
            if let Some((sys, host)) = &mut self.native_accounts {
                self.calls.push(Stage::Accounts);
                return apply_part_with(
                    root,
                    sys,
                    host,
                    "inert-init-completion",
                    self.native_accounts_now.unwrap_or(
                        std::time::SystemTime::UNIX_EPOCH
                            + std::time::Duration::from_secs(1_800_000_000),
                    ),
                    Part::Accounts { on_screen },
                );
            }
            if !on_screen && self.accounts_pending {
                return Ok(Some(Report {
                    action: Action::Apply,
                    rows: vec![Row {
                        name: Stage::Accounts.name().into(),
                        status: WAITING.into(),
                        detail: "injected: waits for System State on screen".into(),
                        elapsed_ms: None,
                    }],
                    manual_steps: Vec::new(),
                    backup: root.join("original-preferences.json"),
                    restore: None,
                    page: None,
                }));
            }
            // As the real sign-out pass marks and records its pass ([`record_pending`],
            // [`record_applied`]).
            record_pending(root)?;
            let rows = self.stage(Stage::Accounts)?;
            if rows_ready(&rows) {
                let planned = authorized_selection(root)?.unwrap_or_default();
                record_applied(root, accounts(planned))?;
            }
            Ok(Some(Report {
                action: Action::Apply,
                rows,
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
                restore: None,
                page: None,
            }))
        }
        fn workstation(
            &mut self,
            root: &Path,
            only: Option<Selection>,
        ) -> Result<Option<Report>, String> {
            if first_run_complete(root)? {
                return Ok(None);
            }
            self.only.push(only);
            // As the real pass marks its pass pending before it sets anything, and records
            // it: over the plan's preferences, or `only` those a choice narrowed it to.
            record_pending(root)?;
            let rows = self.stage(Stage::Workstation)?;
            if rows_ready(&rows) {
                let mut set = preferences(authorized_selection(root)?.unwrap_or_default());
                if let Some(only) = only {
                    for setting in Setting::ALL {
                        set.set(setting, set.contains(setting) && only.contains(setting));
                    }
                }
                record_applied(root, set)?;
            }
            Ok(Some(Report {
                action: Action::Apply,
                rows,
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
                restore: None,
                page: None,
            }))
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Spotlight)
        }
        fn permissions(&mut self, _: &Plan, access: u64) -> Result<Vec<Row>, String> {
            self.access_read = Some(access);
            self.stage(Stage::Permissions)
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            _replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.password_seen = password.is_some();
            self.vault_url = Some(plan.vault_url.clone());
            self.stage(Stage::Secrets)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            self.pace.clone()
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            if self.configured_unreadable {
                return Err("source.toml: no name".into());
            }
            Ok(self.configured.clone())
        }
        fn repositories(&mut self, _vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            if let Some((sys, ctx)) = &self.native_repositories {
                self.calls.push(Stage::Repositories);
                let rows = aterm_machines::init_github::run_initial_repositories(
                    sys,
                    ctx,
                    aterm_machines::model::Mode::Apply,
                )?;
                self.developer_tools_wanted = repositories::want_developer_tools(&rows);
                self.repositories_failure = repositories::failure_note(&rows);
                return Ok(item_rows(rows));
            }
            self.stage(Stage::Repositories)
        }

        fn repositories_want_developer_tools(&mut self) -> bool {
            self.developer_tools_wanted
        }

        fn repositories_failure(&mut self) -> Option<&'static str> {
            self.repositories_failure
        }

        fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
            Ok(None)
        }

        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            self.vault_checks += 1;
            self.checked_url = Some(plan.vault_url.clone());
            if self.vault_error {
                return Err("injected local vault readiness failure".into());
            }
            Ok(vec![readiness_row(
                Stage::Secrets.name(),
                !self.vault_blocked,
                "injected current vault readiness".into(),
            )])
        }
    }

    fn selected(settings: &[Setting]) -> Plan {
        let mut selection = Selection::none();
        for setting in settings {
            selection.set(*setting, true);
        }
        Plan::from(selection)
    }

    /// [`selected`] `settings` and every setting of `groups`.
    fn plan_of(settings: &[Setting], groups: &[aterm_types::macos_setup::Group]) -> Plan {
        let mut plan = selected(settings);
        for group in groups {
            for setting in group.settings() {
                plan.selection.set(*setting, true);
            }
        }
        plan
    }

    /// Bytes a setup record cannot be read from.
    const CORRUPT: &[u8] = b"{\"profile\":";

    #[test]
    fn an_old_startup_offer_cannot_replace_a_newer_pending_doctor_plan() {
        let root = aterm_tempfile::tempdir().unwrap();
        let offered = Plan::default();
        let baseline = observation_at(root.path()).unwrap();
        let mut authorization = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 0);
        std::fs::write(
            root.path().join("original-preferences.json"),
            b"invalid backup",
        )
        .unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());
        save_explicit_workstation_plan(root.path()).unwrap();
        let doctor_plan = saved_plan(root.path()).unwrap().unwrap();
        // The Restore withdrew no permission: the explicit plan keeps them.
        let mut explicit = Selection::all().workstation();
        explicit.set(Setting::Permissions, true);
        assert_eq!(doctor_plan.selection, explicit);
        assert_eq!(revision_at(root.path()).unwrap(), 1);
        assert!(!finished_at(root.path()).unwrap());
        let before = observation_at(root.path()).unwrap();
        assert_ne!(before, baseline);

        let accepted = authorization
            .persist_at(root.path(), &offered)
            .result
            .unwrap();
        let mut old_worker = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        if let Some(revision) = accepted {
            execute_initial_for_revision(
                &mut old_worker,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
        }
        assert!(
            old_worker.calls.is_empty(),
            "a stale offer ran stages from its older plan"
        );
        assert_eq!(accepted, None);
        assert_eq!(revision_at(root.path()).unwrap(), 1);
        assert_eq!(saved_plan(root.path()).unwrap(), Some(doctor_plan));
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }

    fn restart_attempt(
        attempt: &mut InitialSetupRestart,
        root: &Path,
        plan: &Plan,
        commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                attempt.persist_with(root, plan, true, || Ok(None), commit, observe)
            },
        )
    }

    fn observe_restart(root: &Path) -> Result<InitialSetupObservation, String> {
        observation_at(root).map_err(|error| error.to_string())
    }

    fn system_state_apply(
        root: &Path,
        plan: &Plan,
        vault_typed: bool,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    ) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::default().persist_with(
                root,
                plan,
                vault_typed,
                read,
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("admitted")
    }

    /// System State's group choice of `plan`, which sends the vault the worker holds, in a
    /// scratch home that is in no vault: the admitted run's revision.
    fn system_state_choice(root: &Path, plan: &Plan) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                plan,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("admitted")
    }

    /// System State's Join of `plan`, its vault typed, in a scratch home that is in no
    /// vault: the admitted run's revision.
    fn system_state_join(root: &Path, plan: &Plan) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                plan,
                true,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("admitted")
    }

    #[test]
    fn restart_final_save_recovers_before_or_after_publication_without_a_new_revision() {
        for empty in [false, true] {
            for published in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let plan = selected(if empty { &[] } else { &[Setting::Secrets] });
                let original = authorize_at(root.path(), &plan).unwrap().unwrap();
                let mut attempt = InitialSetupRestart::default();
                let failed = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        if published {
                            save_progress(root, next)?;
                        }
                        Err("injected final namespace flush failure".into())
                    },
                    observe_restart,
                );
                assert!(failed.result.is_err());
                assert!(attempt.pending());
                let staged = progress(root.path()).unwrap().unwrap();
                assert_eq!(staged.revision, original + 1);
                assert_eq!(staged.admitted, published);
                let before = observation_at(root.path()).unwrap();
                let commits = std::cell::Cell::new(0);
                let recovered = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        commits.set(commits.get() + 1);
                        save_progress(root, next)
                    },
                    observe_restart,
                );
                assert_eq!(recovered.result.unwrap(), Some(staged.revision));
                assert!(!attempt.pending());
                assert_eq!(commits.get(), usize::from(!published));
                assert_eq!(revision_at(root.path()).unwrap(), staged.revision);
                assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
                if published {
                    assert_eq!(observation_at(root.path()).unwrap(), before);
                }
                let password = SecretBytes::from(b"test-only-retained-password".as_slice());
                let mut host = Fake::default();
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    staged.revision,
                    &mut Some(password.clone()),
                    SetupHooks::NONE,
                )
                .unwrap();
                assert_eq!(
                    host.calls,
                    if empty {
                        Vec::new()
                    } else {
                        vec![Stage::Secrets]
                    }
                );
                assert_eq!(host.password_seen, !empty);
                assert!(finished_at(root.path()).unwrap());
            }
        }
    }

    fn a_stopped_run_before_its_stop_writer_saves() -> (aterm_tempfile::TempDir, Plan, u64) {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let original = authorize_at(root.path(), &plan).unwrap().unwrap();
        // The running pass sees the window's immediate Stop at its boundary;
        // the independent Stop writer has not published its request yet.
        let writer = HeldLock::take(root.path()).unwrap();
        let mut prior = progress(root.path()).unwrap().unwrap();
        assert!(
            stop_at_boundary(
                root.path(),
                &mut prior,
                SetupHooks {
                    stop: |_| true,
                    ..SetupHooks::NONE
                },
            )
            .unwrap()
        );
        assert_eq!(stop_request_at(root.path()), None);
        drop(writer);
        (root, plan, original)
    }

    fn recover_restart_after_a_delayed_old_stop(published: bool) {
        let (root, plan, original) = a_stopped_run_before_its_stop_writer_saves();
        let mut attempt = InitialSetupRestart::chosen();
        let failed = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |root, next| {
                if published {
                    save_progress(root, next)?;
                }
                // This separate lock is available while the new admission holds
                // profile.lock: the old asynchronous save can arrive this late.
                request_stop_at(root, original)?;
                Err("injected final admission flush failure".into())
            },
            observe_restart,
        );
        assert!(failed.result.is_err());
        assert!(attempt.pending());
        let staged = progress(root.path()).unwrap().unwrap();
        assert_eq!(staged.revision, original + 1);
        assert_eq!(staged.admitted, published);
        assert_eq!(
            stop_request_at(root.path()),
            Some(StopRequest::Revision(original))
        );
        let before = observation_at(root.path()).unwrap();
        let commits = std::cell::Cell::new(0);
        let recovered = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |root, next| {
                commits.set(commits.get() + 1);
                save_progress(root, next)
            },
            observe_restart,
        );
        assert_eq!(
            recovered.result.unwrap(),
            Some(staged.revision),
            "a delayed Stop for the old run must not refuse the new choice"
        );
        assert!(!attempt.pending());
        assert_eq!(commits.get(), usize::from(!published));
        if published {
            assert_eq!(observation_at(root.path()).unwrap(), before);
        }
        let mut host = Fake::default();
        let mut password = Some(SecretBytes::from(b"retained-choice-password".as_slice()));
        execute_initial_for_revision(
            &mut host,
            root.path(),
            staged.revision,
            &mut password,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the confirmed new choice executes");
        assert_eq!(host.calls, [Stage::Secrets]);
        assert!(host.password_seen);
        assert_eq!(revision_at(root.path()).unwrap(), staged.revision);
        assert!(finished_at(root.path()).unwrap());
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                staged.revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(host.calls, [Stage::Secrets], "the stage ran once");
    }

    #[test]
    fn owned_restart_stop_recovers_before_final_publication() {
        recover_restart_after_a_delayed_old_stop(false);
    }

    #[test]
    fn owned_restart_stop_recovers_after_final_publication() {
        recover_restart_after_a_delayed_old_stop(true);
    }

    #[test]
    fn owned_restart_stop_keeps_current_requests_and_new_publications_authoritative() {
        for published in [false, true] {
            for replacement in ["current Stop", "future Stop", "unreadable Stop", "new plan"] {
                let (root, plan, original) = a_stopped_run_before_its_stop_writer_saves();
                let mut attempt = InitialSetupRestart::chosen();
                assert!(
                    restart_attempt(
                        &mut attempt,
                        root.path(),
                        &plan,
                        |root, next| {
                            if published {
                                save_progress(root, next)?;
                            }
                            Err("injected final admission flush failure".into())
                        },
                        observe_restart,
                    )
                    .result
                    .is_err()
                );
                {
                    let _writer = HeldLock::take(root.path()).unwrap();
                    match replacement {
                        "current Stop" => request_stop_at(root.path(), original + 1).unwrap(),
                        "future Stop" => request_stop_at(root.path(), original + 2).unwrap(),
                        "unreadable Stop" => {
                            write_json(&root.path().join(STOP_REQUEST), &json!({"invalid":true}))
                                .unwrap();
                        }
                        _ => {
                            let newer = selected(&[Setting::Dock, Setting::Secrets]);
                            save_plan_as(root.path(), &newer, false).unwrap();
                        }
                    }
                }
                let before = observation_at(root.path()).unwrap();
                let recovered = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |_, _| panic!("superseded admission cannot commit"),
                    observe_restart,
                );
                assert_eq!(recovered.result.unwrap(), None, "{replacement}");
                assert!(!attempt.pending());
                assert_eq!(observation_at(root.path()).unwrap(), before);
                if replacement != "future Stop" {
                    let mut host = Fake::default();
                    let _ = execute_initial_for_revision(
                        &mut host,
                        root.path(),
                        original + 1,
                        &mut None,
                        SetupHooks::NONE,
                    );
                    assert!(host.calls.is_empty(), "{replacement}");
                }
            }
        }
    }

    #[test]
    fn repeated_restart_commit_and_confirmation_errors_retain_the_owned_publications() {
        for published in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            let mut attempt = InitialSetupRestart::default();
            assert!(
                restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        if published {
                            save_progress(root, next)?;
                        }
                        Err("injected final flush failure".into())
                    },
                    observe_restart,
                )
                .result
                .is_err()
            );
            let owned = observation_at(root.path()).unwrap();
            for _ in 0..2 {
                let failed = restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |_, _| Err("injected repeated final flush failure".into()),
                    |root| {
                        if published {
                            Err("injected confirmation directory flush failure".into())
                        } else {
                            observe_restart(root)
                        }
                    },
                );
                assert!(failed.result.is_err());
                assert!(attempt.pending());
                assert_eq!(observation_at(root.path()).unwrap(), owned);
            }
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                save_progress,
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), Some(1));
            assert!(!attempt.pending());
            assert_eq!(revision_at(root.path()).unwrap(), 1);
        }
    }

    #[test]
    fn restart_prefinal_failures_never_confirm_an_older_same_plan_admission() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut prior = progress(root.path()).unwrap().unwrap();
        prior.revision = u64::MAX;
        save_progress(root.path(), prior).unwrap();
        let before = observation_at(root.path()).unwrap();
        let mut attempt = InitialSetupRestart::default();
        let failed = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |_, _| panic!("a preflight failure cannot reach final admission"),
            observe_restart,
        );
        assert!(failed.result.is_err());
        assert!(!attempt.pending());
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert_eq!(
            admission_at(root.path()).unwrap().unwrap().revision,
            u64::MAX
        );
        assert_eq!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                save_progress,
                observe_restart
            )
            .result
            .unwrap(),
            None
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);

        let staged = aterm_tempfile::tempdir().unwrap();
        let mut attempt = InitialSetupRestart::default();
        assert!(
            restart_attempt(
                &mut attempt,
                staged.path(),
                &plan,
                |_, _| panic!("an unconfirmed staged run cannot reach final admission"),
                |_| Err("injected staged observation failure".into()),
            )
            .result
            .is_err()
        );
        assert!(!attempt.pending());
        assert!(admission_at(staged.path()).unwrap().is_none());
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, staged.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
    }

    #[test]
    fn restart_recovery_retires_after_a_stop_new_run_or_same_content_publication() {
        for replacement in 0..5 {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            let mut attempt = InitialSetupRestart::default();
            assert!(
                restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        save_progress(root, next)?;
                        Err("injected published admission failure".into())
                    },
                    observe_restart,
                )
                .result
                .is_err()
            );
            {
                let _writer = HeldLock::take(root.path()).unwrap();
                match replacement {
                    0 | 1 => {
                        write_json(
                            &root.path().join("first-run.json"),
                            &json!({"profile":PROFILE,"state":if replacement == 0 {"cancelled"} else {"restored"}}),
                        ).unwrap();
                        let progress = root.path().join("initial-setup.json");
                        std::fs::remove_file(&progress).unwrap();
                        std::fs::create_dir(progress).unwrap();
                    }
                    2 => {
                        restart_at(root.path(), &plan).unwrap();
                    }
                    3 => {
                        save_progress(root.path(), progress(root.path()).unwrap().unwrap())
                            .unwrap();
                    }
                    _ => {
                        aterm_machines::port::write_atomic(
                            &root.path().join("first-run.json"),
                            b"invalid decision",
                            0o600,
                        )
                        .unwrap();
                    }
                }
            }
            let before = observation_at(root.path()).ok();
            let progress_bytes = std::fs::read(root.path().join("initial-setup.json")).ok();
            let decision_bytes = std::fs::read(root.path().join("first-run.json")).unwrap();
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |_, _| panic!("supersession cannot write the old admission"),
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), None);
            assert!(!attempt.pending());
            assert_eq!(observation_at(root.path()).ok(), before);
            assert_eq!(
                std::fs::read(root.path().join("initial-setup.json")).ok(),
                progress_bytes
            );
            if replacement <= 1 {
                assert!(root.path().join("initial-setup.json").is_dir());
            }
            assert_eq!(
                std::fs::read(root.path().join("first-run.json")).unwrap(),
                decision_bytes
            );
        }
    }

    #[test]
    fn failed_restart_publication_capture_cannot_bind_a_later_readable_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let mut attempt = InitialSetupRestart::default();
        let final_progress = std::cell::Cell::new(None);
        assert!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    final_progress.set(Some(next));
                    let path = root.join("initial-setup.json");
                    std::fs::remove_file(&path).unwrap();
                    std::fs::create_dir(path).unwrap();
                    Err("injected inaccessible publication".into())
                },
                observe_restart,
            )
            .result
            .is_err()
        );
        assert!(!attempt.pending());
        std::fs::remove_dir(root.path().join("initial-setup.json")).unwrap();
        save_progress(root.path(), final_progress.get().unwrap()).unwrap();
        let before = observation_at(root.path()).unwrap();
        let retired = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |_, _| panic!("an unbound failure cannot adopt a later publication"),
            observe_restart,
        );
        assert_eq!(retired.result.unwrap(), None);
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }

    #[test]
    fn admitted_run_observation_preserves_the_source_revision_and_stage_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
        plan.vault_url = "https://vault.example/v1/team".into();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.done = Stage::Workstation.bit();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        let before = observation_at(root.path()).unwrap();

        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan: plan.clone(),
                revision,
                completed: false,
            })
        );
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert_eq!(progress(root.path()).unwrap(), Some(saved));
        assert!(!root.path().join("profile.lock").exists());

        saved.done |= Stage::Secrets.bit() | Stage::Repositories.bit();
        saved.running = None;
        saved.completed = true;
        save_progress(root.path(), saved).unwrap();
        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan,
                revision,
                completed: true,
            })
        );
    }

    #[test]
    fn staged_mismatched_legacy_and_missing_choices_are_not_admitted_runs() {
        let absent = aterm_tempfile::tempdir().unwrap();
        let missing_root = absent.path().join("not-created");
        assert_eq!(admission_at(&missing_root).unwrap(), None);
        assert!(!missing_root.exists());

        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let saved = progress(root.path()).unwrap().unwrap();
        save_progress(
            root.path(),
            Progress {
                admitted: false,
                ..saved
            },
        )
        .unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);

        save_progress(root.path(), saved).unwrap();
        let mut replacement = plan.clone();
        replacement.vault_url = "https://vault.example/v1/other".into();
        save_plan(root.path(), &replacement).unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);

        save_automatic_plan(root.path(), plan.selection).unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);
        save_plan(root.path(), &plan).unwrap();
        for decision in [Decision::Missing, Decision::Applied(false)] {
            let path = root.path().join("first-run.json");
            if decision == Decision::Missing {
                std::fs::remove_file(&path).unwrap();
            } else {
                write_json(
                    &path,
                    &json!({"profile":PROFILE,"state":"applied","accounts_verified":false}),
                )
                .unwrap();
            }
            assert_eq!(admission_at(root.path()).unwrap(), None);
        }
        save_workstation_authorization(root.path(), &plan).unwrap();
        for record in ["automatic-plan.json", "initial-setup.json"] {
            let path = root.path().join(record);
            let bytes = std::fs::read(&path).unwrap();
            std::fs::remove_file(&path).unwrap();
            assert_eq!(admission_at(root.path()).unwrap(), None);
            std::fs::write(path, bytes).unwrap();
        }
        assert!(admission_at(root.path()).unwrap().is_some());
    }

    #[test]
    fn an_empty_admission_published_before_an_error_can_be_confirmed_without_rewriting_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let empty = selected(&[]);
        assert!(
            authorize_with(
                root.path(),
                &empty,
                AdmittedVault::None,
                |root, progress| {
                    save_progress(root, progress)?;
                    Err("injected failure after publishing the final admission".into())
                }
            )
            .is_err()
        );
        let before = observation_at(root.path()).unwrap();
        let admitted = admission_at(root.path()).unwrap().unwrap();
        assert_eq!(admitted.plan, empty);
        assert!(admitted.completed);
        assert_eq!(admitted.revision, revision_at(root.path()).unwrap());
        assert_eq!(observation_at(root.path()).unwrap(), before);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());

        for state in ["cancelled", "restored"] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            assert_eq!(admission_at(root.path()).unwrap(), None);
        }
        let next_plan = selected(&[Setting::Secrets]);
        let next_revision = restart_at(root.path(), &next_plan).unwrap();
        assert_eq!(
            admission_at(root.path()).unwrap(),
            Some(InitialSetupAdmission {
                plan: next_plan,
                revision: next_revision,
                completed: false,
            })
        );
        assert_ne!(next_revision, admitted.revision);

        let unpublished = aterm_tempfile::tempdir().unwrap();
        assert!(
            authorize_with(unpublished.path(), &empty, AdmittedVault::None, |_, _| {
                Err("injected failure before publishing the final admission".into())
            })
            .is_err()
        );
        assert_eq!(admission_at(unpublished.path()).unwrap(), None);
        assert!(!finished_at(unpublished.path()).unwrap());
    }

    #[test]
    fn unreadable_admission_records_return_errors_instead_of_absence() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        for record in [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ] {
            let path = root.path().join(record);
            let bytes = std::fs::read(&path).unwrap();
            std::fs::write(&path, b"invalid setup record").unwrap();
            assert!(admission_at(root.path()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
            std::fs::write(path, bytes).unwrap();
        }
        assert!(admission_at(root.path()).unwrap().is_some());
    }

    #[test]
    fn a_durable_stop_ends_admission_even_when_plan_or_progress_is_unreadable() {
        for record in ["automatic-plan.json", "initial-setup.json"] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
            let path = root.path().join(record);
            std::fs::write(&path, b"invalid setup record").unwrap();
            assert!(admission_at(root.path()).is_err());
            for state in ["cancelled", "restored"] {
                write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":state}),
                )
                .unwrap();
                assert!(finished_at(root.path()).unwrap());
                assert_eq!(admission_at(root.path()).unwrap(), None);
                assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
            }
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":"pending"}),
            )
            .unwrap();
            assert!(admission_at(root.path()).is_err());
        }
    }

    #[test]
    fn observations_are_read_only_and_unchanged_records_remain_equal() {
        let root = aterm_tempfile::tempdir().unwrap();
        let absent = observation_at(root.path()).unwrap();
        assert_eq!(observation_at(root.path()).unwrap(), absent);
        assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let current = observation_at(root.path()).unwrap();
        assert_ne!(current, absent);
        assert_eq!(current.clone(), observation_at(root.path()).unwrap());
        assert_eq!(
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .observation,
            current
        );
    }

    #[test]
    fn observed_read_only_pass_expires_across_a_pending_stage_aba() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let original = Records::read(root.path()).unwrap();
        let before = observation_at(root.path()).unwrap();
        let stable = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(stable.result.unwrap(), 7);
        assert_eq!(stable.observation, Some(before.clone()));

        let changed = observe_read_only(Ok(root.path().into()), |root| {
            let _writer = HeldLock::take(root)?;
            let mut host = Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            };
            let report = execute_initial(&mut host, root, &mut None)?.unwrap();
            assert!(report.needs_attention());
            assert_eq!(host.calls, [Stage::Secrets]);
            Ok(7)
        });
        assert_eq!(changed.result.unwrap(), 7);
        assert!(changed.observation.is_none());
        assert_eq!(Records::read(root.path()).unwrap(), original);
        assert_ne!(observation_at(root.path()).unwrap(), before);
    }

    #[test]
    fn observed_check_cannot_bind_during_a_profile_pass_with_unchanged_records() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let context = observation_at(root.path()).unwrap();
        let writer = HeldLock::take(root.path()).unwrap();
        let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(observed.result.unwrap(), 7);
        assert!(observed.observation.is_none());
        assert_eq!(observation_at(root.path()).unwrap(), context);
        drop(writer);
        let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
        assert_eq!(observed.result.unwrap(), 7);
        assert_eq!(observed.observation, Some(context));
    }

    #[test]
    fn observed_restore_error_is_bound_under_lock_and_expires_after_a_new_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        // A run an older aterm cancelled.
        let before = observation_at(root.path()).unwrap();
        write_legacy_cancel_at(root.path());
        let cancelled = observation_at(root.path()).unwrap();
        assert_ne!(cancelled, before);
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        std::fs::write(
            root.path().join("original-preferences.json"),
            b"invalid backup",
        )
        .unwrap();
        let restored = observe_mutation_at(
            root.path(),
            Some(cancelled.clone()),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                super::super::tests::restore_without_effects(root)
            },
        );
        assert!(restored.result.is_err());
        let restored = restored.observation.unwrap();
        assert_ne!(restored, cancelled);
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished);
        assert_eq!(snapshot.observation, restored);

        restart_at(root.path(), &plan).unwrap();
        assert_ne!(observation_at(root.path()).unwrap(), restored);
    }

    #[test]
    fn observed_busy_failure_keeps_only_its_pre_attempt_context() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let before = observation_at(root.path()).unwrap();
        let writer = HeldLock::take(root.path()).unwrap();
        let refused: InitialSetupObserved<()> = observe_mutation_at(
            root.path(),
            Some(before.clone()),
            Err("busy".into()),
            |_| panic!("a busy profile must not run the operation"),
        );
        assert!(refused.result.is_err());
        assert_eq!(refused.observation, Some(before));
        drop(writer);
        restart_at(root.path(), &plan).unwrap();
        assert_ne!(
            refused.observation,
            Some(observation_at(root.path()).unwrap())
        );
    }

    #[test]
    fn observation_read_failures_never_relabel_an_operation_result() {
        for directory in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let path = root.path().join("first-run.json");
            if directory {
                std::fs::create_dir(&path).unwrap();
            } else {
                std::fs::write(&path, b"invalid decision").unwrap();
            }
            assert!(matches!(
                observation_at(root.path()),
                Err(InitialSetupSnapshotError::Read(_))
            ));
            let observed: InitialSetupObserved<()> =
                observe_read_only(Ok(root.path().into()), |_| Err("original failure".into()));
            assert_eq!(observed.result.unwrap_err(), "original failure");
            assert!(observed.observation.is_none());
        }
    }

    #[test]
    fn a_saved_running_stage_needs_a_live_profile_pass() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        {
            let _active = HeldLock::take(root.path()).unwrap();
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(snapshot.rows[0].status, "running");
        }
        for missing_lock in [false, true] {
            if missing_lock {
                std::fs::remove_file(root.path().join("profile.lock")).unwrap();
            }
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                snapshot.rows[0].status, "pending",
                "an ended worker cannot remain live after restart"
            );
            assert!(!snapshot.rows[0].detail.is_empty());
            assert!(!snapshot.finished);
            assert_eq!(snapshot.revision, saved.revision);
            assert_eq!(progress(root.path()).unwrap().unwrap(), saved);
            if missing_lock {
                assert!(!root.path().join("profile.lock").exists());
            }
        }
    }

    #[test]
    fn a_snapshot_samples_profile_activity_after_its_record_reads() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut saved = progress(root.path()).unwrap().unwrap();
        saved.running = Some(Stage::Secrets);
        save_progress(root.path(), saved).unwrap();
        let reads = std::cell::Cell::new(0);
        let writer = std::cell::RefCell::new(None);
        let snapshot = snapshot_with(
            Plan::default(),
            || {
                assert_eq!(reads.get(), 2);
                HeldLock::profile_active(root.path())
            },
            || {
                let record = Records::read(root.path())?;
                reads.set(reads.get() + 1);
                if reads.get() == 2 {
                    *writer.borrow_mut() = Some(HeldLock::take(root.path())?);
                }
                Ok(record)
            },
        )
        .unwrap();
        assert_eq!(snapshot.rows[0].status, "running");
        drop(writer.into_inner());
        assert!(!HeldLock::profile_active(root.path()).unwrap());
        let _next_writer = HeldLock::take(root.path()).unwrap();
    }

    #[test]
    fn explicit_mac_apply_retires_workers_when_it_revives_a_stopped_modern_run() {
        for stop in ["cancelled", "restored"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets]);
            authorize_at(root.path(), &plan).unwrap();
            let original = revision_at(root.path()).unwrap();
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":stop}),
            )
            .unwrap();
            save_explicit_workstation_plan(root.path()).unwrap();
            let current = revision_at(root.path()).unwrap();
            assert!(
                current > original,
                "{stop}: explicit apply revived an older worker's password"
            );
            let _profile = HeldLock::take(root.path()).unwrap();
            let password = SecretBytes::from(b"test-only-old-run-password".as_slice());
            let mut host = Fake::default();
            assert!(
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    original,
                    &mut Some(password.clone()),
                    SetupHooks::NONE
                )
                .unwrap()
                .is_none()
            );
            assert!(host.calls.is_empty());
            assert!(!host.password_seen);
            execute_initial_for_revision(
                &mut host,
                root.path(),
                current,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            // A Restore withdraws no permission, so the explicit plan keeps them; a Cancel does.
            let mut calls = vec![Stage::Workstation, Stage::Secrets, Stage::Accounts];
            if stop == "restored" {
                calls.insert(1, Stage::Permissions);
            }
            assert_eq!(host.calls, calls, "{stop}");
        }
    }

    #[test]
    fn explicit_mac_apply_retires_legacy_run_zero_after_an_earlier_stop() {
        for stop in ["cancelled", "restored"] {
            let root = aterm_tempfile::tempdir().unwrap();
            assert_eq!(revision_at(root.path()).unwrap(), 0);
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":stop}),
            )
            .unwrap();
            save_explicit_workstation_plan(root.path()).unwrap();
            assert_ne!(
                revision_at(root.path()).unwrap(),
                0,
                "{stop}: legacy run zero survived"
            );
        }
    }

    #[test]
    fn explicit_workstation_completion_does_not_complete_independent_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let explicit = save_explicit_workstation_plan(root.path()).unwrap();
        complete_explicit_workstation_plan(root.path(), explicit).unwrap();
        let current = progress(root.path()).unwrap().unwrap();
        assert_eq!(current.revision, revision);
        // The explicit pass signs out with its preferences: both stages finish.
        assert_eq!(
            current.done,
            Stage::Workstation.bit() | Stage::Accounts.bit()
        );
        assert!(!current.completed);
        assert!(!finished_at(root.path()).unwrap());
    }

    #[test]
    fn permission_policy_requires_authorization_without_creating_missing_state() {
        let root = aterm_tempfile::tempdir().unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!snapshot.permissions_allowed);
        assert!(!snapshot.finished);
        assert_eq!(snapshot.revision, 0);
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);

        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
        )
        .unwrap();
        save_plan(root.path(), &Plan::default()).unwrap();
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
    }

    #[test]
    fn permission_policy_survives_completed_selected_setup_without_rerunning_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let pending = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(pending.permissions_allowed);
        assert!(!pending.finished);
        assert_ne!(pending.revision, 0);

        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let completed = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(completed.permissions_allowed);
        assert!(completed.finished);
        assert_eq!(completed.revision, pending.revision);
        assert_eq!(completed.plan, plan);
        assert!(completed.rows.iter().all(|row| row.status == "applied"));
    }

    #[test]
    fn permission_policy_honors_stops_and_the_latest_explicit_selection() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let revision = snapshot_at(root.path(), Plan::default()).unwrap().revision;
        // A Cancel holds aterm's permissions; a Restore puts back Mac preferences and
        // withdraws none of them.
        for (state, allowed) in [("cancelled", false), ("restored", true)] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            let stopped = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(stopped.permissions_allowed, allowed, "{state}");
            assert!(stopped.finished);
            assert_eq!(stopped.revision, revision);
        }

        restart_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let unchecked = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!unchecked.permissions_allowed);
        assert!(unchecked.revision > revision);
        restart_at(root.path(), &plan).unwrap();
        let selected = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(selected.permissions_allowed);
        assert!(selected.revision > unchecked.revision);
    }

    /// The launch's startup as `prepare_setup` reads the records: nothing when setup is finished,
    /// the run already admitted, else the offer answered as the page answers it, admitted.
    fn launch_startup(root: &Path) -> Option<u64> {
        if finished_at(root).unwrap() {
            return None;
        }
        if let Some(admission) = admission_at(root).unwrap() {
            return (!admission.completed).then_some(admission.revision);
        }
        let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
        let offered = startup.offer_reading(root, || Ok(None)).unwrap();
        startup.persist_at(root, &offered).result.unwrap()
    }

    pub(super) fn resume_native_accounts_after_init_for_test(
        root: &Path,
        account: &std::sync::Arc<crate::macos_doctor::accounts_visibility_tests::Account>,
    ) -> Result<Option<Report>, String> {
        use crate::macos_doctor::accounts_visibility_tests::system;

        let Some(revision) = launch_startup(root) else {
            return Ok(None);
        };
        let mut host = Fake {
            native_accounts: Some((
                system(None, account),
                NativeHost::over(
                    Box::new(system(None, account)),
                    Box::new(aterm_machines::init::prepare_workstation_selected),
                ),
            )),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
            .map(|pass| pass.map(|(report, _)| report))
    }

    /// A person's Cancel of the first-launch review holds until they ask again (System State
    /// design §3.3): every group reads Leave as is, aterm's own permissions with them, no launch
    /// offers the review again, and a Restore after it never brings the review back. Set up now
    /// applies the plan the review showed; a group choice sets up that group alone.
    #[test]
    fn a_cancelled_review_holds_until_a_person_asks_again() {
        use aterm_types::macos_setup::Group;
        let reviewed = Plan::default();
        let cancelled = || {
            let root = aterm_tempfile::tempdir().unwrap();
            save_cancelled_decision(root.path(), &reviewed).unwrap();
            root
        };
        let restart = |root: &Path, mut restart: InitialSetupRestart, plan: &Plan| {
            observe_mutation_at(root, None, Err("busy".into()), |root| {
                restart.persist_with(
                    root,
                    plan,
                    false,
                    || Ok(None),
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap()
            .expect("admitted")
        };

        let root = cancelled();
        let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
        assert!(page.cancelled && page.finished && !page.permissions_allowed);
        assert!(page.plan.selection.is_empty(), "every group at Leave as is");
        assert_eq!(
            launch_startup(root.path()),
            None,
            "no launch offers it again"
        );
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        assert_eq!(
            launch_startup(root.path()),
            None,
            "a Restore after a Cancel never brings the review back"
        );
        let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
        assert!(!page.permissions_allowed && page.plan.selection.is_empty());

        // Set up now, as System State sends it: the plan the review showed.
        let root = cancelled();
        restart(
            root.path(),
            InitialSetupRestart::default().changing(Some(Selection::none())),
            &Plan::from(Selection::none()),
        );
        assert_eq!(saved_plan(root.path()).unwrap(), Some(reviewed.clone()));
        let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
        assert!(!page.cancelled && page.permissions_allowed);

        // A group press, from a page that still sends every group at Set up: that group alone.
        let root = cancelled();
        let desktop = group_selection(Group::Desktop);
        restart(
            root.path(),
            InitialSetupRestart::chosen().changing(Some(desktop)),
            &reviewed,
        );
        assert_eq!(
            saved_plan(root.path()).unwrap().map(|plan| plan.selection),
            Some(desktop)
        );
        assert!(
            !snapshot_at(root.path(), reviewed.clone())
                .unwrap()
                .cancelled
        );
    }

    /// A person's Cancel of the review that offered what a Restore brought forward reads and
    /// behaves as any other Cancel (design §3.3): every group at Leave as is, aterm's own
    /// permissions held, the page's Cancel line, no launch offering it again, and Set up now
    /// applying the plan the review showed. The Restore's record stays, held: nothing automatic
    /// sets again the groups it put back, and Set up now, which shows none of them, leaves them
    /// restored. An older aterm reads past the plan the record names, and still holds.
    #[test]
    fn a_cancelled_review_of_a_restore_brought_forward_holds_as_any_cancel() {
        let cancelled = || {
            let root = aterm_tempfile::tempdir().unwrap();
            older_restored_setup(root.path(), Some(Selection::all().workstation()));
            bring_forward_restored_at(root.path())
                .unwrap()
                .expect("brought forward");
            let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let reviewed = startup.offer_reading(root.path(), || Ok(None)).unwrap();
            assert!(reviewed.selection.contains(Setting::Permissions));
            assert!(preferences(reviewed.selection).is_empty());
            let cancel = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                startup.cancel_at(root, &reviewed)
            });
            assert_eq!(cancel.result, Ok(true));
            (root, reviewed)
        };
        let (root, reviewed) = cancelled();
        let restored = Records::read(root.path()).unwrap().restored.unwrap();
        assert!(restored.held, "the Restore's record stays, held");
        assert_eq!(restored.left, preferences(Selection::all()));
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(page.cancelled, "the page's Cancel line, beside Set up now");
        assert!(page.finished && !page.permissions_allowed && !page.provisional);
        assert!(page.plan.selection.is_empty(), "every group at Leave as is");
        assert_eq!(launch_startup(root.path()), None, "nothing offers it again");
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        let record: Value =
            aterm_json::from_slice(&std::fs::read(root.path().join("first-run.json")).unwrap())
                .unwrap();
        assert_eq!(
            (&record["state"], &record["held"], &record["format"]),
            (&json!("restored"), &json!(true), &json!(FORMAT)),
            "a held Restore, as an older aterm reads it"
        );

        // Set up now, as System State sends it: the plan the review showed.
        observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            InitialSetupRestart::default()
                .changing(Some(Selection::none()))
                .persist_with(
                    root,
                    &Plan::from(Selection::none()),
                    false,
                    || Ok(None),
                    save_progress,
                    observe_restart,
                )
        })
        .result
        .unwrap()
        .expect("admitted");
        assert_eq!(saved_plan(root.path()).unwrap(), Some(reviewed.clone()));
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!page.cancelled && page.permissions_allowed);
        let records = Records::read(root.path()).unwrap();
        assert_eq!(
            records.restored.map(|restored| restored.left),
            Some(preferences(Selection::all())),
            "the groups the Restore put back stay put back"
        );

        // An older aterm reads past `reviewed`: the held Restore it knew, offering nothing.
        let (root, _) = cancelled();
        let path = root.path().join("first-run.json");
        let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record.as_object_mut().unwrap().remove("reviewed");
        write_json(&path, &record).unwrap();
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!page.cancelled && !page.permissions_allowed);
        assert!(page.plan.selection.is_empty());
        assert_eq!(launch_startup(root.path()), None);
    }

    /// An older aterm's setup gets aterm's own permissions at the next launch unless a person
    /// cancelled it: a verified completion at once, as before; a pending or unverified one through
    /// the launch's offer, which adds them to the plan that aterm saved; a restored one brought
    /// forward first ([`bring_forward_restored_at`]). Before the launch admits a run, none is
    /// allowed.
    #[test]
    fn permission_policy_approves_legacy_setups_unless_a_person_cancelled() {
        for (state, verified, allowed) in [
            ("applied", true, true),
            ("applied", false, true),
            ("pending", false, true),
            ("cancelled", true, false),
            ("restored", true, true),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
            )
            .unwrap();
            let before = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                before.permissions_allowed,
                state == "applied" && verified,
                "{state}/{verified}: only a verified completion before the launch"
            );
            assert_eq!(before.revision, 0);
            bring_forward_restored_at(root.path()).unwrap();
            let admitted = launch_startup(root.path());
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(snapshot.permissions_allowed, allowed, "{state}/{verified}");
            assert_eq!(
                snapshot.revision,
                admitted.unwrap_or(0),
                "{state}/{verified}"
            );
            if admitted.is_some() {
                assert!(snapshot.plan.selection.contains(Setting::Permissions));
            }
        }
    }

    /// The groups an older aterm's restored setup had: none without a plan, else its plan's.
    fn older_restored_setup(root: &Path, saved: Option<Selection>) {
        if let Some(saved) = saved {
            save_automatic_plan(root, saved).unwrap();
        }
        write_json(
            &root.join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
    }

    /// A Restore an older aterm made, before any run was admitted here, left aterm's own
    /// permissions with no run to hold them, so nothing asked for them again (design §3.4: a
    /// Restore withdraws none). The launch brings it forward, and its startup's offer admits a
    /// run that sets aterm's permissions alone: the restored groups stay at Leave as is,
    /// Messages & FaceTime as the plan had them, and no preference, sign-out or Spotlight step
    /// runs.
    #[test]
    fn a_setup_restored_before_any_run_gets_aterms_permissions_and_nothing_else() {
        let sign_outs = accounts(Selection::all());
        for case in [
            "no plan",
            "an older plan",
            "an older plan without sign-outs",
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let kept = match case {
                "no plan" => {
                    older_restored_setup(root.path(), None);
                    Selection::none()
                }
                "an older plan" => {
                    older_restored_setup(root.path(), Some(Selection::all().workstation()));
                    sign_outs
                }
                _ => {
                    older_restored_setup(root.path(), Some(preferences(Selection::all())));
                    Selection::none()
                }
            };
            let before = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(before.finished, "{case}");
            assert!(!before.permissions_allowed, "{case}: the gap");
            assert_eq!(launch_startup(root.path()), None, "{case}: nothing offered");

            let staged = bring_forward_restored_at(root.path())
                .unwrap()
                .expect("brought forward");
            let page = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!page.finished, "{case}");
            assert!(
                !page.permissions_allowed,
                "{case}: until the offer admits it"
            );
            assert_eq!(page.revision, staged, "{case}");
            let mut wanted = kept;
            wanted.set(Setting::Permissions, true);
            assert_eq!(page.plan.selection, wanted, "{case}");
            assert!(!saved_plan_provisional(root.path()).unwrap(), "{case}");

            let revision = launch_startup(root.path()).expect("the offer admits the run");
            let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(admitted.permissions_allowed, "{case}");
            assert!(!admitted.finished, "{case}");
            assert_eq!(admitted.plan.selection, wanted, "{case}");
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, [Stage::Permissions], "{case}");
            let done = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(done.finished && done.permissions_allowed, "{case}");
            assert_eq!(launch_startup(root.path()), None, "{case}");
            assert_eq!(
                bring_forward_restored_at(root.path()).unwrap(),
                None,
                "{case}"
            );
        }
    }

    /// This aterm's Restore of an older aterm's verified setup (its Dock reset, Messages and
    /// FaceTime signed out, no progress record), from System State or from a terminal. The
    /// Restore keeps the one pass that older aterm made ([`keep_older_pass`]): aterm's own
    /// permissions stay allowed and shown throughout, nothing waits for a launch, and a choice
    /// after it (Desktop & Dock back to Set up) sets that group without signing out again. An
    /// older setup that was pending or unverified kept no pass to hold them: the Restore stages
    /// them for the offer at once, which sets them alone.
    #[test]
    fn a_restore_of_an_older_setup_keeps_its_permissions_and_its_sign_outs() {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
        save_workstation_decision(root.path(), true).unwrap();
        assert!(permissions_allowed(root.path()));
        save_restore(root.path()).unwrap();
        assert!(permissions_allowed(root.path()), "while it puts back");
        leave_restored_as_is(root.path()).unwrap();
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(page.finished && page.permissions_allowed);
        assert!(page.plan.selection.contains(Setting::Permissions));
        assert!(preferences(page.plan.selection).is_empty());
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        assert_eq!(
            launch_startup(root.path()),
            None,
            "nothing waits for a launch"
        );
        let mut plan = page.plan;
        for setting in Group::Desktop.settings() {
            plan.selection.set(*setting, true);
        }
        let revision = system_state_choice(root.path(), &plan);
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        assert!(
            !host.calls.contains(&Stage::Accounts),
            "no sign-out again: {:?}",
            host.calls
        );

        for (state, verified) in [("pending", false), ("applied", false)] {
            let root = aterm_tempfile::tempdir().unwrap();
            save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
            )
            .unwrap();
            save_restore(root.path()).unwrap();
            leave_restored_as_is(root.path()).unwrap();
            assert!(!finished_at(root.path()).unwrap(), "{state}: the offer");
            let revision = launch_startup(root.path()).expect("the offer admits it");
            assert!(permissions_allowed(root.path()), "{state}");
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, [Stage::Permissions], "{state}");
        }

        // A run number reserved here before (a progress write that failed after its
        // reservation) is never used again: the older pass takes the number after it.
        let reserved = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(reserved.path(), Selection::all().workstation()).unwrap();
        save_workstation_decision(reserved.path(), true).unwrap();
        revision::reserve(reserved.path(), 4).unwrap();
        save_restore(reserved.path()).unwrap();
        let kept = progress_file(reserved.path())
            .unwrap()
            .expect("the older pass");
        assert_eq!(kept.revision, 5);
        assert!(kept.admitted);
        leave_restored_as_is(reserved.path()).unwrap();
        let page = snapshot_at(reserved.path(), Plan::default()).unwrap();
        assert!(page.finished && page.permissions_allowed);
    }

    /// Bringing a restored setup forward writes the run it stages into the Restore's record, then
    /// its plan, then its staged progress, then the Restore's record, pending. Cut short before
    /// any of them, the records still read as the Restore's own decision: nothing runs, nothing
    /// is admitted, no permission is allowed; and the next launch finishes the writes as one
    /// never cut short does.
    #[test]
    fn bringing_a_restored_setup_forward_finishes_wherever_it_was_cut_short() {
        let reference = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(reference.path(), Some(Selection::all().workstation()));
        bring_forward_restored_at(reference.path())
            .unwrap()
            .unwrap();
        let whole = Records::read(reference.path()).unwrap();
        for cut in [
            RestoreWrite::Run,
            RestoreWrite::Plan,
            RestoreWrite::Progress,
            RestoreWrite::Record,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            older_restored_setup(root.path(), Some(Selection::all().workstation()));
            let _lock = HeldLock::take(root.path()).unwrap();
            let mut writes = Vec::new();
            let result = bring_forward_restored_with(root.path(), |write| {
                writes.push(write);
                if write == cut {
                    Err("injected end before this write".into())
                } else {
                    Ok(())
                }
            });
            drop(_lock);
            assert!(result.is_err(), "{cut:?}");
            assert_eq!(writes.last(), Some(&cut));
            assert!(finished_at(root.path()).unwrap(), "{cut:?}");
            assert_eq!(admission_at(root.path()).unwrap(), None, "{cut:?}");
            assert!(
                !snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .permissions_allowed,
                "{cut:?}"
            );
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty(), "{cut:?}");
            assert_eq!(launch_startup(root.path()), None, "{cut:?}");

            assert_eq!(bring_forward_restored_at(root.path()).unwrap(), Some(1));
            assert_eq!(Records::read(root.path()).unwrap(), whole, "{cut:?}");
        }
        let all = [
            RestoreWrite::Run,
            RestoreWrite::Plan,
            RestoreWrite::Progress,
            RestoreWrite::Record,
        ];
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), Some(Selection::all().workstation()));
        let mut writes = Vec::new();
        bring_forward_restored_with(root.path(), |write| {
            writes.push(write);
            Ok(())
        })
        .unwrap();
        assert_eq!(writes, all, "in this order");
    }

    #[test]
    fn bringing_forward_reports_its_checkpoint_even_when_the_pending_record_returns_an_error() {
        for published_record in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            older_restored_setup(root.path(), None);
            let lock = HeldLock::take(root.path()).unwrap();
            let mut target = None;
            let result = bring_forward_restored_observing_with(
                root.path(),
                |write| {
                    if !published_record && write == RestoreWrite::Progress {
                        return Err("injected unpublished checkpoint failure".into());
                    }
                    if published_record && write == RestoreWrite::Record {
                        // The real final publication lands, but the caller receives
                        // its later durability error instead of Some(revision).
                        let restored = decision_record(root.path()).unwrap().1.unwrap();
                        save_restored_decision(
                            root.path(),
                            RestoredState::Pending,
                            Restored {
                                run: None,
                                ..restored
                            },
                        )?;
                        return Err("injected pending-decision flush failure".into());
                    }
                    Ok(())
                },
                |revision| target = Some(revision),
            );
            drop(lock);
            assert!(result.is_err());
            assert_eq!(target.is_some(), published_record);
            if let Some(target) = target {
                let before = Records::read(root.path()).unwrap();
                assert_eq!(
                    bring_forward_waiting_observing(
                        root.path(),
                        || false,
                        |_| { panic!("finding an earlier run is not a new publication") }
                    )
                    .unwrap(),
                    BroughtForward::Run(target)
                );
                assert_eq!(Records::read(root.path()).unwrap(), before);
                request_stop_at(root.path(), target).unwrap();
                let before = Records::read(root.path()).unwrap();
                assert!(finished_at(root.path()).unwrap());
                assert_eq!(launch_startup(root.path()), None);
                assert_eq!(Records::read(root.path()).unwrap(), before);
                let mut host = Fake::default();
                assert!(
                    execute_initial(&mut host, root.path(), &mut None)
                        .unwrap()
                        .is_none()
                );
                assert!(host.calls.is_empty());
            }
        }
    }

    #[test]
    fn finishing_a_restore_reports_its_rebound_run_before_late_metadata_failure() {
        let root = restored_after(&Plan::default());
        let previous = progress(root.path()).unwrap().unwrap().revision;
        let lock = HeldLock::take(root.path()).unwrap();
        let mut target = None;
        assert!(
            bring_forward_restored_observing_with(
                root.path(),
                |write| {
                    if write == RestoreWrite::Run {
                        Err("injected late Restore-run metadata failure".into())
                    } else {
                        Ok(())
                    }
                },
                |revision| target = Some(revision),
            )
            .is_err()
        );
        let target = target.expect("the rebound Progress was published before the failure");
        assert!(target > previous);
        assert_eq!(progress(root.path()).unwrap().unwrap().revision, target);
        assert_eq!(decision_record(root.path()).unwrap().1.unwrap().run, None);

        request_stop_at(root.path(), target).unwrap();
        assert!(Records::read(root.path()).unwrap().stopped_run());
        assert_eq!(
            bring_forward_restored_observing_with(
                root.path(),
                |_| Ok(()),
                |_| { panic!("finishing metadata without a rebound writes no checkpoint") }
            )
            .unwrap(),
            Some(target)
        );
        assert_eq!(
            decision_record(root.path()).unwrap().1.unwrap().run,
            Some(target)
        );
        drop(lock);
        assert!(Records::read(root.path()).unwrap().stopped_run());
        assert_eq!(launch_startup(root.path()), None);
    }

    fn progress_namespace_error(finishes_restore: bool) {
        for after_rename in [false, true] {
            let root = if finishes_restore {
                restored_after(&Plan::default())
            } else {
                let root = aterm_tempfile::tempdir().unwrap();
                older_restored_setup(root.path(), None);
                root
            };
            let before = progress_file(root.path()).unwrap();
            let lock = HeldLock::take(root.path()).unwrap();
            let mut reported = None;
            assert!(
                bring_forward_restored_observing_using(
                    root.path(),
                    |_| Ok(()),
                    |revision| reported = Some(revision),
                    |root, progress| progress_publication_error(root, progress, after_rename),
                )
                .is_err(),
                "the failed flush never becomes successful admission"
            );
            let visible = progress_file(root.path()).unwrap();
            if !after_rename {
                assert_eq!(visible, before);
                assert_eq!(reported, None);
                continue;
            }
            let visible = visible.expect("the actual Progress rename landed before the error");
            assert!(before.is_none_or(|before| before.revision < visible.revision));
            if finishes_restore {
                // This retry only finishes Restore.run metadata. It cannot
                // recover a callback the original Progress writer omitted.
                let mut retry_target = None;
                assert_eq!(
                    bring_forward_restored_observing_with(
                        root.path(),
                        |_| Ok(()),
                        |revision| {
                            retry_target = Some(revision);
                        }
                    )
                    .unwrap(),
                    Some(visible.revision)
                );
                assert_eq!(retry_target, None);
            }
            drop(lock);
            request_stop_at(root.path(), visible.revision).unwrap();
            assert!(Records::read(root.path()).unwrap().stopped_run());
            assert!(
                !snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .permissions_allowed
            );
            assert_eq!(launch_startup(root.path()), None);
            assert_eq!(
                reported,
                Some(visible.revision),
                "a visible exact checkpoint needs Stop evidence despite its original error"
            );
        }
    }

    #[test]
    fn progress_namespace_error_retains_a_staged_restore_target() {
        progress_namespace_error(false);
    }

    #[test]
    fn progress_namespace_error_retains_a_rebound_restore_target_before_metadata_only_retry() {
        progress_namespace_error(true);
    }

    #[test]
    fn progress_namespace_error_compares_the_canonical_candidate_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::default();
        let candidate = Progress {
            revision: 8,
            access: Some(8),
            ..Progress::for_plan(&plan)
        };
        let _lock = HeldLock::take(root.path()).unwrap();
        let mut target = None;
        assert!(
            save_progress_observing_with(
                root.path(),
                candidate,
                &mut |revision| target = Some(revision),
                |root, progress| progress_publication_error(root, progress, true),
            )
            .is_err()
        );
        let visible = progress_file(root.path()).unwrap().unwrap();
        assert_eq!(
            visible.access, None,
            "the canonical record omits self access"
        );
        assert_eq!(progress_record(visible), progress_record(candidate));
        assert_eq!(target, Some(candidate.revision));
    }

    #[test]
    fn progress_namespace_error_cannot_claim_a_newer_replacement() {
        let root = aterm_tempfile::tempdir().unwrap();
        let candidate = Progress {
            revision: 8,
            ..Progress::for_plan(&Plan::default())
        };
        let newer = Progress {
            revision: 9,
            ..candidate
        };
        let _lock = HeldLock::take(root.path()).unwrap();
        let mut target = None;
        assert!(
            save_progress_observing_with(
                root.path(),
                candidate,
                &mut |revision| target = Some(revision),
                |root, progress| {
                    save_progress(root, progress)?;
                    save_progress(root, newer)?;
                    Err("a newer checkpoint replaced the attempted publication".into())
                },
            )
            .is_err()
        );
        assert_eq!(progress_file(root.path()).unwrap(), Some(newer));
        assert_eq!(target, None);
    }

    /// Only a Restore before any run was admitted is brought forward. A person's Cancel holds
    /// aterm's permissions, a Restore of an admitted run keeps that run's own approval, and a Mac
    /// with no setup records gets none.
    #[test]
    fn nothing_else_is_brought_forward() {
        let cancelled = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(cancelled.path(), Selection::all().workstation()).unwrap();
        write_legacy_cancel_at(cancelled.path());
        let before = Records::read(cancelled.path()).unwrap();
        assert_eq!(bring_forward_restored_at(cancelled.path()).unwrap(), None);
        assert_eq!(Records::read(cancelled.path()).unwrap(), before);
        assert!(!cancelled.path().join("initial-setup.json").exists());
        assert_eq!(launch_startup(cancelled.path()), None);
        assert!(
            !snapshot_at(cancelled.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );

        let admitted = restored_after(&Plan::default());
        leave_restored_as_is(admitted.path()).unwrap();
        let before = Records::read(admitted.path()).unwrap();
        assert_eq!(bring_forward_restored_at(admitted.path()).unwrap(), None);
        assert_eq!(Records::read(admitted.path()).unwrap(), before);
        assert!(
            snapshot_at(admitted.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );

        let absent = aterm_tempfile::tempdir().unwrap();
        let missing = absent.path().join("not-created");
        assert_eq!(bring_forward_restored_at(&missing).unwrap(), None);
        assert!(!missing.exists());
    }

    #[test]
    fn permission_policy_never_takes_a_snapshot_that_changed_during_observation() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let before = Records::read(root.path()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        let after = Records::read(root.path()).unwrap();
        let unchanged = snapshot_with(Plan::default(), || Ok(false), || Ok(before.clone()));
        assert!(unchanged.unwrap().permissions_allowed, "the policy before");
        // The observation that met the write is taken again: the one after it is the snapshot.
        let mut reads = [before.clone()]
            .into_iter()
            .chain(std::iter::repeat(after.clone()));
        let settled = snapshot_with(Plan::default(), || Ok(false), || Ok(reads.next().unwrap()));
        assert!(!settled.unwrap().permissions_allowed);
        // Records that keep changing are refused.
        let mut reads = [before, after].into_iter().cycle();
        assert!(matches!(
            snapshot_with(Plan::default(), || Ok(false), || Ok(reads.next().unwrap())),
            Err(InitialSetupSnapshotError::Updating)
        ));
        assert!(matches!(
            snapshot_with(
                Plan::default(),
                || Ok(false),
                || Err("injected read failure".into())
            ),
            Err(InitialSetupSnapshotError::Read(_))
        ));
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
    }

    #[test]
    fn permission_policy_waits_for_the_final_admission_commit_including_empty_choices() {
        for plan in [selected(&[Setting::Permissions]), selected(&[])] {
            let root = aterm_tempfile::tempdir().unwrap();
            assert!(
                authorize_with(root.path(), &plan, AdmittedVault::None, |_, _| {
                    Err("injected final admission failure".into())
                })
                .is_err()
            );
            let staged = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!staged.permissions_allowed);
            assert!(!staged.finished);
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty());

            assert!(authorize_at(root.path(), &plan).unwrap().is_some());
            let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                admitted.permissions_allowed,
                plan.selection.contains(Setting::Permissions)
            );
            assert_eq!(admitted.finished, plan.selection.is_empty());
            assert!(admitted.revision > staged.revision);
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            let completed = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(completed.finished);
            assert!(
                authorize_at(root.path(), &Plan::default())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(revision_at(root.path()).unwrap(), completed.revision);
        }
    }

    #[test]
    fn permission_policy_cannot_reuse_old_admission_during_a_plan_restart() {
        let root = aterm_tempfile::tempdir().unwrap();
        let old = selected(&[Setting::Widgets]);
        authorize_at(root.path(), &old).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let next = selected(&[Setting::Widgets, Setting::Permissions]);

        save_plan(root.path(), &next).unwrap();
        let published = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!published.permissions_allowed);
        assert_eq!(published.revision, 0);
        assert!(
            restart_with(root.path(), &next, AdmittedVault::None, |_, _| {
                Err("injected restart admission failure".into())
            })
            .is_err()
        );
        let uncommitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!uncommitted.permissions_allowed);
        assert!(!uncommitted.finished);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());

        restart_at(root.path(), &next).unwrap();
        let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(admitted.permissions_allowed);
        assert!(admitted.revision > uncommitted.revision);
    }

    #[test]
    fn permission_policy_migrates_completed_history_but_pending_history_needs_authorization() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let path = root.path().join("initial-setup.json");
        let remove_admission = || {
            let bytes = std::fs::read(&path).unwrap();
            let mut record: Value = aterm_json::from_slice(&bytes).unwrap();
            record.as_object_mut().unwrap().remove("admitted");
            write_json(&path, &record).unwrap();
        };
        remove_admission();
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(authorize_at(root.path(), &plan).unwrap().is_some());
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        remove_admission();
        let completed = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(completed.permissions_allowed);
        assert!(completed.finished);
    }

    #[test]
    fn unified_setup_orders_sync_before_repositories_and_never_persists_the_password() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::default();
        authorize_at(root.path(), &plan).unwrap();
        let _profile = HeldLock::take(root.path()).unwrap();
        let password = SecretBytes::from(b"test-only-join-password".as_slice());
        let mut host = Fake {
            observe: Some(root.path().into()),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut Some(password.clone()))
            .unwrap()
            .unwrap();
        assert!(!report.needs_attention());
        assert_eq!(host.calls, Stage::ALL);
        assert!(host.password_seen);
        assert!(finished_at(root.path()).unwrap());
        for path in [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ] {
            let bytes = std::fs::read(root.path().join(path)).unwrap();
            assert!(
                !bytes
                    .windows(password.len())
                    .any(|window| window == password.as_slice())
            );
        }
    }

    /// EVERY ROW OF A SETUP PASS CARRIES ITS TIME in the JSON: a stage's rows, its error and
    /// the repositories' wait on the vault alike (presence only, never a budget).
    #[test]
    fn every_setup_row_carries_its_time_in_the_json() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            failure: Some(Stage::Permissions),
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        for (name, status) in [
            (Stage::Permissions.name(), "error"),
            (Stage::Repositories.name(), "blocked"),
        ] {
            assert!(
                report
                    .rows
                    .iter()
                    .any(|row| row.name == name && row.status == status),
                "{name}: {}",
                report.to_text()
            );
        }
        let json: aterm_json::Value = aterm_json::from_str(&report.to_json()).unwrap();
        let rows = json["settings"].as_array().unwrap();
        assert_eq!(rows.len(), report.rows.len());
        assert!(
            rows.iter().all(|row| row["elapsed_ms"].as_u64().is_some()),
            "{json}"
        );
    }

    #[test]
    fn pending_vault_blocks_only_repositories_and_retries_without_repeating_finished_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .unwrap()
                .needs_attention()
        );
        assert_eq!(host.calls, [&Stage::ALL[..4], &[Stage::Accounts]].concat());
        assert!(first_run_complete(root.path()).unwrap());
        assert!(!finished_at(root.path()).unwrap());
        assert!(claim_automatic_setup_at(root.path()).unwrap().is_some());
        host.calls.clear();
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_later_incomplete_vault_blocks_repository_retries_and_reopens_only_its_stage() {
        for readiness_error in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &Plan::default()).unwrap();
            let mut host = Fake {
                failure: Some(Stage::Repositories),
                ..Fake::default()
            };
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            assert_eq!(host.calls, Stage::ALL);
            assert_eq!(host.vault_checks, 1);
            let completed = progress(root.path()).unwrap().unwrap().done;
            assert_ne!(completed & Stage::Secrets.bit(), 0);
            assert_eq!(completed & Stage::Repositories.bit(), 0);
            let mut interrupted = progress(root.path()).unwrap().unwrap();
            interrupted.running = Some(Stage::Repositories);
            save_progress(root.path(), interrupted).unwrap();

            host.calls.clear();
            host.failure = None;
            host.vault_blocked = !readiness_error;
            host.vault_error = readiness_error;
            let report = execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .unwrap();
            assert!(host.calls.is_empty());
            assert_eq!(host.vault_checks, 2);
            assert!(report.needs_attention());
            assert_eq!(
                progress(root.path()).unwrap().unwrap().done,
                completed & !Stage::Secrets.bit()
            );
            assert_eq!(progress(root.path()).unwrap().unwrap().running, None);
            assert!(!finished_at(root.path()).unwrap());

            host.vault_blocked = false;
            host.vault_error = false;
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
            assert_eq!(host.vault_checks, 3);
            assert!(finished_at(root.path()).unwrap());
        }
    }

    /// At the end of a pass, a Repositories stage that stops for want of Apple's Command Line
    /// Tools asks the window to install them ([`SetupHooks::developer_tools`] `true`), as the
    /// live host reads the stop; one that stops on anything else (GitHub unreached, a person's
    /// missing token) withdraws the ask (`false`) at once, before the pass's next stop boundary,
    /// so a pass that fails after it leaves no want; and a pass a Stop ended at any boundary
    /// after the stage withdraws there: before the deferred settings, at the pass's end, or
    /// before a later stage.
    #[test]
    fn a_repositories_stage_without_the_command_line_tools_asks_the_window_to_install_them() {
        use aterm_machines::items::Ctx;
        use aterm_machines::model::{HostFacts, User};
        use aterm_machines::port::{Fake as Native, GithubFailure, Ran, Stat};
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicUsize, Ordering};

        // Each call: the revision, the want, and how many stop boundaries the pass had asked.
        static TOLD: Mutex<Vec<(u64, bool, usize)>> = Mutex::new(Vec::new());
        // The pass's stop boundaries asked so far, and the one a Stop arrives at (0 is the
        // first stage's).
        static ASKED: AtomicUsize = AtomicUsize::new(0);
        static STOP_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
        let told = || std::mem::take(&mut *TOLD.lock().unwrap());
        let hooks = SetupHooks {
            developer_tools: |revision, wanted| {
                TOLD.lock()
                    .unwrap()
                    .push((revision, wanted, ASKED.load(Ordering::SeqCst)));
            },
            stop: |_| ASKED.fetch_add(1, Ordering::SeqCst) >= STOP_AT.load(Ordering::SeqCst),
            ..SetupHooks::NONE
        };
        let host = |tools: bool, token: bool| {
            let user = User {
                name: "tester".into(),
                uid: 501,
                gid: 20,
                home: "/test-repository-home".into(),
                shell: "/bin/zsh".into(),
                admin: false,
            };
            let mut sys = Native::root();
            sys.euid = user.uid;
            let stat = |dir, len| Stat {
                uid: user.uid,
                gid: user.gid,
                mode: if dir { 0o700 } else { 0o600 },
                dir,
                symlink: false,
                len,
            };
            const TOKEN: &str = "inert_repository_tools_token";
            sys.stat_of(&user.home, stat(true, 0));
            if tools {
                sys.file(aterm_machines::items::github::CLT_GIT, b"inert git");
            }
            if token {
                let token = format!("{}/.secrets/gh_access_token", user.home);
                sys.file(&token, TOKEN.as_bytes())
                    .stat_of(&token, stat(false, TOKEN.len() as u64));
            }
            for _ in 1..=2 {
                sys.fail_github_api(&user, "/user", GithubFailure::Unreached);
            }
            sys.simulate(|_, command| {
                let command = command.display();
                if command.contains(" config --null --show-origin --get-regexp ") {
                    Some(Ran::ok(""))
                } else {
                    panic!("unexpected repository effect: {command}");
                }
            });
            let ctx = Ctx {
                facts: HostFacts::default(),
                users: vec![user.clone()],
                console: Some(user),
                host_name: "fixture".into(),
                cache_dir: PathBuf::from("/test-cache"),
                disrupt: false,
            };
            Fake {
                native_repositories: Some((sys, ctx)),
                ..Fake::default()
            }
        };
        let repositories: &[Setting] = &[Setting::Repositories];
        let then_accounts: &[Setting] = &[Setting::Repositories, Setting::Messages];
        let never = usize::MAX;
        // `at`: the stop boundaries asked when the window hears: an ask after the pass's last
        // (2), a withdrawal right after the stage (before the next) or at the Stop's own.
        for (case, settings, tools, token, stop_at, waits, wanted, at) in [
            (
                "no tools",
                repositories,
                false,
                true,
                never,
                SetupNext::Park,
                true,
                3,
            ),
            (
                "GitHub unreached",
                repositories,
                true,
                true,
                never,
                SetupNext::Retry,
                false,
                1,
            ),
            (
                "no token",
                repositories,
                true,
                false,
                never,
                SetupNext::Park,
                false,
                1,
            ),
            (
                "stopped before deferred settings",
                repositories,
                false,
                true,
                1,
                SetupNext::Done,
                false,
                2,
            ),
            (
                "stopped at the pass's end",
                repositories,
                false,
                true,
                2,
                SetupNext::Done,
                false,
                3,
            ),
            (
                "stopped before the Accounts stage",
                then_accounts,
                false,
                true,
                1,
                SetupNext::Done,
                false,
                2,
            ),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &selected(settings))
                .unwrap()
                .unwrap();
            let mut host = host(tools, token);
            told();
            STOP_AT.store(stop_at, Ordering::SeqCst);
            // A stopped run runs no second pass.
            let passes = if stop_at == never { 2 } else { 1 };
            for pass in 1..=passes {
                ASKED.store(0, Ordering::SeqCst);
                let (_, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    hooks,
                )
                .unwrap()
                .unwrap();
                assert_eq!(next, waits, "{case}, pass {pass}");
                assert_eq!(
                    told(),
                    [(revision, wanted, at)],
                    "{case}, pass {pass}: told once, at that boundary"
                );
            }
        }
    }

    fn repositories_recover_after_an_interface_change(with_secrets: bool) {
        use aterm_machines::items::Ctx;
        use aterm_machines::model::{HostFacts, User};
        use aterm_machines::port::{Fake as Native, GithubFailure, GithubResponse, Ran, Stat};
        use aterm_secrets::network::InterfaceAddress;
        use std::net::{IpAddr, Ipv4Addr};

        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Repositories]);
        plan.selection.set(Setting::Secrets, with_secrets);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let user = User {
            name: "tester".into(),
            uid: 501,
            gid: 20,
            home: "/test-repository-home".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Native::root();
        sys.euid = user.uid;
        let stat = |dir, len| Stat {
            uid: user.uid,
            gid: user.gid,
            mode: if dir { 0o700 } else { 0o600 },
            dir,
            symlink: false,
            len,
        };
        const TOKEN: &str = "inert_repository_recovery_token";
        sys.stat_of(&user.home, stat(true, 0));
        sys.file(aterm_machines::items::github::CLT_GIT, b"inert git");
        let token = format!("{}/.secrets/gh_access_token", user.home);
        sys.file(&token, TOKEN.as_bytes())
            .stat_of(&token, stat(false, TOKEN.len() as u64));
        for _ in 0..10 {
            sys.fail_github_api(&user, "/user", GithubFailure::Unreached);
        }
        for (endpoint, body) in [
            ("/user", aterm_json::json!({"login":"tester"})),
            (
                "/user/repos?visibility=all&per_page=30&sort=full_name&direction=asc&page=1",
                aterm_json::json!([]),
            ),
        ] {
            sys.on_github_api(
                &user,
                endpoint,
                Ok(GithubResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: aterm_json::to_vec(&body).unwrap(),
                }),
            );
        }
        // The local helper already returns this token for all repositories.
        // A complete empty inventory therefore needs no credential writes or clones.
        sys.simulate(|_, command| {
            let command = command.display();
            if command.ends_with(" credential fill") {
                Some(Ran::ok(&format!(
                    "protocol=https\nhost=github.com\nusername=tester\npassword={TOKEN}\n"
                )))
            } else if command.contains(" config --null --show-origin --get-regexp ") {
                Some(Ran::ok(
                    "file:/test-repository-home/.gitconfig\0credential.helper\ninert-helper\0",
                ))
            } else {
                panic!("unexpected repository effect: {command}");
            }
        });
        let ctx = Ctx {
            facts: HostFacts::default(),
            users: vec![user.clone()],
            console: Some(user),
            host_name: "fixture".into(),
            cache_dir: PathBuf::from("/test-cache"),
            disrupt: false,
        };
        let mut host = Fake {
            native_repositories: Some((sys, ctx)),
            ..Fake::default()
        };
        for _ in 0..10 {
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            assert_eq!(next, SetupNext::Retry);
            assert!(report.rows.iter().any(|row| row.status == "blocked"));
        }
        let attempts =
            |host: &Fake, stage| host.calls.iter().filter(|called| **called == stage).count();
        assert_eq!(attempts(&host, Stage::Secrets), usize::from(with_secrets));
        assert_eq!(attempts(&host, Stage::Repositories), 10);
        let progress = progress(root.path()).unwrap().unwrap();
        assert_eq!(progress.done & Stage::Repositories.bit(), 0);
        if with_secrets {
            assert_ne!(progress.done & Stage::Secrets.bit(), 0);
        }
        let mark = |address| {
            initial_setup_park_mark_with(Some(root.path()), None, 1_800_000_600, || {
                Some(vec![InterfaceAddress {
                    name: "en0".into(),
                    address: IpAddr::V4(Ipv4Addr::new(192, 0, 2, address)),
                    netmask: Some(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 0))),
                    flags: 1,
                }])
            })
        };
        let parked = mark(1);
        assert_eq!(parked.retry_due(), None);
        assert_eq!(parked.mac_settings_retry_due(), None);
        assert_eq!(parked, mark(1), "an unchanged network stays parked");
        let reattached = mark(2);

        // A worker woken by the changed local attachment can finish the same
        // admitted run, without repeating Secrets or creating a new revision.
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(next, SetupNext::Done);
        assert_eq!(attempts(&host, Stage::Secrets), usize::from(with_secrets));
        assert_eq!(attempts(&host, Stage::Repositories), 11);
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(attempts(&host, Stage::Repositories), 11);
        let (sys, _) = host.native_repositories.as_ref().unwrap();
        assert_eq!(
            sys.calls()
                .iter()
                .filter(|call| call.ends_with(" agh GET /user"))
                .count(),
            11
        );
        assert!(sys.fed().is_empty());
        assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
            && !call.contains("credential approve")
            && !call.contains("--replace-all")));

        let no_interface_read = || {
            initial_setup_park_mark_with(Some(root.path()), None, 1_800_000_600, || {
                panic!("finished or stopped repositories must not inspect interfaces")
            })
        };
        no_interface_read();
        let stopped_revision = restart_at(root.path(), &plan).unwrap();
        request_stop_at(root.path(), stopped_revision).unwrap();
        no_interface_read();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            stopped_revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(attempts(&host, Stage::Repositories), 11);
        assert_ne!(
            parked, reattached,
            "a local interface change must wake unfinished repositories"
        );
    }

    #[test]
    fn a_network_change_wakes_repositories_after_secrets_finishes() {
        repositories_recover_after_an_interface_change(true);
    }

    #[test]
    fn a_network_change_wakes_a_repositories_only_run() {
        repositories_recover_after_an_interface_change(false);
    }

    #[test]
    fn subset_and_empty_plans_never_visit_unselected_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Repositories]);
        authorize_at(root.path(), &plan).unwrap();
        assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Repositories]);
        assert!(!host.password_seen);
        assert_eq!(host.vault_checks, 0);

        let empty = aterm_tempfile::tempdir().unwrap();
        authorize_at(empty.path(), &Plan::from(Selection::none())).unwrap();
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, empty.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(finished_at(empty.path()).unwrap());
    }

    struct WithholdingStages {
        doctor: super::super::tests::Withholding,
        rest: Fake,
        grant_in_permissions: bool,
    }

    impl WithholdingStages {
        fn over(doctor: super::super::tests::Withholding) -> Self {
            Self {
                doctor,
                rest: Fake::default(),
                grant_in_permissions: false,
            }
        }
    }

    impl InitialHost for WithholdingStages {
        fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.doctor.pass(root)
        }
        fn workstation(
            &mut self,
            root: &Path,
            _: Option<Selection>,
        ) -> Result<Option<Report>, String> {
            self.doctor.pass(root)
        }
        fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String> {
            self.rest.accounts(root, on_screen)
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.rest.spotlight()
        }
        fn permissions(&mut self, plan: &Plan, access: u64) -> Result<Vec<Row>, String> {
            if self.grant_in_permissions {
                self.doctor.grant();
            }
            self.rest.permissions(plan, access)
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.rest.secrets(plan, replaces, password)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            self.rest.secrets_pace()
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            self.rest.configured_vault()
        }
        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            self.rest.current_vault(plan)
        }
        fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.rest.repositories(vault)
        }
        fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.doctor.follow_up(root)
        }
        fn withheld_owed(&mut self, root: &Path) -> Vec<Setting> {
            self.doctor.owed(root)
        }
    }

    #[test]
    fn the_grant_that_arrives_while_the_stage_is_pending_clears_notification_center_once() {
        for granted in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Dock]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(
                super::super::tests::Withholding::widgets_and_a_dock_that_never_verifies(),
            );
            let retry = |stages: &mut WithholdingStages| {
                let _ = execute_initial_for_revision(
                    stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("setup still runs");
            };
            retry(&mut stages);
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            let docks = stages.doctor.dock_relaunches();
            stages.doctor.show_desktop_widgets();
            for _ in 0..30 {
                retry(&mut stages);
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert!(stages.doctor.desktop_widgets_shown());
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            assert_eq!(
                stages.doctor.dock_relaunches(),
                docks + 2,
                "the Dock's second try: its relaunch and its rollback's"
            );
            let docks = stages.doctor.dock_relaunches();
            if granted {
                stages.doctor.grant();
            }
            for _ in 0..30 {
                retry(&mut stages);
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.notification_center_cleared(), granted);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
            assert_eq!(
                stages.doctor.notification_center_relaunches(),
                usize::from(granted)
            );
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert!(stages.doctor.desktop_widgets_shown());
            assert_eq!(stages.doctor.dock_relaunches(), docks);
            assert_eq!(
                super::super::tests::Withholding::withheld(root.path()).is_empty(),
                granted
            );
        }
    }

    #[test]
    fn the_widgets_row_never_says_aterm_set_the_list_the_follow_up_still_owes() {
        use super::super::tests::{WIDGETS, Withholding, nc_widgets};
        let list = nc_widgets().describe();
        let owed = format!("{list}; left as it is: {SETTLED_OWED}");
        for keeps in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Dock]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages =
                WithholdingStages::over(Withholding::widgets_and_a_dock_that_never_verifies());
            if !keeps {
                stages.doctor.notification_center_never_keeps();
            }
            let retry = |stages: &mut WithholdingStages| {
                execute_initial_for_revision(
                    stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("setup still runs")
                .0
            };
            retry(&mut stages);
            stages.doctor.grant();
            for at in 0..15 {
                let report = retry(&mut stages);
                assert!(
                    !report.rows.iter().any(|row| row.detail.contains(SETTLED)),
                    "{keeps} {at}: {}",
                    report.to_text()
                );
                let followed_up = !keeps || at == 0;
                let named = report
                    .rows
                    .iter()
                    .any(|row| row.name == WIDGETS && row.status == "ok" && row.detail == owed);
                assert_eq!(named, followed_up, "{keeps} {at}: {}", report.to_text());
                assert_eq!(
                    report.rows.iter().filter(|row| row.name == WIDGETS).count(),
                    1 + usize::from(followed_up),
                    "the Workstation stage's row, and the follow-up's: {keeps} {at}: {}",
                    report.to_text()
                );
            }
            assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
            assert_eq!(stages.doctor.window_manager_writes(), 1);
            assert_eq!(stages.doctor.notification_center_cleared(), keeps);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                if keeps { 1 } else { 4 },
                "a clear that takes once; else two tries and their rollbacks"
            );
            assert_eq!(Withholding::withheld(root.path()).is_empty(), keeps);
        }
    }

    #[test]
    fn the_grant_that_arrives_during_the_permissions_stage_is_followed_up_as_setup_completes() {
        for granted in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Permissions]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages {
                rest: Fake {
                    pending: Some(Stage::Permissions),
                    ..Fake::default()
                },
                ..WithholdingStages::over(super::super::tests::Withholding::widgets())
            };
            for _ in 0..3 {
                execute_initial_for_revision(
                    &mut stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
            }
            assert_eq!(stages.doctor.notification_center_writes(), 0);
            assert!(!finished_at(root.path()).unwrap());
            stages.grant_in_permissions = granted;
            stages.rest.pending = None;
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the retry runs");
            assert!(finished_at(root.path()).unwrap());
            assert_eq!(stages.doctor.notification_center_cleared(), granted);
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
            assert_eq!(
                super::super::tests::Withholding::withheld(root.path()).is_empty(),
                granted
            );
            for _ in 0..2 {
                assert!(
                    execute_initial_for_revision(
                        &mut stages,
                        root.path(),
                        revision,
                        &mut None,
                        SetupHooks::NONE
                    )
                    .unwrap()
                    .is_none()
                );
            }
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(granted)
            );
        }
    }

    #[test]
    fn a_stop_before_pending_follow_up_leaves_withheld_preferences_untouched() {
        use super::super::tests::Withholding;
        use std::sync::atomic::{AtomicBool, Ordering};

        static STOP: AtomicBool = AtomicBool::new(false);
        fn after_permissions(event: StageEvent) {
            if !event.running && event.settings == Stage::Permissions.settings() {
                STOP.store(true, Ordering::SeqCst);
            }
        }

        for (pending, request) in [(true, false), (true, true), (false, true)] {
            STOP.store(false, Ordering::SeqCst);
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Permissions]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages {
                rest: Fake {
                    pending: pending.then_some(Stage::Permissions),
                    ..Fake::default()
                },
                grant_in_permissions: true,
                ..WithholdingStages::over(Withholding::widgets())
            };
            let hooks = SetupHooks {
                stop: |_| STOP.load(Ordering::SeqCst),
                stage: if request { after_permissions } else { |_| {} },
                ..SetupHooks::NONE
            };
            let (_, next) =
                execute_initial_for_revision(&mut stages, root.path(), revision, &mut None, hooks)
                    .unwrap()
                    .expect("the admitted run makes a pass");

            assert_eq!(STOP.load(Ordering::SeqCst), request);
            assert!(
                !root.path().join(STOP_REQUEST).exists(),
                "the save has not run"
            );
            let stopped = pending && request;
            assert_eq!(
                stages.doctor.notification_center_writes(),
                usize::from(!stopped),
                "a Stop holds before follow-up while work is pending; a finished last stage keeps its completion"
            );
            assert_eq!(progress(root.path()).unwrap().unwrap().stopped, stopped);
            assert_eq!(finished_at(root.path()).unwrap(), !pending || request);
            assert_eq!(
                Withholding::withheld(root.path()).is_empty(),
                !stopped,
                "a stopped clear remains withheld"
            );
            assert_eq!(
                next,
                if pending && !request {
                    SetupNext::Retry
                } else {
                    SetupNext::Done
                }
            );
        }
    }

    #[test]
    fn a_stopped_run_follows_nothing_up() {
        use super::super::tests::{WIDGETS, Withholding};
        for stop in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Widgets, Setting::Secrets]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            // The vault is not joined yet: the run is still going.
            stages.rest.pending = Some(Stage::Secrets);
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
            assert!(!finished_at(root.path()).unwrap());
            if stop {
                request_stop_at(root.path(), revision).unwrap();
            }
            assert_eq!(stopped(root.path()).unwrap(), stop);
        }
    }

    /// A later choice that leaves the widgets row as is, the vault-alone one included, withdraws
    /// the clear setup still owes; a restart that keeps the row withdraws nothing, and once the
    /// grant arrives that run's follow-up clears Notification Center's widgets.
    #[test]
    fn a_choice_that_leaves_the_widgets_as_is_withdraws_the_clear_setup_owes() {
        use super::super::tests::{WIDGETS, Withholding};
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        for (what, next, cleared) in [
            ("the vault alone", selected(&[Setting::Secrets]), false),
            (
                "the Dock and the vault",
                selected(&[Setting::Dock, Setting::Secrets]),
                false,
            ),
            ("the same groups", plan.clone(), true),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS], "{what}");
            let revision = restart_at(root.path(), &next).unwrap();
            stages.doctor.grant();
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(
                stages.doctor.notification_center_cleared(),
                cleared,
                "{what}"
            );
            assert!(Withholding::withheld(root.path()).is_empty(), "{what}");
        }
    }

    /// A finished modern run left Notification Center's list for its follow-up.
    fn widgets_owed_before_restore() -> (
        aterm_tempfile::TempDir,
        Plan,
        super::super::tests::Withholding,
    ) {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::from(group_selection(Group::Desktop));
        for setting in Group::Input.settings() {
            plan.selection.set(*setting, true);
        }
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        assert!(progress(root.path()).unwrap().unwrap().only.is_none());
        // This first Workstation stage owes its whole selection, so the
        // stage fixture's unrestricted pass is the production scope here.
        let mut stages = WithholdingStages::over(super::super::tests::Withholding::widgets());
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the initial modern run executes");
        assert!(finished_at(root.path()).unwrap());
        assert!(progress(root.path()).unwrap().unwrap().completed);
        let doctor = stages.doctor;
        assert_eq!(
            super::super::tests::Withholding::withheld(root.path()),
            [super::super::tests::WIDGETS]
        );
        assert_eq!(doctor.notification_center_writes(), 0);
        assert!(!doctor.notification_center_cleared());
        (root, plan, doctor)
    }

    #[test]
    fn an_interrupted_restore_never_revives_its_old_widgets_follow_up_for_another_choice() {
        use super::super::tests::Withholding;
        use aterm_types::macos_setup::Group;

        for join in [false, true] {
            let (root, mut plan, mut doctor) = widgets_owed_before_restore();
            // Restore published its authoritative record, then the process ended
            // before its next write cleared the old withheld-preferences record.
            save_restore(root.path()).unwrap();
            assert!(stopped(root.path()).unwrap());
            if join {
                plan.selection.set(Setting::Secrets, true);
                system_state_join(root.path(), &plan);
            } else {
                for setting in Group::Input.settings() {
                    plan.selection.set(*setting, false);
                }
                system_state_choice(root.path(), &plan);
            }
            assert!(!stopped(root.path()).unwrap());
            let (decision, restored) = decision_record(root.path()).unwrap();
            assert_eq!(decision, Decision::Pending);
            let restored = restored.expect("the unchanged Desktop group stands restored");
            assert!(restored.left.contains(Setting::Widgets));
            assert!(
                !restored
                    .queue(progress(root.path()).unwrap(), Some(&plan))
                    .contains(Setting::Widgets)
            );
            assert!(progress(root.path()).unwrap().unwrap().admitted);
            doctor.grant();
            let followed = doctor.follow_up(root.path()).unwrap();
            assert_eq!(doctor.notification_center_writes(), 0);
            assert!(
                followed.is_none(),
                "an unrelated choice or Join never authorizes restored Widgets"
            );
            assert!(!doctor.notification_center_cleared());
            assert!(Withholding::withheld(root.path()).is_empty());
        }
    }

    #[test]
    fn a_widgets_follow_up_requires_fresh_authority_after_restore() {
        use super::super::tests::{WIDGETS, Withholding};
        use aterm_types::macos_setup::Group;

        for route in [
            "Restore stop",
            "Restore cleared deferred work",
            "Desktop set up",
            "Set up now",
        ] {
            let (root, mut plan, mut doctor) = widgets_owed_before_restore();
            if route == "Restore cleared deferred work" {
                doctor.restore(root.path()).unwrap();
                assert!(Withholding::withheld(root.path()).is_empty());
                for setting in Group::Input.settings() {
                    plan.selection.set(*setting, false);
                }
                system_state_choice(root.path(), &plan);
            } else {
                save_restore(root.path()).unwrap();
                match route {
                    "Desktop set up" => {
                        for setting in Group::Desktop.settings() {
                            plan.selection.set(*setting, false);
                        }
                        system_state_choice(root.path(), &plan);
                        for setting in Group::Desktop.settings() {
                            plan.selection.set(*setting, true);
                        }
                        system_state_choice(root.path(), &plan);
                        let restored = decision_record(root.path()).unwrap().1.unwrap();
                        assert!(
                            restored
                                .queue(progress(root.path()).unwrap(), Some(&plan))
                                .contains(Setting::Widgets)
                        );
                    }
                    "Set up now" => {
                        system_state_apply(root.path(), &plan, false, || Ok(None));
                        // The put-back still waits for Full Disk Access, so the record
                        // survives, queueing what the Workstation stage now sets.
                        let restored = decision_record(root.path()).unwrap().1.unwrap();
                        assert!(
                            restored
                                .queue(progress(root.path()).unwrap(), Some(&plan))
                                .contains(Setting::Widgets)
                        );
                    }
                    _ => assert!(stopped(root.path()).unwrap()),
                }
            }
            doctor.grant();
            let follows = matches!(route, "Desktop set up" | "Set up now");
            assert_eq!(
                doctor.follow_up(root.path()).unwrap().is_some(),
                follows,
                "{route}"
            );
            assert_eq!(
                doctor.notification_center_writes(),
                usize::from(follows),
                "{route}"
            );
            assert_eq!(doctor.notification_center_cleared(), follows, "{route}");
            assert_eq!(
                Withholding::withheld(root.path()),
                if route == "Restore stop" {
                    vec![WIDGETS.to_owned()]
                } else {
                    Vec::new()
                },
                "{route}"
            );
        }
    }

    #[test]
    fn an_unadmitted_modern_widgets_follow_up_waits_for_its_own_final_commit() {
        use super::super::tests::{WIDGETS, Withholding};
        use aterm_types::macos_setup::Group;

        let (root, mut plan, mut doctor) = widgets_owed_before_restore();
        save_restore(root.path()).unwrap();
        for setting in Group::Desktop.settings() {
            plan.selection.set(*setting, false);
        }
        system_state_choice(root.path(), &plan);
        for setting in Group::Desktop.settings() {
            plan.selection.set(*setting, true);
        }
        let mut attempt = InitialSetupRestart::chosen();
        let staged = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Ok(None),
                |_, _| Err("injected failure before the final admission save".into()),
                observe_restart,
            )
        });
        assert!(staged.result.is_err());
        assert!(attempt.pending());
        let pending = progress(root.path()).unwrap().unwrap();
        assert!(pending.belongs_to(&plan));
        assert!(!pending.admitted);
        let restored = decision_record(root.path()).unwrap().1.unwrap();
        assert!(
            restored
                .queue(Some(pending), Some(&plan))
                .contains(Setting::Widgets)
        );
        let before = observation_at(root.path()).unwrap();
        doctor.grant();
        let followed = doctor.follow_up(root.path()).unwrap();
        assert_eq!(doctor.notification_center_writes(), 0);
        assert!(
            followed.is_none(),
            "a staged queue is not a durable admission"
        );
        assert!(!doctor.notification_center_cleared());
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert_eq!(observation_at(root.path()).unwrap(), before);

        let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap();
        assert_eq!(admitted, Some(pending.revision));
        assert!(!attempt.pending());
        assert!(doctor.follow_up(root.path()).unwrap().is_some());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert!(doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
        assert_eq!(doctor.notification_center_writes(), 1);
    }

    /// `aterm doctor --apply` over a choice whose admission never landed admits that run at its
    /// last write, as the launch's offer would have: the plan the person chose, with the stages
    /// the apply ran recorded finished, so the next launch runs them no more. Its Widgets change,
    /// withheld for want of Full Disk Access, follows the grant, once.
    #[test]
    fn an_explicit_apply_over_a_staged_choice_admits_it_and_its_widgets_follow_the_grant() {
        use super::super::tests::{WIDGETS, Withholding};

        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Permissions]);
        let mut attempt = InitialSetupRestart::chosen();
        let staged = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Ok(None),
                |_, _| Err("injected failure before the final admission save".into()),
                observe_restart,
            )
        });
        assert!(staged.result.is_err());
        assert!(attempt.pending());
        assert!(Withholding::withheld(root.path()).is_empty());
        let mut doctor = Withholding::widgets();
        doctor.explicit_apply(root.path()).unwrap();
        let expanded = saved_plan(root.path()).unwrap().unwrap();
        assert!(expanded.selection.contains(Setting::Secrets));
        assert!(expanded.selection.contains(Setting::Permissions));
        let admitted = progress(root.path()).unwrap().unwrap();
        assert!(admitted.belongs_to(&expanded));
        assert!(admitted.admitted);
        assert_ne!(admitted.done & Stage::Workstation.bit(), 0);
        assert!(admission_at(root.path()).unwrap().is_some());
        assert!(permissions_allowed(root.path()));
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert_eq!(doctor.notification_center_writes(), 0);

        doctor.grant();
        assert!(doctor.follow_up(root.path()).unwrap().is_some());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert!(doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
        assert_eq!(doctor.notification_center_writes(), 1);
        drop(attempt);
    }

    #[test]
    fn a_widgets_follow_up_waits_for_its_saved_plan_to_match_existing_progress() {
        use super::super::tests::{WIDGETS, Withholding};

        for legacy_mismatch in [false, true] {
            let (root, _, mut doctor) = widgets_owed_before_restore();
            let withheld = root.path().join("withheld-preferences.json");
            let owed = std::fs::read(&withheld).unwrap();
            let path = root.path().join("automatic-plan.json");
            let retained = root.path().join("automatic-plan-retained.json");
            std::fs::rename(&path, &retained).unwrap();
            if legacy_mismatch {
                // A selection-only plan is not fallback authority over the
                // modern run whose progress it has not reset yet.
                super::super::save_automatic_plan(
                    root.path(),
                    selected(&[Setting::Widgets]).selection,
                )
                .unwrap();
            }
            assert!(progress(root.path()).unwrap().unwrap().admitted);
            doctor.grant();
            assert!(
                doctor.follow_up(root.path()).is_err(),
                "an unconfirmed run must not reset its deferred retry record"
            );
            assert_eq!(doctor.notification_center_writes(), 0);
            assert!(!doctor.notification_center_cleared());
            assert_eq!(std::fs::read(&withheld).unwrap(), owed);

            std::fs::rename(retained, path).unwrap();
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
            assert!(doctor.follow_up(root.path()).unwrap().is_some());
            assert_eq!(doctor.notification_center_writes(), 1);
            assert!(doctor.notification_center_cleared());
            assert!(Withholding::withheld(root.path()).is_empty());
            assert!(doctor.follow_up(root.path()).unwrap().is_none());
            assert_eq!(doctor.notification_center_writes(), 1);
        }
    }

    #[test]
    fn a_new_setup_run_starts_the_follow_ups_tries_afresh() {
        use super::super::{load_withheld, record_withheld};
        let root = aterm_tempfile::tempdir().unwrap();
        let widgets = super::super::tests::WIDGETS.to_owned();
        let spend = |root: &Path| {
            record_withheld(root, std::slice::from_ref(&widgets), &[], &[], ("b", 0)).unwrap();
            for at in [0, 600] {
                record_withheld(root, &[], &[], std::slice::from_ref(&widgets), ("b", at)).unwrap();
            }
            let owed = load_withheld(root).unwrap();
            assert_eq!(owed.failed.len(), 1, "{:?}", owed.failed);
            assert_eq!(owed.failed[0].tries, 2);
        };
        spend(root.path());
        let plan = selected(&[Setting::Widgets]);
        authorize_at(root.path(), &plan).unwrap().unwrap();
        let owed = load_withheld(root.path()).unwrap();
        assert_eq!(owed.items, std::slice::from_ref(&widgets));
        assert!(owed.failed.is_empty(), "{:?}", owed.failed);
        spend(root.path());
        restart_at(root.path(), &plan).unwrap();
        let owed = load_withheld(root.path()).unwrap();
        assert_eq!(owed.items, [widgets]);
        assert!(owed.failed.is_empty(), "{:?}", owed.failed);
    }

    #[test]
    fn notification_centers_clear_is_tried_twice_per_build_whichever_pass_makes_each_try() {
        use super::super::tests::Withholding;
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages =
            WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
        let retry = |stages: &mut WithholdingStages| {
            let _ = execute_initial_for_revision(
                stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("setup still runs");
        };
        retry(&mut stages);
        assert_eq!(stages.doctor.notification_center_writes(), 0);
        assert_eq!(
            Withholding::withheld(root.path()),
            [super::super::tests::WIDGETS]
        );
        stages.doctor.grant();
        stages.doctor.notification_center_never_keeps();
        let mut tries = Vec::new();
        for _ in 0..40 {
            let writes = stages.doctor.notification_center_writes();
            retry(&mut stages);
            match stages.doctor.notification_center_writes() - writes {
                0 => {}
                2 => tries.push(stages.doctor.now()),
                more => panic!("a try and its rollback in one retry, not {more} writes"),
            }
        }
        assert!(!finished_at(root.path()).unwrap(), "the stage is held");
        assert_eq!(tries.len(), 2, "{tries:?}");
        let apart = tries[1].duration_since(tries[0]).unwrap();
        assert!(apart >= SECOND_TRY_AFTER, "{apart:?}");
    }

    #[test]
    fn apply_in_system_state_tries_a_held_clear_again() {
        use super::super::tests::Withholding;
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets]);
        let mut revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages =
            WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
        let retry = |stages: &mut WithholdingStages, revision: u64| {
            let writes = stages.doctor.notification_center_writes();
            let _ = execute_initial_for_revision(
                stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("setup still runs");
            stages.doctor.notification_center_writes() - writes
        };
        retry(&mut stages, revision);
        stages.doctor.grant();
        stages.doctor.notification_center_never_keeps();
        for run in 0..2 {
            let tried: usize = (0..40).map(|_| retry(&mut stages, revision)).sum();
            assert_eq!(tried, 4, "run {run}: two tries, each with its rollback");
            assert_eq!(
                Withholding::withheld(root.path()),
                [super::super::tests::WIDGETS],
                "run {run}: still owed"
            );
            revision = restart_at(root.path(), &plan).unwrap();
            assert_eq!(
                Withholding::withheld(root.path()),
                [super::super::tests::WIDGETS],
                "run {run}: the restart keeps it owed"
            );
        }
    }

    #[test]
    fn a_choice_runs_only_the_steps_it_changed() {
        let root = aterm_tempfile::tempdir().unwrap();
        let allowed = || {
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        };
        let run = |revision: u64| {
            let mut host = Fake::default();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(finished_at(root.path()).unwrap());
            assert!(first_run_complete(root.path()).unwrap());
            host.calls
        };
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert!(allowed());

        let mut joined = Plan {
            vault_url: "http://team.local:7484/v1/team".into(),
            ..Plan::default()
        };
        let revision = system_state_join(root.path(), &joined);
        assert!(!finished_at(root.path()).unwrap(), "the vault's steps run");
        assert!(first_run_complete(root.path()).unwrap());
        assert!(allowed());
        assert_eq!(run(revision), [Stage::Secrets, Stage::Repositories]);
        assert!(allowed(), "the permission steps stay allowed");

        joined.selection.set(Setting::Spotlight, false);
        assert!(run(system_state_choice(root.path(), &joined)).is_empty());
        joined.selection.set(Setting::Spotlight, true);
        let revision = system_state_choice(root.path(), &joined);
        assert_eq!(run(revision), [Stage::Spotlight]);
        joined.selection.set(Setting::Secrets, false);
        assert!(run(system_state_choice(root.path(), &joined)).is_empty());
        joined.selection.set(Setting::Secrets, true);
        let revision = system_state_join(root.path(), &joined);
        assert_eq!(run(revision), [Stage::Secrets, Stage::Repositories]);

        let revision = system_state_apply(root.path(), &joined, false, || Ok(None));
        let calls = run(revision);
        assert!(calls.contains(&Stage::Workstation), "{calls:?}");
        assert!(calls.contains(&Stage::Accounts), "{calls:?}");
    }

    #[test]
    fn a_workstation_stage_that_never_verifies_disrupts_twice_across_worker_retries() {
        struct Stages {
            doctor: super::super::tests::NeverVerifies,
            rest: Fake,
        }
        impl InitialHost for Stages {
            fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.doctor.pass(root)
            }
            fn workstation(
                &mut self,
                root: &Path,
                _: Option<Selection>,
            ) -> Result<Option<Report>, String> {
                self.doctor.pass(root)
            }
            fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String> {
                self.rest.accounts(root, on_screen)
            }
            fn spotlight(&mut self) -> Result<Vec<Row>, String> {
                self.rest.spotlight()
            }
            fn permissions(&mut self, plan: &Plan, access: u64) -> Result<Vec<Row>, String> {
                self.rest.permissions(plan, access)
            }
            fn secrets(
                &mut self,
                plan: &Plan,
                replaces: Replaces,
                password: &mut Option<SecretBytes>,
            ) -> Result<Vec<Row>, String> {
                self.rest.secrets(plan, replaces, password)
            }
            fn secrets_pace(&mut self) -> Option<Pace> {
                self.rest.secrets_pace()
            }
            fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
                self.rest.configured_vault()
            }
            fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
                self.rest.current_vault(plan)
            }
            fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
                self.rest.repositories(vault)
            }
            fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.withheld(root)
            }
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Dock, Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = Stages {
            doctor: super::super::tests::NeverVerifies::dock(),
            rest: Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            },
        };
        let retries = |stages: &mut Stages, count: usize, why: &str| {
            for retry in 0..count {
                let report = execute_initial_for_revision(
                    stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .unwrap()
                .0;
                assert!(report.needs_attention(), "retry {retry}");
                assert!(
                    report
                        .rows
                        .iter()
                        .any(|row| row.status == "blocked" && row.detail == why),
                    "retry {retry}: {}",
                    report.to_text()
                );
            }
        };
        let first = execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap()
        .0;
        assert!(first.needs_attention());
        let once = (stages.doctor.changes(), stages.doctor.relaunches());
        assert_eq!(once.1, 1);
        // macos_doctor's held-row text, not this module's `waiting` row status.
        retries(&mut stages, 9, super::super::WAITING);
        assert_eq!(
            (stages.doctor.changes(), stages.doctor.relaunches()),
            once,
            "nothing between the tries"
        );
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        let twice = (stages.doctor.changes(), stages.doctor.relaunches());
        assert_eq!(twice.1, 2);
        assert!(twice.0 > once.0);
        retries(&mut stages, 20, HELD);
        assert_eq!((stages.doctor.changes(), stages.doctor.relaunches()), twice);
        assert_eq!(
            stages
                .rest
                .calls
                .iter()
                .filter(|stage| **stage == Stage::Secrets)
                .count(),
            31,
            "the pending vault keeps retrying"
        );
        assert!(!finished_at(root.path()).unwrap());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
            0,
            "a held setting keeps its stage open for the person"
        );
    }

    #[test]
    fn a_rerun_retires_an_unreadable_held_record() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Dock, Setting::Widgets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        std::fs::write(root.path().join(HELD_RECORD), b"not json").unwrap();
        let mut doctor = super::super::tests::NeverVerifies::dock();
        assert!(
            doctor.pass(root.path()).is_err(),
            "an unreadable record holds the pass"
        );
        let rerun = restart_at(root.path(), &plan).unwrap();
        assert_ne!(rerun, revision);
        assert!(!root.path().join(HELD_RECORD).exists());
        let report = doctor
            .pass(root.path())
            .expect("the rerun's pass runs")
            .expect("its setup is pending");
        assert!(report.needs_attention(), "{}", report.to_text());
        assert_eq!(doctor.relaunches(), 1, "it tried the Dock once");
    }

    #[test]
    fn failed_stages_remain_pending_and_cancel_or_restore_stops_every_stage() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[
            Setting::Permissions,
            Setting::Secrets,
            Setting::Repositories,
        ]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            failure: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Permissions, Stage::Secrets]);
        assert!(!finished_at(root.path()).unwrap());
        assert!(first_run_complete(root.path()).unwrap());
        write_legacy_cancel_at(root.path());
        assert!(finished_at(root.path()).unwrap());
        let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            before
        );
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        assert!(finished_at(root.path()).unwrap());
    }

    /// A stage that fails as a whole is said on the page under the rows it covers (design §3.6):
    /// a grant for Permissions, a setting the plan selects for the others, never a grant a stage
    /// only needs (the sign-outs' Accessibility), and none of the stage's own words, which
    /// `aterm doctor` keeps under the stage's own name.
    #[test]
    fn a_stage_that_fails_as_a_whole_is_paged_under_its_rows() {
        use aterm_types::macos_setup::Access;
        for (stage, settings, said) in [
            (
                Stage::Spotlight,
                vec![Setting::Spotlight],
                vec![Subject::Setting(Setting::Spotlight)],
            ),
            (
                Stage::Permissions,
                vec![Setting::Permissions],
                [
                    Access::FullDiskAccess,
                    Access::DeveloperTools,
                    Access::Debugger,
                    Access::LocalNetwork,
                ]
                .map(Subject::Access)
                .to_vec(),
            ),
            (
                Stage::Workstation,
                vec![Setting::CapsLock, Setting::Dock],
                vec![
                    Subject::Setting(Setting::CapsLock),
                    Subject::Setting(Setting::Dock),
                ],
            ),
            (
                Stage::Accounts,
                vec![Setting::Messages, Setting::FaceTime],
                vec![
                    Subject::Setting(Setting::Messages),
                    Subject::Setting(Setting::FaceTime),
                ],
            ),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &selected(&settings)).unwrap();
            let mut host = Fake {
                failure: Some(stage),
                ..Fake::default()
            };
            let report = execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .unwrap();
            assert!(
                report.rows.iter().any(|row| row.name == stage.name()
                    && row.status == "error"
                    && row.detail == "offline; retry later"),
                "{stage:?}: `aterm doctor` keeps the stage's own words"
            );
            let page: Vec<(&str, &str, &str)> = report
                .page
                .as_ref()
                .expect("a pass has a page")
                .rows
                .iter()
                .filter(|row| row.status == "error")
                .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
                .collect();
            let want: Vec<(&str, &str, &str)> = said
                .iter()
                .map(|subject| (subject.name(), "error", ""))
                .collect();
            assert_eq!(page, want, "{stage:?}");
        }
    }

    /// The vault's stage says a failed try on the page as the vault's row, in the page's words
    /// (design §3.6): what aterm does next, and never the subscriber, a path or the vault's own
    /// words, which `aterm doctor` keeps.
    #[test]
    fn a_failed_vault_try_is_paged_in_the_pages_words() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut host = Fake {
            failure: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.detail.starts_with("offline; retry later")),
            "`aterm doctor` keeps the stage's own words"
        );
        let page: Vec<(&str, &str, &str)> = report
            .page
            .as_ref()
            .expect("a pass has a page")
            .rows
            .iter()
            .filter(|row| row.status == "error")
            .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
            .collect();
        assert_eq!(
            page,
            [("Secrets vault", "error", secrets_retry::PAGE_RETRIES)]
        );
    }

    #[test]
    fn an_explicit_profile_sets_up_permissions_unless_setup_was_cancelled() {
        // No setup record yet: the explicit plan is the profile and the permissions.
        let fresh = aterm_tempfile::tempdir().unwrap();
        save_explicit_workstation_plan(fresh.path()).unwrap();
        let plan = saved_plan(fresh.path()).unwrap().unwrap();
        assert!(plan.selection.contains(Setting::Permissions));
        for (state, permissions) in [("cancelled", false), ("restored", true)] {
            // A legacy Cancel: the explicit pass does not re-open aterm's permissions. A
            // Restore never withdrew them (design §3.4), so the plan keeps them.
            let legacy = aterm_tempfile::tempdir().unwrap();
            write_json(
                &legacy.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            save_explicit_workstation_plan(legacy.path()).unwrap();
            let plan = saved_plan(legacy.path()).unwrap().unwrap();
            assert_eq!(
                plan.selection.contains(Setting::Permissions),
                permissions,
                "{state}"
            );
            assert_eq!(plan.selection.workstation(), Selection::all().workstation());
        }
        // A modern run an older aterm cancelled: Permissions comes off, even
        // where it was set up.
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        write_legacy_cancel_at(root.path());
        save_explicit_workstation_plan(root.path()).unwrap();
        let plan = saved_plan(root.path()).unwrap().unwrap();
        assert!(!plan.selection.contains(Setting::Permissions));
        assert_eq!(plan.selection.workstation(), Selection::all().workstation());
    }

    #[test]
    fn explicit_mac_apply_retains_modern_stages_and_legacy_completion_stays_honored() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
        plan.vault_url = "https://vault.example/v1/team".into();
        authorize_at(root.path(), &plan).unwrap();
        save_explicit_workstation_plan(root.path()).unwrap();
        let revised = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(
            revised.selection.workstation(),
            Selection::all().workstation()
        );
        assert!(revised.selection.contains(Setting::Secrets));
        assert!(revised.selection.contains(Setting::Repositories));
        // aterm's own permissions join an explicit profile by default.
        assert!(revised.selection.contains(Setting::Permissions));
        assert_eq!(revised.vault_url, plan.vault_url);

        let legacy = aterm_tempfile::tempdir().unwrap();
        for state in ["cancelled", "restored", "applied"] {
            write_json(
                &legacy.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":true}),
            )
            .unwrap();
            authorize_at(legacy.path(), &Plan::default()).unwrap();
            assert!(finished_at(legacy.path()).unwrap());
            assert!(!legacy.path().join("initial-setup.json").exists());
        }
    }

    #[test]
    fn restore_stops_completed_workstation_reporting_without_undoing_other_stage_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();

        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert_eq!(rows.len(), Stage::ALL.len());
        for (stage, row) in Stage::ALL.iter().zip(rows) {
            assert_eq!(
                row.status,
                if *stage == Stage::Workstation {
                    "stopped"
                } else {
                    "applied"
                }
            );
        }
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            history
        );
    }

    fn restore_preflight_failure_stops_every_stage(record: &str) {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[
            Setting::Widgets,
            Setting::Permissions,
            Setting::Secrets,
            Setting::Repositories,
        ]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        let invalid = root.path().join(record);
        std::fs::write(&invalid, b"invalid recovery record").unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());

        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished, "Restore did not stop the durable run");
        assert!(
            snapshot.permissions_allowed,
            "a Restore never withdraws aterm's permissions"
        );
        assert_eq!(snapshot.plan, plan);
        assert_eq!(snapshot.revision, revision);
        assert!(snapshot.rows.iter().all(|row| row.status == "stopped"));
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            history
        );
        assert_eq!(std::fs::read(&invalid).unwrap(), b"invalid recovery record");
        let mut automatic = Fake::default();
        assert!(
            execute_initial(&mut automatic, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(automatic.calls.is_empty());
        assert!(authorize_at(root.path(), &plan).unwrap().is_none());
    }

    #[test]
    fn restore_stops_automatic_setup_before_reading_a_corrupt_backup() {
        restore_preflight_failure_stops_every_stage("original-preferences.json");
    }

    #[test]
    fn restore_stops_automatic_setup_before_reading_corrupt_activation() {
        restore_preflight_failure_stops_every_stage("pending-refresh.json");
    }

    #[test]
    fn restore_stop_write_failure_refuses_recovery_preflight_and_effects() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(
            root.path(),
            &selected(&[Setting::Widgets, Setting::Secrets]),
        )
        .unwrap();
        let backup = root.path().join("original-preferences.json");
        save_backup(
            &backup,
            &[Saved {
                target: Target::new("com.apple.dock", "show-recents"),
                value: Some(PValue::Bool(true)),
                granted: true,
            }],
        )
        .unwrap();
        let before = std::fs::read(&backup).unwrap();
        let decision = root.path().join("first-run.json");
        std::fs::remove_file(&decision).unwrap();
        std::fs::create_dir(&decision).unwrap();
        assert!(super::super::tests::restore_without_effects(root.path()).is_err());
        assert!(
            !root.path().join("pending-refresh.json").exists(),
            "a refused stop must precede recovery preparation"
        );
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        let mut automatic = Fake::default();
        assert!(execute_initial(&mut automatic, root.path(), &mut None).is_err());
        assert!(automatic.calls.is_empty());
    }

    #[test]
    fn an_older_cancel_keeps_applied_preferences_and_stops_only_unfinished_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        write_legacy_cancel_at(root.path());
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "applied");
        assert_eq!(rows[1].status, "stopped");
    }

    #[test]
    fn failed_explicit_preflight_cannot_accept_the_earlier_subset_completion() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(first_run_complete(root.path()).unwrap());
        let revision = revision_at(root.path()).unwrap();

        save_explicit_workstation_plan(root.path()).unwrap();
        host.calls.clear();
        host.failure = Some(Stage::Workstation);
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        // The explicit profile carries aterm's permissions, so their stage runs too.
        assert_eq!(
            host.calls,
            [
                Stage::Workstation,
                Stage::Permissions,
                Stage::Secrets,
                Stage::Accounts
            ]
        );
        assert!(!first_run_complete(root.path()).unwrap());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
            0
        );
        assert_eq!(revision_at(root.path()).unwrap(), revision);

        host.calls.clear();
        host.failure = None;
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn an_explicit_restart_reopens_stopped_stages_and_preserves_the_original_backup() {
        let root = aterm_tempfile::tempdir().unwrap();
        let original = root.path().join("original-preferences.json");
        std::fs::write(&original, b"the original preference snapshot").unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        write_json(
            &root.path().join("automatic-attempt.json"),
            &json!({"profile":PROFILE,"accessibility_ready":false}),
        )
        .unwrap();
        let plan = selected(&[Setting::Spotlight, Setting::Secrets]);
        restart_at(root.path(), &plan).unwrap();
        assert_eq!(
            std::fs::read(&original).unwrap(),
            b"the original preference snapshot"
        );
        assert!(!root.path().join("automatic-attempt.json").exists());
        let (saved, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert_eq!(saved, plan);
        assert!(!finished);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.status == "pending"));
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
    }

    fn configure_vault(paths: &aterm_secrets::client::Paths, url: &str) -> ConfiguredVault {
        use std::os::unix::fs::PermissionsExt;
        let name = url.rsplit('/').next().unwrap();
        std::fs::create_dir_all(&paths.dir).unwrap();
        std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let source = paths.dir.join("source.toml");
        std::fs::write(
            &source,
            format!(
                "url = \"{url}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m99\"\n",
                aterm_secrets::b64::encode_padded(&[7; 32])
            ),
        )
        .unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
        ConfiguredVault::read(paths).unwrap().expect("configured")
    }

    #[test]
    fn an_unedited_apply_resumes_the_configured_vault_instead_of_the_saved_one() {
        use aterm_secrets::client::{Membership, Paths, Status};
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(home.path());
        let alab = aterm_types::macos_setup::DEFAULT_VAULT_URL;
        let team = "http://vault.example:7484/v1/team";
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(ConfiguredVault::read(&paths).unwrap(), None);
        let unconfigured = snapshot_in_vault(root.path(), Ok(None), None).unwrap();
        assert_eq!(unconfigured.plan, Plan::default());

        let vault = configure_vault(&paths, team);
        let configured = Some(vault.clone());
        let snapshot = snapshot_in_vault(root.path(), Ok(configured.clone()), None).unwrap();
        assert_eq!(snapshot.plan.vault_url, team);
        assert_eq!(snapshot.plan.selection, Plan::default().selection);
        assert_eq!(
            (
                snapshot.finished,
                snapshot.revision,
                format!("{:?}", snapshot.rows)
            ),
            (
                unconfigured.finished,
                unconfigured.revision,
                format!("{:?}", unconfigured.rows)
            ),
            "the rows read the saved plan's progress"
        );
        assert!(snapshot.rows.iter().all(|row| row.status == "applied"));
        let saved = || saved_plan_record(root.path()).unwrap();
        assert_eq!(
            offered_plan(saved(), Ok(configured.clone())).vault_url,
            team
        );
        assert_eq!(
            offered_plan(saved(), Err("unreadable source.toml".into())),
            Plan::default()
        );
        assert_eq!(
            offered_plan(None, Err("unreadable source.toml".into())),
            Plan::default(),
            "an unreadable vault configuration reads as none"
        );

        let revision = restart_at(root.path(), &snapshot.plan).unwrap();
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        let ran = host.vault_url.expect("the Secrets stage ran");
        assert_eq!(ran, team);
        let status = Status {
            config: aterm_secrets::client::config(&paths).unwrap(),
            machine: None,
            membership: Membership::In { seq: 3, heard: 3 },
            names: Vec::new(),
        };
        let reads = |url: &str| {
            aterm_machines::init_secrets::selected_vault(url, home.path(), status.config.as_ref())
                .unwrap()
                .is_some()
        };
        assert!(reads(&ran), "resumes {ran}");
        assert!(!reads(alab), "joins {alab}");

        let pinned = Plan {
            vault_url: format!("{team}#{}", aterm_secrets::b64::encode_padded(&[7; 32])),
            ..Plan::default()
        };
        assert_eq!(vault.present(pinned.clone()), pinned);
        let file = ConfiguredVault {
            config: aterm_secrets::client::Config {
                source: aterm_secrets::transport::Source::Path(home.path().join("team.vault")),
                name: "team".into(),
                signer: None,
                followed: Vec::new(),
                host: "m99".into(),
            },
            home: home.path().to_path_buf(),
        };
        let typed = Plan {
            vault_url: "~/team.vault".into(),
            ..Plan::default()
        };
        assert_eq!(file.present(typed.clone()), typed);
        assert_eq!(
            file.present(Plan::default()).vault_url,
            home.path().join("team.vault").to_string_lossy()
        );
    }

    #[test]
    fn a_vault_typed_in_system_state_stays_while_setup_still_joins_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        let other = "http://other.example:7484/v1/other";
        let configured = Some(configure_vault(&paths, team));
        let shown = || snapshot_in_vault(root.path(), Ok(configured.clone()), None).unwrap();
        let apply = |plan: &Plan| {
            let typed = plan.vault_url != shown().plan.vault_url;
            system_state_apply(root.path(), plan, typed, || Ok(configured.clone()))
        };
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert_eq!(shown().plan.vault_url, team);

        let typed = Plan {
            vault_url: other.into(),
            ..Plan::default()
        };
        let failing = || Fake {
            failure: Some(Stage::Secrets),
            ..Fake::default()
        };
        let revision = apply(&typed);
        assert_eq!(shown().plan, typed);
        for pass in 0..2 {
            let mut host = failing();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(host.vault_url.as_deref(), Some(other), "pass {pass}");
            let snapshot = shown();
            assert!(!snapshot.finished);
            assert_eq!(snapshot.plan, typed, "pass {pass}");
            assert!(
                snapshot
                    .rows
                    .iter()
                    .any(|row| row.name == Stage::Secrets.name() && row.status == "pending"),
                "{:?}",
                snapshot.rows
            );
        }
        let revision = apply(&shown().plan);
        let mut host = failing();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(other));
        assert_eq!(shown().plan, typed);

        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"cancelled"}),
        )
        .unwrap();
        assert_eq!(shown().plan.vault_url, team);
    }

    #[test]
    fn an_unchecked_vault_source_cannot_block_selected_preferences() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets]);
        plan.vault_url = "unfinished source".into();
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation]);
        assert!(finished_at(root.path()).unwrap());
        let before = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.path().join(name)).unwrap());
        plan.selection.set(Setting::Secrets, true);
        assert!(restart_at(root.path(), &plan).is_err());
        let after = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.path().join(name)).unwrap());
        assert_eq!(
            before, after,
            "a selected invalid vault cannot authorize changes"
        );
    }

    #[test]
    fn an_invalid_vault_source_changes_no_authorization_and_its_error_contains_no_value() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan {
            vault_url: "https://user:fixture-private@vault.example/v1/team".into(),
            ..Plan::default()
        };
        assert!(validate_initial_setup_plan(&plan).is_err());
        let error = authorize_at(root.path(), &plan).unwrap_err();
        assert!(!error.contains("fixture-private"));
        assert!(!root.path().join("automatic-plan.json").exists());
        assert!(!root.path().join("initial-setup.json").exists());
    }

    #[test]
    fn a_plan_published_before_a_failed_progress_reset_requires_authorization() {
        let root = aterm_tempfile::tempdir().unwrap();
        let old = selected(&[Setting::Widgets, Setting::Spotlight]);
        authorize_at(root.path(), &old).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let new = selected(&[Setting::Dock, Setting::Secrets]);
        save_plan(root.path(), &new).unwrap();
        assert!(!finished_at(root.path()).unwrap());
        let (_, _, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(rows.iter().all(|row| row.status == "pending"));
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(!finished_at(root.path()).unwrap());
        assert!(authorize_at(root.path(), &new).unwrap().is_some());
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn identical_plan_restarts_retire_older_workers_but_stage_updates_keep_the_revision() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 0);
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let first = revision_at(root.path()).unwrap();
        assert_eq!(first, 1);
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first);
        authorize_at(root.path(), &plan).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first + 1);
        restart_at(root.path(), &plan).unwrap();
        let rerun = revision_at(root.path()).unwrap();
        assert_eq!(rerun, first + 2);
        host.pending = None;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(revision_at(root.path()).unwrap(), rerun);
        restart_at(root.path(), &plan).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), rerun + 1);
        assert!(!finished_at(root.path()).unwrap());
    }

    #[test]
    fn revision_reads_refuse_interrupted_plan_resets_and_counters_never_wrap() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let next = selected(&[Setting::Repositories]);
        save_plan(root.path(), &next).unwrap();
        assert!(revision_at(root.path()).is_err());
        authorize_at(root.path(), &next).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 2);
        let mut latest = progress(root.path()).unwrap().unwrap();
        latest.revision = u64::MAX;
        save_progress(root.path(), latest).unwrap();
        let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        assert!(restart_at(root.path(), &plan).is_err());
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            before
        );
        assert_eq!(saved_plan(root.path()).unwrap(), Some(next));
        assert_eq!(revision_at(root.path()).unwrap(), u64::MAX);
    }

    #[test]
    fn a_stop_between_stages_ends_the_revision_and_a_new_plan_runs_normally() {
        use std::sync::Mutex;
        static ENDED: Mutex<Vec<Vec<Setting>>> = Mutex::new(Vec::new());
        fn stage(event: StageEvent) {
            if !event.running {
                ENDED.lock().unwrap().push(event.settings.to_vec());
            }
        }
        // Stop once the first stage has ended.
        fn stop(_: u64) -> bool {
            !ENDED.lock().unwrap().is_empty()
        }
        let hooks = SetupHooks {
            stop,
            stage,
            on_screen: || true,
            developer_tools: |_, _| {},
        };
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake::default();
        let (report, next) =
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks)
                .unwrap()
                .unwrap();
        assert_eq!(host.calls, [Stage::Workstation]);
        assert_eq!(next, SetupNext::Done);
        assert!(report.rows.iter().any(|row| row.status == "stopped"));
        let stopped = progress(root.path()).unwrap().unwrap();
        assert!(stopped.stopped && stopped.admitted && !stopped.completed);
        assert_eq!(stopped.done, Stage::Workstation.bit());
        // The revision ended without cancelling the plan: nothing more runs for it.
        assert!(finished_at(root.path()).unwrap());
        assert!(admission_at(root.path()).unwrap().unwrap().completed);
        assert_eq!(saved_plan(root.path()).unwrap(), Some(plan.clone()));
        assert!(
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks)
                .unwrap()
                .is_none()
        );

        // A later admission is a new revision, and it runs normally.
        let next_revision = restart_at(root.path(), &plan).unwrap();
        assert_eq!(next_revision, revision + 1);
        assert!(!finished_at(root.path()).unwrap());
        host.calls.clear();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            next_revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(finished_at(root.path()).unwrap());
        assert!(!progress(root.path()).unwrap().unwrap().stopped);
    }

    #[test]
    fn a_row_waiting_on_a_person_parks_the_run_instead_of_retrying_it() {
        let row = |status: &str| Row {
            name: "row".into(),
            status: status.into(),
            detail: String::new(),
            elapsed_ms: None,
        };
        assert_eq!(SetupNext::of(&[row("ok"), row("applied")]), SetupNext::Done);
        assert_eq!(SetupNext::of(&[row("ok"), row(WAITING)]), SetupNext::Park);
        assert_eq!(SetupNext::of(&[row("ok"), row(UNREACHED)]), SetupNext::Park);
        assert_eq!(
            SetupNext::of(&[row(WAITING), row("blocked")]),
            SetupNext::Retry
        );
        assert_eq!(SetupNext::of(&[]), SetupNext::Retry);
        assert_eq!(SetupNext::Done.and(SetupNext::Park), SetupNext::Park);
        assert_eq!(SetupNext::Park.and(SetupNext::Retry), SetupNext::Retry);

        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Permissions, Setting::Widgets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake {
            waiting: Some(Stage::Permissions),
            ..Fake::default()
        };
        let (_, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SetupNext::Park);
        let left = progress(root.path()).unwrap().unwrap();
        assert_eq!(left.done, Stage::Workstation.bit());
        assert!(!left.completed);
        // Once the person acts, the next pass finishes only the parked stage.
        host.calls.clear();
        host.waiting = None;
        let (_, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SetupNext::Done);
        assert_eq!(host.calls, [Stage::Permissions]);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_record_from_before_the_sign_out_stage_counts_its_finished_workstation_as_signed_out() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::default();
        authorize_at(root.path(), &plan).unwrap();
        let mut done = progress(root.path()).unwrap().unwrap();
        done.done = Stage::Workstation.bit();
        save_progress(root.path(), done).unwrap();
        let path = root.path().join("initial-setup.json");
        let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record["stages"].as_u64(), Some(6));
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done,
            Stage::Workstation.bit()
        );
        record.as_object_mut().unwrap().remove("stages");
        write_json(&path, &record).unwrap();
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done,
            Stage::Workstation.bit() | Stage::Accounts.bit()
        );
    }

    #[test]
    fn a_change_to_another_group_keeps_this_approvals_access_steps() {
        let root = aterm_tempfile::tempdir().unwrap();
        let record = || -> Value {
            aterm_json::from_slice(&std::fs::read(root.path().join("initial-setup.json")).unwrap())
                .unwrap()
        };
        let first = selected(&[Setting::Permissions, Setting::Widgets]);
        let approved = authorize_at(root.path(), &first).unwrap().unwrap();
        assert_eq!(access_revision_at(root.path()).unwrap(), approved);
        assert!(record().get("access_revision").is_none());

        // Another group changes at once: a new revision, the same approval of the
        // grants, so nothing is handed to macOS again (§3.3, §6).
        let wider = selected(&[Setting::Permissions, Setting::Widgets, Setting::Dock]);
        let revision = restart_at(root.path(), &wider).unwrap();
        assert!(revision > approved);
        assert_eq!(access_revision_at(root.path()).unwrap(), approved);
        assert_eq!(record()["access_revision"].as_u64(), Some(approved));
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.access_read, Some(approved));
        assert_eq!(
            admission_at(root.path()).unwrap().unwrap().revision,
            revision
        );

        // Permissions off and on again is a new approval: this revision's own.
        restart_at(root.path(), &selected(&[Setting::Widgets])).unwrap();
        let again = restart_at(root.path(), &first).unwrap();
        assert_eq!(access_revision_at(root.path()).unwrap(), again);
        assert!(record().get("access_revision").is_none());

        // A record never names an access revision after its own.
        let mut bad = record();
        bad.as_object_mut()
            .unwrap()
            .insert("access_revision".into(), Value::from(again + 1));
        write_json(&root.path().join("initial-setup.json"), &bad).unwrap();
        assert!(progress(root.path()).is_err());
    }

    #[test]
    fn the_sign_outs_wait_for_system_state_on_screen_and_park_without_it() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Messages]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake {
            accounts_pending: true,
            ..Fake::default()
        };
        let hidden = SetupHooks {
            on_screen: || false,
            ..SetupHooks::NONE
        };
        let (report, next) =
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hidden)
                .unwrap()
                .unwrap();
        // The sign-outs did not run: they wait, and the run parks rather than retries.
        assert_eq!(host.calls, [Stage::Workstation]);
        assert_eq!(next, SetupNext::Park);
        assert!(report.rows.iter().any(|row| row.status == WAITING));
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Accounts.bit(),
            0
        );
        // With System State on screen they run.
        let shown = SetupHooks {
            on_screen: || true,
            ..SetupHooks::NONE
        };
        host.calls.clear();
        let (_, next) =
            execute_initial_for_revision(&mut host, root.path(), revision, &mut None, shown)
                .unwrap()
                .unwrap();
        assert_eq!(host.calls, [Stage::Accounts]);
        assert_eq!(next, SetupNext::Done);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_legacy_run_that_waits_on_a_person_parks_like_a_modern_one() {
        let root = aterm_tempfile::tempdir().unwrap();
        save_plan(root.path(), &Plan::default()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"pending"}),
        )
        .unwrap();
        assert!(progress(root.path()).unwrap().is_none());
        let mut host = Fake {
            waiting: Some(Stage::Workstation),
            ..Fake::default()
        };
        let (report, next) = execute_pass(&mut host, root.path(), &mut None, SetupHooks::NONE)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(next, SetupNext::Park);
        let mut host = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        assert!(execute_pass(&mut host, root.path(), &mut None, SetupHooks::NONE).is_err());
    }

    #[test]
    fn the_same_plan_again_stays_provisional_and_a_new_one_is_the_persons() {
        let root = aterm_tempfile::tempdir().unwrap();
        let automatic = selected(&[Setting::Permissions]);
        save_plan_as(root.path(), &automatic, true).unwrap();
        restart_at(root.path(), &automatic).unwrap();
        assert!(saved_plan_provisional(root.path()).unwrap());
        let shaped = selected(&[Setting::Permissions, Setting::Widgets]);
        restart_at(root.path(), &shaped).unwrap();
        assert!(!saved_plan_provisional(root.path()).unwrap());
        // Shaping it back to what aterm chose is still the person's plan.
        restart_at(root.path(), &automatic).unwrap();
        assert!(!saved_plan_provisional(root.path()).unwrap());
    }

    #[test]
    fn a_group_choice_back_to_the_provisional_plan_is_the_persons_and_set_up_now_is_not() {
        let root = aterm_tempfile::tempdir().unwrap();
        let automatic = selected(&[Setting::Permissions]);
        save_plan_as(root.path(), &automatic, true).unwrap();
        // Keyboard & Trackpad to Set up and back, while busy: the first choice
        // was overtaken unsaved, and the second admits aterm's own plan again.
        let mut choice = InitialSetupRestart::chosen();
        let admitted = restart_attempt(
            &mut choice,
            root.path(),
            &automatic,
            save_progress,
            observe_restart,
        );
        assert!(admitted.result.unwrap().is_some());
        let records = Records::read(root.path()).unwrap();
        assert_eq!(records.plan, Some((automatic.clone(), true)));
        assert!(
            !records.provisional,
            "a later ALab signal must not widen groups the person left as is"
        );

        // Set up now runs the plan as it stands: a provisional one stays so.
        save_plan_as(root.path(), &automatic, true).unwrap();
        let mut again = InitialSetupRestart::default();
        let admitted = restart_attempt(
            &mut again,
            root.path(),
            &automatic,
            save_progress,
            observe_restart,
        );
        assert!(admitted.result.unwrap().is_some());
        assert!(saved_plan_provisional(root.path()).unwrap());
    }

    #[test]
    fn a_stop_asked_for_while_a_stage_runs_is_never_resumed_after_a_quit() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Repositories, Setting::Messages]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake {
            stop_during: Some((Stage::Repositories, root.path().to_owned())),
            ..Fake::default()
        };
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        // Had the process ended inside Repositories, the next launch would
        // read the run as over: the window's startup finishes instead of
        // resuming it, and the sign-outs never run.
        let relaunch = host.relaunch.take().expect("the Stop came mid-stage");
        assert!(relaunch.finished, "{relaunch:?}");
        assert!(relaunch.snapshot_finished, "{relaunch:?}");
        let admission = relaunch.admission.expect("the run stays admitted");
        assert_eq!(admission.revision, revision);
        assert!(admission.completed, "a stopped run has nothing to resume");
        // This process lived on: the pass stopped at the next boundary and
        // folded the request into the run's own record.
        assert_eq!(host.calls, [Stage::Workstation, Stage::Repositories]);
        assert_eq!(next, SetupNext::Done);
        assert!(report.rows.iter().any(|row| row.status == "stopped"));
        let stopped = progress(root.path()).unwrap().unwrap();
        assert!(stopped.stopped && !stopped.completed);
        assert_eq!(stop_request_at(root.path()), None);
        assert!(finished_at(root.path()).unwrap());
        assert!(admission_at(root.path()).unwrap().unwrap().completed);
    }

    #[test]
    fn a_stop_request_is_folded_by_the_next_lock_holder_and_an_older_ones_is_spent() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        let first = authorize_at(root.path(), &plan).unwrap().unwrap();
        // The Stop of a run whose process ended before any boundary: the next
        // pass folds it before it runs anything.
        request_stop_at(root.path(), first).unwrap();
        let mut host = Fake::default();
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                first,
                &mut None,
                SetupHooks::NONE
            )
            .unwrap()
            .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(progress(root.path()).unwrap().unwrap().stopped);
        assert_eq!(stop_request_at(root.path()), None);

        // A request for a revision a person's later choice replaced is ignored
        // by every reader, and the next pass removes it and runs normally.
        let second = restart_at(root.path(), &plan).unwrap();
        request_stop_at(root.path(), first).unwrap();
        assert!(!finished_at(root.path()).unwrap());
        assert!(!admission_at(root.path()).unwrap().unwrap().completed);
        assert!(!status_at(root.path(), Plan::default()).unwrap().1);
        execute_initial_for_revision(&mut host, root.path(), second, &mut None, SetupHooks::NONE)
            .unwrap();
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert_eq!(stop_request_at(root.path()), None);
        assert!(!progress(root.path()).unwrap().unwrap().stopped);

        // A restart folds a Stop of the run it replaces, then runs its own.
        let third = restart_at(root.path(), &plan).unwrap();
        request_stop_at(root.path(), third).unwrap();
        let fourth = restart_at(root.path(), &plan).unwrap();
        assert_eq!(fourth, third + 1);
        assert_eq!(stop_request_at(root.path()), None);
        assert!(!finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_late_old_stop_never_replaces_the_current_runs_stop() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let first = authorize_at(root.path(), &plan).unwrap().unwrap();
        let second = restart_at(root.path(), &plan).unwrap();
        request_stop_at(root.path(), first).unwrap();
        assert!(
            !finished_at(root.path()).unwrap(),
            "an old Stop alone cannot stop a newer run"
        );
        request_stop_at(root.path(), second).unwrap();
        assert!(finished_at(root.path()).unwrap());
        request_stop_at(root.path(), first).unwrap();
        assert!(
            finished_at(root.path()).unwrap(),
            "a delayed old process must not erase the current Stop"
        );
        assert_eq!(
            stop_request_at(root.path()),
            Some(StopRequest::Revision(second))
        );
        let mut host = Fake::default();
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                second,
                &mut None,
                SetupHooks::NONE
            )
            .unwrap()
            .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(progress(root.path()).unwrap().unwrap().stopped);
    }

    #[test]
    fn settling_an_old_stop_never_unlinks_a_new_request_published_meanwhile() {
        use std::sync::mpsc;
        use std::time::Duration;
        let root = aterm_tempfile::tempdir().unwrap();
        request_stop_at(root.path(), 1).unwrap();
        let path = root.path().to_owned();
        let (go, begin) = mpsc::sync_channel(0);
        let (entered, entering) = mpsc::sync_channel(1);
        let publisher = std::thread::spawn(move || {
            begin
                .recv_timeout(Duration::from_secs(60))
                .expect("settler reached its removal boundary");
            let lock = HeldLock::try_take_named(&path, STOP_REQUEST_LOCK).unwrap();
            if lock.is_some() {
                // No settler excludes this writer: publish in the open gap.
                drop(lock);
                request_stop_at(&path, 2).unwrap();
                entered.send(()).unwrap();
            } else {
                // The settler excludes publication until its compare/unlink
                // finishes. The actual writer then waits for that short lock.
                entered.send(()).unwrap();
                request_stop_at(&path, 2).unwrap();
            }
        });
        settle_stop_request_with(root.path(), StopRequest::Revision(1), || {
            go.send(()).unwrap();
            entering
                .recv_timeout(Duration::from_secs(60))
                .expect("publisher reached the Stop protocol");
            Ok(())
        })
        .unwrap();
        publisher.join().expect("new Stop published");
        assert_eq!(
            stop_request_at(root.path()),
            Some(StopRequest::Revision(2)),
            "settling the old request must leave the newer publication intact"
        );
    }

    #[test]
    fn a_retained_stop_is_confirmed_durable_under_the_short_lock() {
        let root = aterm_tempfile::tempdir().unwrap();
        request_stop_at(root.path(), 9).unwrap();
        let original = std::fs::read(root.path().join(STOP_REQUEST)).unwrap();
        for revision in [8, 9] {
            let mut flushed = false;
            let result = request_stop_with_flush(root.path(), revision, |_, _| {
                assert!(
                    HeldLock::try_take_named(root.path(), STOP_REQUEST_LOCK)
                        .unwrap()
                        .is_none(),
                    "retained publication stays locked through durability confirmation"
                );
                flushed = true;
                Err(std::io::Error::other(
                    "injected Stop directory flush failure",
                ))
            });
            assert!(
                result.is_err(),
                "a visible request alone is not a saved Stop"
            );
            assert!(flushed);
            assert_eq!(
                std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
                original
            );
        }
        request_stop_at(root.path(), 8).unwrap();
        assert_eq!(
            std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
            original
        );
    }

    #[test]
    fn an_unreadable_stop_is_never_replaced_or_reported_saved() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let invalid = b"unreadable Stop request";
        aterm_machines::port::write_atomic(&root.path().join(STOP_REQUEST), invalid, 0o600)
            .unwrap();
        assert!(request_stop_at(root.path(), revision).is_err());
        assert_eq!(
            std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
            invalid
        );
        assert!(
            finished_at(root.path()).unwrap(),
            "an unreadable Stop never releases setup"
        );
        let mut host = Fake::default();
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE
            )
            .unwrap()
            .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(progress(root.path()).unwrap().unwrap().stopped);
    }

    #[test]
    fn saving_a_stop_never_waits_for_the_running_profile_pass() {
        use std::sync::mpsc;
        use std::time::Duration;
        let root = aterm_tempfile::tempdir().unwrap();
        let profile = HeldLock::take(root.path()).unwrap();
        let path = root.path().to_owned();
        let (saved, saving) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            saved.send(request_stop_at(&path, 7)).unwrap();
        });
        saving
            .recv_timeout(Duration::from_secs(60))
            .expect("Stop is saved while the profile lock remains held")
            .unwrap();
        assert!(HeldLock::try_take(root.path()).unwrap().is_none());
        assert_eq!(stop_request_at(root.path()), Some(StopRequest::Revision(7)));
        drop(profile);
        worker.join().unwrap();
    }

    #[test]
    fn a_stop_during_the_last_stage_of_a_run_that_finishes_comes_too_late() {
        // Permissions is in the plan, so the snapshot's permission policy says
        // whether the run reads as stopped (a stopped run allows no access step).
        let plan = selected(&[Setting::Widgets, Setting::Permissions, Setting::Messages]);
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake {
            stop_during: Some((Stage::Accounts, root.path().to_owned())),
            ..Fake::default()
        };
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        // The sign-outs were already running when Stop was pressed, and they
        // finished: the run is completed, not stopped, and the request is spent.
        assert_eq!(
            host.calls,
            [Stage::Workstation, Stage::Permissions, Stage::Accounts]
        );
        assert_eq!(next, SetupNext::Done);
        assert!(report.rows.iter().all(|row| row.status != "stopped"));
        let done = progress(root.path()).unwrap().unwrap();
        assert!(done.completed && !done.stopped);
        assert_eq!(stop_request_at(root.path()), None);
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished);
        assert!(
            snapshot.permissions_allowed,
            "a completed run is not stopped"
        );
        // A Stop that lands after the last stage, before the worker lets the
        // run go, is as late: every reader still reads the run as completed.
        request_stop_at(root.path(), revision).unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished && snapshot.permissions_allowed);
        assert!(admission_at(root.path()).unwrap().unwrap().completed);

        // A Stop during the last stage of a pass that leaves work (the stage
        // waits on a person) stops the run at the pass's end: no later pass of
        // it runs, and the request is folded in.
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake {
            stop_during: Some((Stage::Accounts, root.path().to_owned())),
            waiting: Some(Stage::Accounts),
            ..Fake::default()
        };
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SetupNext::Done);
        assert!(report.rows.iter().any(|row| row.status == "stopped"));
        let stopped = progress(root.path()).unwrap().unwrap();
        assert!(stopped.stopped && !stopped.completed);
        assert_eq!(stop_request_at(root.path()), None);
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.finished && !snapshot.permissions_allowed);
    }

    mod vault {
        use super::*;
        use aterm_machines::init_secrets::SecretsOps;
        use aterm_secrets::client::{Config, JoinOutcome, Look, Membership, ProbeOutcome, Status};
        use aterm_secrets::service::State;
        use aterm_secrets::transport::Source;
        use std::sync::{Arc, Mutex};

        pub(super) const NO_ROUTE: &str =
            "fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)";

        pub(super) struct Lan {
            pub(super) answers: Mutex<bool>,
            pub(super) network: Mutex<String>,
            status: Mutex<Status>,
            subscribed: Mutex<bool>,
            pub(super) asked: Mutex<u32>,
            pub(super) refusal: Mutex<Option<String>>,
            pub(super) fleeting: Mutex<Option<PathBuf>>,
            install_error: Mutex<Option<String>>,
            pub(super) subscriber_error_once: Mutex<bool>,
            subscriber_error_after_install: Mutex<bool>,
            pub(super) status_error_once: Mutex<bool>,
            pub(super) routes_error_once: Mutex<bool>,
            log: Mutex<Vec<String>>,
        }

        impl Lan {
            pub(super) fn new(answers: bool, network: &str) -> Arc<Self> {
                Arc::new(Self {
                    answers: Mutex::new(answers),
                    network: Mutex::new(network.into()),
                    status: Mutex::new(Status {
                        config: None,
                        machine: None,
                        membership: Membership::Unconfigured,
                        names: Vec::new(),
                    }),
                    subscribed: Mutex::new(false),
                    asked: Mutex::new(0),
                    refusal: Mutex::new(None),
                    fleeting: Mutex::new(None),
                    install_error: Mutex::new(None),
                    subscriber_error_once: Mutex::new(false),
                    subscriber_error_after_install: Mutex::new(false),
                    status_error_once: Mutex::new(false),
                    routes_error_once: Mutex::new(false),
                    log: Mutex::new(Vec::new()),
                })
            }

            pub(super) fn log(&self) -> Vec<String> {
                self.log.lock().unwrap().clone()
            }

            pub(super) fn approve(&self, heard: u64) {
                let mut status = self.status.lock().unwrap();
                status.membership = Membership::In { seq: 7, heard };
                status.names = vec!["GH_TOKEN".into()];
            }

            pub(super) fn status_now(&self) -> Status {
                self.status.lock().unwrap().clone()
            }

            pub(super) fn set(&self, status: Status, subscribed: bool) {
                *self.status.lock().unwrap() = status;
                *self.subscribed.lock().unwrap() = subscribed;
            }

            pub(super) fn in_vault(&self, source: &str, heard: u64) {
                let mut status = member(source, Membership::In { seq: 7, heard });
                status.names = vec!["GH_TOKEN".into()];
                self.set(status, false);
            }
        }

        fn member(source: &str, membership: Membership) -> Status {
            let recipient = aterm_secrets::age::Identity::from_secret_bytes([31; 32]).recipient();
            let code = aterm_secrets::client::join_code(&recipient);
            let (source, pin) =
                aterm_secrets::transport::parse_source(source, Path::new("/test-home")).unwrap();
            Status {
                config: Some(Config {
                    name: aterm_secrets::transport::vault_name(&source).unwrap(),
                    source,
                    signer: Some(pin.unwrap_or([3; 32])),
                    followed: Vec::new(),
                    host: "m22".into(),
                }),
                machine: Some((recipient, code)),
                membership,
                names: Vec::new(),
            }
        }

        pub(super) struct LanReads {
            pub(super) lan: Arc<Lan>,
            pub(super) vault: bool,
            pub(super) build: String,
            pub(super) now: u64,
        }

        impl secrets_retry::Reads for LanReads {
            fn build(&self) -> String {
                self.build.clone()
            }
            fn now(&self) -> u64 {
                self.now
            }
            fn routes(&self) -> Option<String> {
                if std::mem::take(&mut *self.lan.routes_error_once.lock().unwrap()) {
                    return None;
                }
                Some(self.lan.network.lock().unwrap().clone())
            }
            fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
                self.vault
                    .then(|| Arc::clone(&self.lan) as Arc<dyn SecretsOps>)
            }
        }

        struct MaterializationReads {
            lan: Arc<Lan>,
            paths: aterm_secrets::client::Paths,
        }

        impl MaterializationReads {
            fn new(paths: aterm_secrets::client::Paths) -> Self {
                Self {
                    lan: Lan::new(true, "unchanged scratch interface"),
                    paths,
                }
            }

            fn pace(&self) -> Pace {
                self.lan
                    .set(aterm_secrets::client::status(&self.paths).unwrap(), true);
                secrets_retry::pace(self)
            }

            fn park(&self, root: &Path) -> InitialSetupParkMark {
                initial_setup_park_mark_at(Some(root), Some(&self.paths), 1_000, || None)
            }
        }

        impl secrets_retry::Reads for MaterializationReads {
            fn build(&self) -> String {
                "scratch-materialization-build".into()
            }
            fn now(&self) -> u64 {
                1_000
            }
            fn routes(&self) -> Option<String> {
                Some("unchanged scratch interface".into())
            }
            fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
                Some(Arc::clone(&self.lan) as Arc<dyn SecretsOps>)
            }
            fn paths(&self) -> Option<aterm_secrets::client::Paths> {
                Some(self.paths.clone())
            }
        }

        fn hold_materialization_attempt(root: &Path, revision: u64, pace: &Pace) {
            for _ in 0..2 {
                let mut rows = vec![Row {
                    name: Stage::Secrets.name().into(),
                    status: "error".into(),
                    detail: "injected failed write of the scratch vault".into(),
                    elapsed_ms: None,
                }];
                secrets_retry::record(root, revision, pace, false, &mut rows).unwrap();
            }
            assert!(secrets_retry::paused(root, revision, pace).is_some());
        }

        fn without_heartbeat(mut status: Status) -> Status {
            if let Membership::In { heard, .. } = &mut status.membership {
                *heard = 0;
            }
            status
        }

        #[test]
        fn a_same_signed_version_materialization_change_releases_the_held_stage() {
            use aterm_secrets::client::{self, Paths};
            let scratch = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&scratch.path().join("home"));
            let source = scratch.path().join("scratch.vault");
            client::create(&paths, source.to_str().unwrap(), None, Some("scratch")).unwrap();
            let opened = client::admin_open(&paths).unwrap();
            let mut doc = opened.doc.clone();
            doc.set_entry(
                &aterm_secrets::text::Audience::Everyone,
                "gh_access_token",
                b"inert-signed-test-value",
            )
            .unwrap();
            client::admin_publish(&paths, &opened, &doc).unwrap();
            client::sync(&paths, None).unwrap();
            let tracked = paths.home.join(".secrets/gh_access_token");
            std::fs::remove_file(&tracked).unwrap();
            assert!(!client::materialization_ready(&paths).unwrap());
            let status = without_heartbeat(client::status(&paths).unwrap());
            assert!(matches!(status.membership, Membership::In { .. }));
            let root = scratch.path().join("doctor");
            let plan = Plan {
                vault_url: source.to_string_lossy().into_owned(),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let revision = authorize_at(&root, &plan).unwrap().unwrap();
            let reads = MaterializationReads::new(paths.clone());
            let before = reads.pace();
            let parked = reads.park(&root);
            hold_materialization_attempt(&root, revision, &before);
            let mut held = Fake {
                pace: Some(reads.pace()),
                ..Fake::default()
            };
            execute_initial_for_revision(&mut held, &root, revision, &mut None, SetupHooks::NONE)
                .unwrap();
            assert!(held.calls.is_empty(), "the unchanged failed try stays held");
            assert_eq!(parked, reads.park(&root));

            let writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(paths.dir.join(".lock"))
                .unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "a scratch writer excludes the observer; this File's Drop releases the lock"
            )]
            writer.lock().unwrap();
            let unknown = reads.pace();
            assert!(unknown.materialization.is_none(), "the writer is busy");
            assert!(secrets_retry::paused(&root, revision, &unknown).is_some());
            let mut contended = reads.park(&root);
            assert!(!contended.facts_changed_since(&parked));
            contended.retain_known_from(&parked);
            assert_eq!(parked, contended, "writer contention does not wake setup");
            drop(writer);
            assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_some());

            client::sync(&paths, None).unwrap();
            assert!(client::materialization_ready(&paths).unwrap());
            assert_eq!(status, without_heartbeat(client::status(&paths).unwrap()));
            assert_ne!(parked, reads.park(&root), "the real repair wakes setup");
            assert!(reads.park(&root).facts_changed_since(&parked));
            let mut repaired = Fake {
                pace: Some(reads.pace()),
                ..Fake::default()
            };
            let (_, next) = execute_initial_for_revision(
                &mut repaired,
                &root,
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                repaired.calls,
                [Stage::Secrets, Stage::Repositories],
                "a real same-version repair must release the old held outcome"
            );
            assert_eq!(next, SetupNext::Done);
            let mut completed = reads.park(&root);
            completed.retain_known_from(&parked);
            assert_ne!(completed, parked, "completion ends the old watch");
            assert!(completed.materialization.is_none());

            let stopped_root = scratch.path().join("stopped-doctor");
            let stopped = authorize_at(&stopped_root, &plan).unwrap().unwrap();
            let before_stop = reads.park(&stopped_root);
            request_stop_at(&stopped_root, stopped).unwrap();
            let mut after_stop = reads.park(&stopped_root);
            after_stop.retain_known_from(&before_stop);
            assert_ne!(after_stop, before_stop, "Stop ends the old watch");
            assert!(after_stop.materialization.is_none());
            let mut stopped_host = Fake {
                pace: Some(reads.pace()),
                ..Fake::default()
            };
            assert!(
                execute_initial_for_revision(
                    &mut stopped_host,
                    &stopped_root,
                    stopped,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .is_none()
            );
            assert!(stopped_host.calls.is_empty(), "a repair never revives Stop");

            restart_at(&stopped_root, &plan).unwrap();
            let older_run = reads.park(&stopped_root);
            restart_at(&stopped_root, &plan).unwrap();
            let writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(paths.dir.join(".lock"))
                .unwrap();
            #[expect(
                clippy::disallowed_methods,
                reason = "a scratch writer excludes the observer; this File's Drop releases the lock"
            )]
            writer.lock().unwrap();
            let mut newer_run = reads.park(&stopped_root);
            newer_run.retain_known_from(&older_run);
            assert_ne!(newer_run, older_run, "a new run ends the old watch");
            assert!(newer_run.materialization.is_none());
            drop(writer);
        }

        #[test]
        fn a_plain_same_entry_materialization_change_wakes_and_releases_the_pause() {
            use aterm_secrets::client::{self, JoinOptions, Paths};
            let scratch = aterm_tempfile::tempdir().unwrap();
            let source = scratch.path().join("scratch.secrets");
            aterm_machines::port::write_atomic(
                &source,
                b"gh_access_token=inert-plain-first\n",
                0o600,
            )
            .unwrap();
            let paths = Paths::for_home(&scratch.path().join("home"));
            client::join(
                &paths,
                &JoinOptions {
                    source: source.to_string_lossy().into_owned(),
                    host: Some("scratch".into()),
                    interactive: false,
                    wait: std::time::Duration::ZERO,
                    confirm: &|_| false,
                    password: aterm_secrets::client::JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
            assert!(client::materialization_ready(&paths).unwrap());
            let status = client::status(&paths).unwrap();
            let root = scratch.path().join("doctor");
            let plan = Plan {
                vault_url: source.to_string_lossy().into_owned(),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let revision = authorize_at(&root, &plan).unwrap().unwrap();
            let reads = MaterializationReads::new(paths.clone());
            let before = reads.pace();
            let parked = reads.park(&root);
            hold_materialization_attempt(&root, revision, &before);
            client::sync(&paths, None).unwrap();
            assert_eq!(parked, reads.park(&root), "an unchanged sync stays parked");
            assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_some());

            aterm_machines::port::write_atomic(
                &source,
                b"gh_access_token=inert-plain-second\n",
                0o600,
            )
            .unwrap();
            client::sync(&paths, None).unwrap();
            assert!(client::materialization_ready(&paths).unwrap());
            assert_eq!(status, client::status(&paths).unwrap());
            assert_ne!(
                parked,
                reads.park(&root),
                "new credential bytes under the same entry name must wake setup"
            );
            assert!(reads.park(&root).facts_changed_since(&parked));
            assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_none());
        }

        enum SubscriberReadFailure {
            None,
            BeforeHeldTry,
            AfterFirstInstall,
            AfterNetworkChange,
        }

        fn subscriber_recovery_after_install_failures(read_failure: SubscriberReadFailure) {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets, Setting::Repositories]);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let lan = Lan::new(true, "unchanged network");
            lan.in_vault(&plan.vault_url, START);
            *lan.install_error.lock().unwrap() = Some("injected subscriber install refusal".into());
            *lan.subscriber_error_after_install.lock().unwrap() =
                matches!(read_failure, SubscriberReadFailure::AfterFirstInstall);
            let mut host = Vaulted::new(&lan);
            let mut password = Some(join_password());
            // The worker parks after ten retry passes; only its first two can
            // reach the failed change, through the actual Secrets item.
            for minute in 0..10 {
                host.now = START + minute * 60;
                let (report, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut password,
                    SetupHooks::NONE,
                )
                .unwrap()
                .unwrap();
                if minute == 0 && matches!(read_failure, SubscriberReadFailure::AfterFirstInstall) {
                    assert!(host.pace_reads[0].is_some(), "the first inputs were known");
                    assert!(host.pace_reads[1].is_none(), "the post-Pace read failed");
                    let attempt: Value = aterm_json::from_slice(
                        &std::fs::read(root.path().join(secrets_retry::RECORD)).unwrap(),
                    )
                    .unwrap();
                    assert_eq!(attempt["marks"].as_str(), host.pace_reads[0].as_deref());
                    assert_eq!(attempt["failures"].as_u64(), Some(1));
                }
                assert!(report.needs_attention());
                assert_eq!(next, SetupNext::Retry);
            }
            assert_eq!(host.minutes(), [0, 1]);
            assert!(
                host.rest.calls.is_empty(),
                "repositories still wait for Secrets"
            );
            assert_eq!(
                lan.log().iter().filter(|line| *line == "install").count(),
                2
            );
            let status = lan.status_now();
            let before_pace = host.secrets_pace().unwrap();
            let mark_with = |subscriber| {
                initial_setup_park_mark_reading(
                    Some(root.path()),
                    Some(lan.status_now()),
                    START + 600,
                    || Some(Vec::new()),
                    || None,
                    || subscriber,
                )
            };
            let mark = || mark_with(lan.subscriber().ok().map(secrets_retry::subscriber_running));
            let parked = mark();
            assert_eq!(parked.retry_due(), None, "two failures hold the timer");
            assert_eq!(parked, mark(), "stable service state must stay parked");
            let mut unknown = mark_with(None);
            assert!(!unknown.facts_changed_since(&parked));
            unknown.retain_known_from(&parked);
            assert_eq!(unknown, parked, "an unreadable service is no new fact");

            if matches!(read_failure, SubscriberReadFailure::BeforeHeldTry) {
                // Only the pre-Pace read fails. The actual item's probe and
                // post-Pace see the same absent subscriber as both failed tries.
                *lan.subscriber_error_once.lock().unwrap() = true;
                host.now = START + 600;
                let (report, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut password,
                    SetupHooks::NONE,
                )
                .unwrap()
                .unwrap();
                assert!(report.needs_attention());
                assert_eq!(next, SetupNext::Retry);
                assert!(!*lan.subscriber_error_once.lock().unwrap());
                assert_eq!(lan.subscriber().unwrap(), State::Absent);
                assert_eq!(lan.status_now(), status);
                assert_eq!(host.secrets_pace().unwrap().marks, before_pace.marks);
                assert_eq!(parked, mark());
                assert!(host.rest.calls.is_empty());
            }

            let expected_installs =
                if matches!(read_failure, SubscriberReadFailure::AfterNetworkChange) {
                    *lan.network.lock().unwrap() = "new local network".into();
                    for minute in 10..20 {
                        *lan.subscriber_error_once.lock().unwrap() = true;
                        host.now = START + minute * 60;
                        let (report, next) = execute_initial_for_revision(
                            &mut host,
                            root.path(),
                            revision,
                            &mut password,
                            SetupHooks::NONE,
                        )
                        .unwrap()
                        .unwrap();
                        assert!(report.needs_attention());
                        assert_eq!(next, SetupNext::Retry);
                        assert!(!*lan.subscriber_error_once.lock().unwrap());
                        assert_eq!(lan.status_now(), status);
                        assert!(host.rest.calls.is_empty());
                    }
                    assert_eq!(
                        lan.log().iter().filter(|line| *line == "install").count(),
                        4,
                        "the known new network permits two tries even while subscriber reads fail"
                    );
                    let known = host.secrets_pace().unwrap();
                    assert!(secrets_retry::paused(root.path(), revision, &known).is_some());
                    host.now = START + 1_200;
                    execute_initial_for_revision(
                        &mut host,
                        root.path(),
                        revision,
                        &mut password,
                        SetupHooks::NONE,
                    )
                    .unwrap();
                    assert_eq!(host.minutes(), [0, 1, 10, 11]);
                    4
                } else {
                    2
                };

            // launchd, the terminal's agent-on or the existing mend can start
            // this same subscriber without changing any vault files/version.
            *lan.install_error.lock().unwrap() = None;
            lan.set(status.clone(), true);
            assert_eq!(lan.status_now(), status);
            let after_pace = host.secrets_pace().unwrap();
            assert_eq!(before_pace.materialization, after_pace.materialization);
            assert_ne!(before_pace.marks, after_pace.marks);
            let repaired = mark();
            assert!(repaired.facts_changed_since(&parked));
            let mut unknown = mark_with(None);
            unknown.retain_known_from(&repaired);
            assert_eq!(
                unknown, repaired,
                "running stays known through a read failure"
            );

            host.now = START + 1_200;
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            assert!(rows_ready(&report.rows), "{:?}", report.rows);
            assert_eq!(next, SetupNext::Done);
            assert_eq!(host.rest.calls, [Stage::Repositories]);
            assert!(finished_at(root.path()).unwrap());
            assert_eq!(revision_at(root.path()).unwrap(), revision);
            assert!(
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut password,
                    SetupHooks::NONE,
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(
                host.rest.calls,
                [Stage::Repositories],
                "no second repository pass"
            );
            assert_ne!(
                parked, repaired,
                "the service repair must wake the parked worker"
            );

            let no_pending_reads = || {
                initial_setup_park_mark_reading(
                    Some(root.path()),
                    Some(lan.status_now()),
                    START + 600,
                    || panic!("no pending Secrets interface read"),
                    || panic!("no pending materialization read"),
                    || panic!("no pending subscriber read"),
                )
            };
            let completed = no_pending_reads();
            let mut kept = completed.clone();
            kept.retain_known_from(&repaired);
            assert_eq!(
                kept, completed,
                "completion retires the service observation"
            );

            let next_revision = restart_at(root.path(), &plan).unwrap();
            assert!(next_revision > revision);
            let next = mark_with(None);
            let mut kept = next.clone();
            kept.retain_known_from(&repaired);
            assert_eq!(
                kept, next,
                "a new run cannot borrow the older service state"
            );
            request_stop_at(root.path(), next_revision).unwrap();
            let stopped = no_pending_reads();
            let mut kept = stopped.clone();
            kept.retain_known_from(&repaired);
            assert_eq!(kept, stopped, "Stop retires the service observation");
            assert!(password.is_some(), "no join consumes the retained password");
            assert!(lan.log().iter().all(|line| !line.starts_with("join ")));
            assert_eq!(
                lan.log().iter().filter(|line| *line == "install").count(),
                expected_installs,
                "an unreadable subscriber is no new fact permitting another failed install"
            );
        }

        #[test]
        fn subscriber_recovery_wakes_held_secrets_and_unblocks_repositories() {
            subscriber_recovery_after_install_failures(SubscriberReadFailure::None);
        }

        #[test]
        fn subscriber_read_failure_does_not_release_a_held_install() {
            subscriber_recovery_after_install_failures(SubscriberReadFailure::BeforeHeldTry);
        }

        #[test]
        fn subscriber_read_failure_after_the_first_install_preserves_its_failure_budget() {
            subscriber_recovery_after_install_failures(SubscriberReadFailure::AfterFirstInstall);
        }

        #[test]
        fn subscriber_read_failure_does_not_hide_a_known_network_change() {
            subscriber_recovery_after_install_failures(SubscriberReadFailure::AfterNetworkChange);
        }

        impl SecretsOps for Lan {
            fn status(&self) -> aterm_secrets::Result<Status> {
                if std::mem::take(&mut *self.status_error_once.lock().unwrap()) {
                    return Err(aterm_secrets::Error::new(
                        "injected temporary vault-state read failure",
                    ));
                }
                Ok(self.status.lock().unwrap().clone())
            }
            fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
                Ok(self.status.lock().unwrap().config.clone())
            }
            fn subscriber(&self) -> aterm_secrets::Result<State> {
                if std::mem::take(&mut *self.subscriber_error_once.lock().unwrap()) {
                    return Err(aterm_secrets::Error::new(
                        "injected temporary subscriber-state read failure",
                    ));
                }
                Ok(if *self.subscribed.lock().unwrap() {
                    State::Installed {
                        pid: Some(4242),
                        unit: PathBuf::from("/test-home/Library/LaunchAgents/subscriber.plist"),
                    }
                } else {
                    State::Absent
                })
            }
            fn terminal(&self) -> bool {
                false
            }
            fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
                Err(aterm_secrets::Error::new(
                    "the setup never looks for a vault",
                ))
            }
            fn reachable(&self, _source: &Source) -> Look {
                *self.asked.lock().unwrap() += 1;
                if *self.answers.lock().unwrap() {
                    Look::Answers
                } else {
                    Look::Held(NO_ROUTE.into())
                }
            }
            fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
                if let Replaces::Read(expected) = replaces
                    && self.status.lock().unwrap().config != *expected
                {
                    return Err(aterm_secrets::Error::new("the fixture's vault changed"));
                }
                self.log
                    .lock()
                    .unwrap()
                    .push(format!("keep {}", into.display()));
                Ok(())
            }
            fn join(
                &self,
                _sys: &dyn aterm_machines::port::System,
                (source, replaces): (&str, &Replaces),
                _password: Option<&SecretBytes>,
                interactive: bool,
                _wait: std::time::Duration,
                requested: &dyn Fn() -> bool,
            ) -> aterm_secrets::Result<JoinOutcome> {
                assert!(!interactive, "the setup's join is unattended");
                self.log.lock().unwrap().push(format!("join {source}"));
                if let Some(why) = self.refusal.lock().unwrap().clone() {
                    return Err(aterm_secrets::Error::new(why));
                }
                let status = member(source, Membership::Pending);
                let code = status.machine.as_ref().map(|(_, code)| code.clone());
                {
                    let mut current = self.status.lock().unwrap();
                    if let Replaces::Read(expected) = replaces
                        && current.config != *expected
                    {
                        return Err(aterm_secrets::Error::new("the fixture's vault changed"));
                    }
                    *current = status;
                }
                requested();
                Ok(JoinOutcome::Pending {
                    code: code.expect("a join makes a key"),
                    host: "m22".into(),
                })
            }
            fn install_subscriber(&self) -> aterm_secrets::Result<String> {
                self.log.lock().unwrap().push("install".into());
                if let Some(exe) = self.fleeting() {
                    aterm_secrets::service::service_program(&exe, &|_| false)?;
                }
                if let Some(why) = self.install_error.lock().unwrap().as_ref() {
                    if std::mem::take(&mut *self.subscriber_error_after_install.lock().unwrap()) {
                        *self.subscriber_error_once.lock().unwrap() = true;
                    }
                    return Err(aterm_secrets::Error::new(why));
                }
                *self.subscribed.lock().unwrap() = true;
                Ok("installed the subscriber".into())
            }
            fn turned_off(&self) -> bool {
                false
            }
            fn started(&self) -> Result<(), String> {
                Ok(())
            }
            fn fleeting(&self) -> Option<PathBuf> {
                self.fleeting.lock().unwrap().clone()
            }
            fn synced(&self) -> aterm_secrets::Result<bool> {
                Ok(true)
            }
            fn sync(&self) -> aterm_secrets::Result<()> {
                self.log.lock().unwrap().push("sync".into());
                Ok(())
            }
            fn say(&self, _line: &str) {}
        }

        struct Lines(Arc<Lan>, bool);

        impl aterm_converge::Journal for Lines {
            fn before_change(
                &mut self,
                id: &str,
                _from: &str,
                _to: &str,
                revert: Option<&str>,
            ) -> Result<(), String> {
                if self.1 {
                    return Err("No space left on device (os error 28)".into());
                }
                assert!(
                    revert.is_some(),
                    "every injected vault change is reversible"
                );
                if !*self.0.subscribed.lock().unwrap() {
                    assert!(
                        revert.is_some_and(|revert| revert.contains("aterm secrets agent off"))
                    );
                }
                self.0.log.lock().unwrap().push(format!("line {id}"));
                Ok(())
            }
        }

        pub(super) const START: u64 = 1_800_000_000;

        pub(super) struct Vaulted {
            pub(super) lan: Arc<Lan>,
            rest: Fake,
            pub(super) refuse: bool,
            pub(super) paced: bool,
            pub(super) marks_vault: bool,
            pub(super) build: String,
            pub(super) now: u64,
            tries: Vec<u64>,
            pace_reads: Vec<Option<String>>,
            pub(super) joins_after_read: Option<&'static str>,
        }

        impl Vaulted {
            pub(super) fn new(lan: &Arc<Lan>) -> Self {
                Self {
                    lan: Arc::clone(lan),
                    rest: Fake::default(),
                    refuse: false,
                    paced: true,
                    marks_vault: true,
                    build: "1.0.0+gtest".into(),
                    now: START,
                    tries: Vec::new(),
                    pace_reads: Vec::new(),
                    joins_after_read: None,
                }
            }

            pub(super) fn minutes(&self) -> Vec<u64> {
                self.tries.iter().map(|at| (at - START) / 60).collect()
            }

            pub(super) fn reads(&self, vault: bool) -> LanReads {
                LanReads {
                    lan: Arc::clone(&self.lan),
                    vault,
                    build: self.build.clone(),
                    now: self.now,
                }
            }
        }

        fn context() -> aterm_machines::items::Ctx {
            let user = aterm_machines::model::User {
                name: "tester".into(),
                uid: 501,
                gid: 20,
                home: "/test-home".into(),
                shell: "/bin/zsh".into(),
                admin: true,
            };
            aterm_machines::items::Ctx {
                facts: aterm_machines::model::HostFacts::default(),
                users: vec![user.clone()],
                console: Some(user),
                host_name: "m22".into(),
                cache_dir: PathBuf::from("/test-cache"),
                disrupt: false,
            }
        }

        impl InitialHost for Vaulted {
            fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.legacy(root)
            }
            fn workstation(
                &mut self,
                root: &Path,
                only: Option<Selection>,
            ) -> Result<Option<Report>, String> {
                self.rest.workstation(root, only)
            }
            fn spotlight(&mut self) -> Result<Vec<Row>, String> {
                self.rest.spotlight()
            }
            fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String> {
                self.rest.permissions(plan, revision)
            }
            fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String> {
                self.rest.accounts(root, on_screen)
            }
            fn secrets(
                &mut self,
                plan: &Plan,
                replaces: Replaces,
                password: &mut Option<SecretBytes>,
            ) -> Result<Vec<Row>, String> {
                self.tries.push(self.now);
                let ops: Arc<dyn SecretsOps> = self.lan.clone();
                aterm_machines::init::run_initial_secrets_with(
                    &super::super::super::tests::Inert::default(),
                    &context(),
                    ops,
                    &plan.vault_url,
                    replaces,
                    password,
                    APPROVAL_WAIT,
                    &mut Lines(Arc::clone(&self.lan), self.refuse),
                )
                .map(secrets_rows)
            }
            fn secrets_pace(&mut self) -> Option<Pace> {
                let pace = self
                    .paced
                    .then(|| secrets_retry::pace(&self.reads(self.marks_vault)));
                self.pace_reads
                    .push(pace.as_ref().and_then(|pace| pace.marks.clone()));
                pace
            }
            fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
                let read = self.lan.status_now().config.map(|config| ConfiguredVault {
                    config,
                    home: PathBuf::from("/test-home"),
                });
                if let Some(source) = self.joins_after_read.take() {
                    self.lan.in_vault(source, START);
                }
                Ok(read)
            }
            fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
                let subscribed = if *self.lan.subscribed.lock().unwrap() {
                    check::Subscribed::Running
                } else {
                    check::Subscribed::Down
                };
                check::vault_rows(
                    plan,
                    Path::new("/test-home"),
                    &self.lan.status_now(),
                    |_| Ok(true),
                    || Ok(subscribed),
                )
            }
            fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
                self.rest.repositories(vault)
            }
            fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
                self.rest.withheld(root)
            }
        }

        #[test]
        fn a_same_source_vault_joined_under_another_signer_allows_repositories() {
            let root = aterm_tempfile::tempdir().unwrap();
            let source = "https://team.example/v1/team";
            let plan = Plan {
                vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let lan = Lan::new(true, "office");
            lan.in_vault(source, START);
            let joined = lan.status_now();
            lan.set(joined.clone(), true);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the admitted run executes");

            assert!(rows_ready(&report.rows), "{:?}", report.rows);
            assert_eq!(next, SetupNext::Done);
            assert_eq!(host.rest.calls, [Stage::Repositories]);
            assert!(host.minutes().is_empty(), "no replacement join");
            assert!(lan.log().is_empty(), "{:?}", lan.log());
            assert_eq!(lan.status_now(), joined);
            assert!(finished_at(root.path()).unwrap());
        }

        #[test]
        fn a_same_source_authority_joined_after_admission_is_preserved() {
            for legacy in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let source = "https://team.example/v1/team";
                let plan = Plan {
                    vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
                    ..selected(&[Setting::Secrets, Setting::Repositories])
                };
                let lan = Lan::new(true, "office");
                lan.in_vault(&plan.vault_url, START);
                let admitted = ConfiguredVault {
                    config: lan.status_now().config.unwrap(),
                    home: PathBuf::from("/test-home"),
                };
                let revision = authorize_reading(root.path(), &plan, || Ok(Some(admitted)))
                    .unwrap()
                    .unwrap();
                if legacy {
                    let mut saved = progress(root.path()).unwrap().unwrap();
                    // The source/name-only stamp written before authority was included.
                    saved.admitted_vault = AdmittedVault::Configured(aterm_digest::Sha256::digest(
                        format!("url\0{source}\0team").as_bytes(),
                    ));
                    save_progress(root.path(), saved).unwrap();
                }
                lan.in_vault(source, START);
                let joined = lan.status_now();
                lan.set(joined.clone(), true);
                let mut host = Vaulted::new(&lan);
                host.paced = false;
                let (report, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("the admitted run executes");

                assert!(
                    host.minutes().is_empty(),
                    "legacy={legacy}: {:?}",
                    lan.log()
                );
                assert!(lan.log().is_empty(), "legacy={legacy}: {:?}", lan.log());
                assert_eq!(lan.status_now(), joined);
                assert!(
                    rows_ready(&report.rows),
                    "legacy={legacy}: {:?}",
                    report.rows
                );
                assert_eq!(host.rest.calls, [Stage::Repositories]);
                assert_eq!(next, SetupNext::Done);
                assert!(finished_at(root.path()).unwrap());
            }
        }

        #[test]
        fn a_verified_rotation_continues_the_admitted_vault_authority() {
            let root = aterm_tempfile::tempdir().unwrap();
            let source = "https://team.example/v1/team";
            let plan = Plan {
                vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let lan = Lan::new(true, "office");
            lan.in_vault(&plan.vault_url, START);
            let admitted = ConfiguredVault {
                config: lan.status_now().config.unwrap(),
                home: PathBuf::from("/test-home"),
            };
            let identity = admitted.identity();
            let revision = authorize_reading(root.path(), &plan, || Ok(Some(admitted)))
                .unwrap()
                .unwrap();
            lan.in_vault(source, START);
            let mut rotated = lan.status_now();
            rotated.config.as_mut().unwrap().followed.push([9; 32]);
            lan.set(rotated.clone(), true);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            assert!(!AdmittedVault::Configured(identity).stands_in(
                &Plan {
                    vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[11; 32])),
                    ..plan.clone()
                },
                &host.configured_vault().unwrap().unwrap(),
            ));
            let (report, _) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the admitted run executes");

            assert!(rows_ready(&report.rows), "{:?}", report.rows);
            assert_eq!(host.minutes(), [0], "normal member read, not left in place");
            assert_eq!(host.rest.calls, [Stage::Repositories]);
            assert!(lan.log().is_empty(), "no join or sync: {:?}", lan.log());
            assert_eq!(lan.status_now(), rotated);
            assert!(finished_at(root.path()).unwrap());
        }

        #[test]
        fn a_fresh_explicit_join_can_replace_a_same_source_authority() {
            let root = aterm_tempfile::tempdir().unwrap();
            let source = "https://team.example/v1/team";
            let plan = Plan {
                vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let lan = Lan::new(true, "office");
            lan.in_vault(source, START);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let revision = system_state_apply(root.path(), &plan, true, || host.configured_vault());
            let (_, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the explicit join executes");

            assert_eq!(host.minutes(), [0]);
            assert!(lan.log().contains(&format!("join {}", plan.vault_url)));
            assert_eq!(lan.status_now().config.unwrap().signer, Some([9; 32]));
            assert!(
                host.rest.calls.is_empty(),
                "approval still holds repositories"
            );
            assert_ne!(next, SetupNext::Done);
            assert!(!finished_at(root.path()).unwrap());
        }

        fn a_group_choice_after_a_terminal_join()
        -> (aterm_tempfile::TempDir, Plan, Arc<Lan>, Vaulted) {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut plan = Plan {
                vault_url: "http://older.example:7484/v1/team".into(),
                ..selected(&[Setting::Secrets])
            };
            let lan = Lan::new(true, "office");
            lan.set(member(&plan.vault_url, Membership::Pending), true);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let configured = host.configured_vault().unwrap();
            authorize_reading(root.path(), &plan, || Ok(configured)).unwrap();
            lan.set(
                member(
                    "http://newer.example:7484/v1/team",
                    Membership::In {
                        seq: 7,
                        heard: START,
                    },
                ),
                true,
            );
            plan.selection.set(Setting::Dock, true);
            (root, plan, lan, host)
        }

        /// A group choice whose read of the vault configuration fails is saved, with the vault
        /// this Mac is in recorded as none: its run sets the group chosen and never replaces the
        /// vault a terminal joined since with the plan's older one.
        #[test]
        fn manual_vault_admission_read_failure_never_replaces_a_newer_terminal_join() {
            let (root, plan, lan, mut host) = a_group_choice_after_a_terminal_join();
            let joined = lan.status_now();
            let mut attempt = InitialSetupRestart::chosen();
            let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                attempt.persist_with(
                    root,
                    &plan,
                    false,
                    || Err("injected temporary source.toml read failure".into()),
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap()
            .expect("the person's choice is saved");
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(
                lan.log().is_empty(),
                "an admission read failure authorized the older vault: {:?}",
                lan.log()
            );
            assert_eq!(lan.status_now(), joined);
            assert_eq!(host.rest.calls, [Stage::Workstation]);
        }

        #[test]
        fn manual_vault_admission_read_errors_refuse_join_but_allow_unrelated_groups() {
            for secrets in [false, true] {
                let (root, mut plan, lan, mut host) = a_group_choice_after_a_terminal_join();
                plan.selection.set(Setting::Secrets, secrets);
                let joined = lan.status_now();
                let before = observation_at(root.path()).unwrap();
                let mut attempt = InitialSetupRestart::chosen();
                let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    attempt.persist_with(
                        root,
                        &plan,
                        secrets,
                        || Err("injected temporary source.toml read failure".into()),
                        save_progress,
                        observe_restart,
                    )
                });
                if secrets {
                    assert!(admitted.result.is_err());
                    assert_eq!(observation_at(root.path()).unwrap(), before);
                    assert!(host.rest.calls.is_empty());
                } else {
                    execute_initial_for_revision(
                        &mut host,
                        root.path(),
                        admitted.result.unwrap().unwrap(),
                        &mut None,
                        SetupHooks::NONE,
                    )
                    .unwrap();
                    assert_eq!(host.rest.calls, [Stage::Workstation]);
                }
                assert_eq!(lan.status_now(), joined);
                assert!(lan.log().is_empty());
            }
        }

        #[test]
        fn manual_vault_admission_reads_the_newer_vault_for_an_unrelated_group_choice() {
            let (root, plan, lan, mut host) = a_group_choice_after_a_terminal_join();
            let joined = lan.status_now();
            let configured = host.configured_vault().unwrap();
            let mut attempt = InitialSetupRestart::chosen();
            let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                attempt.persist_with(
                    root,
                    &plan,
                    false,
                    || Ok(configured),
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap()
            .unwrap();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(host.rest.calls, [Stage::Workstation]);
            assert_eq!(lan.status_now(), joined);
            assert!(lan.log().is_empty());
            assert_eq!(
                saved_plan(root.path()).unwrap().unwrap().vault_url,
                "http://newer.example:7484/v1/team"
            );
        }

        fn admit_page_vault_delta(
            root: &Path,
            host: &mut Vaulted,
            plan: &Plan,
            changed: Selection,
            typed: bool,
        ) -> u64 {
            let configured = host.configured_vault().unwrap();
            observe_mutation_at(root, None, Err("busy".into()), |root| {
                let restart = if typed || !changed.is_empty() {
                    InitialSetupRestart::chosen()
                } else {
                    InitialSetupRestart::default()
                };
                restart.changing(Some(changed)).persist_with(
                    root,
                    plan,
                    typed,
                    || Ok(configured),
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap()
            .unwrap()
        }

        fn page_before_a_newer_pending_join(
            configured: bool,
        ) -> (aterm_tempfile::TempDir, Plan, Plan, Arc<Lan>, Vaulted) {
            let root = aterm_tempfile::tempdir().unwrap();
            let older = Plan {
                vault_url: "https://older.example/v1/team".into(),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let lan = Lan::new(false, "office");
            if configured {
                lan.in_vault("https://prior.example/v1/team", START);
            }
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let read = host.configured_vault().unwrap();
            system_state_apply(root.path(), &older, true, || Ok(read));
            // A different page explicitly chooses this vault, but its first
            // real pass cannot reach it, so source.toml still names no vault
            // (or the prior membership), and the newer intent lives in the plan.
            let newer = Plan {
                vault_url: "https://newer.example/v1/team".into(),
                ..older.clone()
            };
            let mut changed = Selection::none();
            changed.set(Setting::Secrets, true);
            let revision = admit_page_vault_delta(root.path(), &mut host, &newer, changed, true);
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(lan.log().is_empty(), "an offline join changes nothing");
            assert_eq!(saved_plan(root.path()).unwrap(), Some(newer.clone()));
            let pending = progress(root.path()).unwrap().unwrap();
            assert!(pending.admitted && !pending.completed);
            assert_eq!(pending.done & Stage::Secrets.bit(), 0);
            *lan.answers.lock().unwrap() = true;
            (root, older, newer, lan, host)
        }

        #[test]
        fn page_vault_delta_keeps_a_newer_pending_join_for_an_unrelated_group() {
            for configured in [false, true] {
                let (root, mut stale, newer, lan, mut host) =
                    page_before_a_newer_pending_join(configured);
                stale.selection.set(Setting::Spotlight, true);
                let mut changed = Selection::none();
                changed.set(Setting::Spotlight, true);
                let revision =
                    admit_page_vault_delta(root.path(), &mut host, &stale, changed, false);
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
                assert_eq!(host.rest.calls, [Stage::Spotlight]);
                assert!(
                    lan.log().contains(&format!("join {}", newer.vault_url)),
                    "an unrelated group must keep the pending vault: {:?}",
                    lan.log()
                );
                assert!(!lan.log().contains(&format!("join {}", stale.vault_url)));
                let saved = saved_plan(root.path()).unwrap().unwrap();
                assert_eq!(saved.vault_url, newer.vault_url);
                assert!(saved.selection.contains(Setting::Spotlight));
            }
        }

        #[test]
        fn page_vault_delta_set_up_now_runs_the_newer_pending_vault() {
            let (root, stale, newer, lan, mut host) = page_before_a_newer_pending_join(false);
            let revision =
                admit_page_vault_delta(root.path(), &mut host, &stale, Selection::none(), false);
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(host.rest.calls.is_empty());
            assert!(
                lan.log().contains(&format!("join {}", newer.vault_url)),
                "Set up now must run the in-force vault: {:?}",
                lan.log()
            );
            assert_eq!(saved_plan(root.path()).unwrap(), Some(newer));
        }

        #[test]
        fn page_vault_delta_explicit_join_can_replace_the_pending_vault() {
            for configured in [false, true] {
                let (root, typed, _, lan, mut host) = page_before_a_newer_pending_join(configured);
                let mut changed = Selection::none();
                changed.set(Setting::Secrets, true);
                let revision =
                    admit_page_vault_delta(root.path(), &mut host, &typed, changed, true);
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
                assert!(lan.log().contains(&format!("join {}", typed.vault_url)));
                assert_eq!(saved_plan(root.path()).unwrap(), Some(typed));
            }
        }

        #[test]
        fn manual_vault_admission_allows_a_first_join_and_a_fresh_explicit_switch() {
            for switch in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let plan = Plan {
                    vault_url: "http://selected.example:7484/v1/team".into(),
                    ..selected(&[Setting::Secrets])
                };
                let lan = Lan::new(true, "office");
                if switch {
                    lan.in_vault("http://other.example:7484/v1/team", START);
                }
                let mut host = Vaulted::new(&lan);
                host.paced = false;
                let configured = host.configured_vault().unwrap();
                let mut attempt = InitialSetupRestart::chosen();
                let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    attempt.persist_with(
                        root,
                        &plan,
                        switch,
                        || Ok(configured),
                        save_progress,
                        observe_restart,
                    )
                })
                .result
                .unwrap()
                .unwrap();
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
                assert!(lan.log().contains(&format!("join {}", plan.vault_url)));
                assert_eq!(
                    lan.status_now().config.unwrap().source,
                    Source::Url(plan.vault_url)
                );
            }
        }

        fn a_relaunched_staged_choice_after_a_terminal_join()
        -> (aterm_tempfile::TempDir, Plan, Arc<Lan>, Vaulted) {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = Plan {
                vault_url: "http://older.example:7484/v1/team".into(),
                ..selected(&[Setting::Secrets, Setting::Dock])
            };
            let lan = Lan::new(true, "office");
            lan.set(member(&plan.vault_url, Membership::Pending), true);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let configured = host.configured_vault().unwrap();
            let mut interrupted = InitialSetupRestart::chosen();
            let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                interrupted.persist_with(
                    root,
                    &plan,
                    false,
                    || Ok(configured),
                    |_, next| {
                        assert!(next.admitted, "the preceding real writes staged the plan");
                        Err("the process ends before publishing final admission".into())
                    },
                    observe_restart,
                )
            });
            assert!(failed.result.is_err());
            assert!(interrupted.pending());
            let staged = progress(root.path()).unwrap().unwrap();
            assert!(staged.belongs_to(&plan));
            assert!(!staged.admitted);
            assert!(admission_at(root.path()).unwrap().is_none());
            drop(interrupted);
            lan.set(
                member(
                    "http://newer.example:7484/v1/team",
                    Membership::In {
                        seq: 7,
                        heard: START,
                    },
                ),
                true,
            );
            (root, plan, lan, host)
        }

        /// The startup's offer reads an unreadable vault configuration as none and goes ahead
        /// (design §3.3): the offer's answer is admitted with no vault recorded, so the run's
        /// Secrets stage, reading the newer terminal join by then, leaves it in place and runs
        /// the rest of the plan, never replacing a vault it did not read.
        #[test]
        fn startup_vault_admission_read_failure_admits_and_keeps_the_newer_join() {
            let (root, plan, lan, mut host) = a_relaunched_staged_choice_after_a_terminal_join();
            let joined = lan.status_now();
            let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let offered = ticket
                .offer_reading(root.path(), || {
                    Err("injected temporary source.toml read failure".into())
                })
                .expect("the offer goes ahead");
            assert_eq!(offered, plan);
            let revision = ticket
                .persist_at(root.path(), &offered)
                .result
                .unwrap()
                .expect("the offer's answer is admitted");
            assert_eq!(
                progress(root.path()).unwrap().unwrap().admitted_vault,
                AdmittedVault::None
            );
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(
                lan.log().is_empty(),
                "a startup read failure replaced the newer join: {:?}",
                lan.log()
            );
            assert_eq!(lan.status_now(), joined);
            assert_eq!(host.rest.calls, [Stage::Workstation]);
        }

        #[test]
        fn one_shot_vault_admission_read_failure_keeps_the_newer_terminal_join() {
            let (root, plan, lan, mut host) = a_relaunched_staged_choice_after_a_terminal_join();
            let joined = lan.status_now();
            let before = observation_at(root.path()).unwrap();
            let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                restart_reading(root, &plan, || {
                    Err("injected temporary source.toml read failure".into())
                })
            });
            if let Ok(revision) = &admitted.result {
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    *revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
            }
            assert!(
                lan.log().is_empty(),
                "a fresh restart read failure replaced the newer join: {:?}",
                lan.log()
            );
            assert_eq!(lan.status_now(), joined);
            assert!(host.rest.calls.is_empty());
            assert_eq!(observation_at(root.path()).unwrap(), before);
            assert!(admitted.result.is_err());
        }

        pub(super) fn authorized() -> (aterm_tempfile::TempDir, u64) {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &selected(&[Setting::Secrets]))
                .unwrap()
                .unwrap();
            (root, revision)
        }

        pub(super) fn passes(
            host: &mut Vaulted,
            root: &Path,
            revision: u64,
            password: &mut Option<SecretBytes>,
            minutes: std::ops::RangeInclusive<u64>,
            mut before: impl FnMut(u64, &mut Vaulted),
        ) -> Vec<Row> {
            let mut rows = Vec::new();
            for minute in minutes {
                host.now = START + minute * 60;
                before(minute, host);
                let Some((report, _)) =
                    execute_initial_for_revision(host, root, revision, password, SetupHooks::NONE)
                        .unwrap()
                else {
                    break;
                };
                rows.extend(
                    report
                        .rows
                        .into_iter()
                        .filter(|row| row.name == Stage::Secrets.name()),
                );
            }
            rows
        }
    }

    fn join_password() -> SecretBytes {
        SecretBytes::from(b"test-only-setup-join-password".as_slice())
    }

    #[test]
    fn an_unreachable_vault_is_asked_at_growing_pauses_and_nothing_is_changed() {
        use vault::{Lan, NO_ROUTE, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=20,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0, 1, 3, 7, 11, 15, 19]);
        assert_eq!(*lan.asked.lock().unwrap(), 7);
        assert!(
            lan.log().is_empty(),
            "journaled or changed: {:?}",
            lan.log()
        );
        assert!(password.is_some(), "kept for a try that reaches the vault");
        assert_eq!(rows.len(), 21);
        let offline = format!(
            "the vault does not answer, so nothing was changed: {NO_ROUTE}; if macOS asked \
             whether this app may find devices on your local network and it was not allowed, \
             turn it on in System Settings > Privacy & Security > Local Network; "
        );
        assert!(
            rows.iter()
                .all(|row| row.status == "blocked" && row.detail.starts_with(&offline)),
            "{rows:?}"
        );
        assert!(rows[0].detail.ends_with(
            "aterm tries again by itself a minute after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
        ));
        assert_eq!(
            rows[2].detail, rows[1].detail,
            "a paused pass shows its try"
        );
        assert!(rows[2].detail.contains("2 minutes after this try"));
        assert!(rows[20].detail.contains("4 minutes after this try"));
        let distinct: std::collections::BTreeSet<&str> =
            rows.iter().map(|row| row.detail.as_str()).collect();
        assert_eq!(distinct.len(), 3, "{distinct:?}");

        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut unpaced = Vaulted::new(&lan);
        unpaced.paced = false;
        passes(
            &mut unpaced,
            root.path(),
            revision,
            &mut None,
            0..=20,
            |_, _| {},
        );
        assert_eq!(unpaced.minutes(), (0..=20).collect::<Vec<_>>());
        assert_eq!(*lan.asked.lock().unwrap(), 21);
        assert!(lan.log().is_empty());
    }

    #[test]
    fn a_paused_try_comes_at_once_on_a_new_network_build_or_run() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=14,
            |minute, host| match minute {
                5 => *host.lan.network.lock().unwrap() = "office".into(),
                10 => host.build = "1.0.0+gnext".into(),
                _ => {}
            },
        );
        assert_eq!(host.minutes(), [0, 1, 3, 5, 6, 8, 10, 11, 13]);

        let plan = selected(&[Setting::Secrets]);
        let rerun = restart_at(root.path(), &plan).unwrap();
        assert!(!root.path().join(secrets_retry::RECORD).exists());
        let mut host = Vaulted::new(&lan);
        host.now = vault::START + 14 * 60;
        execute_initial_for_revision(&mut host, root.path(), rerun, &mut None, SetupHooks::NONE)
            .unwrap();
        assert_eq!(host.minutes(), [14], "the person's choice tries at once");
    }

    #[test]
    fn an_approval_is_picked_up_at_the_next_pass_and_within_the_longest_pause() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        for marks_vault in [true, false] {
            let (root, revision) = authorized();
            let lan = Lan::new(true, "home");
            let mut host = Vaulted::new(&lan);
            host.marks_vault = marks_vault;
            let mut password = Some(join_password());
            let approved = 8;
            let rows = passes(
                &mut host,
                root.path(),
                revision,
                &mut password,
                0..=30,
                |minute, host| {
                    if minute == approved {
                        host.lan.approve(START + minute * 60);
                    }
                },
            );
            let tries = host.minutes();
            let picked = *tries.last().unwrap();
            assert_eq!(tries[..4], [0, 1, 3, 7], "{tries:?}");
            if marks_vault {
                assert_eq!(picked, approved, "{tries:?}");
            } else {
                assert_eq!(picked, 11, "{tries:?}");
            }
            assert!(
                (picked - tries[tries.len() - 2]) * 60 <= secrets_retry::LONGEST.as_secs(),
                "{tries:?}"
            );
            let last = rows.last().unwrap();
            assert_eq!(last.status, "ok", "{rows:?}");
            assert!(finished_at(root.path()).unwrap());
            assert!(!root.path().join(secrets_retry::RECORD).exists());
            assert!(
                rows[1].status == WAITING && rows[1].detail.contains("aterm secrets approve"),
                "{rows:?}"
            );
            assert_eq!(
                lan.log(),
                [
                    "line init.secrets@tester".to_owned(),
                    format!("join {}", aterm_types::macos_setup::DEFAULT_VAULT_URL),
                    "install".to_owned(),
                ]
            );
            assert!(password.is_none(), "the join's proof is with the vault");
            assert_eq!(*lan.asked.lock().unwrap(), 1, "only the join reached out");
        }
    }

    #[test]
    fn the_live_host_paces_each_secrets_try_by_the_network_and_the_vault_state() {
        use aterm_secrets::client::{Membership, Status};
        use std::sync::Arc;
        use vault::{Lan, LanReads};
        let lan = Lan::new(false, "away");
        let reads = |vault: bool| LanReads {
            lan: Arc::clone(&lan),
            vault,
            build: "1.0.0+gtest".into(),
            now: vault::START,
        };
        let mut host = LiveInitial {
            reads: reads(true),
            developer_tools_wanted: false,
            repositories_failure: None,
        };
        let first = host.secrets_pace().expect("the live host paces every try");
        assert_eq!(first, secrets_retry::pace(&reads(true)));
        assert_eq!(first.build, "1.0.0+gtest");
        assert_eq!(first.now, vault::START);
        // An old-format known digest must still hold after upgrading aterm.
        let old_facts = format!(
            "away; {:?}; subscriber running: Ok(false); runs from: None",
            lan.status_now()
        );
        let mut old = first.clone();
        old.inputs = None;
        old.marks = Some(
            aterm_digest::Sha256::digest(old_facts.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        );
        let root = aterm_tempfile::tempdir().unwrap();
        for _ in 0..2 {
            let mut failed = vec![Row {
                name: Stage::Secrets.name().into(),
                status: "error".into(),
                detail: "injected failed change before upgrade".into(),
                elapsed_ms: None,
            }];
            secrets_retry::record(root.path(), 7, &old, false, &mut failed).unwrap();
        }
        assert!(secrets_retry::paused(root.path(), 7, &first).is_some());

        let known = first.inputs.clone().unwrap();
        for (fault, expected) in [
            (
                &lan.routes_error_once,
                secrets_retry::Inputs {
                    routes: None,
                    ..known.clone()
                },
            ),
            (
                &lan.status_error_once,
                secrets_retry::Inputs {
                    vault: None,
                    ..known.clone()
                },
            ),
            (
                &lan.subscriber_error_once,
                secrets_retry::Inputs {
                    subscriber: None,
                    ..known.clone()
                },
            ),
        ] {
            *fault.lock().unwrap() = true;
            let unknown = host.secrets_pace().unwrap();
            assert!(unknown.marks.is_none());
            assert_eq!(unknown.inputs, Some(expected));
            assert!(secrets_retry::paused(root.path(), 7, &unknown).is_some());
            assert_eq!(host.secrets_pace().unwrap().marks, first.marks);
        }
        let mut host_marks = vec![host.secrets_pace().unwrap().marks];
        assert_eq!(host_marks[0], first.marks, "nothing changed");

        *lan.network.lock().unwrap() = "office".into();
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        let pending = Status {
            membership: Membership::Pending,
            ..lan.status_now()
        };
        lan.set(pending, true);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        lan.approve(vault::START);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        lan.approve(vault::START + 600);
        assert_eq!(
            secrets_retry::pace(&host.reads).marks,
            *host_marks.last().unwrap()
        );
        let joined = lan.status_now();
        lan.set(joined, false);
        host_marks.push(secrets_retry::pace(&host.reads).marks);
        let distinct: std::collections::BTreeSet<_> = host_marks.iter().collect();
        assert_eq!(distinct.len(), host_marks.len(), "{host_marks:?}");

        let unread = || secrets_retry::pace(&reads(false)).marks;
        let before = unread();
        lan.approve(vault::START + 900);
        let pending = Status {
            membership: Membership::Pending,
            ..lan.status_now()
        };
        lan.set(pending, true);
        assert_eq!(unread(), before);
    }

    #[test]
    fn builds_taking_turns_share_the_pause_of_a_vault_that_does_not_answer() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=20,
            |minute, host| {
                host.build = if minute % 2 == 0 {
                    "1.0.0+gold".into()
                } else {
                    "1.0.0+gnew".into()
                };
            },
        );
        assert_eq!(host.minutes(), [0, 1, 2, 4, 8, 12, 16, 20]);
        assert_eq!(*lan.asked.lock().unwrap(), 8);
        assert!(lan.log().is_empty());

        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            21..=21,
            |_, host| host.build = "1.0.0+gthird".into(),
        );
        assert_eq!(*host.minutes().last().unwrap(), 21, "the control");
    }

    #[test]
    fn a_join_that_keeps_failing_is_journaled_twice_then_waits_for_a_change() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        *lan.refusal.lock().unwrap() = Some(
            "http://alab-secrets.local:7484/v1/alab is signed by 0123-4567, which nothing pins: run this in a terminal to confirm it, or add #<signer> to the URL".into(),
        );
        let mut host = Vaulted::new(&lan);
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=1440,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0, 1]);
        let lines = |lan: &Lan| lan.log().iter().filter(|e| e.starts_with("line ")).count();
        assert_eq!(lines(&lan), 2, "{:?}", lan.log());
        assert!(password.is_some(), "kept: no join spent it");
        let last = rows.last().unwrap();
        assert_eq!(last.status, "error", "{rows:?}");
        assert!(
            last.detail.ends_with(
                "aterm tried this twice by itself and it did not take, so it tries again only when this Mac's network or vault changes, after an update of aterm, or on Set up now or Join in System State"
            ),
            "{last:?}"
        );

        passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            1441..=1500,
            |minute, host| {
                if minute == 1441 {
                    *host.lan.network.lock().unwrap() = "office".into();
                }
            },
        );
        assert_eq!(host.minutes(), [0, 1, 1441, 1442]);
        assert_eq!(lines(&lan), 4);

        let (root, revision) = authorized();
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=60,
            |_, _| {},
        );
        assert_eq!(host.minutes().len(), 17, "{:?}", host.minutes());
        assert_eq!(lines(&lan), 0);
    }

    #[test]
    fn a_setup_from_a_disk_image_waits_and_tries_at_once_from_applications() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let image = PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
        for opened in [Some(5), None] {
            let (root, revision) = authorized();
            let lan = Lan::new(true, "home");
            lan.in_vault(&selected(&[Setting::Secrets]).vault_url, START);
            *lan.fleeting.lock().unwrap() = Some(image.clone());
            let mut host = Vaulted::new(&lan);
            let rows = passes(
                &mut host,
                root.path(),
                revision,
                &mut None,
                0..=8,
                |minute, host| {
                    if Some(minute) == opened {
                        *host.lan.fleeting.lock().unwrap() = None;
                    }
                },
            );
            let waits = &rows[..opened.map_or(rows.len(), |at| at as usize)];
            assert!(
                waits.iter().all(|row| row.status == WAITING
                    && row
                        .detail
                        .starts_with(&format!("this aterm runs from {}", image.display()))
                    && row
                        .detail
                        .contains("move aterm to Applications and open it from there")
                    && !row.detail.contains("--program")),
                "{rows:?}"
            );
            let installs = lan.log().iter().filter(|event| *event == "install").count();
            if opened.is_some() {
                assert_eq!(host.minutes(), [0, 1, 3, 5]);
                assert_eq!(installs, 4);
                assert_eq!(rows.last().unwrap().status, "applied", "{rows:?}");
            } else {
                assert_eq!(host.minutes(), [0, 1, 3, 7]);
                assert_eq!(installs, 4);
                assert_eq!(rows.len(), 9);
            }
        }
    }

    #[test]
    fn a_refused_journal_line_refuses_the_secrets_stages_change() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        let mut host = Vaulted::new(&lan);
        host.refuse = true;
        let mut password = Some(join_password());
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=0,
            |_, _| {},
        );
        assert_eq!(rows[0].status, "error", "{rows:?}");
        assert!(
            rows[0]
                .detail
                .starts_with("not applied: the journal refused the record"),
            "{rows:?}"
        );
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(password.is_some());

        host.refuse = false;
        passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            1..=1,
            |_, _| {},
        );
        assert_eq!(lan.log().len(), 3, "{:?}", lan.log());
        assert!(lan.log()[0].starts_with("line "));
        assert!(password.is_none());
    }

    const JOINED_SINCE: &str = "http://vault.example:7484/v1/team?k=fixture-query";

    #[test]
    fn a_vault_joined_after_setup_began_stays_when_the_setup_vault_answers() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(record.contains(r#""admitted_vault":null"#), "{record}");
        let lan = Lan::new(false, "away");
        let mut host = Vaulted::new(&lan);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(rows[0].status, "blocked", "{rows:?}");

        lan.in_vault(JOINED_SINCE, START);
        let joined = lan.status_now();
        let shown = snapshot_in_vault(root.path(), host.configured_vault(), None).unwrap();
        assert!(!shown.finished);
        assert_eq!(shown.plan.vault_url, JOINED_SINCE);

        *lan.answers.lock().unwrap() = true;
        host.now = START + 60;
        let report = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap()
        .0;
        assert_eq!(host.minutes(), [0], "the vault item ran again");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert_eq!(lan.status_now(), joined);
        let [left, vault, subscriber] = report.rows.as_slice() else {
            panic!("{:?}", report.rows)
        };
        assert_eq!(
            (left.name.as_str(), left.status.as_str()),
            ("Secrets vault", "ok")
        );
        assert_eq!(vault.status, "ok", "{vault:?}");
        assert_eq!(
            (subscriber.name.as_str(), subscriber.status.as_str()),
            ("Secrets subscriber", "blocked")
        );
        assert!(!finished_at(root.path()).unwrap());

        lan.set(joined.clone(), true);
        host.now = START + 120;
        let report = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap()
        .0;
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(host.minutes(), [0]);
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(finished_at(root.path()).unwrap());
        assert!(!root.path().join(secrets_retry::RECORD).exists());
    }

    #[test]
    fn a_vault_left_in_place_that_waits_for_approval_is_not_done() {
        use aterm_secrets::client::Membership;
        use vault::{Lan, START, Vaulted, authorized};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut waiting = lan.status_now();
        waiting.membership = Membership::Pending;
        lan.set(waiting, true);
        let mut host = Vaulted::new(&lan);
        let report = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap()
        .0;
        assert!(host.minutes().is_empty(), "the vault item ran");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        let vault = report
            .rows
            .iter()
            .find(|row| row.detail.starts_with("waiting for vault approval, code "))
            .unwrap_or_else(|| panic!("{:?}", report.rows));
        assert_eq!(
            (vault.name.as_str(), vault.status.as_str()),
            ("Secrets vault", "blocked")
        );
        assert!(!rows_ready(&report.rows));
        assert!(!finished_at(root.path()).unwrap());

        lan.approve(START);
        host.now = START + 60;
        let report = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap()
        .0;
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert!(finished_at(root.path()).unwrap());
        assert!(lan.log().is_empty(), "{:?}", lan.log());
    }

    #[test]
    fn presenting_the_configured_vault_preserves_only_admitted_signer_pins() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let source = "https://team.example/v1/team";
        let mut vault = configure_vault(&paths, source);
        vault.config.followed.push([9; 32]);
        for pin in [None, Some([7; 32]), Some([9; 32])] {
            let plan = Plan {
                vault_url: pin.map_or_else(
                    || source.to_owned(),
                    |pin| format!("{source}#{}", aterm_secrets::b64::encode_padded(&pin)),
                ),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            assert_eq!(vault.present(plan.clone()), plan);
        }

        let unrelated = Plan {
            vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[11; 32])),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let presented = vault.present(unrelated.clone());
        assert_eq!(presented.selection, unrelated.selection);
        assert!(
            aterm_machines::init_secrets::selected_vault(
                &presented.vault_url,
                &vault.home,
                Some(&vault.config),
            )
            .unwrap()
            .is_some(),
            "the presented source must admit the configured signer"
        );
    }

    #[test]
    fn an_apply_from_a_tab_shown_before_a_terminal_join_keeps_that_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let alab = Plan::default().vault_url;
        configure_vault(&paths, &alab);
        let read = || ConfiguredVault::read(&paths);
        authorize_reading(root.path(), &selected(&[Setting::Secrets]), read).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let shown = snapshot_in_vault(root.path(), read(), None).unwrap().plan;
        assert_eq!(shown.vault_url, alab);

        let team = "http://vault.example:7484/v1/team";
        configure_vault(&paths, team);
        let ticked = Plan {
            vault_url: shown.vault_url.clone(),
            ..selected(&[Setting::Secrets, Setting::Spotlight])
        };
        let revision = system_state_apply(root.path(), &ticked, false, read);
        assert_eq!(saved_plan(root.path()).unwrap().unwrap().vault_url, team);
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(team));

        let other = "https://other.example/v1/other";
        let typed = Plan {
            vault_url: other.into(),
            ..ticked
        };
        let revision = system_state_apply(root.path(), &typed, true, read);
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(other));
    }

    #[test]
    fn a_vault_joined_after_the_stage_read_it_is_not_joined_over() {
        use vault::{Lan, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let lan = Lan::new(true, "office");
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        host.joins_after_read = Some(JOINED_SINCE);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        let [row] = rows.as_slice() else {
            panic!("{rows:?}")
        };
        assert_eq!(row.status, "unverified", "{row:?}");
        assert_eq!(
            row.detail,
            "this Mac's vault changed after it was read (now vault team (http://vault.example:7484/v1/team)), so nothing was changed; the next try reads it"
        );
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert!(!finished_at(root.path()).unwrap());

        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            1..=1,
            |_, _| {},
        );
        assert_eq!(rows[0].status, "ok", "{rows:?}");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
    }

    #[test]
    fn repository_setup_reads_the_vault_the_secrets_stage_left_in_place() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            configured: Some(configure_vault(&paths, team)),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert_eq!(host.vault_url, None, "joined");
        assert_eq!(host.checked_url.as_deref(), Some(team));
        assert!(
            host.calls.contains(&Stage::Repositories),
            "{:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap());
    }

    /// The ALab vault's probe stops an unjoined Mac's Secrets stage at what it heard, except the
    /// first probe nobody answered in a process while no look has heard how macOS's Local Network
    /// switch stands (no answer, or a question asked): that join goes ahead and its own look
    /// waits for the answer. A later one, or one after a look heard the switch, stops unreached.
    /// Negative controls: stopping at every probe nobody answered, as before, fails the untold
    /// and asked first tries; letting every one through fails the second.
    #[test]
    fn only_a_first_unanswered_alab_probe_lets_the_join_wait_for_the_local_network_answer() {
        use access::LanAnswer::{Asked, Reached, Refused};
        use scope::VaultHere;
        use std::sync::atomic::{AtomicBool, Ordering};
        let stops = |here, lan, joined: &AtomicBool| {
            unjoined_alab_row(here, lan, joined).map(|row| (row.name, row.status, row.detail))
        };
        let row = |status: &str, detail: &str| {
            Some((
                Stage::Secrets.name().to_owned(),
                status.to_owned(),
                detail.to_owned(),
            ))
        };
        let unreached = row(UNREACHED, scope::NOT_REACHED);
        for lan in [None, Some(Asked)] {
            let joined = AtomicBool::new(false);
            assert_eq!(stops(VaultHere::Unreached, lan, &joined), None, "{lan:?}");
            assert!(joined.load(Ordering::Relaxed));
            assert_eq!(
                stops(VaultHere::Unreached, lan, &joined),
                unreached,
                "{lan:?}"
            );
        }
        for lan in [Some(Reached), Some(Refused)] {
            let joined = AtomicBool::new(false);
            assert_eq!(
                stops(VaultHere::Unreached, lan, &joined),
                unreached,
                "{lan:?}"
            );
            assert!(!joined.load(Ordering::Relaxed), "{lan:?}: no join waited");
        }
        for lan in [None, Some(Asked), Some(Reached), Some(Refused)] {
            let joined = AtomicBool::new(false);
            assert_eq!(
                stops(VaultHere::Absent, lan, &joined),
                row(WAITING, scope::NO_VAULT),
                "{lan:?}"
            );
            assert_eq!(stops(VaultHere::Found, lan, &joined), None, "{lan:?}");
            assert_eq!(stops(VaultHere::Unasked, lan, &joined), None, "{lan:?}");
            assert!(!joined.load(Ordering::Relaxed), "{lan:?}");
        }
    }

    /// The join a probe nobody answered let through parks as the probe's own row would have:
    /// a row stood aside because the vault did not answer is unreached, and one whose look heard
    /// a server with no vault waits, each with the look's words, so the pass parks; any other row
    /// is as [`secrets_rows`] has it. Negative control: the plain rows leave it blocked, which
    /// retries the pass every minute.
    #[test]
    fn a_join_an_unanswered_probe_let_through_parks_where_the_vault_does_not_answer() {
        use aterm_converge::StoodAside;
        use aterm_machines::model::{Effect, ItemReport, Outcome};
        let item = |outcome: Outcome, stood_aside: Option<StoodAside>, why: &str| ItemReport {
            id: "secrets.vault".into(),
            family: "secrets".into(),
            title: "vault".into(),
            outcome,
            from: None,
            to: None,
            why: Some(why.to_owned()),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside,
            essential: false,
            elapsed_ms: None,
        };
        let offline = "the vault does not answer, so nothing was changed: no route";
        let no_vault = format!(
            "{}: a login page",
            aterm_machines::init_secrets::NO_VAULT_NOW
        );
        let reports = vec![
            item(
                Outcome::Blocked,
                Some(StoodAside::Offline(offline.into())),
                offline,
            ),
            item(
                Outcome::Blocked,
                Some(StoodAside::Waiting("an admin Mac".into())),
                "approve",
            ),
            item(Outcome::Changed, None, "joined"),
            item(
                Outcome::Blocked,
                Some(StoodAside::Offline(no_vault.clone())),
                &no_vault,
            ),
        ];
        let seen = |rows: Vec<Row>| {
            rows.into_iter()
                .map(|row| (row.name, row.status, row.detail))
                .collect::<Vec<_>>()
        };
        let secrets = Stage::Secrets.name().to_owned();
        assert_eq!(
            seen(unanswered_join_rows(reports.clone())),
            [
                (secrets.clone(), UNREACHED.to_owned(), offline.to_owned()),
                (secrets.clone(), WAITING.to_owned(), "approve".to_owned()),
                (secrets.clone(), "applied".to_owned(), "joined".to_owned()),
                (secrets.clone(), WAITING.to_owned(), no_vault.clone()),
            ]
        );
        assert_eq!(
            SetupNext::of(&unanswered_join_rows(reports.clone())[..1]),
            SetupNext::Park
        );
        assert_eq!(SetupNext::of(&secrets_rows(reports)[..1]), SetupNext::Retry);
    }

    /// The join a probe nobody answered let through keeps what its look heard of macOS's Local
    /// Network switch, as the probe would have: a join made or waiting for an admin Mac, or a
    /// server there with no vault, reads the switch on; macOS refusing the vault's `.local` name
    /// reads it off; a look nothing answered leaves it untold, so a laptop away from the vault's
    /// LAN never reads on. A join that failed after the vault answered it (its subscriber not
    /// installed) reads on; one that failed before reaching it says nothing. What is heard is
    /// handed to the keeper the live pass gives (`access::lan_answered`) through
    /// [`unanswered_join`]. Negative controls: a join made whose answer is not kept leaves its row
    /// untold; reading every stand-aside as a reach fails the unanswered case.
    #[test]
    fn a_join_an_unanswered_probe_let_through_keeps_what_its_look_heard_of_the_lan() {
        use access::{Lan, LanAnswer};
        use aterm_converge::StoodAside;
        use aterm_machines::model::{Effect, ItemReport, Outcome};
        let item = |outcome: Outcome, stood_aside: Option<StoodAside>| ItemReport {
            id: "secrets.vault".into(),
            family: "secrets".into(),
            title: "vault".into(),
            outcome,
            from: None,
            to: None,
            why: stood_aside.as_ref().map(|aside| aside.why().to_owned()),
            revert: None,
            effect: Effect::Now,
            detail: None,
            layer: None,
            class: None,
            explain: None,
            stood_aside,
            essential: false,
            elapsed_ms: None,
        };
        let failed = |why: &str| ItemReport {
            why: Some(why.to_owned()),
            ..item(Outcome::Failed, None)
        };
        let refused = format!(
            "the vault does not answer, so nothing was changed: fetching \
             http://alab-secrets.local:7484/v1/alab: {}",
            aterm_secrets::client::LOCAL_NETWORK_REFUSED
        );
        let cases = [
            (
                item(Outcome::Changed, None),
                Some(LanAnswer::Reached),
                Lan::On,
            ),
            (
                item(
                    Outcome::Blocked,
                    Some(StoodAside::Waiting("an admin Mac approves it".into())),
                ),
                Some(LanAnswer::Reached),
                Lan::On,
            ),
            (
                item(
                    Outcome::Blocked,
                    Some(StoodAside::Offline(format!(
                        "{}: a login page",
                        aterm_machines::init_secrets::NO_VAULT_NOW
                    ))),
                ),
                Some(LanAnswer::Reached),
                Lan::On,
            ),
            (
                item(Outcome::Blocked, Some(StoodAside::Offline(refused))),
                Some(LanAnswer::Refused),
                Lan::Off,
            ),
            (
                item(
                    Outcome::Blocked,
                    Some(StoodAside::Offline(
                        "the vault does not answer, so nothing was changed: no route".into(),
                    )),
                ),
                None,
                Lan::Untold,
            ),
            (
                item(
                    Outcome::Blocked,
                    Some(StoodAside::Offline(format!(
                        "{}: http://alab-secrets.local:7484/v1/alab: no vault there",
                        aterm_machines::init_secrets::NO_VAULT_NOW
                    ))),
                ),
                Some(LanAnswer::Reached),
                Lan::On,
            ),
            (
                failed(&format!(
                    "{} the subscriber could not be installed: launchctl bootstrap failed",
                    aterm_machines::init_secrets::VAULT_ANSWERED_BUT
                )),
                Some(LanAnswer::Reached),
                Lan::On,
            ),
            (
                failed("joining the vault: could not write source.toml"),
                None,
                Lan::Untold,
            ),
        ];
        for (report, answer, lan) in cases {
            let heard = join_lan_answer(std::slice::from_ref(&report));
            assert_eq!(heard, answer, "{report:?}");
            assert_eq!(Lan::of(heard, &[]), lan, "{report:?}");
            let mut kept = None;
            let rows = unanswered_join(vec![report.clone()], |answer| kept = Some(answer));
            assert_eq!(kept, answer, "kept: {report:?}");
            assert_eq!(
                format!("{rows:?}"),
                format!("{:?}", unanswered_join_rows(vec![report]))
            );
        }
    }

    /// A Mac where aterm saved no plan and the vault's `source.toml` cannot be read (a
    /// root-owned file from a `sudo aterm secrets …`, a newer format): the snapshot reads it as
    /// no vault and shows the plan in force, never "could not read what it saved about this
    /// Mac's setup", since aterm saved nothing and the file it could not read is the vault's.
    #[test]
    fn a_snapshot_reads_an_unreadable_vault_config_as_no_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        let shown = snapshot_in_vault(root.path(), Err("source.toml: no name".into()), None)
            .expect("an unreadable vault config is no unread saved setup");
        assert_eq!(shown.plan, Plan::default());
        authorize_at(root.path(), &selected(&[Setting::Permissions])).unwrap();
        let shown =
            snapshot_in_vault(root.path(), Err("source.toml: no name".into()), None).unwrap();
        assert_eq!(shown.plan, selected(&[Setting::Permissions]));
    }

    /// A Repositories step that failed says on the page the reason its step decided
    /// (`init_observe::failure_note`), under the page's row, in place of the words a change
    /// that did not take carries or the stage's own line, which the log keeps.
    #[test]
    fn a_failed_repositories_step_says_the_reason_it_decided() {
        const REASON: &str = "GitHub can't be reached";
        let failed = aterm_types::macos_setup::Unfinished::Failed;
        let said = |page: &PassPage| {
            page.rows
                .iter()
                .map(|row| (row.name.clone(), row.status.clone(), row.detail.clone()))
                .collect::<Vec<_>>()
        };
        let reason = vec![(
            Setting::Repositories.name().to_owned(),
            "error".to_owned(),
            REASON.to_owned(),
        )];
        let item = Row {
            name: "GitHub repositories from synced development credentials".into(),
            status: "error".into(),
            detail: "GET /user: connection refused".into(),
            elapsed_ms: None,
        };
        let mut page = items_page(Stage::Repositories, &[item]).unwrap();
        assert_eq!(page.outcome.unfinished, [(Setting::Repositories, failed)]);
        page.failed_because(Setting::Repositories, REASON);
        assert!(page.outcome.unfinished.is_empty());
        assert_eq!(said(&page), reason);

        let stage = Row {
            name: Stage::Repositories.name().into(),
            status: "error".into(),
            detail: "the stage's own line".into(),
            elapsed_ms: None,
        };
        let mut page = items_page(Stage::Repositories, &[stage]).unwrap();
        assert_eq!(page.rows[0].detail, "", "no words of the stage's own");
        page.failed_because(Setting::Repositories, REASON);
        assert_eq!(said(&page), reason);

        let mut calm = PassPage::default();
        calm.failed_because(Setting::Repositories, REASON);
        assert!(calm.rows.is_empty() && calm.outcome.unfinished.is_empty());
    }

    /// The same Mac's first-launch offer goes ahead, so aterm's own permissions, which need no
    /// vault, are set up with no press (design §6); its admission records no vault, so the
    /// Secrets stage never replaces one it did not read.
    #[test]
    fn the_first_offer_goes_ahead_past_an_unreadable_vault_config() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let offered = ticket
            .offer_reading(root.path(), || Err("source.toml: no name".into()))
            .expect("the offer goes ahead");
        assert_eq!(offered, Plan::default());
        let revision = ticket.persist_at(root.path(), &offered).result.unwrap();
        assert!(revision.is_some(), "the offer's answer is admitted");
        assert_eq!(
            progress(root.path()).unwrap().unwrap().admitted_vault,
            AdmittedVault::None
        );
    }

    /// The Secrets stage that cannot read which vault this Mac is in says nothing of its own on
    /// the page: the Secrets vault row's calm note says it ([`observe::VAULT_UNREAD`]), once.
    #[test]
    fn an_unreadable_vault_config_is_said_once_by_its_row() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut host = Fake {
            configured_unreadable: true,
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.detail.starts_with(VAULT_CONFIG_UNREAD)),
            "{:?}",
            report.rows
        );
        let page = report.page.expect("the pass's page");
        assert!(page.rows.is_empty(), "{:?}", page.rows);
        assert!(page.outcome.unfinished.is_empty());
    }

    #[test]
    fn an_unreadable_vault_configuration_joins_nothing_and_counts_no_failure() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut host = Fake {
            configured_unreadable: true,
            pace: Some(Pace {
                build: "1.0.0+gtest".into(),
                marks: Some("unreadable".into()),
                inputs: None,
                materialization: None,
                now: vault::START,
            }),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(host.calls.is_empty(), "{:?}", host.calls);
        let [row] = report.rows.as_slice() else {
            panic!("{:?}", report.rows)
        };
        assert_eq!(row.status, "unverified");
        assert!(
            row.detail
                .starts_with("cannot read which vault this Mac is in: source.toml: no name; "),
            "{row:?}"
        );
        let attempt: Value = aterm_json::from_slice(
            &std::fs::read(root.path().join(secrets_retry::RECORD)).unwrap(),
        )
        .unwrap();
        assert_eq!(attempt["failures"].as_u64(), Some(0), "{attempt:?}");
        assert!(!finished_at(root.path()).unwrap());
        host.pace = None;

        let older = Progress {
            admitted_vault: AdmittedVault::Unknown,
            ..progress(root.path()).unwrap().unwrap()
        };
        save_progress(root.path(), older).unwrap();
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets]);
    }

    /// A vault configuration aterm cannot read (a root-owned `source.toml`) refuses no choice:
    /// the offer admits the whole profile, and a later group choice that keeps Secrets is saved
    /// as the person's, with the vault this Mac is in recorded as none.
    #[test]
    fn an_unreadable_vault_configuration_never_refuses_a_group_choice() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
        let offered = startup
            .offer_reading(root, || Err("source.toml: Permission denied".into()))
            .unwrap();
        assert!(offered.selection.contains(Setting::Secrets));
        startup
            .persist_at(root, &offered)
            .result
            .unwrap()
            .expect("the offer is admitted");
        let mut plan = offered.clone();
        for setting in aterm_types::macos_setup::Group::Desktop.settings() {
            plan.selection.set(*setting, false);
        }
        let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen()
                .changing(Some(group_selection(
                    aterm_types::macos_setup::Group::Desktop,
                )))
                .persist_with(
                    root,
                    &plan,
                    false,
                    || Err("source.toml: Permission denied".into()),
                    save_progress,
                    observe_restart,
                )
        })
        .result;
        assert!(
            matches!(chosen, Ok(Some(_))),
            "the person's press: {chosen:?}"
        );
        assert_eq!(
            progress(root).unwrap().unwrap().admitted_vault,
            AdmittedVault::None
        );
    }

    /// A choice of Secrets made while the vault configuration cannot be read: its Secrets step
    /// only waits, joining and configuring nothing, and goes ahead by itself once the
    /// configuration reads.
    #[test]
    fn a_choice_over_an_unreadable_vault_configuration_waits_and_joins_nothing() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let plan = selected(&[Setting::Secrets]);
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &plan,
                false,
                || Err("source.toml: Permission denied".into()),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("the person's choice is saved");
        let mut host = Fake {
            configured_unreadable: true,
            ..Fake::default()
        };
        execute_initial(&mut host, root, &mut None).unwrap();
        assert!(host.calls.is_empty(), "{:?}", host.calls);
        assert!(!finished_at(root).unwrap());
        host.configured_unreadable = false;
        execute_initial(&mut host, root, &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets]);
    }

    #[test]
    fn a_vault_typed_in_system_state_while_in_another_still_moves_the_mac() {
        use vault::{Lan, START, Vaulted, passes};
        let root = aterm_tempfile::tempdir().unwrap();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        let admitted = AdmittedVault::of(host.configured_vault());
        assert!(matches!(admitted, AdmittedVault::Configured(_)));
        let plan = selected(&[Setting::Secrets]);
        let revision = restart_in_vault(root.path(), &plan, admitted).unwrap();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(record.contains("admitted_vault"), "{record}");
        assert!(!record.contains("vault.example") && !record.contains("fixture-query"));
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let log = lan.log();
        assert!(log[0].starts_with("line "), "{log:?}");
        assert!(
            log.iter().any(|event| event.starts_with("keep ")),
            "{log:?}"
        );
        assert!(log.contains(&format!("join {}", plan.vault_url)), "{log:?}");
    }

    #[test]
    fn a_run_whose_plan_names_the_vault_joined_since_reads_it_as_before() {
        use vault::{Lan, START, Vaulted, passes};
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan {
            vault_url: JOINED_SINCE.into(),
            ..selected(&[Setting::Secrets])
        };
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let log = lan.log();
        assert!(log.iter().any(|event| event == "install"), "{log:?}");
        assert!(
            !log.iter().any(|event| event.starts_with("join ")),
            "{log:?}"
        );
        assert!(rows_ready(&rows), "{rows:?}");
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_run_an_older_build_admitted_joins_as_before() {
        use vault::{Lan, START, Vaulted, authorized, passes};
        let (root, revision) = authorized();
        let older = Progress {
            admitted_vault: AdmittedVault::Unknown,
            ..progress(root.path()).unwrap().unwrap()
        };
        save_progress(root.path(), older).unwrap();
        let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
        assert!(!record.contains("admitted_vault"), "{record}");
        assert_eq!(progress(root.path()).unwrap(), Some(older));
        let lan = Lan::new(true, "office");
        lan.in_vault(JOINED_SINCE, START);
        let mut host = Vaulted::new(&lan);
        passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=0,
            |_, _| {},
        );
        assert_eq!(host.minutes(), [0]);
        let joined = format!("join {}", selected(&[Setting::Secrets]).vault_url);
        assert!(lan.log().contains(&joined), "{:?}", lan.log());
    }

    #[test]
    fn every_admission_keeps_the_vault_this_mac_is_in() {
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        configure_vault(&paths, "http://vault.example:7484/v1/team");
        let read = || ConfiguredVault::read(&paths);
        let vault = AdmittedVault::of(read());
        assert!(matches!(vault, AdmittedVault::Configured(_)));
        let plan = selected(&[Setting::Secrets, Setting::Spotlight]);
        let kept = |root: &Path| progress(root).unwrap().unwrap().admitted_vault;
        for index in 0..4 {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = match index {
                0 => {
                    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                    ticket.offer_reading(root.path(), read).unwrap();
                    ticket.persist_at(root.path(), &plan).result.unwrap()
                }
                1 => Some(restart_reading(root.path(), &plan, read).unwrap()),
                2 => observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                    InitialSetupRestart::default().persist_with(
                        root,
                        &plan,
                        true,
                        read,
                        save_progress,
                        observe_restart,
                    )
                })
                .result
                .unwrap(),
                _ => authorize_reading(root.path(), &plan, read).unwrap(),
            }
            .expect("admitted");
            assert_eq!(kept(root.path()), vault, "admission {index}");
            let mut host = Fake {
                pending: Some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
            assert_eq!(
                kept(root.path()),
                vault,
                "after a pass of admission {index}"
            );
        }
    }

    #[test]
    fn the_startup_review_keeps_the_vault_its_offer_was_loaded_in() {
        let root = aterm_tempfile::tempdir().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let paths = aterm_secrets::client::Paths::for_home(home.path());
        let team = "http://vault.example:7484/v1/team";
        let other = "https://other.example/v1/other";
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        configure_vault(&paths, team);
        let read = || ConfiguredVault::read(&paths);
        let offered = ticket.offer_reading(root.path(), read).unwrap();
        assert_eq!(offered.vault_url, team);
        let typed = Plan {
            vault_url: other.into(),
            ..offered
        };
        let revision = ticket
            .persist_at(root.path(), &typed)
            .result
            .unwrap()
            .expect("admitted");
        assert_eq!(
            progress(root.path()).unwrap().unwrap().admitted_vault,
            AdmittedVault::of(read())
        );
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert_eq!(host.vault_url.as_deref(), Some(other));

        let root = aterm_tempfile::tempdir().unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
        let revision = ticket
            .persist_at(root.path(), &offered)
            .result
            .unwrap()
            .expect("admitted");
        let mut host = Fake {
            configured: read().unwrap(),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
        assert!(finished_at(root.path()).unwrap());
    }

    /// The settings of one page group, as a selection.
    fn group_selection(group: aterm_types::macos_setup::Group) -> Selection {
        let mut selection = Selection::none();
        for setting in group.settings() {
            selection.set(*setting, true);
        }
        selection
    }

    /// Keyboard & Trackpad turned to Leave as is keeps the finished Workstation stage finished,
    /// so the Dock is not set again for another group's choice.
    #[test]
    fn a_group_turned_to_leave_as_is_keeps_the_finished_workstation_stage() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let mut plan = Plan::default();
        for setting in aterm_types::macos_setup::Group::Input.settings() {
            plan.selection.set(*setting, false);
        }
        assert!(preferences(plan.selection).contains(Setting::Dock));
        let revision = system_state_choice(root.path(), &plan);
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "Keyboard & Trackpad to Leave as is sets the Desktop & Dock preferences again: {:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap());
    }

    /// A group turned to Set up after the Workstation stage finished is the only one it sets
    /// again.
    #[test]
    fn a_group_turned_on_after_the_stage_finished_sets_only_that_groups_preferences() {
        use aterm_types::macos_setup::Group;
        use std::sync::Mutex;
        static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
        fn stage(event: StageEvent) {
            EVENTS.lock().unwrap().push((
                event.running,
                event.settings.to_vec(),
                event.subjects.to_vec(),
            ));
        }
        let hooks = SetupHooks {
            stage,
            ..SetupHooks::NONE
        };
        let events = |group: Group| {
            let settings = group.settings().to_vec();
            let subjects = settings
                .iter()
                .copied()
                .map(Subject::Setting)
                .collect::<Vec<_>>();
            vec![
                (true, settings.clone(), subjects.clone()),
                (false, settings, subjects),
            ]
        };
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::default();
        for setting in Group::Desktop.settings() {
            plan.selection.set(*setting, false);
        }
        authorize_at(root.path(), &plan).unwrap();
        let mut first = Fake::default();
        execute_initial(&mut first, root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(
            first.only,
            [None],
            "a run of a whole plan sets all its preferences"
        );
        let revision = system_state_choice(root.path(), &Plan::default());
        let mut host = Fake::default();
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks).unwrap();
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        assert_eq!(
            *EVENTS.lock().unwrap(),
            events(Group::Desktop),
            "only the preferences this pass sets may appear changing or get read back"
        );
        EVENTS.lock().unwrap().clear();
        assert!(finished_at(root.path()).unwrap());
        // Turning Keyboard & Trackpad off and on again sets that group alone.
        let mut plan = Plan::default();
        for setting in Group::Input.settings() {
            plan.selection.set(*setting, false);
        }
        execute_initial_for_revision(
            &mut Fake::default(),
            root.path(),
            system_state_choice(root.path(), &plan),
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        let mut host = Fake::default();
        let revision = system_state_choice(root.path(), &Plan::default());
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks).unwrap();
        assert_eq!(host.only, [Some(group_selection(Group::Input))]);
        assert_eq!(*EVENTS.lock().unwrap(), events(Group::Input));
    }

    #[test]
    fn stage_events_for_one_account_exclude_the_unselected_account_and_keep_accessibility() {
        use aterm_types::macos_setup::Access;
        use std::sync::Mutex;
        static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
        fn stage(event: StageEvent) {
            EVENTS.lock().unwrap().push((
                event.running,
                event.settings.to_vec(),
                event.subjects.to_vec(),
            ));
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Messages]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks {
                stage,
                ..SetupHooks::NONE
            },
        )
        .unwrap();
        assert_eq!(host.calls, [Stage::Accounts]);
        let settings = vec![Setting::Messages];
        let subjects = vec![
            Subject::Setting(Setting::Messages),
            Subject::Access(Access::Accessibility),
        ];
        assert_eq!(
            *EVENTS.lock().unwrap(),
            [
                (true, settings.clone(), subjects.clone()),
                (false, settings, subjects)
            ]
        );
    }

    /// An explicit full profile supersedes a group-scoped Workstation stage,
    /// including when the process ends between authorization and OS effects.
    #[test]
    fn an_interrupted_explicit_full_apply_resumes_every_preference_after_a_partial_choice() {
        use aterm_types::macos_setup::Group;
        for completed in [true, false] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut previous = Plan::default();
            for setting in Group::Desktop.settings() {
                previous.selection.set(*setting, false);
            }
            authorize_at(root.path(), &previous).unwrap();
            execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
            let partial = system_state_choice(root.path(), &Plan::default());
            let desktop = group_selection(Group::Desktop);
            assert_eq!(progress(root.path()).unwrap().unwrap().only, Some(desktop));
            if completed {
                let mut chosen = Fake::default();
                execute_initial_for_revision(
                    &mut chosen,
                    root.path(),
                    partial,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("the chosen group applies");
                assert_eq!(
                    chosen.only,
                    [Some(desktop)],
                    "a normal group choice keeps its scope"
                );
            }

            // The public explicit Apply stages its full profile before touching
            // preferences. Ending there leaves this same durable run to resume.
            save_explicit_workstation_plan(root.path()).unwrap();
            let revision = revision_at(root.path()).unwrap();
            let selected = saved_plan(root.path()).unwrap().unwrap();
            assert_eq!(
                selected.selection.workstation(),
                Selection::all().workstation()
            );
            let mut resumed = Fake::default();
            execute_initial_for_revision(
                &mut resumed,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the interrupted explicit profile resumes");
            assert_eq!(
                resumed.only,
                [None],
                "the full Apply must retire the earlier group restriction"
            );
            assert!(finished_at(root.path()).unwrap());
        }
    }

    /// A finished run whose Mac preferences a Restore put back (`plan` first), with the
    /// record Restore writes.
    fn restored_after(plan: &Plan) -> aterm_tempfile::TempDir {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), plan).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        save_restore(root.path()).unwrap();
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Stopped(Stop::Restored)
        );
        root
    }

    pub(super) fn staged_restored_choice() -> (aterm_tempfile::TempDir, Plan) {
        use aterm_types::macos_setup::Group;
        let root = restored_after(&leaving(&[Group::Desktop]));
        let plan = Plan::default();
        let mut attempt = InitialSetupRestart::chosen();
        let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Ok(None),
                |_, _| Err("injected failure before final admission".into()),
                observe_restart,
            )
        });
        assert!(failed.result.is_err());
        assert!(attempt.pending());
        let staged = progress(root.path()).unwrap().unwrap();
        assert!(!staged.admitted);
        assert_eq!(staged.only, Some(group_selection(Group::Desktop)));
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
        drop(attempt);
        (root, plan)
    }

    #[test]
    fn a_fresh_restored_startup_keeps_the_groups_no_pass_set_again() {
        use aterm_types::macos_setup::Group;
        let (root, plan) = staged_restored_choice();
        let original = revision_at(root.path()).unwrap();
        let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let revision = startup
            .persist_at(root.path(), &plan)
            .result
            .unwrap()
            .unwrap();
        assert_eq!(revision, original + 1);
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input))),
            "Keyboard & Trackpad were restored and no pass set them again"
        );
        assert_ne!(workstation_row(root.path()).status, "applied");
    }

    /// After a Restore, a choice turned Desktop & Dock on and its process ended before the
    /// admission; the next launch's offer is answered with another plan than the one staged: a
    /// group choice made while the offer waited, a Join, or, with no person at all, the staged
    /// plan presented in a vault the Mac joined since. Each is a choice over the Restore, as
    /// the same one made on System State is (design §3.4): it sets only the group the person
    /// turned on, keeps the sign-outs and the access revision, and leaves Keyboard & Trackpad
    /// restored.
    #[test]
    fn a_restored_startup_answer_of_another_plan_sets_only_what_the_person_turned_on() {
        use aterm_types::macos_setup::Group;
        let desktop = group_selection(Group::Desktop);
        for route in ["group choice", "join", "vault joined since"] {
            let (root, staged) = staged_restored_choice();
            let before = progress(root.path()).unwrap().unwrap();
            let home = aterm_tempfile::tempdir().unwrap();
            let url = "https://joined.example/v1/joined";
            let joined = configure_vault(&aterm_secrets::client::Paths::for_home(home.path()), url);
            let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let (plan, answer) = match route {
                "group choice" => {
                    let offered = startup.offer_reading(root.path(), || Ok(None)).unwrap();
                    let mut plan = offered;
                    plan.selection.set(Setting::Spotlight, false);
                    let answer = startup.persist_at(root.path(), &plan);
                    (plan, answer)
                }
                "join" => {
                    startup.offer_reading(root.path(), || Ok(None)).unwrap();
                    let plan = Plan {
                        vault_url: url.into(),
                        ..staged.clone()
                    };
                    let answer = startup.persist_join_at(root.path(), &plan, Some(joined));
                    (plan, answer)
                }
                _ => {
                    let plan = startup
                        .offer_reading(root.path(), || Ok(Some(joined.clone())))
                        .unwrap();
                    assert_ne!(plan, staged, "the offer is presented in the joined vault");
                    let answer = startup.persist_at(root.path(), &plan);
                    (plan, answer)
                }
            };
            let revision = answer.result.unwrap().expect("admitted");
            let admitted = progress(root.path()).unwrap().unwrap();
            assert!(admitted.belongs_to(&plan), "{route}");
            assert_eq!(admitted.only, Some(desktop), "{route}");
            assert_eq!(
                admitted.access_revision(),
                before.access_revision(),
                "{route}: the grants are not handed to macOS again"
            );
            assert_eq!(
                decision_record(root.path()).unwrap(),
                (
                    Decision::Pending,
                    Some(Restored {
                        left: preferences(Selection::all()),
                        queued: Some(desktop),
                        selection: Some(plan.selection),
                        run: None,
                        held: false,
                    })
                ),
                "{route}: the Restore's record measures from the admitted plan"
            );
            let host = run_pass(root.path(), revision);
            assert_eq!(host.only, [Some(desktop)], "{route}");
            for kept in [Stage::Accounts, Stage::Permissions] {
                assert!(!host.calls.contains(&kept), "{route}: {:?}", host.calls);
            }
            assert!(finished_at(root.path()).unwrap(), "{route}");
            assert_eq!(
                restored_left(root.path()),
                (Decision::Pending, Some(group_selection(Group::Input))),
                "{route}: Keyboard & Trackpad were restored and nobody turned them on"
            );
            assert_ne!(workstation_row(root.path()).status, "applied", "{route}");
        }
    }

    /// The startup answer and System State's own choice of the same plan over the same
    /// records admit the same run: one path admits a plan over a Restore's record.
    #[test]
    fn a_restored_startup_answer_admits_what_the_same_choice_on_system_state_does() {
        let mut no_spotlight = Plan::default();
        no_spotlight.selection.set(Setting::Spotlight, false);
        for plan in [Plan::default(), no_spotlight] {
            let (startup_root, _) = staged_restored_choice();
            let mut startup = InitialSetupAuthorization::capture_at(startup_root.path()).unwrap();
            startup
                .persist_at(startup_root.path(), &plan)
                .result
                .unwrap();
            let (window_root, _) = staged_restored_choice();
            system_state_choice(window_root.path(), &plan);
            let (by_startup, by_window) = (
                progress(startup_root.path()).unwrap().unwrap(),
                progress(window_root.path()).unwrap().unwrap(),
            );
            assert_eq!(
                (
                    by_startup.done,
                    by_startup.only,
                    by_startup.access_revision()
                ),
                (by_window.done, by_window.only, by_window.access_revision()),
                "{plan:?}"
            );
            assert_eq!(
                decision_record(startup_root.path()).unwrap(),
                decision_record(window_root.path()).unwrap(),
                "{plan:?}"
            );
        }
    }

    /// The first-run decision, and the Mac preferences it keeps restored, if any.
    fn restored_left(root: &Path) -> (Decision, Option<Selection>) {
        let (decision, restored) = decision_record(root).unwrap();
        (decision, restored.map(|restored| restored.left))
    }

    /// One pass of the admitted run `revision` on a fake host: what it set.
    fn run_pass(root: &Path, revision: u64) -> Fake {
        let mut host = Fake::default();
        execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE)
            .unwrap();
        host
    }

    /// The default plan with `groups` turned to Leave as is.
    fn leaving(groups: &[aterm_types::macos_setup::Group]) -> Plan {
        let mut plan = Plan::default();
        for group in groups {
            for setting in group.settings() {
                plan.selection.set(*setting, false);
            }
        }
        plan
    }

    /// The Workstation row of the run a snapshot shows.
    fn workstation_row(root: &Path) -> Row {
        snapshot_at(root, Plan::default())
            .unwrap()
            .rows
            .into_iter()
            .find(|row| row.name == Stage::Workstation.name())
            .expect("the plan selects Mac settings")
    }

    /// After Restore, a choice of another group, a group turned to Leave as is, or a Join sets no
    /// Mac preference again, and none is recorded applied by this choice or any later one before
    /// a pass sets them.
    #[test]
    fn a_choice_or_a_join_after_restore_leaves_the_restored_preferences_alone() {
        use aterm_types::macos_setup::Group;
        let mut no_spotlight = Plan::default();
        no_spotlight.selection.set(Setting::Spotlight, false);
        let mut no_accounts = Plan::default();
        for setting in Group::Accounts.settings() {
            no_accounts.selection.set(*setting, false);
        }
        for how in ["another group", "leave as is", "join"] {
            let root = restored_after(&Plan::default());
            let revision = match how {
                "another group" => system_state_choice(root.path(), &no_spotlight),
                "leave as is" => system_state_choice(root.path(), &no_accounts),
                _ => system_state_join(root.path(), &Plan::default()),
            };
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Pending,
                "{how}: the run goes on, and the restored preferences are not applied"
            );
            let mut host = Fake::default();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(
                !host.calls.contains(&Stage::Workstation),
                "{how}: {:?}",
                host.calls
            );
            assert!(finished_at(root.path()).unwrap(), "{how}");
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Pending,
                "{how}"
            );
            assert_ne!(workstation_row(root.path()).status, "applied", "{how}");
            // A later choice still knows the preferences stand restored.
            let revision = system_state_choice(root.path(), &Plan::default());
            let mut host = Fake::default();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(!host.calls.contains(&Stage::Workstation), "{how}");
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Pending,
                "{how}"
            );
        }
        // Set up now is the person asking for every preference again.
        let root = restored_after(&Plan::default());
        let revision = system_state_apply(root.path(), &Plan::default(), false, || Ok(None));
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.only, [None]);
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
    }

    /// A Restore leaves both groups it puts back (Keyboard & Trackpad, Desktop & Dock) at Leave
    /// as is (design §3.4): the plan is saved as a person's choice is, a person's and bound to
    /// its run's progress, which keeps every other stage finished and the run's admission, so
    /// aterm's permissions go on by themselves, and starts no run. One later press of Set up
    /// on one of those groups sets that group alone. With no saved plan there is none to
    /// change.
    #[test]
    fn a_restore_leaves_the_groups_it_put_back_as_is_and_starts_nothing() {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let before = progress(root.path()).unwrap().unwrap();
        assert!(before.completed);
        // What `macos_doctor::restore` writes around its effects.
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        let left = leaving(&[Group::Input, Group::Desktop]);
        assert_eq!(
            saved_plan_record(root.path()).unwrap(),
            Some((left.clone(), true))
        );
        assert!(!saved_plan_provisional(root.path()).unwrap());
        let after = progress(root.path()).unwrap().unwrap();
        assert!(after.belongs_to(&left));
        assert_eq!(after.selection, Some(left.selection));
        assert_eq!(after.revision, before.revision + 1);
        assert!(after.admitted, "a Restore withdraws no permission");
        assert!(
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        assert_eq!(after.access_revision(), before.access_revision());
        assert_eq!(
            after.done, before.done,
            "every stage stays finished: a group left as is has nothing to set"
        );
        assert!(admission_at(root.path()).unwrap().is_none());
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Stopped(Stop::Restored)
        );
        // One press of Set up on Keyboard & Trackpad sets that group alone.
        let mut input = left.clone();
        for setting in Group::Input.settings() {
            input.selection.set(*setting, true);
        }
        let revision = system_state_choice(root.path(), &input);
        let staged = progress(root.path()).unwrap().unwrap();
        assert_eq!(staged.only, Some(group_selection(Group::Input)));
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Input))]);
        for kept in [Stage::Accounts, Stage::Permissions, Stage::Secrets] {
            assert!(!host.calls.contains(&kept), "{:?}", host.calls);
        }
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Applied(true),
            "every Mac setting the plan selects is set, and Desktop & Dock is left as is"
        );
        // With no saved plan, there is none to change, and no run to carry aterm's own
        // permissions: the Restore stages them alone for the offer, as a launch would.
        let bare = aterm_tempfile::tempdir().unwrap();
        save_restore(bare.path()).unwrap();
        leave_restored_as_is(bare.path()).unwrap();
        let mut permissions = Selection::none();
        permissions.set(Setting::Permissions, true);
        let staged = saved_plan(bare.path()).unwrap().expect("staged");
        assert_eq!(staged.selection, permissions);
        assert!(!progress(bare.path()).unwrap().unwrap().admitted);
        let revision = launch_startup(bare.path()).expect("the offer admits it");
        assert_eq!(run_pass(bare.path(), revision).calls, [Stage::Permissions]);
    }

    fn restore_plan_publication(interrupted: bool) -> aterm_tempfile::TempDir {
        let root = restored_after(&Plan::default());
        // Restore's actual final metadata path: its preferences are back, but the
        // plan publication can fail after the new decision is durable.
        let restored = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            leave_restored_as_is_with(root, |write| {
                if interrupted && write == RestoreWrite::Plan {
                    Err("injected failure before Restore's plan publication".into())
                } else {
                    Ok(())
                }
            })
        });
        assert_eq!(restored.result.is_err(), interrupted);
        root
    }

    #[test]
    fn an_interrupted_restore_plan_does_not_turn_restored_groups_back_on() {
        use aterm_types::macos_setup::Group;

        for interrupted in [false, true] {
            let root = restore_plan_publication(interrupted);
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Stopped(Stop::Restored)
            );
            assert_eq!(
                decision_record(root.path()).unwrap().1.unwrap().selection,
                Some(leaving(&[Group::Input, Group::Desktop]).selection)
            );
            if interrupted {
                assert_eq!(saved_plan(root.path()).unwrap(), Some(Plan::default()));
            }

            // A new process reads only the retained records. The person changes an
            // unrelated group from that page, without turning any Mac setting on.
            let before = observation_at(root.path()).unwrap();
            let page = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(page.finished);
            assert!(
                page.permissions_allowed,
                "a Restore never withdraws aterm's permissions"
            );
            assert_eq!(page.observation, before);
            let prior = progress(root.path()).unwrap().unwrap();
            if interrupted {
                assert_eq!(page.revision, prior.revision);
                assert_eq!(page.access_revision, prior.access_revision());
                assert!(
                    page.rows.iter().any(|row| {
                        row.name == Stage::Accounts.name() && row.status == "applied"
                    })
                );
            }
            let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            assert_eq!(
                startup.offer_reading(root.path(), || Ok(None)).unwrap(),
                page.plan
            );
            let left = decision_record(root.path()).unwrap().1.unwrap().left;
            assert!(!left.is_empty());
            let mut unrelated = page.plan.clone();
            unrelated.selection.set(Setting::Spotlight, false);
            let revision = system_state_choice(root.path(), &unrelated);
            let host = run_pass(root.path(), revision);
            assert!(
                !host.calls.contains(&Stage::Workstation),
                "Restore's failed plan write turned Mac preferences back on: {:?}",
                host.calls
            );
            assert!(preferences(page.plan.selection).is_empty());
            // The preferences the Restore put back stay unset, so its record survives the
            // unrelated choice, which reads applied.
            assert_eq!(
                restored_left(root.path()),
                (Decision::Applied(true), Some(left))
            );
        }
    }

    #[test]
    fn restore_presentation_keeps_early_decisions_and_later_explicit_groups_distinct() {
        use aterm_types::macos_setup::Group;

        // Restore writes this first decision before its OS work. It still names P,
        // so merely asking for Restore does not fabricate its final Q selection.
        let early = restored_after(&Plan::default());
        let page = snapshot_at(early.path(), Plan::default()).unwrap();
        assert_eq!(page.plan, Plan::default());
        let mut unrelated = page.plan;
        unrelated.selection.set(Setting::Spotlight, false);
        let revision = system_state_choice(early.path(), &unrelated);
        assert!(
            !run_pass(early.path(), revision)
                .calls
                .contains(&Stage::Workstation)
        );
        assert_eq!(
            restored_left(early.path()),
            (Decision::Pending, Some(preferences(Selection::all())))
        );

        // The failed final plan save leaves Q durable. A new explicit Input-on
        // choice from its recovered page still sets Input alone, never Desktop.
        let root = restore_plan_publication(true);
        let mut input = snapshot_at(root.path(), Plan::default()).unwrap().plan;
        for setting in Group::Input.settings() {
            input.selection.set(*setting, true);
        }
        let revision = system_state_choice(root.path(), &input);
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Input))]);
        assert_eq!(saved_plan(root.path()).unwrap(), Some(input));
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
    }

    #[test]
    fn a_restore_without_progress_presents_its_final_choice() {
        for modern in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let original = Plan::default();
            if modern {
                save_plan(root.path(), &original).unwrap();
            } else {
                save_automatic_plan(root.path(), original.selection).unwrap();
            }
            save_restore(root.path()).unwrap();
            let restored = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                leave_restored_as_is_with(root, |write| {
                    if write == RestoreWrite::Plan {
                        Err("injected failure before Restore's plan publication".into())
                    } else {
                        Ok(())
                    }
                })
            });
            assert!(restored.result.is_err());
            let page = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(page.finished);
            assert!(preferences(page.plan.selection).is_empty());
            assert_eq!(saved_plan(root.path()).unwrap(), Some(original));
            assert_eq!(progress(root.path()).unwrap(), None);
            let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            assert_eq!(
                startup.offer_reading(root.path(), || Ok(None)).unwrap(),
                page.plan
            );
        }
    }

    /// After Restore, a group a choice turns to Set up is the only one set again; one whose
    /// pass has not landed stays owed through a later choice of another group. The pass
    /// that sets it records nothing applied while another group stays restored.
    #[test]
    fn a_group_turned_on_after_restore_is_the_only_one_set_again() {
        use aterm_types::macos_setup::Group;
        let mut no_desktop = Plan::default();
        for setting in Group::Desktop.settings() {
            no_desktop.selection.set(*setting, false);
        }
        let root = restored_after(&no_desktop);
        let revision = system_state_choice(root.path(), &Plan::default());
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
        let mut failing = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut failing,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(failing.only, [Some(group_selection(Group::Desktop))]);
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
        let mut no_spotlight = Plan::default();
        no_spotlight.selection.set(Setting::Spotlight, false);
        let revision = system_state_choice(root.path(), &no_spotlight);
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        // Keyboard & Trackpad were never set again: the record still stands restored.
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input)))
        );
    }

    /// A group turned to Set up whose pass failed, then back to Leave as is, leaves the stage
    /// finished: it owes nothing.
    #[test]
    fn a_group_turned_on_then_off_before_its_pass_finishes_the_stage() {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let mut no_desktop = Plan::default();
        for setting in Group::Desktop.settings() {
            no_desktop.selection.set(*setting, false);
        }
        let revision = system_state_choice(root.path(), &no_desktop);
        execute_initial_for_revision(
            &mut Fake::default(),
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        // Desktop & Dock back to Set up: its pass fails (offline Dock, say).
        let revision = system_state_choice(root.path(), &Plan::default());
        let mut failing = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut failing,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(failing.only, [Some(group_selection(Group::Desktop))]);
        // The person gives up on it: Desktop & Dock to Leave as is again.
        let revision = system_state_choice(root.path(), &no_desktop);
        let record = progress(root.path()).unwrap().unwrap();
        assert_ne!(record.done & Stage::Workstation.bit(), 0, "{record:?}");
        assert_eq!(record.only, None);
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
    }

    /// After Restore, a choice that turns Desktop & Dock and the sign-outs back to Set up
    /// sets only that group of the Mac preferences, and neither its pass nor the sign-outs'
    /// records anything applied: Keyboard & Trackpad, which no pass set again, still read
    /// restored, so the Workstation row never reads applied (design §3.3: nothing restored
    /// reads applied until a pass sets it).
    #[test]
    fn a_restored_group_no_pass_set_does_not_read_applied() {
        use aterm_types::macos_setup::Group;
        let mut no_desktop = Plan::default();
        for setting in Group::Desktop
            .settings()
            .iter()
            .chain(Group::Accounts.settings())
        {
            no_desktop.selection.set(*setting, false);
        }
        let root = restored_after(&no_desktop);
        let revision = system_state_choice(root.path(), &Plan::default());
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(
            host.only,
            [Some(group_selection(Group::Desktop))],
            "only Desktop was set"
        );
        assert!(host.calls.contains(&Stage::Accounts), "{:?}", host.calls);
        let row = workstation_row(root.path());
        assert_ne!(
            row.status,
            "applied",
            "Keyboard & Trackpad were restored and never set again: {row:?}, {:?}",
            decision_record(root.path())
        );
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input)))
        );
        // Set up now sets every group again, and only then is the profile applied.
        let revision = restart_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.only, [None]);
        assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
    }

    /// A choice after Restore cut short before its decision landed (a crash or an I/O
    /// error after its plan was saved, or after its progress too, before admission) leaves
    /// the Restore's record, which names the plan it was taken against. The next choice is
    /// measured from that plan, not from the plan the cut-short choice saved, so Desktop &
    /// Dock, which the person turned on, is set; and it is the only step that runs: the
    /// run's other steps are measured from the plan its progress was staged for, so
    /// Spotlight, Permissions (whose access revision stands), the vault and the sign-outs
    /// stay as the run left them.
    #[test]
    fn a_choice_whose_decision_write_failed_is_not_lost() {
        use aterm_types::macos_setup::Group;
        for final_restore in [false, true] {
            for cut in ["after its plan", "after its progress"] {
                let root = restored_after(&leaving(&[Group::Desktop]));
                if final_restore {
                    leave_restored_as_is(root.path()).unwrap();
                }
                let mut choice = saved_plan(root.path()).unwrap().unwrap();
                for setting in Group::Desktop.settings() {
                    choice.selection.set(*setting, true);
                }
                let access = access_revision_at(root.path()).unwrap();
                let before = |name: &str| std::fs::read(root.path().join(name)).unwrap();
                let (decision, progress_record) =
                    (before("first-run.json"), before("initial-setup.json"));
                system_state_choice(root.path(), &choice);
                // What that choice leaves when it stops at `cut`.
                std::fs::write(root.path().join("first-run.json"), &decision).unwrap();
                if cut == "after its plan" {
                    std::fs::write(root.path().join("initial-setup.json"), &progress_record)
                        .unwrap();
                } else {
                    let staged = progress(root.path()).unwrap().unwrap();
                    save_progress(
                        root.path(),
                        Progress {
                            admitted: false,
                            ..staged
                        },
                    )
                    .unwrap();
                }
                assert_eq!(saved_plan(root.path()).unwrap(), Some(choice.clone()));
                let page = snapshot_at(root.path(), Plan::default()).unwrap();
                assert_eq!(page.plan, choice, "the later staged choice stays visible");
                let revision = system_state_choice(root.path(), &page.plan);
                assert_eq!(
                    access_revision_at(root.path()).unwrap(),
                    access,
                    "{cut}: nothing is handed to macOS again"
                );
                let host = run_pass(root.path(), revision);
                assert_eq!(host.calls, [Stage::Workstation], "{cut}");
                assert_eq!(
                    host.only,
                    [Some(group_selection(Group::Desktop))],
                    "{cut}: the group the person turned on is set"
                );
            }
        }
    }

    /// A choice cut short once it saved its plan, before its progress landed, never ran:
    /// the run is still the one its progress records, staged for the plan before it, which
    /// the progress keeps. The next choice is measured from that plan, not from the one the
    /// cut-short choice saved, so it runs only what it changed: Desktop & Dock turned on
    /// sets that group alone, and a Join runs only the vault's steps. Spotlight, Permissions
    /// (whose access revision stands, so nothing is handed to macOS again) and the
    /// sign-outs (Messages and FaceTime are not signed out again) stay as the run left
    /// them. NEGATIVE CONTROL: Set up now still runs every step.
    #[test]
    fn a_choice_cut_short_after_its_plan_runs_only_what_it_changed() {
        use aterm_types::macos_setup::Group;
        let joined = Plan {
            vault_url: "http://team.local:7484/v1/team".into(),
            ..Plan::default()
        };
        for (how, first, then, runs) in [
            (
                "a choice",
                leaving(&[Group::Desktop]),
                Plan::default(),
                &[Stage::Workstation][..],
            ),
            (
                "a join",
                Plan::default(),
                joined.clone(),
                &[Stage::Secrets, Stage::Repositories][..],
            ),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &first).unwrap();
            execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
            assert!(finished_at(root.path()).unwrap(), "{how}");
            let access = access_revision_at(root.path()).unwrap();
            let choose = |root: &Path| {
                if how == "a join" {
                    system_state_join(root, &then)
                } else {
                    system_state_choice(root, &then)
                }
            };
            let before = |name: &str| std::fs::read(root.path().join(name)).unwrap();
            let (decision, progress_record) =
                (before("first-run.json"), before("initial-setup.json"));
            choose(root.path());
            // What that choice leaves when it stops after its plan.
            std::fs::write(root.path().join("first-run.json"), &decision).unwrap();
            std::fs::write(root.path().join("initial-setup.json"), &progress_record).unwrap();
            assert_eq!(
                saved_plan(root.path()).unwrap(),
                Some(then.clone()),
                "{how}"
            );
            let revision = choose(root.path());
            assert_eq!(
                access_revision_at(root.path()).unwrap(),
                access,
                "{how}: nothing is handed to macOS again"
            );
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, runs, "{how}");
            if how == "a choice" {
                assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
            }
            assert!(finished_at(root.path()).unwrap(), "{how}");
            let revision = system_state_apply(root.path(), &then, false, || Ok(None));
            let host = run_pass(root.path(), revision);
            assert!(
                host.calls.contains(&Stage::Workstation) && host.calls.contains(&Stage::Accounts),
                "{how}: Set up now runs every step: {:?}",
                host.calls
            );
        }
    }

    /// A choice staged but never admitted (its admission commit failed) and then a Restore:
    /// the Restore stands over it. A later choice of another group sets no Mac preference
    /// again, not even the group that choice turned on before the Restore.
    #[test]
    fn a_choice_staged_before_a_restore_is_not_set_after_it() {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &leaving(&[Group::Desktop])).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &Plan::default(),
                false,
                || Ok(None),
                |_, _| Err("disk full".into()),
                observe_restart,
            )
        })
        .result;
        assert!(failed.is_err(), "{failed:?}");
        let staged = progress(root.path()).unwrap().unwrap();
        assert!(!staged.admitted, "{staged:?}");
        assert_eq!(staged.only, Some(group_selection(Group::Desktop)));
        save_restore(root.path()).unwrap();
        let mut no_spotlight = Plan::default();
        no_spotlight.selection.set(Setting::Spotlight, false);
        let revision = system_state_choice(root.path(), &no_spotlight);
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "Desktop & Dock was set after the Restore: {:?}",
            host.calls
        );
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(preferences(Selection::all())))
        );
    }

    /// After a Restore, every group the plan selects set again piece by piece reads
    /// applied, as Set up now does: Desktop & Dock turned on (its pass sets it), then
    /// Keyboard & Trackpad turned to Leave as is and back to Set up (its pass sets it).
    /// Each pass takes its group out of the restored record.
    #[test]
    fn every_group_set_again_since_a_restore_reads_applied() {
        use aterm_types::macos_setup::Group;
        let root = restored_after(&leaving(&[Group::Desktop]));
        let revision = system_state_choice(root.path(), &Plan::default());
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input)))
        );
        assert_ne!(workstation_row(root.path()).status, "applied");
        // Keyboard & Trackpad to Leave as is: nothing is set, and nothing the plan
        // selects stands restored any more.
        let revision = system_state_choice(root.path(), &leaving(&[Group::Input]));
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{:?}",
            host.calls
        );
        // And back to Set up: its pass sets it, and the profile reads applied.
        let revision = system_state_choice(root.path(), &Plan::default());
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Input))]);
        assert_eq!(restored_left(root.path()), (Decision::Applied(true), None));
        assert_eq!(workstation_row(root.path()).status, "applied");
    }

    /// A Restore record an older aterm wrote names no preferences, and reads as every
    /// one restored; so does a pending record of a choice since with `"restored": true`,
    /// which kept its queue in the run's progress, as before.
    #[test]
    fn an_older_restored_record_reads_every_preference_restored() {
        use aterm_types::macos_setup::Group;
        let root = restored_after(&leaving(&[Group::Desktop]));
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        assert_eq!(
            restored_left(root.path()),
            (
                Decision::Stopped(Stop::Restored),
                Some(preferences(Selection::all()))
            )
        );
        system_state_choice(root.path(), &Plan::default());
        // What an older aterm's choice wrote, its queue in the progress.
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"pending","restored":true}),
        )
        .unwrap();
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(preferences(Selection::all())))
        );
        let mut no_spotlight = Plan::default();
        no_spotlight.selection.set(Setting::Spotlight, false);
        let revision = system_state_choice(root.path(), &no_spotlight);
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input)))
        );
    }

    /// System State's group choice from a page that changed only `changed` since it last took
    /// the plan in force ([`InitialSetupRestart::changing`]): the admitted run's revision.
    fn page_choice(root: &Path, plan: &Plan, changed: Selection) -> u64 {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen()
                .changing(Some(changed))
                .persist_with(
                    root,
                    plan,
                    false,
                    || Ok(None),
                    save_progress,
                    observe_restart,
                )
        })
        .result
        .unwrap()
        .expect("admitted")
    }

    /// A choice takes only the groups the page changed onto the plan in force (design §3.3).
    /// A press made while Restore ran was sent with the plan the page showed before it, where
    /// Keyboard & Trackpad and Desktop & Dock were still Set up: admitted after the Restore,
    /// it changes Messages & FaceTime alone, never sets the restored groups again, and keeps
    /// the person's own press, a group pressed and pressed back included.
    #[test]
    fn a_choice_from_a_page_shown_before_restore_never_sets_the_restored_groups_again() {
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        let restored = leaving(&[Group::Input, Group::Desktop]);
        let stale = leaving(&[Group::Accounts]);
        let revision = page_choice(root.path(), &stale, group_selection(Group::Accounts));
        let saved = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(
            saved,
            leaving(&[Group::Input, Group::Desktop, Group::Accounts]),
            "the press is kept, and the restored groups stay as is"
        );
        let staged = progress(root.path()).unwrap().unwrap();
        assert_eq!(staged.only, None, "{staged:?}");
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{:?}",
            host.calls
        );
        // Pressed and pressed back while the first was admitted: the page's value stands.
        page_choice(root.path(), &restored, group_selection(Group::Accounts));
        assert_eq!(saved_plan(root.path()).unwrap(), Some(restored));
    }

    /// A Restore cut short at any of its writes leaves the records whole (design §3.4): a
    /// choice made from the page as it shows the plan in force sets none of the restored
    /// groups for a change of another group, and sets the group a press of Set up turns on.
    #[test]
    fn a_restore_cut_short_at_any_write_never_lets_another_choice_undo_it() {
        use aterm_types::macos_setup::Group;
        for cut in [
            Some(RestoreWrite::Record),
            Some(RestoreWrite::Plan),
            Some(RestoreWrite::Progress),
            None,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_at(root.path(), &Plan::default()).unwrap();
            execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
            save_restore(root.path()).unwrap();
            let ended = leave_restored_as_is_with(root.path(), |write| {
                if Some(write) == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            assert_eq!(ended.is_err(), cut.is_some(), "{cut:?}");
            // The page shows the plan in force; Rust in Spotlight is turned to Leave as is.
            let mut shown = snapshot_at(root.path(), Plan::default()).unwrap().plan;
            shown.selection.set(Setting::Spotlight, false);
            let mut spotlight = Selection::none();
            spotlight.set(Setting::Spotlight, true);
            let host = run_pass(root.path(), page_choice(root.path(), &shown, spotlight));
            assert!(
                !host.calls.contains(&Stage::Workstation),
                "{cut:?}: an unrelated choice after Restore set {:?}",
                host.only
            );
            if shown.selection.contains(Setting::Dock) {
                continue;
            }
            for setting in Group::Desktop.settings() {
                shown.selection.set(*setting, true);
            }
            let host = run_pass(
                root.path(),
                page_choice(root.path(), &shown, group_selection(Group::Desktop)),
            );
            assert_eq!(
                host.only,
                [Some(group_selection(Group::Desktop))],
                "{cut:?}: Set up on Desktop & Dock sets that group"
            );
        }
    }

    /// A person's Stop, a Restore, then a plan record that cannot be read: the set-aside holds
    /// the Stop in the Restore's decision ([`Restored::held`]), so no bring-forward stages
    /// aterm's permissions over it and no launch runs them; a Restore pressed again keeps it.
    #[test]
    fn a_stop_a_set_aside_held_is_never_brought_forward_by_a_restore() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let plan = plan_of(
            &[Setting::Permissions],
            &[aterm_types::macos_setup::Group::Desktop],
        );
        let revision = system_state_choice(root, &plan);
        request_stop_at(root, revision).unwrap();
        save_restore(root).unwrap();
        leave_restored_as_is(root).unwrap();
        assert_eq!(
            bring_forward_restored_at(root).unwrap(),
            None,
            "the Stop holds"
        );
        assert_eq!(launch_startup(root), None);

        std::fs::write(root.join("automatic-plan.json"), CORRUPT).unwrap();
        set_aside_unreadable_setup_at(root, false)
            .unwrap()
            .expect("the damaged plan is set aside");
        assert!(saved_plan(root).unwrap().unwrap().selection.is_empty());
        let (decision, restored) = decision_record(root).unwrap();
        assert_eq!(decision, Decision::Stopped(Stop::Restored));
        assert!(
            restored.is_some_and(|restored| restored.held),
            "held in the decision"
        );
        assert_eq!(bring_forward_restored_at(root).unwrap(), None);
        assert_eq!(launch_startup(root), None, "nothing runs");
        assert!(!permissions_allowed(root));

        save_restore(root).unwrap();
        leave_restored_as_is(root).unwrap();
        assert!(
            decision_record(root)
                .unwrap()
                .1
                .is_some_and(|restored| restored.held)
        );
        assert_eq!(
            launch_startup(root),
            None,
            "a Restore pressed again keeps the hold"
        );
    }

    /// A Stop of a staged run nobody admitted, whose save lands after a Restore carried that run
    /// on, holds the run it was carried on to ([`Progress::continues`]); so does one that lands
    /// after the Restore staged aterm's permissions over it for the launch's offer.
    #[test]
    fn a_stop_saved_after_a_restore_carried_its_unadmitted_run_still_holds_it() {
        let stage = |root: &Path| {
            authorize_at(
                root,
                &plan_of(
                    &[Setting::Permissions],
                    &[aterm_types::macos_setup::Group::Desktop],
                ),
            )
            .unwrap();
            let mut staged = progress(root).unwrap().unwrap();
            staged.admitted = false;
            save_progress(root, staged).unwrap();
            staged.revision
        };
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let staged = stage(root);
        save_restore(root).unwrap();
        leave_restored_as_is_with(root, |write| {
            if write == RestoreWrite::Run {
                request_stop_at(root, staged)?;
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(bring_forward_restored_with(root, |_| Ok(())).unwrap(), None);
        assert_eq!(launch_startup(root), None);

        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let staged = stage(root);
        save_restore(root).unwrap();
        leave_restored_as_is(root).unwrap();
        assert!(
            progress(root).unwrap().unwrap().revision > staged,
            "staged for the offer"
        );
        request_stop_at(root, staged).unwrap();
        assert_eq!(bring_forward_restored_at(root).unwrap(), None);
        assert_eq!(
            launch_startup(root),
            None,
            "the late Stop holds the staged run"
        );
    }

    /// A Restore never withdraws aterm's permissions (design §3.4, §6): the run it ends keeps
    /// its admission and its access revision, so the access steps go on by themselves, while no
    /// pass runs. A person's Stop of that run still holds them.
    #[test]
    fn a_restore_keeps_the_permissions_approval_and_a_persons_stop() {
        for stop in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &Plan::default())
                .unwrap()
                .unwrap();
            let mut first = Fake {
                waiting: Some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial(&mut first, root.path(), &mut None).unwrap();
            if stop {
                request_stop_at(root.path(), revision).unwrap();
            }
            let before = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(before.permissions_allowed, !stop);
            save_restore(root.path()).unwrap();
            leave_restored_as_is(root.path()).unwrap();
            let after = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(after.permissions_allowed, !stop, "stop={stop}");
            assert_eq!(after.access_revision, before.access_revision);
            assert!(after.finished);
            assert!(admission_at(root.path()).unwrap().is_none());
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty(), "a Restore starts no run");
        }
    }

    /// `aterm doctor --apply` after a Restore admits its plan and finishes it, aterm's
    /// permissions kept (design §6), and its Widgets change waiting for Full Disk Access
    /// follows once the grant comes; the next launch runs nothing again.
    #[test]
    fn an_explicit_apply_after_restore_finishes_and_its_widgets_change_follows() {
        use super::super::tests::{WIDGETS, Withholding};
        use aterm_types::macos_setup::Group;
        for whole_restore in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut plan = Plan::from(group_selection(Group::Desktop));
            for setting in Group::Input.settings() {
                plan.selection.set(*setting, true);
            }
            plan.selection.set(Setting::Permissions, true);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the run executes");
            assert!(finished_at(root.path()).unwrap());
            let mut doctor = stages.doctor;
            if whole_restore {
                doctor.restore(root.path()).unwrap();
            } else {
                save_restore(root.path()).unwrap();
            }
            doctor.explicit_apply(root.path()).unwrap();
            assert!(
                finished_at(root.path()).unwrap(),
                "whole_restore={whole_restore}: {:?}",
                progress(root.path()).unwrap()
            );
            assert!(
                saved_plan(root.path())
                    .unwrap()
                    .unwrap()
                    .selection
                    .contains(Setting::Permissions)
            );
            assert!(
                snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .permissions_allowed
            );
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
            doctor.grant();
            assert!(doctor.follow_up(root.path()).unwrap().is_some());
            assert!(doctor.notification_center_cleared());
            assert!(Withholding::withheld(root.path()).is_empty());
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty(), "{:?}", host.calls);
        }
    }

    /// The Widgets change a pass makes once Full Disk Access arrives changes the row as a stage
    /// does, so it is fenced the same way (design §3.5): the row paints busy while it runs, a
    /// reading taken meanwhile is dropped, and the row is read again once it ends.
    #[test]
    fn a_runs_deferred_widgets_change_is_fenced_from_readings() {
        use super::super::tests::Withholding;
        use std::sync::Mutex;
        static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
        fn stage(event: StageEvent) {
            EVENTS.lock().unwrap().push((
                event.running,
                event.settings.to_vec(),
                event.subjects.to_vec(),
            ));
        }
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        stages.grant_in_permissions = true;
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks {
                stage,
                ..SetupHooks::NONE
            },
        )
        .unwrap()
        .expect("the run executes");
        assert!(stages.doctor.notification_center_cleared());
        let widgets = (
            vec![Setting::Widgets],
            vec![Subject::Setting(Setting::Widgets)],
        );
        let events = EVENTS.lock().unwrap().clone();
        assert_eq!(
            events[events.len() - 2..],
            [
                (true, widgets.0.clone(), widgets.1.clone()),
                (false, widgets.0, widgets.1)
            ],
            "{events:?}"
        );
    }

    /// A Restore's put-back that waits for Full Disk Access lets go of a group a person set up
    /// again since: once the grant comes, the follow-up makes setup's change, never the
    /// Restore's.
    #[test]
    fn a_waiting_restore_lets_go_of_a_group_set_up_again_since() {
        use super::super::tests::Withholding;
        use aterm_types::macos_setup::Group;
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan::from(group_selection(Group::Desktop));
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        stages.doctor.grant();
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the run executes");
        assert!(stages.doctor.notification_center_cleared());
        stages.doctor.revoke();
        stages.doctor.restore(root.path()).unwrap();
        assert_eq!(
            stages.doctor.owed(root.path()),
            Vec::<Setting>::new(),
            "nothing to do without the grant"
        );
        let revision = page_choice(root.path(), &plan, group_selection(Group::Desktop));
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the chosen run executes");
        stages.doctor.grant();
        let followed = stages.doctor.follow_up(root.path()).unwrap();
        assert!(
            followed.is_none_or(|report| report.restore.is_none()),
            "the Restore's put-back is let go"
        );
        assert!(stages.doctor.notification_center_cleared());
        assert!(
            super::super::restore_waiting(root.path())
                .unwrap()
                .is_empty()
        );
    }

    /// A Restore's put-back that waits for Full Disk Access survives an admission that never
    /// turned its group to Set up, whatever it is: a choice of another group, Set up now of the
    /// plan the page shows, or a Join (design §3.4: "put back by itself once aterm has the grant
    /// ... unless a choice since turned its group to Set up"). The record stays; that the plan
    /// owes no Mac setting decides only that the decision reads applied.
    #[test]
    fn a_waiting_restore_survives_any_admission_that_leaves_its_group_alone() {
        use super::super::tests::Withholding;
        use aterm_types::macos_setup::Group;
        for how in ["choice", "set-up-now", "join"] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut selection = group_selection(Group::Desktop);
            selection.set(Setting::Permissions, true);
            selection.set(Setting::Spotlight, true);
            let plan = Plan::from(selection);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            stages.doctor.grant();
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the run executes");
            assert!(stages.doctor.notification_center_cleared());
            stages.doctor.revoke();
            stages.doctor.restore(root.path()).unwrap();
            assert!(
                !super::super::restore_waiting(root.path())
                    .unwrap()
                    .is_empty(),
                "{how}: the Widgets put-back waits for Full Disk Access"
            );
            // The page shows the plan in force: Desktop & Dock left as is by the Restore.
            let shown = snapshot_at(root.path(), plan.clone()).unwrap().plan;
            assert!(!shown.selection.contains(Setting::Widgets), "{how}");
            let revision = match how {
                "choice" => {
                    // Rust in Spotlight turned to Leave as is.
                    let mut sent = shown.clone();
                    sent.selection.set(Setting::Spotlight, false);
                    page_choice(root.path(), &sent, group_selection(Group::Developer))
                }
                "set-up-now" => {
                    observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                        InitialSetupRestart::default()
                            .changing(Some(Selection::none()))
                            .persist_with(
                                root,
                                &shown,
                                false,
                                || Ok(None),
                                save_progress,
                                observe_restart,
                            )
                    })
                    .result
                    .unwrap()
                    .expect("admitted")
                }
                _ => system_state_join(root.path(), &shown),
            };
            assert_eq!(
                decision_at(root.path()).unwrap(),
                Decision::Applied(true),
                "{how}: the plan owes no Mac setting"
            );
            assert!(
                decision_record(root.path()).unwrap().1.is_some(),
                "{how}: the Restore's record stays"
            );
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(
                stages.doctor.notification_center_cleared(),
                "{how}: nothing was put back before the grant"
            );
            stages.doctor.grant();
            stages.doctor.follow_up(root.path()).unwrap();
            assert!(
                !stages.doctor.notification_center_cleared(),
                "{how}: the grant arrived, and the Restore's waiting Widgets put-back is done \
                 (first-run: {})",
                std::fs::read_to_string(root.path().join("first-run.json")).unwrap()
            );
            assert!(
                super::super::restore_waiting(root.path())
                    .unwrap()
                    .is_empty(),
                "{how}"
            );
        }
    }

    /// A choice cut short anywhere between its first write and its admission (design §3.3)
    /// leaves setup resumable: the next launch's review binds whatever the records hold, offers
    /// the plan saved, and admits its answer measured from the run the choice left, so the
    /// stages it left alone stay finished and the person never reads an engine error. A pass
    /// never runs while the saved plan and the run's progress disagree.
    #[test]
    fn the_next_launch_after_a_choice_cut_short_anywhere_resumes_setup() {
        for cut in [
            Some(RestartWrite::Plan),
            Some(RestartWrite::Progress),
            Some(RestartWrite::Reset),
            Some(RestartWrite::Decision),
            None,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &Plan::default())
                .unwrap()
                .unwrap();
            let mut first = Fake {
                waiting: Some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial_for_revision(
                &mut first,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(first.calls.contains(&Stage::Workstation));
            assert!(first.calls.contains(&Stage::Accounts));
            // The page's choice: Rust in Spotlight to Leave as is, its process ended before
            // `cut` (`None`: before its admission's own save).
            let mut sent = Plan::default();
            sent.selection.set(Setting::Spotlight, false);
            let staged = stage_restart_with(
                root.path(),
                &sent,
                Restart {
                    chosen: true,
                    joins: false,
                },
                AdmittedVault::None,
                save_progress,
                |write| {
                    if Some(write) == cut {
                        Err("the process ended here".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert_eq!(staged.is_ok(), cut.is_none(), "{cut:?}");
            if cut == Some(RestartWrite::Progress) {
                assert!(
                    revision_at(root.path()).is_err(),
                    "no pass runs on a plan its progress does not belong to"
                );
            }
            assert!(!finished_at(root.path()).unwrap(), "{cut:?}");
            if cut == Some(RestartWrite::Plan) {
                // Nothing landed: the next launch resumes the run the choice would have replaced.
                let admission = admission_at(root.path()).unwrap().expect("admitted");
                assert_eq!((admission.revision, admission.completed), (revision, false));
                let host = run_pass(root.path(), revision);
                assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
                continue;
            }
            // The next launch: the choice's run is not admitted, so it offers the plan saved.
            assert_eq!(admission_at(root.path()).unwrap(), None, "{cut:?}");
            let mut ticket = InitialSetupAuthorization::capture_at(root.path())
                .unwrap_or_else(|why| panic!("{cut:?}: the next launch's review binds: {why}"));
            let records = observation_at(root.path()).unwrap().records;
            assert_eq!(ticket.revision(), review_revision(&records).unwrap());
            let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
            assert_eq!(offered, sent, "{cut:?}: the plan saved is offered");
            let revision = ticket
                .persist_at(root.path(), &offered)
                .result
                .unwrap()
                .unwrap_or_else(|| panic!("{cut:?}: the answer is admitted"));
            let host = run_pass(root.path(), revision);
            assert!(
                !host.calls.contains(&Stage::Workstation) && !host.calls.contains(&Stage::Accounts),
                "{cut:?}: the Dock and the sign-outs stay as the first run left them: {:?}",
                host.calls
            );
            assert_eq!(
                host.calls,
                [Stage::Secrets, Stage::Repositories],
                "{cut:?}: the vault steps the first run left waiting run"
            );
        }
    }

    /// The startup's answer after a choice cut short once its staged writes landed, whatever the
    /// page answered while the offer waited (a group choice, or a Join), is measured as a choice
    /// on the page is (design §3.3): no Dock reset and no sign-out again for another group, and
    /// a Join runs only the vault steps again.
    #[test]
    fn a_startup_answer_after_a_choice_cut_short_keeps_the_dock_and_sign_outs() {
        use aterm_types::macos_setup::Group;
        for join in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &Plan::default())
                .unwrap()
                .unwrap();
            let mut first = Fake {
                waiting: Some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial_for_revision(
                &mut first,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(first.calls.contains(&Stage::Workstation));
            assert!(first.calls.contains(&Stage::Accounts));
            // On the page: Rust in Spotlight to Leave as is, cut short before its final save.
            let mut sent = Plan::default();
            sent.selection.set(Setting::Spotlight, false);
            let mut spotlight = Selection::none();
            spotlight.set(Setting::Spotlight, true);
            let cut = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                InitialSetupRestart::chosen()
                    .changing(Some(spotlight))
                    .persist_with(
                        root,
                        &sent,
                        false,
                        || Ok(None),
                        |_, _| Err("the process ended here".into()),
                        observe_restart,
                    )
            });
            assert!(cut.result.is_err());
            // The next launch offers the saved plan; the person answers it.
            let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
            assert_eq!(offered, sent);
            let revision = if join {
                let mut typed = offered.clone();
                typed.vault_url = "https://vault.example/v1/team".into();
                ticket.persist_join_at(root.path(), &typed, None)
            } else {
                // Keyboard & Trackpad turned to Leave as is while the offer waited.
                let mut answer = offered.clone();
                for setting in Group::Input.settings() {
                    answer.selection.set(*setting, false);
                }
                ticket.persist_at(root.path(), &answer)
            }
            .result
            .unwrap()
            .expect("admitted");
            let host = run_pass(root.path(), revision);
            assert!(
                !host.calls.contains(&Stage::Accounts) && !host.calls.contains(&Stage::Workstation),
                "join={join}: stages run again {:?}",
                host.calls
            );
            assert_eq!(
                host.calls,
                [Stage::Secrets, Stage::Repositories],
                "join={join}"
            );
        }
    }

    /// A person's Stop is never lost to a Restore: one saved after the Restore wrote its run's
    /// next revision still holds that run (it carries on the run the Stop named), so aterm's
    /// permissions stay held after a relaunch, as before the Restore. A Stop that came after the
    /// run finished stays spent, and a choice made after it starts a run the Stop does not hold.
    #[test]
    fn a_stop_saved_after_a_restore_wrote_its_revision_still_holds_the_run() {
        for finished in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = authorize_at(root.path(), &Plan::default())
                .unwrap()
                .unwrap();
            let mut first = Fake {
                waiting: (!finished).then_some(Stage::Secrets),
                ..Fake::default()
            };
            execute_initial_for_revision(
                &mut first,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(
                snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .permissions_allowed
            );
            // Restore, while the person's Stop of `revision` is still being saved.
            save_restore(root.path()).unwrap();
            leave_restored_as_is(root.path()).unwrap();
            assert_eq!(
                progress(root.path()).unwrap().unwrap().revision,
                revision + 1
            );
            request_stop_at(root.path(), revision).unwrap();
            // The next launch.
            let held = !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed;
            assert_eq!(held, !finished, "finished: {finished}");
            assert_eq!(Records::read(root.path()).unwrap().stopped_run(), !finished);
            fold_stop_request(root.path()).unwrap();
            assert_eq!(progress(root.path()).unwrap().unwrap().stopped, !finished);
            assert!(
                stop_request_at(root.path()).is_none(),
                "the request is spent"
            );
            // A choice after it runs as a run of its own.
            let choice = system_state_choice(root.path(), &leaving(&[]));
            assert!(!stopped(root.path()).unwrap());
            assert_eq!(revision_at(root.path()).unwrap(), choice);
        }
    }

    /// A page choice made while another holds the profile lock for a moment (the ALab watch
    /// writing its upgrade) waits for it and is then admitted; it is never dropped. Only a holder
    /// that outlasts the wait refuses it, in the person's words.
    #[test]
    fn a_choice_waits_for_the_profile_lock_another_holds_briefly() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut sent = Plan::default();
        sent.selection.set(Setting::Spotlight, false);
        let choose = |root: &Path| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &sent,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        };
        let mut watch = Some(HeldLock::take(root.path()).unwrap());
        let mut waits = 0;
        let admitted = observe_choice_at(
            root.path(),
            None,
            || {
                waits += 1;
                // The watch's write ends.
                watch = None;
                true
            },
            choose,
        );
        assert_eq!(waits, 1);
        let revision = admitted.result.unwrap().expect("the choice is admitted");
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert_eq!(saved_plan(root.path()).unwrap(), Some(sent.clone()));
        let _holder = HeldLock::take(root.path()).unwrap();
        let refused = observe_choice_at(root.path(), None, || false, choose);
        assert_eq!(refused.result, Err(SETUP_BUSY.to_string()));
        assert_eq!(revision_at(root.path()).unwrap(), revision);
    }

    /// A press that finds the profile lock another setup holds says so the moment it starts
    /// waiting, before its first wait (System State design §3.3), so the page shows it waiting
    /// from then, never a run going for the ten seconds the wait can last; a press that finds
    /// the lock free says nothing.
    #[test]
    fn a_press_says_it_waits_the_moment_it_finds_the_lock_held() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut sent = Plan::default();
        sent.selection.set(Setting::Dock, false);
        let said = Arc::new(AtomicUsize::new(0));
        let press = || {
            let said = Arc::clone(&said);
            InitialSetupRestart::chosen().noting_wait(move || {
                said.fetch_add(1, Ordering::AcqRel);
            })
        };
        let mut holder = Some(HeldLock::take(root.path()).unwrap());
        let mut waits = 0;
        let admitted = press().persist_at(
            root.path(),
            None,
            || {
                waits += 1;
                assert_eq!(
                    said.load(Ordering::Acquire),
                    1,
                    "said before the first wait"
                );
                holder = None;
                true
            },
            &sent,
            false,
            || Ok(None),
        );
        assert!(admitted.result.unwrap().is_some(), "the choice is admitted");
        assert_eq!((said.load(Ordering::Acquire), waits), (1, 1));
        sent.selection.set(Setting::Spotlight, false);
        let again = press().persist_at(root.path(), None, || true, &sent, false, || Ok(None));
        assert!(
            again.result.unwrap().is_some(),
            "the next choice is admitted"
        );
        assert_eq!(said.load(Ordering::Acquire), 1, "a free lock says nothing");
    }

    /// The launch's mend of the vault's sync process runs only while the plan in force has the
    /// Secrets vault at Set up (every group, before any plan is saved) and no person's Stop, on
    /// record or asked for, nor an older aterm's Cancel holds its run. A Restore holds nothing:
    /// it leaves credentials as they are.
    #[test]
    fn the_vault_sync_mend_runs_only_while_the_plan_in_force_sets_up_the_vault() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert!(
            keeps_vault_sync_at(root.path()).unwrap(),
            "no plan saved yet"
        );
        authorize_at(root.path(), &Plan::default()).unwrap();
        assert!(keeps_vault_sync_at(root.path()).unwrap());
        let mut without = Plan::default();
        without.selection.set(Setting::Secrets, false);
        system_state_choice(root.path(), &without);
        assert!(
            !keeps_vault_sync_at(root.path()).unwrap(),
            "Secrets left as is"
        );
        let revision = system_state_choice(root.path(), &Plan::default());
        assert!(keeps_vault_sync_at(root.path()).unwrap());
        request_stop_at(root.path(), revision).unwrap();
        assert!(
            !keeps_vault_sync_at(root.path()).unwrap(),
            "a Stop asked for"
        );
        fold_stop_request(root.path()).unwrap();
        assert!(
            !keeps_vault_sync_at(root.path()).unwrap(),
            "a Stop on record"
        );
        system_state_choice(root.path(), &Plan::default());
        assert!(keeps_vault_sync_at(root.path()).unwrap());
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert!(
            keeps_vault_sync_at(root.path()).unwrap(),
            "a Restore holds nothing"
        );
        super::super::write_legacy_cancel_at(root.path());
        assert!(
            !keeps_vault_sync_at(root.path()).unwrap(),
            "an older aterm's Cancel"
        );
    }

    /// A Restore's put-back that waits for Full Disk Access outlives a run that leaves its group
    /// at Leave as is (design §3.4): a choice of another group, or the run the Restore of an older
    /// aterm's pending setup brings forward for the offer, which sets aterm's permissions and so
    /// gets the grant itself. A Restore of an older aterm's verified setup keeps its permissions
    /// with no run at all. Once granted the widget list is put back, once.
    #[test]
    fn a_waiting_restore_puts_back_after_a_run_that_leaves_its_group_as_is() {
        use super::super::tests::Withholding;
        use aterm_types::macos_setup::Group;
        for case in [
            "a choice",
            "an older verified setup",
            "an older pending setup",
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut plan = Plan::from(group_selection(Group::Desktop));
            plan.selection.set(Setting::Permissions, true);
            let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut stages = WithholdingStages::over(Withholding::widgets());
            stages.doctor.grant();
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the run executes");
            assert!(stages.doctor.notification_center_cleared());
            if case != "a choice" {
                // What an older aterm left: its plan and first-run record, and no run.
                std::fs::remove_file(root.path().join("initial-setup.json")).unwrap();
                save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
                save_workstation_decision(root.path(), case == "an older verified setup").unwrap();
            }
            stages.doctor.revoke();
            stages.doctor.restore(root.path()).unwrap();
            assert!(stages.doctor.notification_center_cleared(), "it waits");
            assert_eq!(super::super::restore_waiting(root.path()).unwrap().len(), 1);
            assert_eq!(
                bring_forward_restored_at(root.path()).unwrap().is_some(),
                case == "an older pending setup",
                "{case}: only a run the Restore staged waits for the offer"
            );
            let revision = match case {
                "a choice" => {
                    let spotlight = selected(&[Setting::Spotlight]);
                    Some(page_choice(root.path(), &spotlight, spotlight.selection))
                }
                "an older verified setup" => {
                    assert!(permissions_allowed(root.path()), "{case}");
                    assert_eq!(launch_startup(root.path()), None, "{case}: no run");
                    None
                }
                _ => {
                    stages.grant_in_permissions = true;
                    Some(launch_startup(root.path()).expect("the offer admits the run"))
                }
            };
            let desktop = stages.doctor.window_manager_writes();
            if let Some(revision) = revision {
                let calls = stages.rest.calls.len();
                execute_initial_for_revision(
                    &mut stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("the run executes");
                let ran = &stages.rest.calls[calls..];
                assert_eq!(
                    ran,
                    if case == "a choice" {
                        [Stage::Spotlight]
                    } else {
                        [Stage::Permissions]
                    },
                    "{case}"
                );
            }
            assert_eq!(
                stages.doctor.window_manager_writes(),
                desktop,
                "{case}: no preference set"
            );
            stages.doctor.grant();
            let report = stages.doctor.follow_up(root.path()).unwrap();
            let put_back = !stages.doctor.notification_center_cleared();
            if case == "an older pending setup" {
                assert!(report.is_none(), "the run's own follow-up put it back");
            } else {
                assert!(
                    report.is_some_and(|report| report.restore.is_some()),
                    "{case}: the follow-up puts it back"
                );
            }
            assert!(put_back, "{case}");
            assert!(
                super::super::restore_waiting(root.path())
                    .unwrap()
                    .is_empty()
            );
            let writes = stages.doctor.notification_center_writes();
            assert!(stages.doctor.follow_up(root.path()).unwrap().is_none());
            assert_eq!(stages.doctor.notification_center_writes(), writes, "once");
            assert!(finished_at(root.path()).unwrap());
        }
    }

    /// Whether aterm's own permission steps may run now, as the window reads the records.
    fn permissions_allowed(root: &Path) -> bool {
        snapshot_at(root, Plan::default())
            .unwrap()
            .permissions_allowed
    }

    /// An admitted run of the default plan that waits on the vault: its preferences, Spotlight and
    /// aterm's permissions are set, the rest still owed. Its revision.
    fn admitted_run_waiting_on_the_vault(root: &Path) -> u64 {
        let revision = authorize_at(root, &Plan::default()).unwrap().unwrap();
        let mut first = Fake {
            waiting: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut first, root, revision, &mut None, SetupHooks::NONE)
            .unwrap();
        assert!(permissions_allowed(root));
        revision
    }

    /// A Restore of an admitted run, cut short before any of its writes, never withdraws aterm's
    /// own permissions (design §3.4): the next launch, or Restore pressed again, finishes its
    /// writes. The run keeps its admission and the Permissions approval it continues, the groups
    /// the Restore put back read Leave as is, and nothing runs: no pass, no offer.
    #[test]
    fn a_restore_cut_short_at_any_write_keeps_aterms_permissions_and_runs_nothing_else() {
        for cut in [
            RestoreWrite::Record,
            RestoreWrite::Plan,
            RestoreWrite::Progress,
            RestoreWrite::Run,
        ] {
            for again in [false, true] {
                let case = format!(
                    "cut before {cut:?}, then {}",
                    if again {
                        "Restore again"
                    } else {
                        "the next launch"
                    }
                );
                let root = aterm_tempfile::tempdir().unwrap();
                let revision = admitted_run_waiting_on_the_vault(root.path());
                save_restore(root.path()).unwrap();
                let ended = leave_restored_as_is_with(root.path(), |write| {
                    if write == cut {
                        Err("the process ended here".into())
                    } else {
                        Ok(())
                    }
                });
                assert!(ended.is_err(), "{case}");
                if again {
                    save_restore(root.path()).unwrap();
                    leave_restored_as_is(root.path()).unwrap();
                } else {
                    bring_forward_restored_at(root.path()).unwrap();
                    assert_eq!(launch_startup(root.path()), None, "{case}: nothing offered");
                }
                let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
                assert!(snapshot.permissions_allowed, "{case}");
                assert!(snapshot.finished, "{case}");
                assert_eq!(
                    snapshot.access_revision, revision,
                    "{case}: the same approval"
                );
                assert!(
                    preferences(snapshot.plan.selection).is_empty(),
                    "{case}: the restored groups are left as is"
                );
                assert!(!saved_plan_provisional(root.path()).unwrap(), "{case}");
                assert!(
                    admission_at(root.path()).unwrap().is_none(),
                    "{case}: no run resumes"
                );
                let mut host = Fake::default();
                assert!(
                    execute_initial(&mut host, root.path(), &mut None)
                        .unwrap()
                        .is_none(),
                    "{case}"
                );
                assert!(host.calls.is_empty(), "{case}");
                let whole = Records::read(root.path()).unwrap();
                assert_eq!(
                    whole.restored.and_then(|restored| restored.run),
                    Some(snapshot.revision),
                    "{case}: the record names the run in force"
                );
                assert_eq!(
                    bring_forward_restored_at(root.path()).unwrap(),
                    None,
                    "{case}"
                );
                assert_eq!(Records::read(root.path()).unwrap(), whole, "{case}: once");
            }
        }
    }

    /// A run number reserved past the run a Restore ends (a progress write that failed after its
    /// reservation, say) is never used again: the Restore carries that run on as the number after
    /// the reserved one, and its record names the run its progress holds, whether or not a write
    /// was cut short, so the next launch finds nothing left to finish.
    #[test]
    fn a_restore_carries_its_run_past_a_reserved_run_number() {
        for cut in [
            None,
            Some(RestoreWrite::Record),
            Some(RestoreWrite::Plan),
            Some(RestoreWrite::Progress),
            Some(RestoreWrite::Run),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = admitted_run_waiting_on_the_vault(root.path());
            revision::reserve(root.path(), revision + 5).unwrap();
            save_restore(root.path()).unwrap();
            if let Some(cut) = cut {
                let ended = leave_restored_as_is_with(root.path(), |write| {
                    if write == cut {
                        Err("the process ended here".into())
                    } else {
                        Ok(())
                    }
                });
                assert!(ended.is_err(), "{cut:?}");
            }
            leave_restored_as_is(root.path()).unwrap();
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(
                snapshot.revision,
                revision + 6,
                "{cut:?}: past the reserved number"
            );
            assert!(snapshot.permissions_allowed, "{cut:?}");
            assert_eq!(
                snapshot.access_revision, revision,
                "{cut:?}: the same approval"
            );
            let records = Records::read(root.path()).unwrap();
            assert_eq!(
                records.restored.and_then(|restored| restored.run),
                Some(snapshot.revision),
                "{cut:?}: the record names the run in force"
            );
            assert_eq!(
                bring_forward_restored_at(root.path()).unwrap(),
                None,
                "{cut:?}"
            );
            assert_eq!(
                Records::read(root.path()).unwrap(),
                records,
                "{cut:?}: once"
            );
        }
    }

    /// `aterm doctor --apply` after a Restore, cut short before any of its writes, never
    /// withdraws aterm's own permissions: the run the Restore ended keeps its admission and the
    /// approval it continues while the decision still reads restored, and where the explicit
    /// plan landed ahead of its progress the next launch's offer admits it.
    #[test]
    fn aterm_doctor_apply_after_a_restore_keeps_aterms_permissions_wherever_it_is_cut_short() {
        for cut in [
            ExplicitWrite::Revision,
            ExplicitWrite::Decision,
            ExplicitWrite::Plan,
            ExplicitWrite::Progress,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = admitted_run_waiting_on_the_vault(root.path());
            save_restore(root.path()).unwrap();
            leave_restored_as_is(root.path()).unwrap();
            assert!(permissions_allowed(root.path()), "{cut:?}");
            let ended = save_explicit_workstation_plan_with(root.path(), |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            assert!(ended.is_err(), "{cut:?}");
            if cut != ExplicitWrite::Progress {
                // The run in force is admitted and bound to the plan saved.
                assert!(permissions_allowed(root.path()), "{cut:?}");
                assert_eq!(
                    snapshot_at(root.path(), Plan::default())
                        .unwrap()
                        .access_revision,
                    revision,
                    "{cut:?}: the same approval"
                );
            }
            bring_forward_restored_at(root.path()).unwrap();
            launch_startup(root.path());
            assert!(
                permissions_allowed(root.path()),
                "{cut:?}: after the next launch"
            );
        }
    }

    /// A Restore pressed while the launch's offer still waits for its answer, over a run whose
    /// admission never landed, leaves no admitted run to carry aterm's own permissions. The
    /// Restore brings it forward under its own lock, so the offer made again in this launch
    /// admits a run that sets aterm's permissions alone: the restored groups stay at Leave as
    /// is, and no sign-out, Spotlight or Dock step runs. So does a Restore that left that plan
    /// as it was (aterm's own plan of Permissions alone). A Restore cut short before it brought
    /// the setup forward is brought forward at the next launch, and so are the bring-forward's
    /// writes cut short anywhere.
    #[test]
    fn a_restore_over_an_unanswered_offer_gets_aterms_permissions_from_the_offer() {
        let unadmitted = |root: &Path, plan: &Plan| {
            authorize_at(root, plan).unwrap();
            let mut staged = progress(root).unwrap().unwrap();
            staged.admitted = false;
            save_progress(root, staged).unwrap();
            assert!(!finished_at(root).unwrap(), "the launch offers the plan");
            save_restore(root).unwrap();
        };
        // The Restore's writes, cut short before it brought the setup forward.
        let restored_short = |root: &Path, plan: &Plan| {
            unadmitted(root, plan);
            leave_restored_as_is_with(root, |_| Ok(())).unwrap();
            assert!(finished_at(root).unwrap());
            assert!(!permissions_allowed(root), "until the next launch");
        };
        let mut permissions_only = Plan::from(Selection::none());
        permissions_only.selection.set(Setting::Permissions, true);
        for plan in [Plan::default(), permissions_only] {
            let root = aterm_tempfile::tempdir().unwrap();
            unadmitted(root.path(), &plan);
            leave_restored_as_is(root.path()).unwrap();
            assert!(!finished_at(root.path()).unwrap(), "the offer comes again");
            let before = Records::read(root.path()).unwrap();
            let staged = bring_forward_restored_at(root.path())
                .unwrap()
                .expect("the run waits for the offer");
            assert_eq!(
                Records::read(root.path()).unwrap(),
                before,
                "the Restore brought it forward itself"
            );
            assert!(
                !permissions_allowed(root.path()),
                "until the offer admits it"
            );
            let revision = launch_startup(root.path()).expect("the offer admits the run");
            assert!(revision > staged);
            let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(admitted.permissions_allowed);
            assert!(preferences(admitted.plan.selection).is_empty());
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, [Stage::Permissions]);
            assert!(finished_at(root.path()).unwrap());
            assert!(permissions_allowed(root.path()));
            assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        }

        // The Restore saved the plan this stages already: the run it stages, its progress, then
        // the record.
        let reference = aterm_tempfile::tempdir().unwrap();
        restored_short(reference.path(), &Plan::default());
        let mut writes = Vec::new();
        bring_forward_restored_with(reference.path(), |write| {
            writes.push(write);
            Ok(())
        })
        .unwrap()
        .unwrap();
        assert_eq!(
            writes,
            [
                RestoreWrite::Run,
                RestoreWrite::Progress,
                RestoreWrite::Record
            ]
        );
        let whole = Records::read(reference.path()).unwrap();
        let brought = aterm_tempfile::tempdir().unwrap();
        unadmitted(brought.path(), &Plan::default());
        leave_restored_as_is(brought.path()).unwrap();
        assert_eq!(
            Records::read(brought.path()).unwrap(),
            whole,
            "the Restore's own bring-forward writes the same records"
        );
        for cut in [
            RestoreWrite::Run,
            RestoreWrite::Progress,
            RestoreWrite::Record,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            restored_short(root.path(), &Plan::default());
            let _lock = HeldLock::take(root.path()).unwrap();
            let ended = bring_forward_restored_with(root.path(), |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            drop(_lock);
            assert!(ended.is_err(), "{cut:?}");
            assert!(finished_at(root.path()).unwrap(), "{cut:?}");
            assert_eq!(
                launch_startup(root.path()),
                None,
                "{cut:?}: nothing offered yet"
            );
            bring_forward_restored_at(root.path()).unwrap().unwrap();
            assert_eq!(Records::read(root.path()).unwrap(), whole, "{cut:?}");
        }
    }

    /// A person's Stop of the run a Restore left unadmitted holds aterm's permissions, and so does
    /// a person's plan that leaves the Permissions group as is: neither is brought forward.
    #[test]
    fn a_stop_or_a_plan_without_permissions_is_never_brought_forward() {
        let stopped = aterm_tempfile::tempdir().unwrap();
        authorize_at(stopped.path(), &Plan::default()).unwrap();
        let mut staged = progress(stopped.path()).unwrap().unwrap();
        staged.admitted = false;
        staged.stopped = true;
        save_progress(stopped.path(), staged).unwrap();
        save_restore(stopped.path()).unwrap();
        leave_restored_as_is(stopped.path()).unwrap();
        let before = Records::read(stopped.path()).unwrap();
        assert_eq!(bring_forward_restored_at(stopped.path()).unwrap(), None);
        assert_eq!(Records::read(stopped.path()).unwrap(), before);
        assert_eq!(launch_startup(stopped.path()), None);
        assert!(!permissions_allowed(stopped.path()));

        let declined = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::default();
        plan.selection.set(Setting::Permissions, false);
        authorize_at(declined.path(), &plan).unwrap();
        let mut staged = progress(declined.path()).unwrap().unwrap();
        staged.admitted = false;
        save_progress(declined.path(), staged).unwrap();
        save_restore(declined.path()).unwrap();
        leave_restored_as_is(declined.path()).unwrap();
        let before = Records::read(declined.path()).unwrap();
        assert_eq!(bring_forward_restored_at(declined.path()).unwrap(), None);
        assert_eq!(Records::read(declined.path()).unwrap(), before);
    }

    /// A Restore over a choice cut short after it saved its plan, before its progress, keeps the
    /// admission of the run the progress records: aterm's permissions go on at once, and the
    /// choice that never ran does not run.
    #[test]
    fn a_restore_over_a_choice_cut_short_keeps_aterms_permissions() {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = admitted_run_waiting_on_the_vault(root.path());
        let mut chosen = Plan::default();
        chosen.selection.set(Setting::Spotlight, false);
        let cut = stage_restart_with(
            root.path(),
            &chosen,
            Restart {
                chosen: true,
                joins: false,
            },
            AdmittedVault::Unknown,
            save_progress,
            |write| {
                if write == RestartWrite::Progress {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(cut.is_err());
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(snapshot.permissions_allowed);
        assert_eq!(snapshot.access_revision, revision);
        assert!(snapshot.finished);
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        assert_eq!(launch_startup(root.path()), None);
    }

    /// An older aterm's setup keeps no progress record: its plan (with no vault field) and its
    /// decision say what its one pass did. The launch admits nothing over a finished one, and a
    /// page choice over it, or the launch's offer over one whose sign-outs were never verified,
    /// sets only what the plan admitted turns on: the Dock reset and the Messages and FaceTime
    /// sign-outs that pass made never run again. aterm's own permissions, which that plan
    /// predates, stay approved through the choice, since nobody turned them off.
    #[test]
    fn a_choice_after_an_older_aterms_setup_runs_only_what_it_turned_on() {
        let older = |root: &Path, verified: bool| {
            save_automatic_plan(root, Selection::all().workstation()).unwrap();
            write_json(
                &root.join("first-run.json"),
                &json!({"profile":PROFILE,"state":"applied","accounts_verified":verified}),
            )
            .unwrap();
            bring_forward_restored_at(root).unwrap();
        };
        // Finished: a page choice of one more group.
        let root = aterm_tempfile::tempdir().unwrap();
        older(root.path(), true);
        assert_eq!(
            launch_startup(root.path()),
            None,
            "a finished setup offers nothing"
        );
        assert!(permissions_allowed(root.path()));
        let mut plan = snapshot_at(root.path(), Plan::default()).unwrap().plan;
        assert!(
            plan.selection.contains(Setting::Permissions),
            "nobody turned them off"
        );
        plan.selection.set(Setting::Spotlight, true);
        let revision = system_state_choice(root.path(), &plan);
        assert!(permissions_allowed(root.path()), "the choice keeps them");
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Spotlight, Stage::Permissions]);
        assert!(finished_at(root.path()).unwrap());

        // Applied with its sign-outs unverified: the launch's offer runs those, not the Dock.
        let root = aterm_tempfile::tempdir().unwrap();
        older(root.path(), false);
        let revision = launch_startup(root.path()).expect("the offer admits the plan");
        assert!(permissions_allowed(root.path()));
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Permissions, Stage::Accounts]);
    }

    /// An older aterm's Cancel over its pending run, then `aterm doctor --apply`: the apply's
    /// pass resets the Dock and signs out, and records those stages finished though the Cancel
    /// holds the run's admission, so no launch runs them again.
    #[test]
    fn doctor_apply_over_a_cancel_leaves_the_next_launch_nothing_it_ran() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        crate::macos_doctor::save_automatic_plan(root, Selection::all().workstation()).unwrap();
        write_json(
            &root.join("initial-setup.json"),
            &json!({"profile":PROFILE,"state":"pending","done":0}),
        )
        .unwrap();
        crate::macos_doctor::write_legacy_cancel_at(root);
        doctor_apply(root);
        let calls = launch_startup(root).map(|revision| run_pass(root, revision).calls);
        assert!(
            calls.as_ref().is_none_or(|calls| {
                !calls.contains(&Stage::Workstation) && !calls.contains(&Stage::Accounts)
            }),
            "the next launch reset the Dock or signed out again: {calls:?}"
        );
    }

    /// `aterm doctor --apply`'s record writes around its pass ([`super::super::run_mutation`]):
    /// the explicit plan, the pass's own completion record, then the explicit completion.
    fn doctor_apply(root: &Path) {
        let explicit = save_explicit_workstation_plan(root).unwrap();
        // The explicit pass set every Mac preference and signed out (`execute(Apply)`).
        record_applied(root, Selection::all().workstation()).unwrap();
        complete_explicit_workstation_plan(root, explicit).unwrap();
    }

    /// `aterm init`'s pass of the whole profile holds the records the first launch holds and
    /// records what it verified, as `aterm doctor --apply` does: the next launch neither resets
    /// the Dock nor signs out again. Without that record the launch's offer runs both.
    #[test]
    fn a_whole_init_pass_leaves_the_next_launch_nothing_it_ran() {
        use aterm_machines::init::Held as _;
        let ran_by_the_launch = |init: bool| {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            if init {
                let hold = super::super::init_hold_at(root, true, &|line| {
                    panic!("nothing else holds the records: {line}")
                })
                .unwrap();
                Box::new(hold).verified().unwrap();
            }
            launch_startup(root).map(|revision| run_pass(root, revision).calls)
        };
        let calls = ran_by_the_launch(true);
        assert!(
            calls.as_ref().is_none_or(|calls| {
                !calls.contains(&Stage::Workstation) && !calls.contains(&Stage::Accounts)
            }),
            "the next launch reset the Dock or signed out again: {calls:?}"
        );
        let calls = ran_by_the_launch(false).expect("a fresh Mac's launch offers its setup");
        assert!(
            calls.contains(&Stage::Workstation) && calls.contains(&Stage::Accounts),
            "{calls:?}"
        );
    }

    fn automatic_completion_account_retry(block_after_signout: bool) {
        use crate::macos_doctor::accounts_visibility_tests::{
            Account, system, system_after_signout,
        };
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let root = aterm_tempfile::tempdir().unwrap();
        // Secrets keeps the same admitted run unfinished after Accounts verifies.
        let plan = selected(&[Setting::Messages, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let _profile = HeldLock::take(root.path()).unwrap();
        let held = root.path().join(HELD_RECORD);
        let account = Arc::new(Account {
            signed_in: AtomicBool::new(true),
            unreadable: false,
            signouts: AtomicUsize::new(0),
        });
        if !block_after_signout {
            std::fs::create_dir(&held).unwrap();
        }
        let blocker = held.clone();
        let machine = system_after_signout(None, &account, move || {
            assert!(
                block_after_signout,
                "an unreadable cache must stop before the effect"
            );
            std::fs::create_dir(&blocker).unwrap();
        });
        let mut first = Fake {
            pending: Some(Stage::Secrets),
            native_accounts: Some((
                system(None, &account),
                NativeHost::over(
                    Box::new(machine),
                    Box::new(aterm_machines::init::prepare_workstation_selected),
                ),
            )),
            native_accounts_now: Some(std::time::SystemTime::now()),
            ..Fake::default()
        };
        let (report, _) = execute_initial_for_revision(
            &mut first,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert!(held.is_dir());
        assert!(
            report.rows.iter().any(|row| {
                row.name == Stage::Accounts.name()
                    && row.status == "error"
                    && row.detail.contains(HELD_RECORD)
            }),
            "{}",
            report.to_text()
        );
        assert_eq!(
            account.signouts.load(Ordering::SeqCst),
            usize::from(block_after_signout)
        );
        assert_eq!(
            first_run_complete(root.path()).unwrap(),
            block_after_signout
        );
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert!(
            !finished_at(root.path()).unwrap(),
            "Secrets is still pending"
        );

        std::fs::remove_dir(&held).unwrap();
        if block_after_signout {
            // The first pass verified the sign-out and published Applied. A person's
            // later login is not work for the cache write's retry to undo.
            assert!(!account.signed_in.load(Ordering::SeqCst));
            account.signed_in.store(true, Ordering::SeqCst);
        }
        let retry_at = std::fs::metadata(root.path().join("automatic-attempt.json"))
            .unwrap()
            .modified()
            .unwrap()
            + std::time::Duration::from_secs(60);
        let mut retry = Fake {
            pending: Some(Stage::Secrets),
            native_accounts: Some((
                system(None, &account),
                NativeHost::over(
                    Box::new(system(None, &account)),
                    Box::new(aterm_machines::init::prepare_workstation_selected),
                ),
            )),
            native_accounts_now: Some(retry_at),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut retry,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert_eq!(
            account.signouts.load(Ordering::SeqCst),
            1,
            "retry the record, never repeat a verified sign-out"
        );
        assert_eq!(
            account.signed_in.load(Ordering::SeqCst),
            block_after_signout,
            "an earlier cache read error still leaves the account to be signed out"
        );
    }

    #[test]
    fn automatic_completion_held_save_failure_preserves_a_later_login() {
        automatic_completion_account_retry(true);
    }

    #[test]
    fn automatic_completion_held_read_failure_still_retries_the_unperformed_signout() {
        automatic_completion_account_retry(false);
    }

    #[test]
    fn automatic_completion_partial_account_pass_does_not_complete_the_other_account() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Messages, Setting::FaceTime, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap().unwrap();
        let _profile = HeldLock::take(root.path()).unwrap();
        let before = progress(root.path()).unwrap().unwrap();
        record_applied(root.path(), selected(&[Setting::Messages]).selection).unwrap();
        assert_eq!(
            progress(root.path()).unwrap(),
            Some(before),
            "one completed account cannot credit the other account or an independent stage"
        );
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum InitCompletion {
        BeforeVerification,
        AfterApplied,
        SummaryError,
        Complete,
    }

    fn init_completion_account_pass(cut: InitCompletion) -> (usize, bool) {
        use crate::macos_doctor::accounts_visibility_tests::{Account, system};
        use aterm_machines::init::Held as _;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let root = aterm_tempfile::tempdir().unwrap();
        let mut held =
            Some(super::super::init_hold_at(root.path(), true, &|line| panic!("{line}")).unwrap());
        let account = Arc::new(Account {
            signed_in: AtomicBool::new(true),
            unreadable: false,
            signouts: AtomicUsize::new(0),
        });
        let native = || {
            NativeHost::over(
                Box::new(system(None, &account)),
                Box::new(aterm_machines::init::prepare_workstation_selected),
            )
        };
        if cut != InitCompletion::BeforeVerification {
            // The Desktop lane's real account items, with all subprocesses and
            // account state supplied by the same inert machine as the resumed pass.
            let mut desktop = native();
            desktop.select(Selection::all().workstation());
            let rows = desktop
                .workstation(WorkstationGroup::Accounts, true)
                .unwrap();
            assert!(rows.iter().all(|(_, row)| row_ready(row)), "{rows:?}");
            assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
            assert!(!account.signed_in.load(Ordering::SeqCst));
            match cut {
                InitCompletion::AfterApplied => {
                    // The actual first completion write of InitHold::verified;
                    // the process ends before its final Progress write.
                    record_applied(root.path(), Selection::all().workstation()).unwrap();
                }
                InitCompletion::SummaryError => {
                    let before = progress(root.path()).unwrap().unwrap();
                    let written =
                        record_applied_with(root.path(), Selection::all().workstation(), |root| {
                            let read = decision_record(root)?;
                            std::fs::rename(
                                root.join("first-run.json"),
                                root.join("first-run.saved"),
                            )
                            .unwrap();
                            std::fs::create_dir(root.join("first-run.json")).unwrap();
                            Ok(read)
                        });
                    assert!(written.is_err(), "the actual summary publication must fail");
                    std::fs::remove_dir(root.path().join("first-run.json")).unwrap();
                    std::fs::rename(
                        root.path().join("first-run.saved"),
                        root.path().join("first-run.json"),
                    )
                    .unwrap();
                    let mut expected = before;
                    expected.done |= Stage::Workstation.bit() | Stage::Accounts.bit();
                    assert_eq!(
                        progress(root.path()).unwrap(),
                        Some(expected),
                        "verified work survives a failed summary without changing other run fields"
                    );
                }
                InitCompletion::Complete => Box::new(held.take().unwrap()).verified().unwrap(),
                InitCompletion::BeforeVerification => unreachable!(),
            }
            // A later login is the person's change, after the verified sign-out.
            account.signed_in.store(true, Ordering::SeqCst);
        }
        drop(held);
        assert!(
            !root.path().join(HELD_RECORD).exists(),
            "raw init keeps no automatic item cache"
        );
        let before = account.signouts.load(Ordering::SeqCst);
        let mut resumed = Fake {
            native_accounts: Some((system(None, &account), native())),
            ..Fake::default()
        };
        if let Some(revision) = launch_startup(root.path()) {
            let _profile = HeldLock::take(root.path()).unwrap();
            execute_initial_for_revision(
                &mut resumed,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the actual admitted startup pass resumes");
        }
        (
            account.signouts.load(Ordering::SeqCst) - before,
            account.signed_in.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn init_completion_interrupted_after_applied_keeps_verified_signouts() {
        assert_eq!(
            init_completion_account_pass(InitCompletion::AfterApplied),
            (0, true),
            "the next launch must preserve the person's later login"
        );
    }

    #[test]
    fn init_completion_summary_error_keeps_verified_signouts() {
        assert_eq!(
            init_completion_account_pass(InitCompletion::SummaryError),
            (0, true)
        );
    }

    #[test]
    fn init_completion_success_keeps_verified_signouts() {
        assert_eq!(
            init_completion_account_pass(InitCompletion::Complete),
            (0, true)
        );
    }

    #[test]
    fn init_completion_before_verification_still_runs_accounts() {
        assert_eq!(
            init_completion_account_pass(InitCompletion::BeforeVerification),
            (1, false)
        );
    }

    #[test]
    fn init_completion_partial_pass_does_not_credit_the_whole_profile() {
        let root = aterm_tempfile::tempdir().unwrap();
        let _held =
            super::super::init_hold_at(root.path(), true, &|line| panic!("{line}")).unwrap();
        let mut expected = progress(root.path()).unwrap().unwrap();
        record_applied(root.path(), preferences(Selection::all())).unwrap();
        expected.done |= Stage::Workstation.bit();
        assert_eq!(
            progress(root.path()).unwrap(),
            Some(expected),
            "verified preferences cannot complete Accounts or change other run fields"
        );
    }

    /// `aterm init` behind a first-launch pass that holds the profile lock says one line and
    /// waits; once the pass lets the lock go, init holds it and runs.
    #[test]
    fn init_waits_with_one_line_while_a_first_launch_pass_holds_the_records() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let pass = super::super::HeldLock::take(root).unwrap();
        let (heard, said) = std::sync::mpsc::channel::<String>();
        let waiting = std::thread::scope(|threads| {
            let init = threads.spawn(|| {
                super::super::init_hold_at(root, false, &|line| {
                    heard.send(line.to_owned()).unwrap();
                })
                .map(|_| ())
            });
            // A hang detector: init says it waits while the pass holds the records.
            let line = said
                .recv_timeout(std::time::Duration::from_secs(60))
                .expect("init said it waits");
            assert!(
                !init.is_finished(),
                "init ran while the pass held the records"
            );
            drop(pass);
            init.join().unwrap().unwrap();
            line
        });
        assert_eq!(waiting, super::super::INIT_WAITS);
        assert!(said.try_recv().is_err(), "one line, not one per look");
        assert!(super::super::HeldLock::try_take(root).unwrap().is_some());
    }

    /// `aterm init`'s grant rows name the app a grant must go to: Full Disk Access and
    /// Accessibility were read for the app macOS holds responsible (Terminal, under Terminal),
    /// Developer Tools from aterm's own entry; a grant read as unknown is unread, not missing.
    #[test]
    fn init_grant_rows_name_the_app_the_reading_measured() {
        use aterm_machines::init::Grant;
        use aterm_types::macos_setup::{Access, Standing};
        let how = |access, app| match init_grant(access, Standing::NeedsYou, None, app) {
            Grant::NeedsYou(how) => how,
            other => panic!("{access:?}: {other:?}"),
        };
        assert!(how(Access::FullDiskAccess, "Terminal").ends_with("switch on Terminal"));
        assert!(how(Access::Accessibility, "Terminal").ends_with("switch on Terminal"));
        assert!(how(Access::FullDiskAccess, "aterm").ends_with("switch on aterm"));
        assert!(how(Access::DeveloperTools, "Terminal").ends_with("switch on aterm"));
        assert_eq!(
            init_grant(Access::FullDiskAccess, Standing::InPlace, None, "Terminal"),
            Grant::On
        );
        assert!(matches!(
            init_grant(Access::Debugger, Standing::Unknown, None, "Terminal"),
            Grant::Unread(_)
        ));
    }

    /// `aterm doctor --apply` while a staged run waits for the launch's offer (here the run a
    /// restored setup was brought forward to) resets the Dock and signs out in its own pass, and
    /// admits that run at its last write with those stages finished: the next launch runs
    /// aterm's permissions, never the Dock or the sign-outs again (design §3.4). A Cancel that
    /// waits holds it: the apply admits nothing, and nothing runs at a launch.
    #[test]
    fn doctor_apply_over_a_staged_run_leaves_the_next_launch_nothing_it_did() {
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), Some(Selection::all().workstation()));
        bring_forward_restored_at(root.path())
            .unwrap()
            .expect("staged");
        // The offer waits for a focused window; the person runs `aterm doctor --apply` meanwhile.
        doctor_apply(root.path());
        assert!(permissions_allowed(root.path()));
        let revision = launch_startup(root.path()).expect("the admitted run goes on");
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Permissions]);
        assert!(finished_at(root.path()).unwrap());

        let cancelled = aterm_tempfile::tempdir().unwrap();
        authorize_at(
            cancelled.path(),
            &Plan::from(Selection::all().workstation()),
        )
        .unwrap();
        super::super::write_legacy_cancel_at(cancelled.path());
        let mut staged = progress(cancelled.path()).unwrap().unwrap();
        staged.admitted = false;
        save_progress(cancelled.path(), staged).unwrap();
        let explicit = save_explicit_workstation_plan(cancelled.path()).unwrap();
        complete_explicit_workstation_plan(cancelled.path(), explicit).unwrap();
        assert!(!progress(cancelled.path()).unwrap().unwrap().admitted);
        assert!(!permissions_allowed(cancelled.path()));
    }

    /// After a whole Restore a person turns Desktop & Dock back to Set up, and the choice is cut
    /// short at one of its writes before its decision ([`RestartWrite`]): the plan landed, the
    /// decision still says restored. The next launch hands that choice to its offer, which
    /// admits the plan the person chose and sets Desktop & Dock alone; it never puts the plan
    /// back to the Restore's. A person's Stop of the staged choice holds it: nothing is offered
    /// or run, and the plan stays theirs.
    #[test]
    fn a_choice_after_a_restore_cut_short_is_offered_at_the_next_launch() {
        use aterm_types::macos_setup::Group;
        let desktop = group_selection(Group::Desktop);
        let cut_short = |cut: RestartWrite| {
            let root = restored_after(&Plan::default());
            leave_restored_as_is(root.path()).unwrap();
            let mut chosen = snapshot_at(root.path(), Plan::default()).unwrap().plan;
            for setting in Group::Desktop.settings() {
                chosen.selection.set(*setting, true);
            }
            let ended = stage_restart_with(
                root.path(),
                &chosen,
                Restart {
                    chosen: true,
                    joins: false,
                },
                AdmittedVault::None,
                save_progress,
                |write| {
                    if write == cut {
                        Err("the process ended here".into())
                    } else {
                        Ok(())
                    }
                },
            );
            assert!(ended.is_err(), "{cut:?}");
            (root, chosen)
        };
        for cut in [
            RestartWrite::Progress,
            RestartWrite::Reset,
            RestartWrite::Decision,
        ] {
            let (root, chosen) = cut_short(cut);
            bring_forward_restored_at(root.path())
                .unwrap()
                .expect("handed to the offer");
            assert_eq!(
                saved_plan(root.path()).unwrap(),
                Some(chosen.clone()),
                "{cut:?}"
            );
            let revision = launch_startup(root.path()).expect("the offer admits the choice");
            assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen), "{cut:?}");
            let host = run_pass(root.path(), revision);
            assert_eq!(host.only, [Some(desktop)], "{cut:?}");
            assert!(
                !host.calls.contains(&Stage::Accounts),
                "{cut:?}: {:?}",
                host.calls
            );
            assert!(finished_at(root.path()).unwrap(), "{cut:?}");
        }

        let (root, chosen) = cut_short(RestartWrite::Decision);
        request_stop_at(root.path(), revision_at(root.path()).unwrap()).unwrap();
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
        assert_eq!(launch_startup(root.path()), None, "the Stop holds it");
        assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen));
    }

    /// A launch whose bring-forward meets profile.lock held by another pass (the follow-up with
    /// Full Disk Access a grant started, say) hears that it is busy, never that there is nothing
    /// to bring forward, and brings the setup forward once the holder lets go: aterm's
    /// permissions get their run in this launch.
    #[test]
    fn a_busy_profile_lock_holds_the_bring_forward_until_it_is_let_go() {
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), Some(Selection::all().workstation()));
        let busy = HeldLock::take(root.path()).unwrap();
        assert_eq!(
            bring_forward_waiting(root.path(), || false).unwrap(),
            BroughtForward::Busy
        );
        assert_eq!(launch_startup(root.path()), None, "nothing to offer yet");
        drop(busy);
        assert!(matches!(
            bring_forward_waiting(root.path(), || false).unwrap(),
            BroughtForward::Run(_)
        ));
        let revision = launch_startup(root.path()).expect("the offer admits the run");
        assert_eq!(run_pass(root.path(), revision).calls, [Stage::Permissions]);
        assert_eq!(
            bring_forward_waiting(root.path(), || false).unwrap(),
            BroughtForward::Nothing
        );
    }

    /// A newer aterm (a build beside this one, in the same state folder) saved a plan with a
    /// group this build does not know, or progress with a stage it does not know: that record is
    /// sound, so nothing is set aside and the person's plan stays as it is. This build reads it
    /// as the newer aterm's ([`NEWER_SETUP`]) and changes none of it.
    #[test]
    fn a_newer_aterms_records_are_never_set_aside() {
        for case in [
            "plan with a new group",
            "progress with a new stage",
            "newer format",
        ] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            let plan = plan_of(
                &[Setting::Permissions],
                &[aterm_types::macos_setup::Group::Desktop],
            );
            let revision = system_state_choice(root, &plan);
            run_pass(root, revision);
            assert!(finished_at(root).unwrap());
            if case == "plan with a new group" {
                write_json(
                    &root.join("automatic-plan.json"),
                    &json!({"profile":PROFILE,"selection":plan.selection.bits() | 1 << 12,
                            "vault_url":plan.vault_url}),
                )
                .unwrap();
            } else {
                let mut record: Value = aterm_json::from_slice(
                    &std::fs::read(root.join("initial-setup.json")).unwrap(),
                )
                .unwrap();
                let fields = record.as_object_mut().unwrap();
                if case == "newer format" {
                    fields.insert("format".into(), Value::from(FORMAT + 1));
                } else {
                    let done = fields["done"].as_u64().unwrap() | 1 << 6;
                    fields.insert("done".into(), Value::from(done));
                    fields.insert("stages".into(), Value::from(7));
                }
                write_json(&root.join("initial-setup.json"), &record).unwrap();
            }
            let before = [
                "automatic-plan.json",
                "initial-setup.json",
                "first-run.json",
            ]
            .map(|name| std::fs::read(root.join(name)).unwrap());
            assert_eq!(
                set_aside_unreadable_setup_at(root, false).unwrap(),
                None,
                "{case}"
            );
            assert_eq!(
                [
                    "automatic-plan.json",
                    "initial-setup.json",
                    "first-run.json"
                ]
                .map(|name| std::fs::read(root.join(name)).unwrap()),
                before,
                "{case}: nothing rewritten"
            );
            assert!(
                matches!(
                    snapshot_at(root, Plan::default()),
                    Err(InitialSetupSnapshotError::Newer)
                ),
                "{case}"
            );
            let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
                InitialSetupRestart::chosen().persist_with(
                    root,
                    &plan,
                    false,
                    || Ok(None),
                    save_progress,
                    observe_restart,
                )
            })
            .result;
            assert!(chosen.is_err_and(|why| setup_is_newer(&why)), "{case}");
        }
    }

    /// A damaged record beside a newer aterm's: that setup is the newer aterm's, which no
    /// set-aside touches, so every reader says so (the page's calm line, no read again by
    /// itself), never that it reads the damaged record again, which would try a set-aside that
    /// is refused, again and again.
    #[test]
    fn a_damaged_record_beside_a_newer_aterms_reads_as_the_newer_aterms() {
        for damaged in ["first-run.json", "automatic-plan.json"] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            let plan = plan_of(&[Setting::Permissions], &[]);
            let revision = system_state_choice(root, &plan);
            run_pass(root, revision);
            let mut record: Value =
                aterm_json::from_slice(&std::fs::read(root.join("initial-setup.json")).unwrap())
                    .unwrap();
            record
                .as_object_mut()
                .unwrap()
                .insert("format".into(), Value::from(FORMAT + 1));
            write_json(&root.join("initial-setup.json"), &record).unwrap();
            std::fs::write(root.join(damaged), CORRUPT).unwrap();
            assert!(
                matches!(
                    snapshot_at(root, Plan::default()),
                    Err(InitialSetupSnapshotError::Newer)
                ),
                "{damaged}"
            );
            assert!(
                matches!(
                    snapshot_in_vault(root, Ok(None), None),
                    Err(InitialSetupSnapshotError::Newer)
                ),
                "{damaged}"
            );
            assert_eq!(
                set_aside_unreadable_setup_at(root, false).unwrap(),
                None,
                "{damaged}: nothing is set aside"
            );
            assert_eq!(std::fs::read(root.join(damaged)).unwrap(), CORRUPT);
        }
    }

    /// The run counter that every progress save reserves in first cannot be read: it is rebuilt,
    /// so a later choice and `aterm doctor --apply` go on as before.
    #[test]
    fn a_damaged_run_counter_never_stops_a_later_choice() {
        for set_aside in [true, false] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            let revision = system_state_choice(root, &selected(&[Setting::Permissions]));
            run_pass(root, revision);
            std::fs::write(root.join("run-revision.json"), CORRUPT).unwrap();
            if set_aside {
                assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
                assert!(!revision::damaged(root).unwrap(), "rebuilt");
            }
            let more = selected(&[Setting::Permissions, Setting::Spotlight]);
            let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
                InitialSetupRestart::chosen().persist_with(
                    root,
                    &more,
                    false,
                    || Ok(None),
                    save_progress,
                    observe_restart,
                )
            })
            .result;
            assert!(
                matches!(chosen, Ok(Some(later)) if later > revision),
                "{chosen:?}"
            );
            assert!(save_explicit_workstation_plan(root).is_ok());
            assert!(root.join("run-revision.json.unreadable").exists());
        }
    }

    /// A finished set-aside's marker that cannot be read says nothing of a hold: with every
    /// record sound, it is written again, finished, and aterm's permissions nobody stopped stay
    /// set up.
    #[test]
    fn a_damaged_set_aside_marker_never_holds_what_nobody_stopped() {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        system_state_choice(root, &selected(&[Setting::Permissions]));
        std::fs::write(root.join("initial-setup.json"), CORRUPT).unwrap();
        set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        let revision = launch_startup(root).expect("the offer admits the recovered plan");
        run_pass(root, revision);
        assert!(finished_at(root).unwrap());
        let before = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.join(name)).unwrap());
        std::fs::write(root.join("set-aside.json"), CORRUPT).unwrap();
        assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
        assert_eq!(
            [
                "automatic-plan.json",
                "initial-setup.json",
                "first-run.json"
            ]
            .map(|name| std::fs::read(root.join(name)).unwrap()),
            before,
            "nothing set aside again"
        );
        assert!(
            saved_plan(root)
                .unwrap()
                .unwrap()
                .selection
                .contains(Setting::Permissions)
        );
        assert!(permissions_allowed(root));
        assert_eq!(
            std::fs::read(root.join("set-aside.json.unreadable")).unwrap(),
            CORRUPT
        );
        assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
    }

    /// Design §3.5: the set-aside's calm note goes once a person's choice is saved. A person's
    /// answer to the startup's offer is one; the window's own answer of the plan it offered,
    /// with no press, is not.
    #[test]
    fn a_persons_answer_to_the_offer_takes_the_set_aside_note_away() {
        for person in [true, false] {
            let scratch = aterm_tempfile::tempdir().unwrap();
            let root = scratch.path();
            system_state_choice(root, &selected(&[Setting::Permissions]));
            std::fs::write(root.join("initial-setup.json"), CORRUPT).unwrap();
            set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
            assert!(set_aside::shown(root));
            let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
            let offered = startup.offer_reading(root, || Ok(None)).unwrap();
            let admitted = if person {
                let chosen = selected(&[Setting::Permissions, Setting::Spotlight]);
                assert_ne!(offered, chosen);
                startup.persist_at(root, &chosen)
            } else {
                startup.persist_offered_at(root, &offered)
            };
            admitted.result.unwrap().expect("the answer is admitted");
            assert_eq!(set_aside::shown(root), !person, "person: {person}");
        }
    }
}
