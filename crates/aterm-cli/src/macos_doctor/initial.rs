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
mod access;
mod observe;
pub use observe::observe_initial_setup;
pub(super) use observe::{changed_at_login, forget_at_login, note_at_login};
mod scope;
pub use scope::{alab_mac, upgrade_provisional_setup};
mod secrets_retry;
use secrets_retry::Pace;
mod repositories;

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

/// The workstation preferences a plan selects: its original eight rows without
/// the two sign-outs, which are the Accounts stage's.
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
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum AdmittedVault {
    #[default]
    Unknown,
    None,
    Configured([u8; 32]),
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
        Self {
            completed: plan.selection.is_empty(),
            plan: plan_digest(plan),
            ..Self::default()
        }
    }

    fn belongs_to(self, plan: &Plan) -> bool {
        self.plan == plan_digest(plan)
    }

    fn next_for_plan(self, plan: &Plan) -> Result<Self, String> {
        Ok(Self {
            revision: next_revision(self.revision)?,
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
    /// whose every stage finished is completed, not stopped: a Stop asked for
    /// during its last stage, or after it, came too late to stop anything.
    fn stopped_by(self, request: Option<StopRequest>) -> bool {
        self.stopped
            || (!self.completed && request.is_some_and(|request| request.names(self.revision)))
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
    let Some(request) = stop_request_at(root) else {
        return Ok(());
    };
    if let Some(mut progress) = progress(root)?
        && !progress.completed
        && (request.names(progress.revision) || progress.stopped)
    {
        progress.stopped = true;
        progress.running = None;
        // An upgrade can derive this Stop from its predecessor until the
        // physical next-run record catches up. Persist it before unlinking
        // the request, or a later read could otherwise lose the Stop.
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

/// Ask the admitted run `revision` to stop at its next stage boundary (System
/// State's Stop, or a choice that replaces its plan). It is durable when this
/// returns: a relaunch reads the run as stopped even if the process ends before
/// the running stage does. Takes only the short Stop-record lock, never the
/// profile lock, and probes nothing.
pub fn request_initial_setup_stop(revision: u64) -> Result<(), String> {
    ensure_person_scope()?;
    request_stop_at(&state_root()?, revision)
}

/// The access revision a new revision of `plan` continues, when its prior run
/// (`previous` and its `progress`) managed Permissions too: a change to another
/// group is not a new approval of the grants, so it hands nothing to macOS again
/// and a Not now stays answered (System State design §3.3, §6). Turning the
/// Permissions group off and on again is a new approval.
fn carried_access(previous: Option<&Plan>, progress: Option<Progress>, plan: &Plan) -> Option<u64> {
    let (previous, progress) = previous.zip(progress)?;
    (progress.belongs_to(previous)
        && previous.selection.contains(Setting::Permissions)
        && plan.selection.contains(Setting::Permissions))
    .then(|| progress.access_revision())
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "initial setup run revision is exhausted".into())
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
    let mut done = value["done"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok())
        .filter(|bits| bits & !Stage::KNOWN == 0)
        .ok_or("unrecognized initial setup progress")?;
    // A record from before the sign-outs were their own stage ran them inside the
    // Workstation stage: its finished Workstation finished them too.
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
        Some(value) => Some(
            Stage::ALL
                .into_iter()
                .find(|stage| value.as_u64() == Some(u64::from(stage.bit())))
                .ok_or("unrecognized running setup stage")?,
        ),
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
        Some(value) => Some(
            value
                .as_u64()
                .and_then(|bits| u16::try_from(bits).ok())
                .and_then(Selection::from_bits)
                .ok_or("invalid initial setup preference scope")?,
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
    })
}

fn save_progress(root: &Path, progress: Progress) -> Result<(), String> {
    write_json(&root.join("initial-setup.json"), &progress_record(progress))
}

fn progress_record(progress: Progress) -> Value {
    let mut record = json!({"profile":PROFILE,"state":if progress.completed {"completed"} else {"pending"},"admitted":progress.admitted,"done":progress.done,"running":progress.running.map(Stage::bit),"plan":aterm_secrets::b64::encode_padded(&progress.plan),"revision":progress.revision,"stages":Stage::ALL.len()});
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
    let selection = value["selection"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok())
        .and_then(Selection::from_bits)
        .ok_or("unrecognized automatic setup selection")?;
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
    let mut record = plan_record(plan, provisional);
    scope::keep_upgrade(root, plan, &mut record)?;
    write_json(&root.join("automatic-plan.json"), &record)
}

fn plan_record(plan: &Plan, provisional: bool) -> Value {
    let mut record =
        json!({"profile":PROFILE,"selection":plan.selection.bits(),"vault_url":plan.vault_url});
    if provisional && let Some(fields) = record.as_object_mut() {
        fields.insert("provisional".into(), Value::Bool(true));
    }
    record
}

/// Whether the saved plan is one aterm chose by itself ([`save_plan_as`]).
fn saved_plan_provisional(root: &Path) -> Result<bool, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid automatic setup plan")?;
    match value.get("provisional") {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| "invalid automatic setup plan origin".into()),
    }
}

/// `aterm doctor --apply`'s plan: the original eight rows, and aterm's own
/// permissions too (approved by default, System State design §6) unless the
/// person's Cancel or Restore stands, which an explicit apply supersedes only for
/// the rows it names.
pub(super) fn save_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    // A System State Stop asked for the run this revives is part of its record.
    fold_stop_request(root)?;
    let stopped = stopped_at(root)?.is_some();
    let Some(mut progress) = progress(root)? else {
        let mut plan = Plan::from(Selection::all().workstation());
        plan.selection.set(Setting::Permissions, !stopped);
        return restart_in_vault(root, &plan, AdmittedVault::Unknown).map(|_| ());
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    let (previous, prior) = (plan.clone(), progress);
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(&plan)?;
    } else if stopped || progress.stopped {
        progress.revision = next_revision(progress.revision)?;
        progress.stopped = false;
    }
    let admitted = progress.admitted;
    if stopped {
        progress.admitted = false;
        save_progress(root, progress)?;
    }
    for setting in Setting::ALL {
        if setting.is_workstation() {
            plan.selection.set(setting, true);
        }
    }
    // aterm's own permissions are set up by default (the owner's standing
    // approval), so an explicit profile carries them too; after a Cancel or a
    // Restore it does not re-open the hand-overs the person stopped.
    plan.selection.set(Setting::Permissions, !stopped);
    progress.access = carried_access(Some(&previous), Some(prior), &plan);
    save_workstation_authorization(root, &plan)?;
    save_plan(root, &plan)?;
    progress.plan = plan_digest(&plan);
    progress.done &= !(Stage::Workstation.bit() | Stage::Accounts.bit());
    progress.completed = false;
    progress.running = None;
    // The explicit profile selects every preference, superseding any earlier
    // group-only retry even if this process ends before its first OS change.
    progress.only = None;
    progress.admitted = admitted;
    save_progress(root, progress)
}

pub(super) fn complete_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    let Some(mut progress) = progress(root)? else {
        return Ok(());
    };
    let plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) || !progress.admitted {
        return Ok(());
    }
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

fn offered_plan(
    saved: Option<(Plan, bool)>,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Result<Plan, String> {
    match (saved, configured) {
        (Some((plan, true)), Ok(Some(vault))) => Ok(vault.present(plan)),
        (Some((plan, true)), _) => Ok(plan),
        (saved, configured) => {
            let plan = saved.map_or_else(Plan::default, |(plan, _)| plan);
            Ok(match configured? {
                Some(vault) => vault.present(plan),
                None => plan,
            })
        }
    }
}

/// The plan in force: the saved one in the vault this Mac is configured for now
/// ([`offered_plan`]), reading only aterm's own configuration and setup records.
pub fn automatic_plan() -> Result<Plan, String> {
    offered_plan(saved_plan_record(&state_root()?)?, live_configured_vault())
}

pub fn authorize_initial_setup(plan: &Plan) -> Result<Option<u64>, String> {
    authorize_initial_setup_observed(plan).result
}

pub fn authorize_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<Option<u64>> {
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| authorize_reading(root, plan, live_configured_vault),
    )
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
    final_commit: Option<RestartCommit>,
}

struct RestartCommit {
    plan: Plan,
    staged: Records,
    admitted: Records,
    publications: observation::Publications,
    progress: Progress,
}

impl InitialSetupRestart {
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

    pub const fn pending(&self) -> bool {
        self.final_commit.is_some()
    }

    pub fn persist_observed(
        &mut self,
        plan: &Plan,
        vault_typed: bool,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_person_mutation(
            Err("another doctor profile pass is running".into()),
            |root| {
                self.persist_with(
                    root,
                    plan,
                    vault_typed,
                    live_configured_vault,
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
            },
        )
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
                || (current.records != owned.staged && current.records != owned.admitted)
            {
                self.final_commit = None;
                return Ok(None);
            }
            if current.records == owned.admitted {
                let revision = owned.progress.revision;
                self.final_commit = None;
                return Ok(Some(revision));
            }
        } else {
            if self.started {
                return Ok(None);
            }
            self.started = true;
            let configured = read();
            let admitted_plan = match &configured {
                Ok(Some(vault)) if !vault_typed => shown_in_vault(
                    plan.clone(),
                    &observation_at(root)
                        .map_err(|error| error.to_string())?
                        .records,
                    vault,
                ),
                _ => plan.clone(),
            };
            let mut next = stage_restart(
                root,
                &admitted_plan,
                Restart {
                    chosen: self.chosen,
                    joins: self.chosen && vault_typed,
                },
                AdmittedVault::of(configured),
            )?;
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
    restart_in_vault(root, plan, AdmittedVault::of(read()))
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
    let Restart { chosen, .. } = restart;
    validate_plan(plan)?;
    // A Stop asked for the run this replaces ends it on record first: should
    // this restart fail part-way, that run is still not resumed.
    fold_stop_request(root)?;
    let prior = progress(root)?;
    let previous = saved_plan_record(root)?;
    let mut next = prior.unwrap_or_default().next_for_plan(plan)?;
    next.access = carried_access(previous.as_ref().map(|(plan, _)| plan), prior, plan);
    next.admitted_vault = vault;
    (next.done, next.only) = chosen_keeps(restart, previous.as_ref(), prior, plan);
    // Restore put the Mac preferences back, and only the person sets them again: Set
    // up now sets every one the plan selects; a choice sets only the groups it turned
    // on (and those a choice since the Restore turned on and no pass has set yet, which
    // `chosen_keeps` carries), and a Join or a Leave as is none. Nothing restored is
    // recorded applied until a pass sets it (the pass writes `applied` itself). An
    // unreadable decision is rewritten below, as before.
    let (decision, restored) = decision_record(root).unwrap_or((Decision::Missing, false));
    // A choice since Restore whose decision never landed (a crash after its plan and its
    // progress, not yet admitted, were saved) left the record restored: the groups it
    // queued are still the person's, and `chosen_keeps` carries them with this choice's.
    let unlanded = prior.is_some_and(|prior| {
        !prior.admitted
            && previous
                .as_ref()
                .is_some_and(|(saved, _)| prior.belongs_to(saved))
    });
    if restored {
        let everything = next.done & Stage::Workstation.bit() == 0 && next.only.is_none();
        if !chosen {
            next.done &= !Stage::Workstation.bit();
            next.only = None;
        } else if (decision == Decision::Stopped(Stop::Restored) && !unlanded) || everything {
            let before = previous
                .as_ref()
                .map_or(plan.selection, |(saved, _)| saved.selection);
            let added = turned_on(before, plan);
            next.done &= !Stage::Workstation.bit();
            next.only = None;
            if added.is_empty() {
                next.done |= Stage::Workstation.bit();
            } else {
                next.only = Some(added);
            }
        }
    }
    // The same plan again by Set up now, or the window resuming the plan it
    // offered, is not a person shaping it: a provisional plan stays one (§3.5).
    // A group choice is, even one that returns to aterm's plan.
    let provisional = !chosen
        && previous
            .as_ref()
            .is_some_and(|(saved, modern)| *modern && saved == plan)
        && saved_plan_provisional(root)?;
    save_plan_as(root, plan, provisional)?;
    save_progress(root, next)?;
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
    if restored && chosen {
        save_restored_decision(root)?;
    } else if mac_settings_kept(plan, next.done) {
        save_workstation_decision(root, true)?;
    } else {
        save_workstation_authorization(root, plan)?;
    }
    Ok(next)
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

/// What a person's choice keeps of the run it replaces (System State design §3.3): a step
/// that run finished stays finished while the choice leaves its groups and the vault as
/// they were, so a choice runs only what it changed, and never resets the Dock or signs
/// Messages and FaceTime out again for another group. A Join runs the vault's steps again:
/// Secrets, and GitHub repositories, whose token the vault brings. Set up now keeps
/// nothing. The done bits it keeps, and the preferences the Workstation stage sets
/// ([`Progress::only`]).
///
/// The Workstation stage holds two groups, Keyboard & Trackpad and Desktop & Dock, so
/// it keeps its progress per preference: a group turned to Leave as is changes nothing
/// on the Mac, and keeps a finished stage finished; a group turned to Set up is the only
/// one the stage sets again, so the other group's changes since stay.
fn chosen_keeps(
    restart: Restart,
    previous: Option<&(Plan, bool)>,
    prior: Option<Progress>,
    plan: &Plan,
) -> (u16, Option<Selection>) {
    let (true, Some((previous, true)), Some(prior)) = (restart.chosen, previous, prior) else {
        return (0, None);
    };
    if !prior.belongs_to(previous) {
        return (0, None);
    }
    let vault_again = restart.joins || previous.vault_url != plan.vault_url;
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

/// The first-run decision after a choice made since Restore: pending, and the Mac
/// preferences still restored, so a later choice keeps them so until a pass sets them.
fn save_restored_decision(root: &Path) -> Result<(), String> {
    write_json(
        &root.join("first-run.json"),
        &json!({"profile":PROFILE,"state":"pending","restored":true}),
    )
}

/// The first-run decision: `applied` for Mac settings with nothing left to do, else pending.
fn save_workstation_decision(root: &Path, applied: bool) -> Result<(), String> {
    write_json(
        &root.join("first-run.json"),
        &if applied {
            json!({"profile":PROFILE,"state":"applied","accounts_verified":true})
        } else {
            json!({"profile":PROFILE,"state":"pending"})
        },
    )
}

#[cfg(test)]
fn authorize_at(root: &Path, plan: &Plan) -> Result<Option<u64>, String> {
    authorize_reading(root, plan, || Ok(None))
}

fn authorize_reading(
    root: &Path,
    plan: &Plan,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> Result<Option<u64>, String> {
    authorize_with(root, plan, AdmittedVault::of(read()), save_progress)
}

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
    let revision = next_revision(prior_progress.unwrap_or_default().revision)?;
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
                && (progress.stopped
                    || (progress.admitted
                        && (progress.completed || progress.stopped_by(stop_request_at(root)))))
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
    /// The revision whose admission of the Permissions group this run continues:
    /// the window's access lanes key their once-per-approval steps on it, so a
    /// change to another group hands nothing to macOS again (design §6).
    pub access_revision: u64,
    pub permissions_allowed: bool,
    pub observation: InitialSetupObservation,
}

/// A concurrent progress publication is expected; a record read failure is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InitialSetupSnapshotError {
    Updating,
    Read(String),
}

impl std::fmt::Display for InitialSetupSnapshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Updating => formatter.write_str("initial setup records are changing"),
            Self::Read(why) => formatter.write_str(why),
        }
    }
}

impl std::error::Error for InitialSetupSnapshotError {}

pub fn initial_setup_observation() -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(&state_root().map_err(InitialSetupSnapshotError::Read)?)
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
    let root = state_root();
    let before = root
        .as_ref()
        .ok()
        .and_then(|root| observation_at(root).ok());
    if let Err(why) = ensure_person_scope() {
        return InitialSetupObserved::new(Err(why), before);
    }
    let root = match root {
        Ok(root) => root,
        Err(why) => return InitialSetupObserved::new(Err(why), before),
    };
    observe_mutation_at(&root, before, busy, operation)
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

pub fn initial_setup_snapshot() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::Read)?;
    snapshot_in_vault(&root, live_configured_vault())
}

fn snapshot_in_vault(
    root: &Path,
    configured: Result<Option<ConfiguredVault>, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let saved = saved_plan_record(root).map_err(InitialSetupSnapshotError::Read)?;
    let fallback =
        offered_plan(saved, configured.clone()).map_err(InitialSetupSnapshotError::Read)?;
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

pub fn initial_setup_status() -> Result<(Plan, bool, Vec<Row>), String> {
    let snapshot = initial_setup_snapshot().map_err(|error| error.to_string())?;
    Ok((snapshot.plan, snapshot.finished, snapshot.rows))
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

/// Whether the plan still owes Mac settings outside `set`, the selection a pass set, so
/// the pass records nothing applied ([`super::record_applied`]):
///
/// - the Mac preferences stand restored ([`decision_record`]; a record that cannot be
///   read does not) and the authorized plan selects a preference `set` leaves out, which
///   no pass set again since Restore;
/// - or the run's progress has a Mac settings stage that `set` has no part in and has
///   not finished: the sign-outs after a preferences pass that failed, or the preferences
///   before the sign-outs' pass.
pub(super) fn owed_beyond(root: &Path, set: Selection) -> bool {
    let planned = authorized_selection(root)
        .ok()
        .flatten()
        .unwrap_or_default();
    let outside = |part: fn(Selection) -> Selection| {
        let (planned, set) = (part(planned), part(set));
        Setting::ALL
            .into_iter()
            .any(|setting| planned.contains(setting) && !set.contains(setting))
    };
    if decision_record(root).is_ok_and(|(_, restored)| restored) && outside(preferences) {
        return true;
    }
    let (Ok(Some(progress)), Ok(Some(plan))) = (progress(root), saved_plan(root)) else {
        return false;
    };
    progress.belongs_to(&plan)
        && [
            (
                Stage::Workstation,
                preferences as fn(Selection) -> Selection,
            ),
            (Stage::Accounts, accounts),
        ]
        .into_iter()
        .any(|(stage, part)| {
            stage.selected(&plan) && part(set).is_empty() && progress.done & stage.bit() == 0
        })
}

/// The first-run decision, and whether the Mac preferences stand restored: a Restore,
/// or a choice since that has not had a pass set them ([`save_restored_decision`]).
fn decision_record(root: &Path) -> Result<(Decision, bool), String> {
    let path = root.join("first-run.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Decision::Missing, false));
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes).map_err(|_| "invalid first-run decision")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized first-run profile".into());
    }
    let decision = match value["state"].as_str() {
        Some("pending") => Decision::Pending,
        Some("applied") => Decision::Applied(value["accounts_verified"].as_bool() == Some(true)),
        Some("cancelled") => Decision::Stopped(Stop::Cancelled),
        Some("restored") => Decision::Stopped(Stop::Restored),
        _ => return Err("unrecognized first-run decision".into()),
    };
    let restored = decision == Decision::Stopped(Stop::Restored)
        || (decision == Decision::Pending && value["restored"].as_bool() == Some(true));
    flush_record_with(&path, durable_name)?;
    Ok((decision, restored))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Records {
    plan: Option<(Plan, bool)>,
    progress: Option<Progress>,
    decision: Decision,
    /// The Mac preferences stand restored ([`decision_record`]).
    restored: bool,
    /// The saved plan is one aterm chose by itself ([`save_plan_as`]).
    provisional: bool,
    /// A person's Stop no pass has folded in yet ([`request_stop_at`]).
    stop_request: Option<StopRequest>,
}

impl Records {
    fn read(root: &Path) -> Result<Self, String> {
        let (decision, restored) = decision_record(root)?;
        Ok(Self {
            plan: saved_plan_record(root)?,
            progress: progress(root)?,
            decision,
            restored,
            provisional: saved_plan_provisional(root)?,
            stop_request: stop_request_at(root),
        })
    }

    /// Whether a person stopped the admitted run of these records, on record
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

fn snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let observation = observation_at(root)?;
    let active = HeldLock::profile_active(root).map_err(InitialSetupSnapshotError::Read)?;
    Ok(snapshot_from_observation(fallback, active, observation))
}

#[cfg(test)]
fn snapshot_with(
    fallback: Plan,
    activity: impl FnOnce() -> Result<bool, String>,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let records = read().map_err(InitialSetupSnapshotError::Read)?;
    if records != read().map_err(InitialSetupSnapshotError::Read)? {
        return Err(InitialSetupSnapshotError::Updating);
    }
    let active = activity().map_err(InitialSetupSnapshotError::Read)?;
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
    let plan = records
        .plan
        .as_ref()
        .map_or(fallback, |(plan, _)| plan.clone());
    let progress = records
        .progress
        .filter(|progress| progress.belongs_to(&plan));
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
    let run_stopped = progress.is_some_and(|progress| {
        progress.stopped || (progress.admitted && progress.stopped_by(records.stop_request))
    });
    let finished = stopped.is_some() || completed || run_stopped;
    let permissions_allowed = stopped.is_none()
        && !run_stopped
        && match records.progress {
            Some(_) => {
                records.plan.is_some()
                    && progress.is_some_and(|progress| progress.admitted)
                    && matches!(
                        records.decision,
                        Decision::Pending | Decision::Applied(true)
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
                && (stopped == Some(Stop::Restored) || (records.restored && completed));
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
            }
        })
        .collect();
    InitialSetupSnapshot {
        plan,
        finished,
        rows,
        revision: progress.map_or(0, |progress| progress.revision),
        access_revision: progress.map_or(0, Progress::access_revision),
        permissions_allowed,
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
    /// The workstation preferences, without the sign-outs ([`Stage::Accounts`]).
    /// The Workstation stage: the plan's preferences, or `only` these
    /// ([`Progress::only`]).
    fn workstation(
        &mut self,
        root: &Path,
        only: Option<Selection>,
    ) -> Result<Option<Report>, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    /// The Permissions stage of `revision`: complete once every grant is in place,
    /// not needed, or handed to macOS in this revision (System State design §6).
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
    /// The Messages and FaceTime sign-outs, with the Accessibility they need.
    /// `on_screen` is whether System State is the visible view of a focused window
    /// ([`SetupHooks::on_screen`]): without it a sign-out that still has work waits.
    fn accounts(&mut self, root: &Path, on_screen: bool) -> Result<Option<Report>, String>;
    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String>;
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
    /// (`aterm_types::macos_setup::system_state_front`). The Accounts stage opens
    /// Messages and FaceTime and may raise macOS's Accessibility and Automation
    /// prompts, so it runs only then (design §6); otherwise it parks, and the
    /// window wakes the run when the page comes to the front.
    pub on_screen: fn() -> bool,
}

impl SetupHooks {
    /// A pass nobody stops or watches, started by a person where they are (a
    /// terminal), so nothing in it waits for a page.
    pub const NONE: Self = Self {
        stop: |_| false,
        stage: |_| {},
        on_screen: || true,
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
    /// row left waits on a person or an absent vault (status `waiting`).
    fn of(rows: &[Row]) -> Self {
        if rows_ready(rows) {
            Self::Done
        } else if !rows.is_empty()
            && rows
                .iter()
                .all(|row| matches!(row.status.as_str(), "ok" | "applied" | WAITING))
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

/// Local facts that can wake a parked setup run. This is an observation, never
/// authorization to run its stages.
#[derive(Clone, PartialEq, Eq)]
pub struct InitialSetupParkMark {
    vault: Option<aterm_secrets::client::Status>,
    materialization_run: Option<u64>,
    materialization: Option<aterm_secrets::client::MaterializationMark>,
    interfaces: Option<Vec<aterm_secrets::network::InterfaceAddress>>,
    retry_due: Option<u64>,
}

impl std::fmt::Debug for InitialSetupParkMark {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InitialSetupParkMark")
            .finish_non_exhaustive()
    }
}

impl InitialSetupParkMark {
    /// A contended or unreadable local state gives no new materialization fact.
    /// Keep the last known one instead of waking on a subscriber's write lock.
    pub fn retain_known_from(&mut self, previous: &Self) {
        if self.materialization_run.is_some()
            && self.materialization_run == previous.materialization_run
            && self.materialization.is_none()
        {
            self.materialization = previous.materialization;
        }
    }

    /// A retryable unfinished vault try is due. A live worker consumes each
    /// deadline once, so a pass that cannot reach Secrets cannot re-arm it.
    pub fn retry_due(&self) -> Option<u64> {
        self.retry_due
    }
}

/// What a parked run waits on that moves without the window: this login's vault
/// state and, only for admitted pending Secrets, local interface changes or an
/// existing retry deadline. No sockets, vault request or prompt is made here.
pub fn initial_setup_park_mark() -> InitialSetupParkMark {
    initial_setup_park_mark_at(
        state_root().ok().as_deref(),
        aterm_secrets::client::Paths::live().ok().as_ref(),
        secrets_retry::Reads::now(&secrets_retry::Live),
        || aterm_secrets::network::interfaces().ok(),
    )
}

fn initial_setup_park_mark_at(
    root: Option<&Path>,
    paths: Option<&aterm_secrets::client::Paths>,
    now: u64,
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
) -> InitialSetupParkMark {
    let vault = paths.and_then(|paths| aterm_secrets::client::status(paths).ok());
    initial_setup_park_mark_reading(root, vault, now, interfaces, || {
        paths.and_then(|paths| {
            aterm_secrets::client::materialization_mark(paths)
                .ok()
                .flatten()
        })
    })
}

#[cfg(test)]
fn initial_setup_park_mark_with(
    root: Option<&Path>,
    vault: Option<aterm_secrets::client::Status>,
    now: u64,
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
) -> InitialSetupParkMark {
    initial_setup_park_mark_reading(root, vault, now, interfaces, || None)
}

fn initial_setup_park_mark_reading(
    root: Option<&Path>,
    mut vault: Option<aterm_secrets::client::Status>,
    now: u64,
    interfaces: impl FnOnce() -> Option<Vec<aterm_secrets::network::InterfaceAddress>>,
    materialization: impl FnOnce() -> Option<aterm_secrets::client::MaterializationMark>,
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
            )
            && Stage::Secrets.selected(plan))
        .then_some((root, progress, Stage::Repositories.selected(plan)))
    });
    let unfinished_secrets =
        pending.filter(|(_, progress, _)| progress.done & Stage::Secrets.bit() == 0);
    let materialization_run = pending
        .filter(|(_, progress, repositories)| {
            progress.done & Stage::Secrets.bit() == 0
                || (*repositories && progress.done & Stage::Repositories.bit() == 0)
        })
        .map(|(_, progress, _)| progress.revision);
    InitialSetupParkMark {
        vault,
        materialization_run,
        materialization: materialization_run.and_then(|_| materialization()),
        interfaces: unfinished_secrets.and_then(|_| interfaces()),
        retry_due: unfinished_secrets
            .and_then(|(root, progress, _)| secrets_retry::retry_due(root, progress.revision, now)),
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
    fn routes_are_not_sampled_without_a_coherent_pending_secrets_admission() {
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
            marks: "same-network".into(),
            materialization: None,
            now,
        };
        let rows = |status: &str| {
            vec![Row {
                name: Stage::Secrets.name().into(),
                status: status.into(),
                detail: "injected vault result".into(),
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
}

impl LiveInitial<secrets_retry::Live> {
    const fn live() -> Self {
        Self {
            reads: secrets_retry::Live,
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
        Outcome::Blocked | Outcome::Deferred => "blocked",
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
        // The ALab vault's own address, not joined yet: when the probe hears no ALab vault
        // on this network, the row waits with what it heard (design §3.3) until the next
        // launch, a new plan or Check again, rather than asking each pass.
        if plan.vault_url == aterm_types::macos_setup::DEFAULT_VAULT_URL
            && !in_plan_vault(plan, live_configured_vault())
            && let Some(note) = scope::probe_vault().note()
        {
            return Ok(vec![Row {
                name: Stage::Secrets.name().into(),
                status: WAITING.into(),
                detail: note.into(),
            }]);
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
        .map(secrets_rows)
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
        repositories::run(
            &sys,
            &self.context()?,
            vault,
            aterm_machines::model::Mode::Apply,
        )
    }

    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
        super::follow_up_locked_at(root)
    }
}

const APPROVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

fn readiness_row(name: &str, ready: bool, detail: String) -> Row {
    Row {
        name: name.into(),
        status: if ready { "ok" } else { "blocked" }.into(),
        detail,
    }
}

fn rows_ready(rows: &[Row]) -> bool {
    !rows.is_empty()
        && rows
            .iter()
            .all(|row| matches!(row.status.as_str(), "ok" | "applied"))
}

enum SecretsTry {
    LeaveInPlace(ConfiguredVault),
    RunItem(Replaces),
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
        .map_err(|error| format!("cannot read which vault this Mac is in: {error}"))?;
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

/// One pass over the selected stages that are not done, in order, asking `hooks`
/// and the durable stop request before each, and at the end, whether a person
/// stopped this revision. A stop is folded into the progress record here, under
/// the profile lock the pass already holds, and ends the revision without
/// cancelling its plan; one that comes during the last stage of a run that
/// finishes is spent, and the run is completed ([`stop_at_boundary`]).
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
            let next = if report.needs_attention() {
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
    };
    let mut next = SetupNext::Done;
    // What the vault stage left in this pass, for the repositories waiting on it.
    let mut vault = SetupNext::Retry;
    for stage in Stage::ALL {
        if !stage.selected(&plan) || progress.done & stage.bit() != 0 {
            continue;
        }
        if stop_at_boundary(root, &mut progress, hooks)? {
            report.rows.push(Row {
                name: "System State".into(),
                status: "stopped".into(),
                detail: "stopped before this step; nothing more runs until the plan changes".into(),
            });
            return Ok(Some((report, SetupNext::Done)));
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
                    match current {
                        Ok(rows) => report.rows.extend(rows),
                        Err(detail) => report.rows.push(Row {
                            name: Stage::Secrets.name().into(),
                            status: "unverified".into(),
                            detail,
                        }),
                    }
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
        let result = match stage {
            Stage::Workstation => match host.workstation(root, progress.only) {
                Ok(Some(result)) => Ok(result.rows),
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
                        }),
                    }
                    Ok(rows)
                }
                Ok(SecretsTry::RunItem(replaces)) => host.secrets(&plan, replaces, password),
                Err(detail) => Ok(vec![Row {
                    name: stage.name().into(),
                    status: "unverified".into(),
                    detail,
                }]),
            },
            Stage::Repositories => host.repositories(repository_vault.as_ref()),
            Stage::Accounts => match host.accounts(root, (hooks.on_screen)()) {
                Ok(Some(result)) => Ok(result.rows),
                Ok(None) => Ok(vec![readiness_row(
                    stage.name(),
                    false,
                    "waiting for the next automatic sign-out retry".into(),
                )]),
                Err(error) => Err(error),
            },
        };
        let (ready, mut rows) = match result {
            Ok(rows) => (rows_ready(&rows), rows),
            Err(detail) => (
                false,
                vec![Row {
                    name: stage.name().into(),
                    status: "error".into(),
                    detail,
                }],
            ),
        };
        let left = SetupNext::of(&rows);
        if ready {
            progress.done |= stage.bit();
        }
        if stage == Stage::Secrets {
            vault = left;
        }
        next = next.and(left);
        progress.running = None;
        save_progress(root, progress)?;
        if paced.is_some()
            && let Some(after) = host.secrets_pace()
        {
            secrets_retry::record(root, progress.revision, &after, ready, &mut rows)?;
        }
        report.rows.extend(rows);
        (hooks.stage)(StageEvent {
            settings: &settings,
            subjects: &subjects,
            running: false,
        });
    }
    let stages_finished = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0);
    // Follow-up can change preferences too. An in-process Stop holds before
    // that work even while its separate durable save has not run. Preserve
    // the last-stage rule: a run whose selected stages finished is completed.
    if !stages_finished && stop_at_boundary(root, &mut progress, hooks)? {
        report.rows.push(Row {
            name: "System State".into(),
            status: "stopped".into(),
            detail: "stopped before deferred settings; nothing more runs until the plan changes"
                .into(),
        });
        return Ok(Some((report, SetupNext::Done)));
    }
    match host.withheld(root) {
        Ok(Some(follow_up)) => report.rows.extend(follow_up.rows),
        Ok(None) => {}
        Err(detail) => report.rows.push(Row {
            name: "Settings waiting for Full Disk Access".into(),
            status: "unverified".into(),
            detail,
        }),
    }
    if stages_finished {
        progress.completed = true;
        save_progress(root, progress)?;
        next = SetupNext::Done;
    }
    // The pass's end is a boundary too. A Stop asked for during the last stage
    // of a run that then finished came too late: the run is completed, and the
    // request is spent. One asked for while work is left stops the run here,
    // so no later pass of it runs.
    if stop_at_boundary(root, &mut progress, hooks)? {
        report.rows.push(Row {
            name: "System State".into(),
            status: "stopped".into(),
            detail: "stopped after this pass; nothing more runs until the plan changes".into(),
        });
        next = SetupNext::Done;
    }
    Ok(Some((report, next)))
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
            if !on_screen && self.accounts_pending {
                return Ok(Some(Report {
                    action: Action::Apply,
                    rows: vec![Row {
                        name: Stage::Accounts.name().into(),
                        status: WAITING.into(),
                        detail: "injected: waits for System State on screen".into(),
                    }],
                    manual_steps: Vec::new(),
                    backup: root.join("original-preferences.json"),
                }));
            }
            let rows = self.stage(Stage::Accounts)?;
            // As the real sign-out pass records its pass ([`super::super::record_applied`]).
            if rows_ready(&rows) {
                let planned = authorized_selection(root)?.unwrap_or_default();
                record_applied(root, accounts(planned))?;
            }
            Ok(Some(Report {
                action: Action::Apply,
                rows,
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
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
            let rows = self.stage(Stage::Workstation)?;
            // As the real pass records its pass: over the plan's preferences, or `only`
            // those a choice narrowed it to.
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
            self.stage(Stage::Repositories)
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
        assert_eq!(doctor_plan.selection, Selection::all().workstation());
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
            assert_eq!(
                host.calls,
                [Stage::Workstation, Stage::Secrets, Stage::Accounts]
            );
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
        save_explicit_workstation_plan(root.path()).unwrap();
        complete_explicit_workstation_plan(root.path()).unwrap();
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
        for state in ["cancelled", "restored"] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            let stopped = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!stopped.permissions_allowed);
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

    #[test]
    fn permission_policy_preserves_only_verified_legacy_completion() {
        let root = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
        for (state, verified, allowed) in [
            ("applied", true, true),
            ("applied", false, false),
            ("pending", false, false),
            ("cancelled", true, false),
            ("restored", true, false),
        ] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
            )
            .unwrap();
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(snapshot.permissions_allowed, allowed, "{state}/{verified}");
            assert_eq!(snapshot.revision, 0);
        }
    }

    #[test]
    fn permission_policy_refuses_a_snapshot_that_changes_during_observation() {
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
        let mut reads = [before, after].into_iter();
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

    /// A later choice that leaves the widgets row as is withdraws the clear setup still owes,
    /// one that leaves every Mac settings group as is (the vault alone) too; a restart that
    /// keeps the row (a Join leaves the groups as they were) withdraws nothing, and once the
    /// grant arrives that run's follow-up clears Notification Center's widgets. Negative
    /// control: reading a choice without Mac settings as no answer clears them after the
    /// vault-alone plan.
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

    #[test]
    fn an_explicit_profile_sets_up_permissions_unless_setup_was_cancelled_or_restored() {
        // No setup record yet: the explicit plan is the profile and the permissions.
        let fresh = aterm_tempfile::tempdir().unwrap();
        save_explicit_workstation_plan(fresh.path()).unwrap();
        let plan = saved_plan(fresh.path()).unwrap().unwrap();
        assert!(plan.selection.contains(Setting::Permissions));
        for state in ["cancelled", "restored"] {
            // A legacy Cancel or Restore: the explicit pass does not re-open them.
            let legacy = aterm_tempfile::tempdir().unwrap();
            write_json(
                &legacy.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            save_explicit_workstation_plan(legacy.path()).unwrap();
            let plan = saved_plan(legacy.path()).unwrap().unwrap();
            assert!(!plan.selection.contains(Setting::Permissions), "{state}");
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
        assert!(!snapshot.permissions_allowed);
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
        // The explicit profile carries aterm's permissions (a new stage here).
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
        let unconfigured = snapshot_in_vault(root.path(), Ok(None)).unwrap();
        assert_eq!(unconfigured.plan, Plan::default());

        let vault = configure_vault(&paths, team);
        let configured = Some(vault.clone());
        let snapshot = snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
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
            offered_plan(saved(), Ok(configured.clone()))
                .unwrap()
                .vault_url,
            team
        );
        assert_eq!(
            offered_plan(saved(), Err("unreadable source.toml".into())).unwrap(),
            Plan::default()
        );
        assert!(offered_plan(None, Err("unreadable source.toml".into())).is_err());

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
        let shown = || snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
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
        };
        assert_eq!(SetupNext::of(&[row("ok"), row("applied")]), SetupNext::Done);
        assert_eq!(SetupNext::of(&[row("ok"), row(WAITING)]), SetupNext::Park);
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
            fn routes(&self) -> String {
                self.lan.network.lock().unwrap().clone()
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
            fn routes(&self) -> String {
                "unchanged scratch interface".into()
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
            contended.retain_known_from(&parked);
            assert_eq!(parked, contended, "writer contention does not wake setup");
            drop(writer);
            assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_some());

            client::sync(&paths, None).unwrap();
            assert!(client::materialization_ready(&paths).unwrap());
            assert_eq!(status, without_heartbeat(client::status(&paths).unwrap()));
            assert_ne!(parked, reads.park(&root), "the real repair wakes setup");
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
                    password: &|| None,
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
            assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_none());
        }

        impl SecretsOps for Lan {
            fn status(&self) -> aterm_secrets::Result<Status> {
                Ok(self.status.lock().unwrap().clone())
            }
            fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
                Ok(self.status.lock().unwrap().config.clone())
            }
            fn subscriber(&self) -> aterm_secrets::Result<State> {
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
                self.paced
                    .then(|| secrets_retry::pace(&self.reads(self.marks_vault)))
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
        let mut host = LiveInitial { reads: reads(true) };
        let first = host.secrets_pace().expect("the live host paces every try");
        assert_eq!(first, secrets_retry::pace(&reads(true)));
        assert_eq!(first.build, "1.0.0+gtest");
        assert_eq!(first.now, vault::START);
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
        let distinct: std::collections::BTreeSet<&String> = host_marks.iter().collect();
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
        let shown = snapshot_in_vault(root.path(), host.configured_vault()).unwrap();
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
        let shown = snapshot_in_vault(root.path(), read()).unwrap().plan;
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

    #[test]
    fn an_unreadable_vault_configuration_joins_nothing_and_counts_no_failure() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut host = Fake {
            configured_unreadable: true,
            pace: Some(Pace {
                build: "1.0.0+gtest".into(),
                marks: "unreadable".into(),
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

    /// The Workstation stage holds two page groups, Keyboard & Trackpad and Desktop &
    /// Dock. Keyboard & Trackpad turned to Leave as is changes nothing on the Mac, so the
    /// finished stage stays finished: the Dock, which the person may have changed since,
    /// is not set again for another group's choice.
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

    /// A group turned to Set up after the Workstation stage finished is the only one the
    /// stage sets: the other group's preferences, which the person may have changed
    /// since, stay as they are.
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

    /// A finished run whose Mac preferences a Restore put back (`plan` first).
    fn restored_after(plan: &Plan) -> aterm_tempfile::TempDir {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), plan).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Stopped(Stop::Restored)
        );
        root
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

    /// After Restore, a choice of another group, a group turned to Leave as is, or a Join
    /// sets no Mac preference again (the manual: Restore stops automatic setup, and a
    /// group turned to Leave as is changes nothing), and none is recorded applied: not
    /// by this choice, nor by any later one before a pass sets them.
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
            decision_record(root.path()).unwrap(),
            (Decision::Pending, true)
        );
    }

    /// Desktop & Dock turned to Set up after the stage finished, its pass fails, and the
    /// person turns it back to Leave as is: the stage owes nothing, so it is finished,
    /// never waiting on a pass with nothing to set.
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
            decision_record(root.path()).unwrap(),
            (Decision::Pending, true)
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

    /// A choice after Restore whose decision write never landed (a crash or an I/O error
    /// after its plan and its progress were saved, before admission) leaves the record
    /// restored and the saved plan the choice's own. The next choice keeps the groups that
    /// attempt queued: Desktop & Dock, which the person turned on, is set.
    #[test]
    fn a_choice_whose_decision_write_failed_is_not_lost() {
        use aterm_types::macos_setup::Group;
        let mut no_desktop = Plan::default();
        for setting in Group::Desktop.settings() {
            no_desktop.selection.set(*setting, false);
        }
        let root = restored_after(&no_desktop);
        system_state_choice(root.path(), &Plan::default());
        // What that choice leaves when it stops between `save_progress` and
        // `save_restored_decision`: its progress staged, not admitted, and the record
        // still restored.
        let staged = progress(root.path()).unwrap().unwrap();
        save_progress(
            root.path(),
            Progress {
                admitted: false,
                ..staged
            },
        )
        .unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"restored"}),
        )
        .unwrap();
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
            "the group the person turned on is set: {:?}",
            host.calls
        );
    }
}
