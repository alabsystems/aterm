// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable first-launch plan. Every stage is independently retryable; a
//! password lives only in the caller's secret buffer and never enters a record.

use super::*;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::Plan;

mod check;
pub use check::{check_initial_setup, check_initial_setup_observed};
mod observation;
pub use observation::{InitialSetupObservation, InitialSetupObserved};
mod authorization;
pub use authorization::InitialSetupAuthorization;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Workstation,
    Spotlight,
    Permissions,
    Secrets,
    Repositories,
}

impl Stage {
    const ALL: [Self; 5] = [
        Self::Workstation,
        Self::Spotlight,
        Self::Permissions,
        Self::Secrets,
        Self::Repositories,
    ];

    const fn bit(self) -> u16 {
        1 << self as u8
    }

    fn selected(self, plan: &Plan) -> bool {
        match self {
            Self::Workstation => !plan.selection.workstation().is_empty(),
            Self::Spotlight => plan.selection.contains(Setting::Spotlight),
            Self::Permissions => plan.selection.contains(Setting::Permissions),
            Self::Secrets => plan.selection.contains(Setting::Secrets),
            Self::Repositories => plan.selection.contains(Setting::Repositories),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Workstation => "Selected Mac settings",
            Self::Spotlight => "Spotlight and Rust documentation",
            Self::Permissions => "aterm permissions",
            Self::Secrets => "Secrets vault",
            Self::Repositories => "GitHub repositories",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Progress {
    done: u16,
    completed: bool,
    admitted: bool,
    running: Option<Stage>,
    plan: [u8; 32],
    revision: u64,
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
}

fn next_revision(revision: u64) -> Result<u64, String> {
    revision
        .checked_add(1)
        .ok_or_else(|| "initial setup run revision is exhausted".into())
}

fn progress(root: &Path) -> Result<Option<Progress>, String> {
    let path = root.join("initial-setup.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|_| "invalid initial setup progress record")?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized initial setup profile".into());
    }
    let done = value["done"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok())
        .filter(|bits| bits & !31 == 0)
        .ok_or("unrecognized initial setup progress")?;
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
    // A visible rename from a failed write is not a durable stage completion.
    flush_record_with(&path, durable_directory)?;
    Ok(Some(Progress {
        done,
        completed,
        admitted,
        running,
        plan,
        revision,
    }))
}

fn save_progress(root: &Path, progress: Progress) -> Result<(), String> {
    write_json(
        &root.join("initial-setup.json"),
        &json!({"profile":PROFILE,"state":if progress.completed {"completed"} else {"pending"},"admitted":progress.admitted,"done":progress.done,"running":progress.running.map(Stage::bit),"plan":aterm_secrets::b64::encode_padded(&progress.plan),"revision":progress.revision}),
    )
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
    flush_record_with(&path, durable_directory)?;
    Ok(Some((plan, value.get("vault_url").is_some())))
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

/// Validate immutable setup input without reading state or probing the OS.
pub fn validate_initial_setup_plan(plan: &Plan) -> Result<(), String> {
    validate_plan(plan)
}

fn save_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    validate_plan(plan)?;
    write_json(
        &root.join("automatic-plan.json"),
        &json!({"profile":PROFILE,"selection":plan.selection.bits(),"vault_url":plan.vault_url}),
    )
}

pub(super) fn save_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    let stopped = stopped_at(root)?.is_some();
    let Some(mut progress) = progress(root)? else {
        let plan = Plan::from(Selection::all().workstation());
        if stopped {
            // Retire legacy run zero before clearing its stop. A successful
            // explicit profile pass completes this original-eight-only run.
            return restart_at(root, &plan).map(|_| ());
        }
        save_workstation_authorization(root, &plan)?;
        return save_automatic_plan(root, plan.selection);
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(&plan)?;
    } else if stopped {
        progress.revision = next_revision(progress.revision)?;
    }
    let admitted = progress.admitted;
    if stopped {
        // An older worker may retain a password across its backoff. Publish
        // its retirement before replacing Cancel/Restore, including on crash.
        progress.admitted = false;
        save_progress(root, progress)?;
    }
    for setting in Setting::ALL {
        if setting.is_workstation() {
            plan.selection.set(setting, true);
        }
    }
    // Invalidate the old subset's completion before publishing its expanded
    // plan or clearing its done bit. If explicit preflight fails, the existing
    // worker must retry these settings rather than accept that stale marker.
    // A live run retains its revision and vault password; reviving a stopped
    // run above deliberately retires those workers.
    save_workstation_authorization(root, &plan)?;
    save_plan(root, &plan)?;
    progress.plan = plan_digest(&plan);
    progress.done &= !Stage::Workstation.bit();
    progress.completed = false;
    progress.running = None;
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
    progress.done |= Stage::Workstation.bit();
    progress.running = None;
    progress.completed = Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0);
    save_progress(root, progress)
}

/// The saved first-launch plan. With no saved plan, every item is selected and
/// an existing configured vault source is retained; otherwise the LAN default is used.
/// Reads only aterm's own configuration and durable setup records.
pub fn automatic_plan() -> Result<Plan, String> {
    let saved = saved_plan_record(&state_root()?)?;
    if let Some((plan, true)) = saved.as_ref() {
        return Ok(plan.clone());
    }
    let mut plan = saved.map_or_else(Plan::default, |(plan, _)| plan);
    let paths = aterm_secrets::client::Paths::live().map_err(|error| error.to_string())?;
    if let Some(config) =
        aterm_secrets::client::config(&paths).map_err(|error| error.to_string())?
    {
        plan.vault_url = match config.source {
            aterm_secrets::transport::Source::Url(url) => url,
            aterm_secrets::transport::Source::Path(path) => path.to_string_lossy().into_owned(),
        };
    }
    Ok(plan)
}

/// Authorize the exact selected stages and source before any OS probes. A
/// password must be supplied separately to [`apply_initial_setup`] and is never saved.
/// Returns None if an earlier finished decision won, or the exact run revision
/// after this choice is durably admitted, including an empty checklist.
pub fn authorize_initial_setup(plan: &Plan) -> Result<Option<u64>, String> {
    authorize_initial_setup_observed(plan).result
}

/// Authorize with a result identity captured before releasing the profile lock.
pub fn authorize_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<Option<u64>> {
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| authorize_at(root, plan),
    )
}

/// Explicitly run the selected plan again, superseding an earlier completion,
/// cancellation or restoration. Original preference before-images are retained.
pub fn restart_initial_setup(plan: &Plan) -> Result<u64, String> {
    restart_initial_setup_observed(plan).result
}

/// Restart with a result identity captured before releasing the profile lock.
pub fn restart_initial_setup_observed(plan: &Plan) -> InitialSetupObserved<u64> {
    observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| restart_at(root, plan),
    )
}

/// One explicit restart, retaining ownership only while its final admission
/// save needs recovery. Keep the original plan, password and UI receipt until
/// this returns a revision or is no longer pending; never reuse a finished attempt.
#[derive(Default)]
pub struct InitialSetupRestart {
    started: bool,
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
    /// Whether the same final admission step can still be confirmed or retried.
    /// Errors before that step do not leave a pending operation.
    pub fn pending(&self) -> bool {
        self.final_commit.is_some()
    }

    /// Persist this exact restart. Some is its durably admitted revision; None
    /// means it was superseded or already ended. Retry errors only while pending.
    pub fn persist_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        observe_person_mutation(
            Err("another doctor profile pass is running".into()),
            |root| {
                self.persist_with(root, plan, save_progress, |root| {
                    observation_at(root).map_err(|error| error.to_string())
                })
            },
        )
    }

    fn persist_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        // The caller holds profile.lock for staging, confirmation, and any
        // final write. A durable stop wins even if other records are unreadable.
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
            let mut next = stage_restart(root, plan)?;
            let staged = observe(root)?;
            if !staged
                .records
                .plan
                .as_ref()
                .is_some_and(|(saved, modern)| *modern && saved == plan)
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
                // A failed flush can leave either the staged file or our new
                // admitted file visible. Pin its identity before unlocking,
                // without relying on reads or another flush to succeed now.
                match observation::publications(root) {
                    Ok(publications) => {
                        self.final_commit.as_mut().unwrap().publications = publications;
                    }
                    Err(_) => {
                        // An unbound failure must never adopt whatever becomes
                        // readable later as this operation's publication.
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
    // A malformed replacement is still supersession. Only an error reading
    // our unchanged decision belongs to this operation and remains retryable.
    if observation::publications(root)? != *expected {
        return Ok(false);
    }
    stop?;
    Ok(true)
}

fn restart_at(root: &Path, plan: &Plan) -> Result<u64, String> {
    restart_with(root, plan, save_progress)
}

fn restart_with(
    root: &Path,
    plan: &Plan,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<u64, String> {
    let mut next = stage_restart(root, plan)?;
    next.admitted = true;
    commit(root, next)?;
    Ok(next.revision)
}

fn stage_restart(root: &Path, plan: &Plan) -> Result<Progress, String> {
    validate_plan(plan)?;
    let next = progress(root)?.unwrap_or_default().next_for_plan(plan)?;
    save_plan(root, plan)?;
    save_progress(root, next)?;
    match std::fs::remove_file(root.join("automatic-attempt.json")) {
        Ok(()) => durable_directory(root).map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "cannot reset the automatic retry cooldown: {error}"
            ));
        }
    }
    // Release an earlier stop only after the new plan and progress are durable.
    // The final admission record is still required before any setup can run.
    save_workstation_authorization(root, plan)?;
    Ok(next)
}

fn save_workstation_authorization(root: &Path, plan: &Plan) -> Result<(), String> {
    write_json(
        &root.join("first-run.json"),
        &if plan.selection.workstation().is_empty() {
            json!({"profile":PROFILE,"state":"applied","accounts_verified":true})
        } else {
            json!({"profile":PROFILE,"state":"pending"})
        },
    )
}

fn authorize_at(root: &Path, plan: &Plan) -> Result<Option<u64>, String> {
    authorize_with(root, plan, save_progress)
}

fn authorize_with(
    root: &Path,
    plan: &Plan,
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
    current.revision = revision;
    save_plan(root, plan)?;
    save_progress(root, current)?;
    // Original doctor APIs retain their own marker. Workstation completion must
    // not complete the independent vault, repository and permissions stages.
    if current.done & Stage::Workstation.bit() == 0 {
        save_workstation_authorization(root, plan)?;
    }
    // This is the multi-file authorization commit. Earlier records can be
    // visible after an interrupted write without releasing permission UI.
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

pub(super) fn finished_at(root: &Path) -> Result<bool, String> {
    Ok(stopped_at(root)?.is_some() || completed_at(root)?)
}

fn completed_at(root: &Path) -> Result<bool, String> {
    match progress(root)? {
        Some(progress) => Ok(saved_plan(root)?.is_some_and(|plan| {
            progress.belongs_to(&plan) && progress.admitted && progress.completed
        })),
        // Preserve decisions made before unified setup existed.
        None => first_run_complete(root),
    }
}

/// Whether every selected setup stage finished, or the person cancelled or
/// restored it. Completed legacy workstation decisions remain honored.
pub fn initial_setup_finished() -> Result<bool, String> {
    finished_at(&state_root()?)
}

/// The durable identity of the current setup run. Retry workers must compare it
/// after reacquiring the automatic claim, before using any retained password.
/// Zero means no modern run exists. An interrupted plan/progress pair is refused
/// until authorization repairs it, rather than returning an older run's revision.
pub fn initial_setup_revision() -> Result<u64, String> {
    revision_at(&state_root()?)
}

pub(super) fn revision_at(root: &Path) -> Result<u64, String> {
    let Some(progress) = progress(root)? else {
        return Ok(0);
    };
    plan_revision(Some(progress), saved_plan(root)?.as_ref())
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

/// A coherent observation of a durably admitted modern setup run. This does
/// not grant an execution lease; workers must still fence effects by revision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InitialSetupAdmission {
    pub plan: Plan,
    pub revision: u64,
    pub completed: bool,
}

/// Read the admitted run without loading fallback vault configuration, probing
/// the OS or changing records. Staged choices and stopped runs return None;
/// a durable stop takes precedence over unreadable plan/progress records.
/// Otherwise unreadable or changing records are errors, not missing admission.
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
            completed: progress.completed,
        }),
    )
}

/// One observation of the saved plan, its progress and the permission decision.
/// A finished run includes Cancel and Restore; callers must use the explicit
/// permission decision rather than infer permission guidance from completion.
#[derive(Debug)]
pub struct InitialSetupSnapshot {
    pub plan: Plan,
    pub finished: bool,
    pub rows: Vec<Row>,
    /// Zero means that no matching modern setup run exists.
    pub revision: u64,
    /// Whether existing native permission guidance may run. Errors or absent
    /// authorization must be treated as false by callers.
    pub permissions_allowed: bool,
    /// Identity of the same coherent records used for these rows and policy.
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

/// Observe only existing setup records. This neither authorizes work nor
/// waits behind a profile pass; callers must discard results on read failure.
pub fn initial_setup_observation() -> Result<InitialSetupObservation, InitialSetupSnapshotError> {
    observation_at(&state_root().map_err(InitialSetupSnapshotError::Read)?)
}

/// Injected state root for composition tests; never inspects the live machine.
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
    // Capture errors too: Restore may have durably stopped the run before a
    // later recovery failure. The writer lock still excludes a newer choice.
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
    // Restore publishes its stop before its effects. Matching records alone
    // cannot certify a Check that overlaps that already-running writer.
    let observation =
        before.filter(|before| idle_before && idle_after && Some(before) == after.as_ref());
    InitialSetupObserved::new(result, observation)
}

/// Read only aterm's records/configuration, without probing OS grants, accounts
/// or preferences. Concurrent record changes are refused for the caller to retry;
/// this never waits for a long-running profile pass to release its lock.
pub fn initial_setup_snapshot() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::Read)?;
    let fallback = automatic_plan().map_err(InitialSetupSnapshotError::Read)?;
    snapshot_at(&root, fallback)
}

/// Injected record root and fallback plan for composition tests.
#[cfg(any(test, debug_assertions))]
pub fn initial_setup_snapshot_at(
    root: &Path,
    fallback: Plan,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    snapshot_at(root, fallback)
}

/// The saved plan and recorded stage progress for System State. The boolean
/// means the automatic run has ended, including Cancel or Restore.
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
    let path = root.join("first-run.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Decision::Missing);
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
    flush_record_with(&path, durable_directory)?;
    Ok(decision)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Records {
    plan: Option<(Plan, bool)>,
    progress: Option<Progress>,
    decision: Decision,
}

impl Records {
    fn read(root: &Path) -> Result<Self, String> {
        Ok(Self {
            plan: saved_plan_record(root)?,
            progress: progress(root)?,
            decision: decision_at(root)?,
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
    // This is a current liveness sample, not an execution lease. No shared
    // observation lock spans the record durability reads or the UI's work.
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
    let finished = stopped.is_some() || completed;
    let permissions_allowed = stopped.is_none()
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
            // Restore stops convergence before any preference writes. Its marker
            // therefore cannot certify either the profile or successful restoration.
            let restoring = stopped == Some(Stop::Restored) && *stage == Stage::Workstation;
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
                    "Not running; Apply to retry.".into()
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
    fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String>;
    fn spotlight(&mut self) -> Result<Vec<Row>, String>;
    fn permissions(&mut self) -> Result<Vec<Row>, String>;
    fn secrets(&mut self, plan: &Plan, password: Option<&SecretBytes>) -> Result<Vec<Row>, String>;
    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String>;
    fn repositories(&mut self) -> Result<Vec<Row>, String>;
}

struct LiveInitial;

fn resume_vault(
    status: &aterm_secrets::client::Status,
    source: &aterm_secrets::transport::Source,
    requested_pin: Option<[u8; 32]>,
    password_supplied: bool,
) -> bool {
    use aterm_secrets::client::Membership;
    !password_supplied
        && status.config.as_ref().is_some_and(|config| {
            &config.source == source
                && aterm_secrets::client::pin_admitted(config, requested_pin)
                // Signed vault sync needs the key that owns the pending proof
                // or locker. Join recreates a missing key before enrollment.
                && (config.signer.is_none() || status.machine.is_some())
        })
        && matches!(
            status.membership,
            Membership::Pending | Membership::In { .. } | Membership::Plain
        )
}

impl LiveInitial {
    fn context(&self) -> Result<aterm_machines::items::Ctx, String> {
        aterm_machines::init::initial_setup_context(&aterm_machines::port::Live::new())
    }
}

fn item_rows(rows: Vec<aterm_machines::model::ItemReport>) -> Vec<Row> {
    use aterm_machines::model::Outcome;
    rows.into_iter()
        .map(|row| Row {
            name: row.title,
            status: match row.outcome {
                Outcome::Ok | Outcome::NotApplicable => "ok",
                Outcome::Changed => "applied",
                Outcome::Planned => "drift",
                Outcome::Person | Outcome::Blocked | Outcome::Deferred => "blocked",
                Outcome::Unknown => "unverified",
                Outcome::Failed => "error",
            }
            .into(),
            detail: row.why.or(row.to).unwrap_or_else(|| "verified".into()),
        })
        .collect()
}

impl InitialHost for LiveInitial {
    fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
        apply_on_first_run_at(root)
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

    fn permissions(&mut self) -> Result<Vec<Row>, String> {
        use crate::{DebuggerMember, DebuggerMode, FdaState, Mark};
        let privacy = crate::privacy_facts();
        let debugger = crate::debugger_facts();
        let devtools = atpkg::devtools::facts();
        let (debugger_mark, debugger_detail) = crate::debugger_row(&debugger);
        let devtools_ready = devtools.as_ref().is_some_and(|facts| {
            facts.grant() == aterm_containment::consent::DevToolGrant::Granted
        });
        Ok(vec![
            readiness_row(
                "Full Disk Access",
                privacy.fda == FdaState::Granted,
                crate::privacy_row(&privacy).1,
            ),
            readiness_row(
                "Debugger access",
                debugger.probe.mode == DebuggerMode::Enabled
                    && debugger_mark == Mark::Ok
                    && matches!(
                        debugger.probe.member,
                        DebuggerMember::Admin | DebuggerMember::Developer
                    ),
                debugger_detail,
            ),
            readiness_row(
                "Developer Tools",
                devtools_ready,
                devtools
                    .as_ref()
                    .and_then(atpkg::devtools::row)
                    .map_or_else(|| "permission status not measured".into(), |row| row.text),
            ),
        ])
    }

    fn secrets(&mut self, plan: &Plan, password: Option<&SecretBytes>) -> Result<Vec<Row>, String> {
        use aterm_secrets::client::{self, JoinOptions, JoinOutcome, Membership, Paths};
        use aterm_secrets::service::{self, Job, State};
        let ctx = self.context()?;
        let [user] = ctx.users.as_slice() else {
            return Err("initial setup needs exactly one console account".into());
        };
        let paths = Paths::for_home(Path::new(&user.home));
        let install = || -> Result<(), String> {
            if matches!(
                service::state(&paths.home, Job::Subscriber).map_err(|error| error.to_string())?,
                State::Installed { pid: Some(_), .. }
            ) {
                return Ok(());
            }
            let program = service::program_for_service().map_err(|error| error.to_string())?;
            service::install(
                &paths.home,
                Job::Subscriber,
                &program,
                &["secrets".into(), "watch".into()],
            )
            .map_err(|error| error.to_string())?;
            Ok(())
        };
        let installed = std::cell::RefCell::new(None);
        let requested = || *installed.borrow_mut() = Some(install());
        let (source, pin) = aterm_secrets::transport::parse_source(&plan.vault_url, &paths.home)
            .map_err(|error| error.to_string())?;
        let before = client::status(&paths).map_err(|error| error.to_string())?;
        let continuing = resume_vault(&before, &source, pin, password.is_some());
        let outcome = if continuing {
            // A pending join already holds its password proof. Rejoining with no
            // password after a crash would erase it and prevent automatic approval.
            // A changed explicit signer must instead pass through join's admission.
            client::sync(&paths, None).map_err(|error| error.to_string())?;
            None
        } else {
            Some(
                client::join(
                    &paths,
                    &JoinOptions {
                        source: plan.vault_url.clone(),
                        host: None,
                        interactive: false,
                        // An open vault, or one whose password this Apply carries, lets
                        // the Mac in within seconds: wait for it, so one Apply ends synced.
                        wait: APPROVAL_WAIT,
                        confirm: &|_| false,
                        password: &|| password.cloned(),
                        requested: &requested,
                        say: &|_| {},
                    },
                )
                .map_err(|error| error.to_string())?,
            )
        };
        let installed = installed.into_inner().unwrap_or_else(install);
        installed?;
        let status = client::status(&paths).map_err(|error| error.to_string())?;
        let Some(expected) = check::selected_vault(plan, &paths.home, &status)? else {
            return Ok(vec![readiness_row(
                "Secrets vault",
                false,
                "the selected vault changed; setup will retry".into(),
            )]);
        };
        if !matches!(
            service::state(&paths.home, Job::Subscriber).map_err(|error| error.to_string())?,
            State::Installed { pid: Some(_), .. }
        ) {
            return Ok(vec![readiness_row(
                "Secrets vault",
                false,
                "waiting for the secrets subscriber to start".into(),
            )]);
        }
        if matches!(status.membership, Membership::In { .. } | Membership::Plain)
            && !client::materialization_ready_for(&paths, expected)
                .map_err(|error| error.to_string())?
        {
            return Ok(vec![readiness_row(
                "Secrets vault",
                false,
                "credentials are not fully written; setup will retry".into(),
            )]);
        }
        match (outcome, status.membership) {
            (_, Membership::In { seq, .. }) => Ok(vec![readiness_row(
                "Secrets vault",
                true,
                format!("verified credentials synchronized at version {seq}; subscriber running"),
            )]),
            (_, Membership::Plain) => Ok(vec![readiness_row(
                "Secrets vault",
                true,
                "configured local credentials synchronized; subscriber running".into(),
            )]),
            (Some(JoinOutcome::Pending { code, host }), _) => Ok(vec![readiness_row(
                "Secrets vault",
                false,
                format!(
                    "waiting for the vault to approve this Mac ({host}, code {code}); it joins by itself once approved"
                ),
            )]),
            (_, Membership::Pending) => Ok(vec![readiness_row(
                "Secrets vault",
                false,
                match status.machine {
                    Some((_, code)) => format!(
                        "waiting for the vault to approve this Mac (code {code}); it joins by itself once approved"
                    ),
                    None => {
                        "waiting for the vault to approve this Mac; it joins by itself once approved"
                            .into()
                    }
                },
            )]),
            _ => Ok(vec![readiness_row(
                "Secrets vault",
                false,
                "vault membership is not yet verified; setup will retry".into(),
            )]),
        }
    }

    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        check::current_vault_rows(plan)
    }

    fn repositories(&mut self) -> Result<Vec<Row>, String> {
        let sys = aterm_machines::port::Live::new();
        aterm_machines::init_github::run_initial_repositories(
            &sys,
            &self.context()?,
            aterm_machines::model::Mode::Apply,
        )
        .map(item_rows)
    }
}

/// How long the setup's join waits for the vault to let this Mac in. An admin Mac's
/// subscriber reads the join queue every 2 s, so an open vault, or a request that proves the
/// join password, is approved and synced well within it; past it the row says the Mac waits,
/// and the subscriber finishes the join once approved.
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

fn execute_initial(
    host: &mut dyn InitialHost,
    root: &Path,
    password: Option<&SecretBytes>,
) -> Result<Option<Report>, String> {
    if finished_at(root)? {
        return Ok(None);
    }
    let Some(plan) = saved_plan(root)? else {
        return Ok(None);
    };
    let Some(mut progress) = progress(root)? else {
        // A legacy plan authorized its original workstation rows only.
        return host.workstation(root);
    };
    if !progress.belongs_to(&plan) || !progress.admitted {
        // A partially published choice, including a pending record from before
        // admission was explicit, needs authorization before any OS work.
        return Ok(None);
    }
    let mut report = Report {
        action: Action::Apply,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
    };
    for stage in Stage::ALL {
        if !stage.selected(&plan) || progress.done & stage.bit() != 0 {
            continue;
        }
        if stage == Stage::Repositories && Stage::Secrets.selected(&plan) {
            if progress.done & Stage::Secrets.bit() != 0 {
                // A subscriber can fail to materialize a later version while
                // repository setup is pending. Historical completion never
                // admits stale credentials into a new repository attempt.
                let current = host.current_vault(&plan);
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
                report.rows.push(readiness_row(
                    stage.name(),
                    false,
                    "waiting for the selected secrets vault to synchronize credentials".into(),
                ));
                continue;
            }
        }
        progress.running = Some(stage);
        save_progress(root, progress)?;
        let result = match stage {
            Stage::Workstation => match host.workstation(root) {
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
            Stage::Permissions => host.permissions(),
            Stage::Secrets => host.secrets(&plan, password),
            Stage::Repositories => host.repositories(),
        };
        match result {
            Ok(rows) => {
                if rows_ready(&rows) {
                    progress.done |= stage.bit();
                }
                report.rows.extend(rows);
            }
            Err(detail) => report.rows.push(Row {
                name: stage.name().into(),
                status: "error".into(),
                detail,
            }),
        }
        progress.running = None;
        save_progress(root, progress)?;
    }
    if Stage::ALL
        .iter()
        .all(|stage| !stage.selected(&plan) || progress.done & stage.bit() != 0)
    {
        progress.completed = true;
        save_progress(root, progress)?;
    }
    Ok(Some(report))
}

/// Apply independently selected setup stages after authorization. Pending
/// enrollment, permissions and offline services remain retryable. This never
/// reads a terminal or stores the optional join password.
pub fn apply_initial_setup(
    expected_revision: u64,
    password: Option<&SecretBytes>,
) -> Result<Option<Report>, String> {
    apply_initial_setup_observed(expected_revision, password).result
}

/// Apply with an observation captured under the existing profile lock.
pub fn apply_initial_setup_observed(
    expected_revision: u64,
    password: Option<&SecretBytes>,
) -> InitialSetupObserved<Option<Report>> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return InitialSetupObserved::new(Ok(None), None);
    }
    observe_person_mutation(Ok(None), |root| {
        execute_initial_for_revision(&mut LiveInitial, root, expected_revision, password)
    })
}

fn execute_initial_for_revision(
    host: &mut dyn InitialHost,
    root: &Path,
    expected_revision: u64,
    password: Option<&SecretBytes>,
) -> Result<Option<Report>, String> {
    // The caller holds profile.lock: a legacy doctor Apply can revive a stopped
    // run without taking automatic.lock after a scheduler's earlier revision read.
    if revision_at(root)? != expected_revision {
        return Ok(None);
    }
    execute_initial(host, root, password)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Fake {
        calls: Vec<Stage>,
        pending: Option<Stage>,
        failure: Option<Stage>,
        password_seen: bool,
        observe: Option<PathBuf>,
        vault_checks: usize,
        vault_blocked: bool,
        vault_error: bool,
    }

    impl Fake {
        fn stage(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
            self.calls.push(stage);
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
            Ok(vec![readiness_row(
                stage.name(),
                self.pending != Some(stage),
                "injected stage outcome".into(),
            )])
        }
    }

    impl InitialHost for Fake {
        fn workstation(&mut self, root: &Path) -> Result<Option<Report>, String> {
            if first_run_complete(root)? {
                return Ok(None);
            }
            let rows = self.stage(Stage::Workstation)?;
            if rows_ready(&rows) {
                write_json(
                    &root.join("first-run.json"),
                    &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
                )?;
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
        fn permissions(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Permissions)
        }
        fn secrets(
            &mut self,
            _: &Plan,
            password: Option<&SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.password_seen = password.is_some();
            self.stage(Stage::Secrets)
        }
        fn repositories(&mut self) -> Result<Vec<Row>, String> {
            self.stage(Stage::Repositories)
        }

        fn current_vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
            self.vault_checks += 1;
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
        // Restore stops first even when recovery preflight fails. Doctor Apply
        // then revives only its eight groups before hitting the same bad backup.
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

        // The intervening stop is already gone when the old window's untouched
        // timer returns Begin. Its captured publications must still fence it.
        let accepted = authorization
            .persist_at(root.path(), &offered)
            .result
            .unwrap();
        let mut old_worker = Fake {
            failure: Some(Stage::Workstation),
            ..Fake::default()
        };
        if let Some(revision) = accepted {
            execute_initial_for_revision(&mut old_worker, root.path(), revision, None).unwrap();
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
                attempt.persist_with(root, plan, commit, observe)
            },
        )
    }

    fn observe_restart(root: &Path) -> Result<InitialSetupObservation, String> {
        observation_at(root).map_err(|error| error.to_string())
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
                    Some(&password),
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
            execute_initial(&mut host, staged.path(), None)
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
            authorize_with(root.path(), &empty, |root, progress| {
                save_progress(root, progress)?;
                Err("injected failure after publishing the final admission".into())
            })
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
            execute_initial(&mut host, root.path(), None)
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
            authorize_with(unpublished.path(), &empty, |_, _| {
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
            let report = execute_initial(&mut host, root, None)?.unwrap();
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
        let before = observation_at(root.path()).unwrap();
        let cancelled =
            InitialSetupCancellation::default().persist_observed_at(root.path(), Some(before));
        assert!(cancelled.result.unwrap());
        let cancelled = cancelled.observation.unwrap();
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

        // Publication can be delayed until after another process accepts Apply.
        // The old failure must retain its original context rather than borrow it.
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
    fn stale_cancel_retry_cannot_stop_a_newer_pending_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let original = revision_at(root.path()).unwrap();
        let mut cancel = InitialSetupCancellation::default();
        // Another process is still applying this run when Cancel is chosen.
        let active = HeldLock::take(root.path()).unwrap();
        assert!(cancel.persist_at(root.path()).is_err());
        drop(active);

        // That process can accept a new Apply while the old Cancel retains
        // only its own process's busy reservation during the retry backoff.
        restart_at(root.path(), &plan).unwrap();
        assert!(revision_at(root.path()).unwrap() > original);
        assert!(!cancel.persist_at(root.path()).unwrap());
        assert!(
            !finished_at(root.path()).unwrap(),
            "the old retry stopped the new run"
        );
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
            // The scheduler can have read the old revision immediately before
            // this revival. Its actual effect boundary must check under lock.
            let _profile = HeldLock::take(root.path()).unwrap();
            let password = SecretBytes::from(b"test-only-old-run-password".as_slice());
            let mut host = Fake::default();
            assert!(
                execute_initial_for_revision(&mut host, root.path(), original, Some(&password))
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty());
            assert!(!host.password_seen);
            execute_initial_for_revision(&mut host, root.path(), current, None).unwrap();
            assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
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
    fn retained_cancel_retries_the_same_run_and_accepts_its_already_finished_decision() {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let mut cancel = InitialSetupCancellation::default();
        let active = HeldLock::take(root.path()).unwrap();
        assert!(cancel.persist_at(root.path()).is_err());
        drop(active);
        assert!(cancel.persist_at(root.path()).unwrap());
        assert!(finished_at(root.path()).unwrap());
        assert!(cancel.persist_at(root.path()).unwrap());
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
        assert_eq!(current.done, Stage::Workstation.bit());
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

        // A new plan may be published before its progress record. An older
        // successful Mac decision must not authorize the new permission group.
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

        execute_initial(&mut Fake::default(), root.path(), None).unwrap();
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
            // Restore publishes this before preference work and may fail later.
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
                authorize_with(root.path(), &plan, |_, _| {
                    Err("injected final admission failure".into())
                })
                .is_err()
            );
            let staged = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(!staged.permissions_allowed);
            assert!(!staged.finished);
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), None)
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
            execute_initial(&mut host, root.path(), None).unwrap();
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
        execute_initial(&mut Fake::default(), root.path(), None).unwrap();
        let next = selected(&[Setting::Widgets, Setting::Permissions]);

        // The first publication still has the old, admitted progress. Its plan
        // binding must hold permissions until the reset and admission complete.
        save_plan(root.path(), &next).unwrap();
        let published = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!published.permissions_allowed);
        assert_eq!(published.revision, 0);
        assert!(
            restart_with(root.path(), &next, |_, _| {
                Err("injected restart admission failure".into())
            })
            .is_err()
        );
        let uncommitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!uncommitted.permissions_allowed);
        assert!(!uncommitted.finished);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), None)
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
            execute_initial(&mut host, root.path(), None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(authorize_at(root.path(), &plan).unwrap().is_some());
        execute_initial(&mut host, root.path(), None).unwrap();
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
        let report = execute_initial(&mut host, root.path(), Some(&password))
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
            execute_initial(&mut host, root.path(), None)
                .unwrap()
                .unwrap()
                .needs_attention()
        );
        assert_eq!(host.calls, Stage::ALL[..4]);
        assert!(first_run_complete(root.path()).unwrap());
        assert!(!finished_at(root.path()).unwrap());
        // Another launch must still be able to claim this unfinished plan.
        assert!(claim_automatic_setup_at(root.path()).unwrap().is_some());
        host.calls.clear();
        host.pending = None;
        execute_initial(&mut host, root.path(), None).unwrap();
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
            execute_initial(&mut host, root.path(), None).unwrap();
            assert_eq!(host.calls, Stage::ALL);
            assert_eq!(host.vault_checks, 1);
            let completed = progress(root.path()).unwrap().unwrap().done;
            assert_ne!(completed & Stage::Secrets.bit(), 0);
            assert_eq!(completed & Stage::Repositories.bit(), 0);
            let mut interrupted = progress(root.path()).unwrap().unwrap();
            interrupted.running = Some(Stage::Repositories);
            save_progress(root.path(), interrupted).unwrap();

            // A background sync now has incomplete files (or unreadable state).
            // No repository configuration, credential or clone operation may run.
            host.calls.clear();
            host.failure = None;
            host.vault_blocked = !readiness_error;
            host.vault_error = readiness_error;
            let report = execute_initial(&mut host, root.path(), None)
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

            // The next pass repairs the vault before its fresh admission check;
            // preferences, Spotlight and permissions retain their prior history.
            host.vault_blocked = false;
            host.vault_error = false;
            execute_initial(&mut host, root.path(), None).unwrap();
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
        execute_initial(&mut host, root.path(), None).unwrap();
        assert_eq!(host.calls, [Stage::Repositories]);
        assert!(!host.password_seen);
        assert_eq!(host.vault_checks, 0);

        let empty = aterm_tempfile::tempdir().unwrap();
        authorize_at(empty.path(), &Plan::from(Selection::none())).unwrap();
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, empty.path(), None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(finished_at(empty.path()).unwrap());
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
        let report = execute_initial(&mut host, root.path(), None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Permissions, Stage::Secrets]);
        assert!(!finished_at(root.path()).unwrap());
        // Mac completion alone cannot prevent a later Cancel from persisting.
        assert!(first_run_complete(root.path()).unwrap());
        cancel_automatic_setup_at(root.path()).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).unwrap(),
            before
        );
        assert!(
            execute_initial(&mut host, root.path(), None)
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
        assert!(!revised.selection.contains(Setting::Permissions));
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
        execute_initial(&mut Fake::default(), root.path(), None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();

        // This is also the durable state when Restore's preference writes fail:
        // the stop marker precedes those writes and says nothing about success.
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

        // A fresh reader after process exit must retain the stop even though
        // the recovery record prevented Restore from changing preferences.
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
            execute_initial(&mut automatic, root.path(), None)
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
        assert!(execute_initial(&mut automatic, root.path(), None).is_err());
        assert!(automatic.calls.is_empty());
    }

    #[test]
    fn cancel_keeps_applied_preferences_and_stops_only_unfinished_stages() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), None).unwrap();
        cancel_automatic_setup_at(root.path()).unwrap();
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
        execute_initial(&mut host, root.path(), None).unwrap();
        assert!(first_run_complete(root.path()).unwrap());
        let revision = revision_at(root.path()).unwrap();

        save_explicit_workstation_plan(root.path()).unwrap();
        // Inject the error a preference preflight can return after the explicit
        // plan was staged, before it reaches its own pending-marker write.
        host.calls.clear();
        host.failure = Some(Stage::Workstation);
        let report = execute_initial(&mut host, root.path(), None)
            .unwrap()
            .unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
        assert!(!first_run_complete(root.path()).unwrap());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
            0
        );
        assert_eq!(revision_at(root.path()).unwrap(), revision);

        host.calls.clear();
        host.failure = None;
        host.pending = None;
        execute_initial(&mut host, root.path(), None).unwrap();
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
        execute_initial(&mut host, root.path(), None).unwrap();
        let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(finished);
        assert!(rows.iter().all(|row| row.status == "applied"));
        assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
    }

    #[test]
    fn a_signed_vault_without_its_machine_identity_requires_join_before_sync() {
        use aterm_secrets::client::{Config, Membership, Status};
        use aterm_secrets::transport::Source;
        let source = Source::Url("https://vault.example/v1/team".into());
        let mut status = Status {
            config: Some(Config {
                source: source.clone(),
                name: "team".into(),
                signer: Some([7; 32]),
                followed: Vec::new(),
                host: "test-host".into(),
            }),
            machine: None,
            membership: Membership::Pending,
            names: Vec::new(),
        };
        assert!(!resume_vault(&status, &source, None, false));
        status.membership = Membership::In { seq: 1, heard: 1 };
        assert!(!resume_vault(&status, &source, None, false));

        // A genuine identity retains its join proof when no replacement
        // password was supplied; a new password still starts a fresh join.
        status.machine = Some(vault_machine());
        status.membership = Membership::Pending;
        assert!(resume_vault(&status, &source, None, false));
        assert!(!resume_vault(&status, &source, None, true));

        // Plain local vaults intentionally have no machine key.
        let plain = Source::Path(PathBuf::from("/test-vault/team.json"));
        status.config = Some(Config {
            source: plain.clone(),
            name: "team".into(),
            signer: None,
            followed: Vec::new(),
            host: "test-host".into(),
        });
        status.machine = None;
        status.membership = Membership::Plain;
        assert!(resume_vault(&status, &plain, None, false));
    }

    fn vault_machine() -> (aterm_secrets::age::Recipient, String) {
        let recipient = aterm_secrets::age::Identity::from_secret_bytes([31; 32]).recipient();
        (recipient, aterm_secrets::client::join_code(&recipient))
    }

    #[test]
    fn a_pending_vault_without_a_new_password_resumes_sync_instead_of_erasing_its_proof() {
        use aterm_secrets::client::{Config, Membership, Status};
        use aterm_secrets::transport::Source;
        let source = Source::Url("http://vault.local:7484/v1/team".into());
        let mut status = Status {
            config: Some(Config {
                source: source.clone(),
                name: "team".into(),
                signer: Some([7; 32]),
                followed: Vec::new(),
                host: "test-host".into(),
            }),
            machine: Some(vault_machine()),
            membership: Membership::Pending,
            names: Vec::new(),
        };
        assert!(resume_vault(&status, &source, None, false));
        assert!(!resume_vault(&status, &source, None, true));
        assert!(!resume_vault(
            &status,
            &Source::Url("http://other.local/v1/team".into()),
            None,
            false
        ));
        status.membership = Membership::Revoked { seq: 3 };
        assert!(!resume_vault(&status, &source, None, false));
    }

    #[test]
    fn a_verified_rotation_preserves_pending_proof_under_the_original_url_pin() {
        use aterm_secrets::client::{Config, Membership, Status};
        use aterm_secrets::transport::Source;
        let source = Source::Url("https://vault.example/v1/team".into());
        let original = [7; 32];
        let status = Status {
            config: Some(Config {
                source: source.clone(),
                name: "team".into(),
                signer: Some([8; 32]),
                followed: vec![original],
                host: "test-host".into(),
            }),
            machine: Some(vault_machine()),
            membership: Membership::Pending,
            names: Vec::new(),
        };
        assert!(resume_vault(&status, &source, Some(original), false));
        assert!(!resume_vault(&status, &source, Some([9; 32]), false));
        assert!(!resume_vault(&status, &source, Some(original), true));
    }

    #[test]
    fn a_different_explicit_signer_cannot_resume_sync_under_the_stored_pin() {
        use aterm_secrets::client::{Config, Membership, Status};
        use aterm_secrets::transport::{Source, parse_source};
        let source = Source::Url("https://vault.example/v1/team".into());
        let stored_pin = [1; 32];
        let requested_pin = [2; 32];
        let status = Status {
            config: Some(Config {
                source: source.clone(),
                name: "team".into(),
                signer: Some(stored_pin),
                followed: Vec::new(),
                host: "test-host".into(),
            }),
            machine: Some(vault_machine()),
            membership: Membership::Pending,
            names: Vec::new(),
        };
        let url = format!(
            "https://vault.example/v1/team#{}",
            aterm_secrets::b64::encode_padded(&requested_pin)
        );
        let (parsed, pin) = parse_source(&url, Path::new("/test-home")).unwrap();
        assert_eq!(parsed, source);
        assert_eq!(pin, Some(requested_pin));
        assert!(!resume_vault(&status, &parsed, pin, false));
        // The same admitted pin still preserves a pending join's password proof.
        assert!(resume_vault(&status, &source, Some(stored_pin), false));
        assert!(resume_vault(&status, &source, None, false));
    }

    #[test]
    fn an_unchecked_vault_source_cannot_block_selected_preferences() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = selected(&[Setting::Widgets]);
        plan.vault_url = "unfinished source".into();
        authorize_at(root.path(), &plan).unwrap();
        let mut host = Fake::default();
        execute_initial(&mut host, root.path(), None).unwrap();
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
        execute_initial(&mut Fake::default(), root.path(), None).unwrap();
        assert!(finished_at(root.path()).unwrap());
        let new = selected(&[Setting::Dock, Setting::Secrets]);
        // Simulate a crash after the plan rename, before its progress reset.
        save_plan(root.path(), &new).unwrap();
        assert!(!finished_at(root.path()).unwrap());
        let (_, _, rows) = status_at(root.path(), Plan::default()).unwrap();
        assert!(rows.iter().all(|row| row.status == "pending"));
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(!finished_at(root.path()).unwrap());
        assert!(authorize_at(root.path(), &new).unwrap().is_some());
        execute_initial(&mut host, root.path(), None).unwrap();
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
        execute_initial(&mut host, root.path(), None).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first);
        // A new offer can carry a corrected password for the same plan.
        authorize_at(root.path(), &plan).unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), first + 1);
        restart_at(root.path(), &plan).unwrap();
        let rerun = revision_at(root.path()).unwrap();
        assert_eq!(rerun, first + 2);
        host.pending = None;
        execute_initial(&mut host, root.path(), None).unwrap();
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
}
