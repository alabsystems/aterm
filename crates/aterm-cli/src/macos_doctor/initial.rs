// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The durable first-launch plan. Every stage is independently retryable; a
//! password lives only in the caller's secret buffer and never enters a record.

use super::*;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::Plan;

mod check;
pub use check::check_initial_setup;

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

fn save_plan(root: &Path, plan: &Plan) -> Result<(), String> {
    validate_plan(plan)?;
    write_json(
        &root.join("automatic-plan.json"),
        &json!({"profile":PROFILE,"selection":plan.selection.bits(),"vault_url":plan.vault_url}),
    )
}

pub(super) fn save_explicit_workstation_plan(root: &Path) -> Result<(), String> {
    let Some(mut progress) = progress(root)? else {
        save_workstation_authorization(root, &Plan::from(Selection::all().workstation()))?;
        return save_automatic_plan(root, Selection::all().workstation());
    };
    let mut plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        progress = progress.next_for_plan(&plan)?;
    }
    for setting in Setting::ALL {
        if setting.is_workstation() {
            plan.selection.set(setting, true);
        }
    }
    // Invalidate the old subset's completion before publishing its expanded
    // plan or clearing its done bit. If explicit preflight fails, the existing
    // worker must retry these settings rather than accept that stale marker.
    // Keep its revision so independent vault work retains its in-memory password.
    save_workstation_authorization(root, &plan)?;
    save_plan(root, &plan)?;
    progress.plan = plan_digest(&plan);
    progress.done &= !Stage::Workstation.bit();
    progress.completed = false;
    progress.running = None;
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
/// Returns false if an earlier finished decision won, and true only after this
/// exact choice is durably admitted, including an empty checklist.
pub fn authorize_initial_setup(plan: &Plan) -> Result<bool, String> {
    ensure_person_scope()?;
    let root = state_root()?;
    let _lock = HeldLock::take(&root)?;
    authorize_at(&root, plan)
}

/// Explicitly run the selected plan again, superseding an earlier completion,
/// cancellation or restoration. Original preference before-images are retained.
pub fn restart_initial_setup(plan: &Plan) -> Result<(), String> {
    ensure_person_scope()?;
    let root = state_root()?;
    let _lock = HeldLock::take(&root)?;
    restart_at(&root, plan)
}

fn restart_at(root: &Path, plan: &Plan) -> Result<(), String> {
    restart_with(root, plan, save_progress)
}

fn restart_with(
    root: &Path,
    plan: &Plan,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<(), String> {
    validate_plan(plan)?;
    let mut next = progress(root)?.unwrap_or_default().next_for_plan(plan)?;
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
    next.admitted = true;
    commit(root, next)
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

fn authorize_at(root: &Path, plan: &Plan) -> Result<bool, String> {
    authorize_with(root, plan, save_progress)
}

fn authorize_with(
    root: &Path,
    plan: &Plan,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
) -> Result<bool, String> {
    if finished_at(root)? {
        return Ok(false);
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
    Ok(true)
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

fn revision_at(root: &Path) -> Result<u64, String> {
    let Some(progress) = progress(root)? else {
        return Ok(0);
    };
    let plan = saved_plan(root)?.ok_or("the initial setup plan is missing")?;
    if !progress.belongs_to(&plan) {
        return Err("the initial setup plan changed before its progress reset completed".into());
    }
    Ok(progress.revision)
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

/// Read only aterm's records/configuration, without probing OS grants, accounts
/// or preferences. Concurrent record changes are refused for the caller to retry;
/// this never waits for a long-running profile pass to release its lock.
pub fn initial_setup_snapshot() -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let root = state_root().map_err(InitialSetupSnapshotError::Read)?;
    let fallback = automatic_plan().map_err(InitialSetupSnapshotError::Read)?;
    snapshot_at(&root, fallback)
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
    snapshot_with(fallback, || Records::read(root))
}

fn snapshot_with(
    fallback: Plan,
    mut read: impl FnMut() -> Result<Records, String>,
) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
    let records = read().map_err(InitialSetupSnapshotError::Read)?;
    if records != read().map_err(InitialSetupSnapshotError::Read)? {
        return Err(InitialSetupSnapshotError::Updating);
    }
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
            Row {
                name: stage.name().into(),
                status: if restoring {
                    "stopped"
                } else if completed {
                    "applied"
                } else if finished {
                    "stopped"
                } else if progress.is_some_and(|progress| progress.running == Some(*stage)) {
                    "running"
                } else {
                    "pending"
                }
                .into(),
                detail: if restoring {
                    "Restore requested; see results.".into()
                } else if finished && !completed {
                    "Not applied by this run.".into()
                } else {
                    String::new()
                },
            }
        })
        .collect();
    Ok(InitialSetupSnapshot {
        plan,
        finished,
        rows,
        revision: progress.map_or(0, |progress| progress.revision),
        permissions_allowed,
    })
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
            &config.source == source && aterm_secrets::client::pin_admitted(config, requested_pin)
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
                        wait: std::time::Duration::ZERO,
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
                    "waiting for vault approval for {host}, code {code}; subscriber will retry"
                ),
            )]),
            (_, Membership::Pending) => Ok(vec![readiness_row(
                "Secrets vault",
                false,
                match status.machine {
                    Some((_, code)) => {
                        format!("waiting for vault approval, code {code}; subscriber will retry")
                    }
                    None => "waiting for vault approval; subscriber will retry".into(),
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
pub fn apply_initial_setup(password: Option<&SecretBytes>) -> Result<Option<Report>, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    ensure_person_scope()?;
    let root = state_root()?;
    let Some(_lock) = HeldLock::try_take(&root)? else {
        return Ok(None);
    };
    execute_initial(&mut LiveInitial, &root, password)
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
            snapshot_with(Plan::default(), || Ok(reads.next().unwrap())),
            Err(InitialSetupSnapshotError::Updating)
        ));
        assert!(matches!(
            snapshot_with(Plan::default(), || Err("injected read failure".into())),
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

            assert!(authorize_at(root.path(), &plan).unwrap());
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
            assert!(!authorize_at(root.path(), &Plan::default()).unwrap());
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
        assert!(authorize_at(root.path(), &plan).unwrap());
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
            machine: None,
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
            machine: None,
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
            machine: None,
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
        assert!(authorize_at(root.path(), &new).unwrap());
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
