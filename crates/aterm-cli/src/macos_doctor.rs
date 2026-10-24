// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS workstation profile, checks, and `defaults` recovery journal.

use aterm_json::{Value, json};
use aterm_machines::init::{Hold, Workstation, WorkstationGroup};
use aterm_machines::port::{Cmd, System};
use aterm_machines::{model::PValue, plist};
use aterm_types::macos_setup::{Selection, Setting};

mod initial;
pub use initial::{
    InitialSetupAdmission, InitialSetupAuthorization, InitialSetupObservation,
    InitialSetupObserved, InitialSetupRestart, InitialSetupSnapshot, InitialSetupSnapshotError,
    apply_initial_setup, apply_initial_setup_observed, authorize_initial_setup,
    authorize_initial_setup_observed, check_initial_setup, check_initial_setup_observed,
    initial_setup_admission, initial_setup_finished, initial_setup_observation,
    initial_setup_revision, initial_setup_snapshot, initial_setup_status, restart_initial_setup,
    restart_initial_setup_observed, validate_initial_setup_plan,
};
#[cfg(any(test, debug_assertions))]
pub use initial::{initial_setup_observation_at, initial_setup_snapshot_at};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const PROFILE: &str = "macos-workstation-v1";

/// The operation requested by `aterm doctor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Read preferences, without creating state or changing the desktop.
    Check,
    /// Back up and apply the workstation profile.
    Apply,
    /// Restore the original values and disable the automatic first-run apply.
    Restore,
}

impl Action {
    const fn name(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Apply => "apply",
            Self::Restore => "restore",
        }
    }
}

/// One preference's observed outcome.
#[derive(Debug)]
pub struct Row {
    /// Human-readable setting name.
    pub name: String,
    /// `ok`, `drift`, `applied`, `restored`, `blocked`, `unverified` or `error`; a setup
    /// snapshot's rows also `pending`, `running` or `stopped`.
    pub status: String,
    /// Action or failure detail, without account identifiers.
    pub detail: String,
}

/// The machine-readable and human-readable result of a profile pass.
#[derive(Debug)]
pub struct Report {
    /// The requested operation.
    pub action: Action,
    /// Outcomes for settings this tool can configure.
    pub rows: Vec<Row>,
    /// Steps left to the account owner (the JSON's `manual_steps`); no pass records one, so
    /// it is empty.
    pub manual_steps: Vec<String>,
    /// Original per-key values retained for `--restore`.
    pub backup: PathBuf,
}

impl Report {
    /// Whether a setting or account remains out of policy, blocked, unverified or failed.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.rows.iter().any(|r| {
            matches!(
                r.status.as_str(),
                "drift" | "error" | "blocked" | "unverified"
            )
        })
    }

    /// A compact JSON report, suitable for fleet automation.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                json!({
                    "name": r.name, "status": r.status, "detail": r.detail,
                })
            })
            .collect();
        aterm_json::to_string(&json!({
            "profile": PROFILE, "action": self.action.name(),
            "needs_attention": self.needs_attention(), "settings": rows,
            "manual_steps": self.manual_steps,
            "backup": self.backup.to_string_lossy().as_ref(),
        }))
        .unwrap_or_else(|e| format!("{{\"error\":\"JSON report: {e}\"}}"))
    }

    /// The same report for a terminal.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = format!("macOS workstation profile ({})\n", self.action.name());
        for row in &self.rows {
            text.push_str(&format!(
                "  {}: {} — {}\n",
                row.name, row.status, row.detail
            ));
        }
        for step in &self.manual_steps {
            text.push_str(&format!("  owner action: {step}\n"));
        }
        text.push_str(&format!("  backup: {}\n", self.backup.display()));
        text
    }
}

/// Whether this process holds Full Disk Access ([`aterm_machines::items::defaults::withheld`]).
#[must_use]
pub fn full_disk_access() -> Option<bool> {
    use aterm_containment::{FdaState, ProbeGate};
    match aterm_containment::probe_fda(ProbeGate::on()).state {
        FdaState::Granted => Some(true),
        FdaState::Denied => Some(false),
        FdaState::Unknown => None,
    }
}

/// Check, apply, or restore the macOS workstation profile for the actual user.
pub fn run(action: Action) -> Result<Report, String> {
    run_observed(action).result
}

/// Observe the state belonging to a result before releasing its profile lock.
pub fn run_observed(action: Action) -> InitialSetupObserved<Report> {
    if action == Action::Check {
        return initial::observe_read_only(state_root(), |root| {
            execute(&mut NativeHost::new(), root, action)
        });
    }
    initial::observe_person_mutation(
        Err("another doctor profile pass is running".into()),
        |root| {
            if action == Action::Apply {
                initial::save_explicit_workstation_plan(root)?;
            }
            let report = execute(&mut NativeHost::new(), root, action)?;
            if action == Action::Apply && !report.needs_attention() {
                initial::complete_explicit_workstation_plan(root)?;
            }
            Ok(report)
        },
    )
}

/// The saved checklist, or every box checked when no plan has been authorized.
pub fn automatic_selection() -> Result<Selection, String> {
    Ok(authorized_selection(&state_root()?)?.unwrap_or_default())
}

/// Persist the person's exact choice before any OS probes or changes.
pub fn authorize_automatic_setup(selected: Selection) -> Result<(), String> {
    ensure_person_scope()?;
    let root = state_root()?;
    let _lock = HeldLock::take(&root)?;
    authorize_automatic_setup_at(&root, selected)
}

fn authorize_automatic_setup_at(root: &Path, selected: Selection) -> Result<(), String> {
    if first_run_complete(root)? {
        return Ok(());
    }
    save_automatic_plan(root, selected)?;
    write_json(
        &root.join("first-run.json"),
        &if selected.is_empty() {
            json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true })
        } else {
            json!({ "profile": PROFILE, "state": "pending" })
        },
    )
}

fn save_automatic_plan(root: &Path, selected: Selection) -> Result<(), String> {
    write_json(
        &root.join("automatic-plan.json"),
        &json!({ "profile": PROFILE, "selection": selected.bits() }),
    )
}

fn authorized_selection(root: &Path) -> Result<Option<Selection>, String> {
    authorized_selection_with_flush(root, durable_name)
}

fn authorized_selection_with_flush(
    root: &Path,
    flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<Option<Selection>, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let plan: Value = aterm_json::from_slice(&bytes).map_err(|e| format!("automatic plan: {e}"))?;
    if plan["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic setup profile".into());
    }
    let bits = plan["selection"]
        .as_u64()
        .and_then(|bits| u16::try_from(bits).ok());
    let selected = bits
        .and_then(Selection::from_bits)
        .ok_or("unrecognized automatic setup selection")?;
    flush_record_with(&path, flush)?;
    Ok(Some(selected))
}

/// Exclusive ownership of the automatic countdown and its subsequent retry worker.
#[must_use = "keep the claim alive until the automatic setup worker ends"]
pub struct AutomaticSetupClaim {
    _lock: HeldLock,
}

/// Claim the automatic countdown without inspecting preferences or accounts.
pub fn claim_automatic_setup() -> Result<Option<AutomaticSetupClaim>, String> {
    ensure_person_scope()?;
    claim_automatic_setup_at(&state_root()?)
}

/// Claim an explicit System State rerun, including a plan that already ended.
pub fn claim_initial_setup_rerun() -> Result<Option<AutomaticSetupClaim>, String> {
    ensure_person_scope()?;
    Ok(HeldLock::try_take_named(&state_root()?, "automatic.lock")?
        .map(|lock| AutomaticSetupClaim { _lock: lock }))
}

fn claim_automatic_setup_at(root: &Path) -> Result<Option<AutomaticSetupClaim>, String> {
    let Some(lock) = HeldLock::try_take_named(root, "automatic.lock")? else {
        return Ok(None);
    };
    if initial::finished_at(root)? {
        return Ok(None);
    }
    Ok(Some(AutomaticSetupClaim { _lock: lock }))
}

/// Apply after the first-launch offer authorizes setup.
pub fn apply_on_first_run() -> Result<Option<Report>, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    ensure_person_scope()?;
    let root = state_root()?;
    let Some(_lock) = HeldLock::try_take(&root)? else {
        return Ok(None);
    };
    apply_on_first_run_at(&root)
}

fn apply_on_first_run_at(root: &Path) -> Result<Option<Report>, String> {
    let sys = aterm_machines::port::Live::new();
    apply_on_first_run_with(
        root,
        &sys,
        &mut NativeHost::new(),
        &aterm_types::version::build_id(),
        std::time::SystemTime::now(),
    )
}

fn apply_on_first_run_with(
    root: &Path,
    sys: &dyn System,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    if first_run_complete(root)? {
        return Ok(None);
    }
    let Some(selected) = authorized_selection(root)? else {
        return Ok(None);
    };
    if selected.is_empty() {
        authorize_automatic_setup_at(root, selected)?;
        return Ok(None);
    }
    let accounts_selected =
        selected.contains(Setting::Messages) || selected.contains(Setting::FaceTime);
    let accessibility = if accounts_selected {
        aterm_machines::init_accounts::accessibility_ready(sys).ok()
    } else {
        None
    };
    if !automatic_retry_due(root, accessibility, now)? {
        return Ok(None);
    }
    write_json(
        &root.join("automatic-attempt.json"),
        &json!({ "profile": PROFILE, "accessibility_ready": accessibility }),
    )?;
    let consent = root.join("accessibility-request.json");
    let mut request_failure = None;
    if accounts_selected
        && accessibility != Some(true)
        && !consent
            .try_exists()
            .map_err(|e| format!("{}: {e}", consent.display()))?
        && aterm_machines::init_accounts::accessibility_needed_selected(sys, selected)
            .unwrap_or(false)
    {
        write_json(
            &consent,
            &json!({ "profile": PROFILE, "state": "requested" }),
        )?;
        if let Err(detail) = aterm_machines::init_accounts::request_accessibility(sys) {
            request_failure = Some(detail);
        }
    }
    host.select(selected);
    let mut report = automatic_pass(host, root, build, now)?;
    if report.needs_attention()
        && let Some(detail) = request_failure
    {
        report.rows.push(Row {
            name: "Native account permission request".into(),
            status: "blocked".into(),
            detail,
        });
    }
    Ok(Some(report))
}

/// Cancel automatic setup until `aterm doctor --apply` or a System State rerun starts it again.
pub fn cancel_automatic_setup() -> Result<(), String> {
    InitialSetupCancellation::default().persist().map(|_| ())
}

#[cfg(test)]
fn cancel_automatic_setup_at(root: &Path) -> Result<(), String> {
    InitialSetupCancellation::default()
        .persist_at(root)
        .map(|_| ())
}

/// A retained Cancel choice, bound to the first readable setup run.
#[derive(Default)]
pub struct InitialSetupCancellation {
    revision: Option<u64>,
}

impl InitialSetupCancellation {
    /// Persist this choice through transient errors.
    pub fn persist(&mut self) -> Result<bool, String> {
        self.persist_observed().result
    }

    /// Keep this choice's result bound to its observed setup state.
    pub fn persist_observed(&mut self) -> InitialSetupObserved<bool> {
        let root = state_root();
        let before = root
            .as_ref()
            .ok()
            .and_then(|root| initial::observation_at(root).ok());
        if let Err(why) = ensure_person_scope() {
            return InitialSetupObserved::new(Err(why), before);
        }
        match root {
            Ok(root) => self.persist_observed_at(&root, before),
            Err(why) => InitialSetupObserved::new(Err(why), before),
        }
    }

    #[cfg(test)]
    fn persist_at(&mut self, root: &Path) -> Result<bool, String> {
        self.persist_observed_at(root, initial::observation_at(root).ok())
            .result
    }

    fn persist_observed_at(
        &mut self,
        root: &Path,
        before: Option<InitialSetupObservation>,
    ) -> InitialSetupObserved<bool> {
        let revision = match self.revision {
            Some(revision) => revision,
            None => {
                let revision = match initial::revision_at(root) {
                    Ok(revision) => revision,
                    Err(why) => return InitialSetupObserved::new(Err(why), before),
                };
                self.revision = Some(revision);
                revision
            }
        };
        initial::observe_mutation_at(
            root,
            before,
            Err("another doctor profile pass is running".into()),
            |root| {
                if initial::revision_at(root)? != revision {
                    return Ok(false);
                }
                cancel_automatic_setup_locked_at(root)?;
                Ok(true)
            },
        )
    }
}

fn cancel_automatic_setup_locked_at(root: &Path) -> Result<(), String> {
    if initial::finished_at(root)? {
        return Ok(());
    }
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "cancelled" }),
    )
}

/// Whether automatic setup was fully verified, explicitly restored, or cancelled.
pub fn first_run_finished() -> Result<bool, String> {
    first_run_complete(&state_root()?)
}

fn first_run_complete(root: &Path) -> Result<bool, String> {
    first_run_complete_with_flush(root, durable_name)
}

fn first_run_complete_with_flush(
    root: &Path,
    flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<bool, String> {
    let path = root.join("first-run.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let marker: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("first-run marker: {e}"))?;
    if marker["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized first-run profile".into());
    }
    let finished = matches!(marker["state"].as_str(), Some("restored" | "cancelled"))
        || (marker["state"].as_str() == Some("applied")
            && marker["accounts_verified"].as_bool() == Some(true));
    if finished {
        flush_record_with(&path, flush)?;
    }
    Ok(finished)
}

fn automatic_retry_due(
    root: &Path,
    accessibility: Option<bool>,
    now: std::time::SystemTime,
) -> Result<bool, String> {
    let path = root.join("automatic-attempt.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let marker: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("automatic attempt: {e}"))?;
    if marker["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic-attempt profile".into());
    }
    if marker["accessibility_ready"].as_bool() != accessibility {
        return Ok(true);
    }
    let age = now.duration_since(
        std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|e| format!("{}: {e}", path.display()))?,
    );
    Ok(age.map_or(true, |age| age >= std::time::Duration::from_secs(60)))
}

const HELD: &str = "aterm tried this by itself and it did not take, so this build does not try it again on its own; Apply in System State, or run aterm doctor --apply, to try again";

const WAITING: &str = "aterm tried this by itself and it did not take; it tries once more on its own at setup's next try or aterm's next launch once ten minutes have passed; Apply in System State, or run aterm doctor --apply, to try now";

const SETTLED: &str = "this setup had it as wanted once and it has changed since; Apply in System State, or run aterm doctor --apply, to set it again";

const SETTLED_OWED: &str = "Notification Center's widget list is left to the Full Disk Access follow-up, whose row says how that went; Apply in System State, or run aterm doctor --apply, to clear it now";

const TRIES: u32 = 2;

const SECOND_TRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

const HELD_BUILDS: usize = 16;

const HELD_RECORD: &str = "automatic-held.json";

fn automatic_pass(
    host: &mut dyn Host,
    root: &Path,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Report, String> {
    let revision = initial::revision_at(root)?;
    let now = epoch_secs(now);
    let mut record = Automatic::load(root, revision)?;
    let owed = if host.full_disk_access() == Some(true) {
        load_withheld(root).unwrap_or_default()
    } else {
        Withheld::default()
    };
    host.hold(&record.holds(build, now, &owed));
    let pass = execute_pass(host, root, Action::Apply, build, now)?;
    if record.add(build, now, &pass) {
        record.save(root, revision)?;
    }
    Ok(pass.report)
}

fn epoch_secs(now: std::time::SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

fn load_tries(entries: &Value, bad: &dyn Fn() -> String) -> Result<Vec<Failed>, String> {
    entries
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|entry| {
            Ok(Failed {
                item: entry["item"].as_str().ok_or_else(bad)?.to_owned(),
                build: entry["build"].as_str().ok_or_else(bad)?.to_owned(),
                tries: entry["tries"]
                    .as_u64()
                    .and_then(|tries| u32::try_from(tries).ok())
                    .ok_or_else(bad)?,
                at: entry["at"].as_u64().ok_or_else(bad)?,
            })
        })
        .collect()
}

/// `ids` as this build names them, each once, in order: an id an older aterm recorded is the
/// items it stands for now ([`aterm_machines::init_preferences::current_ids`]; `withheld`
/// for the follow-up's record).
fn current_ids(ids: Vec<String>, withheld: bool) -> Vec<String> {
    let mut current = Vec::new();
    for id in ids {
        for now in aterm_machines::init_preferences::current_ids(&id, withheld) {
            if !current.contains(&now) {
                current.push(now);
            }
        }
    }
    current
}

/// `failed` with each item as this build names it ([`current_ids`]).
fn current_tries(failed: Vec<Failed>, withheld: bool) -> Vec<Failed> {
    failed
        .into_iter()
        .flat_map(|f| {
            aterm_machines::init_preferences::current_ids(&f.item, withheld)
                .into_iter()
                .map(move |item| Failed { item, ..f.clone() })
        })
        .collect()
}

fn tries_json(failed: &[Failed]) -> Vec<Value> {
    failed
        .iter()
        .map(|f| json!({ "item": f.item, "build": f.build, "tries": f.tries, "at": f.at }))
        .collect()
}

fn held_back(failed: &[Failed], build: &str, now: u64) -> Vec<(String, Hold)> {
    failed
        .iter()
        .filter(|f| f.build == build)
        .filter_map(|failed| {
            let why = if failed.tries >= TRIES {
                HELD
            } else if now < failed.at.saturating_add(SECOND_TRY_AFTER.as_secs()) {
                WAITING
            } else {
                return None;
            };
            Some((failed.item.clone(), Hold::Back(why.into())))
        })
        .collect()
}

fn count_tries(failed: &mut Vec<Failed>, build: &str, now: u64, items: &[String]) {
    for item in items {
        match failed
            .iter_mut()
            .find(|f| f.item == *item && f.build == build)
        {
            Some(failed) => {
                failed.tries = failed.tries.saturating_add(1);
                failed.at = now;
            }
            None => failed.push(Failed {
                item: item.clone(),
                build: build.to_owned(),
                tries: 1,
                at: now,
            }),
        }
    }
    let mut builds: Vec<(u64, &str)> = Vec::new();
    for failed in failed.iter() {
        match builds.iter_mut().find(|(_, build)| *build == failed.build) {
            Some((at, _)) => *at = (*at).max(failed.at),
            None => builds.push((failed.at, failed.build.as_str())),
        }
    }
    if builds.len() > HELD_BUILDS {
        builds.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
        let kept: Vec<String> = builds[..HELD_BUILDS]
            .iter()
            .map(|(_, build)| (*build).to_owned())
            .collect();
        failed.retain(|f| kept.contains(&f.build));
    }
}

/// What one setup run's automatic passes did ([`HELD_RECORD`]).
#[derive(Debug, Default, PartialEq, Eq)]
struct Automatic {
    /// The items an automatic pass found as wanted, by its change or as they were.
    settled: Vec<String>,
    /// The tries whose change did not verify, per item and build.
    failed: Vec<Failed>,
}

/// One item's tries in one build that did not verify.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Failed {
    item: String,
    build: String,
    tries: u32,
    /// The last try, in seconds since the epoch.
    at: u64,
}

impl Automatic {
    fn load(root: &Path, revision: u64) -> Result<Self, String> {
        let path = root.join(HELD_RECORD);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let record: Value = aterm_json::from_slice(&bytes)
            .map_err(|e| format!("held automatic setup items: {e}"))?;
        if record["profile"].as_str() != Some(PROFILE) {
            return Err("unrecognized held automatic setup profile".into());
        }
        if record["revision"].as_u64() != Some(revision) {
            return Ok(Self::default());
        }
        let bad = || "held automatic setup items are unreadable".to_owned();
        let settled = record["settled"]
            .as_array()
            .ok_or_else(bad)?
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(bad))
            .collect::<Result<_, _>>()?;
        let failed = load_tries(&record["failed"], &bad)?;
        Ok(Self {
            settled: current_ids(settled, false),
            failed: current_tries(failed, false),
        })
    }

    fn save(&self, root: &Path, revision: u64) -> Result<(), String> {
        write_json(
            &root.join(HELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": revision,
                "settled": self.settled, "failed": tries_json(&self.failed),
            }),
        )
    }

    fn holds(&self, build: &str, now: u64, owed: &Withheld) -> Vec<(String, Hold)> {
        let mut holds: Vec<(String, Hold)> = self
            .settled
            .iter()
            .map(|item| {
                let why = if owed.items.contains(item) {
                    SETTLED_OWED
                } else {
                    SETTLED
                };
                (item.clone(), Hold::AsIs(why.into()))
            })
            .collect();
        let owed_tries = held_back(&owed.failed, build, now)
            .into_iter()
            .filter(|(item, _)| owed.items.contains(item));
        for (item, hold) in held_back(&self.failed, build, now)
            .into_iter()
            .chain(owed_tries)
        {
            match holds.iter_mut().find(|(held, _)| *held == item) {
                None => holds.push((item, hold)),
                Some((_, held @ Hold::Back(_))) if hold == Hold::Back(HELD.into()) => *held = hold,
                Some(_) => {}
            }
        }
        holds
    }

    fn add(&mut self, build: &str, now: u64, pass: &Pass) -> bool {
        let before = (self.settled.len(), self.failed.clone());
        for item in &pass.settled {
            if !self.settled.contains(item) {
                self.settled.push(item.clone());
            }
        }
        count_tries(&mut self.failed, build, now, &pass.unverified);
        let settled = &self.settled;
        self.failed.retain(|f| !settled.contains(&f.item));
        (self.settled.len(), &self.failed) != (before.0, &before.1)
    }
}

fn state_root() -> Result<PathBuf, String> {
    aterm_types::dirs::state_dir()
        .map(|d| d.join("doctor"))
        .ok_or_else(|| "cannot resolve aterm's state directory".to_owned())
}

/// Whether this process may change the person's macOS profile.
#[derive(Debug, PartialEq, Eq)]
enum PersonScope {
    /// It may.
    Person,
    /// It never may: an isolated instance, sudo, or a HOME that is not the account's.
    Refused(String),
    /// The account home or HOME could not be read just now.
    Unresolved(String),
}

impl PersonScope {
    fn lasting(self) -> Option<String> {
        match self {
            Self::Refused(why) => Some(why),
            Self::Person | Self::Unresolved(_) => None,
        }
    }
}

fn person_scope() -> PersonScope {
    let uid = aterm_uds::peer::our_uid();
    scope_of(
        aterm_types::dirs::runs_under_private_state_root(),
        uid,
        aterm_types::dirs::home_dir().as_deref(),
        || account_home(uid),
    )
}

fn scope_of(
    private: bool,
    uid: u32,
    home: Option<&Path>,
    account_home: impl FnOnce() -> Result<PathBuf, String>,
) -> PersonScope {
    if private {
        return PersonScope::Refused(
            "macOS profile changes are disabled for an isolated aterm instance".into(),
        );
    }
    if uid == 0 {
        return PersonScope::Refused(
            "run the macOS profile as the logged-in account, without sudo".into(),
        );
    }
    let Some(home) = home else {
        return PersonScope::Unresolved("cannot resolve HOME".into());
    };
    let canonical = account_home().and_then(|actual_home| {
        let canon =
            |p: &Path| std::fs::canonicalize(p).map_err(|e| format!("{}: {e}", p.display()));
        Ok((canon(home)?, canon(&actual_home)?))
    });
    match canonical {
        Err(why) => PersonScope::Unresolved(why),
        Ok((home, actual_home)) if home != actual_home => PersonScope::Refused(
            "macOS profile changes are disabled when HOME differs from the account's home".into(),
        ),
        Ok(_) => PersonScope::Person,
    }
}

fn ensure_person_scope() -> Result<(), String> {
    match person_scope() {
        PersonScope::Person => Ok(()),
        PersonScope::Refused(why) | PersonScope::Unresolved(why) => Err(why),
    }
}

/// Why this process may never change the person's macOS profile.
pub fn lasting_scope_refusal() -> Option<String> {
    person_scope().lasting()
}

fn account_home(uid: u32) -> Result<PathBuf, String> {
    use std::ffi::CStr;
    use std::os::unix::ffi::OsStrExt as _;
    let mut storage = vec![0u8; 65_536];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut found = std::ptr::null_mut();
    // SAFETY: the result and scratch buffers are live and exclusively borrowed
    // for the call; getpwuid_r's strings point into storage until copied below.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            entry.as_mut_ptr(),
            storage.as_mut_ptr().cast(),
            storage.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return Err(format!(
            "cannot resolve the account home (getpwuid_r: {status})"
        ));
    }
    // SAFETY: a non-null successful result initialized entry.
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() {
        return Err("the account has no home directory".into());
    }
    // SAFETY: getpwuid_r's non-null dir is NUL-terminated and points into storage.
    let dir = unsafe { CStr::from_ptr(entry.pw_dir) };
    Ok(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

struct HeldLock(std::fs::File);

impl HeldLock {
    fn profile_active(root: &Path) -> Result<bool, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(root.join("profile.lock"))
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(format!("doctor profile lock: {error}")),
        };
        if !file
            .metadata()
            .map_err(|error| format!("doctor profile lock: {error}"))?
            .file_type()
            .is_file()
        {
            return Err("doctor profile lock is not a regular file".into());
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "HeldLock::drop releases the shared observation by LOCK_UN, including child-spawn paths"
        )]
        match file.try_lock_shared() {
            Ok(()) => {
                drop(Self(file));
                Ok(false)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(true),
            Err(std::fs::TryLockError::Error(error)) => {
                Err(format!("doctor profile lock: {error}"))
            }
        }
    }

    fn take(root: &Path) -> Result<Self, String> {
        Self::try_take(root)?.ok_or_else(|| "another doctor profile pass is running".into())
    }

    fn try_take(root: &Path) -> Result<Option<Self>, String> {
        Self::try_take_named(root, "profile.lock")
    }

    fn try_take_named(root: &Path, name: &str) -> Result<Option<Self>, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(root.join(name))
            .map_err(|e| format!("doctor lock: {e}"))?;
        #[expect(
            clippy::disallowed_methods,
            reason = "HeldLock::drop releases by LOCK_UN before closing, including child-spawn paths"
        )]
        match file.try_lock() {
            Ok(()) => Ok(Some(Self(file))),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(format!("doctor profile lock: {e}")),
        }
    }
}

impl Drop for HeldLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Target {
    domain: String,
    key: String,
    host: bool,
}

impl Target {
    #[cfg(test)]
    fn new(domain: &str, key: &str) -> Self {
        Self {
            domain: domain.into(),
            key: key.into(),
            host: false,
        }
    }

    fn describe(&self) -> String {
        format!(
            "{}{} {}",
            if self.host { "-currentHost " } else { "" },
            self.domain,
            self.key
        )
    }
}

trait Host {
    fn selection(&self) -> Selection {
        Selection::all().workstation()
    }
    fn full_disk_access(&self) -> Option<bool>;
    fn select(&mut self, selected: Selection);
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String>;
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String>;
    fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String>;
    fn attempts(&mut self) -> Vec<Attempt>;
    fn settled(&mut self) -> Vec<String>;
    fn withheld(&mut self) -> Vec<String>;
    fn hold(&mut self, items: &[(String, Hold)]);
    fn held(&self) -> Vec<String>;
    fn journal(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String>;
    /// Relaunch `name`, and return once it runs again and what it reads of `reads` has
    /// settled, so a read-back after it reads what the relaunched process kept.
    fn restart(&mut self, name: &str, reads: &[&Target]) -> Result<(), String>;
    fn workstation(&mut self, group: WorkstationGroup, apply: bool) -> Result<Vec<Row>, String>;
    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<Row>;
}

/// One item whose change an apply pass reached.
#[derive(Clone)]
struct Attempt {
    /// The shared item's id (`init.dock@<user>`).
    item: String,
    /// Whether its read-back verified the change.
    verified: bool,
    /// The before-images of the preference keys it was about to change.
    preferences: Vec<Saved>,
}

type Prepare =
    Box<dyn FnMut(&dyn System, WorkstationGroup, Selection) -> Result<Workstation, String>>;

const DEFAULTS: &str = aterm_uds::os_path!("defaults");

struct NativeHost {
    sys: Box<dyn System>,
    prepare: Prepare,
    selected: Selection,
    domains: BTreeMap<(String, bool), plist::Entries>,
    /// The pass [`Host::preference_changes`] prepared, and its group.
    prepared: Option<(WorkstationGroup, Workstation)>,
    attempts: Vec<Attempt>,
    settled: Vec<String>,
    withheld: Vec<String>,
    held: Vec<(String, Hold)>,
}

impl NativeHost {
    fn new() -> Self {
        Self::over(
            Box::new(aterm_machines::port::Live::new().with_full_disk_access(full_disk_access())),
            Box::new(aterm_machines::init::prepare_workstation_selected),
        )
    }

    fn selected(selected: Selection) -> Self {
        let mut host = Self::new();
        host.selected = selected.workstation();
        host
    }

    fn over(sys: Box<dyn System>, prepare: Prepare) -> Self {
        Self {
            sys,
            prepare,
            selected: Selection::all().workstation(),
            domains: BTreeMap::new(),
            prepared: None,
            attempts: Vec::new(),
            settled: Vec::new(),
            withheld: Vec::new(),
            held: Vec::new(),
        }
    }

    fn granted(&self, target: &Target) -> bool {
        !contained(target) || self.sys.full_disk_access() == Some(true)
    }

    fn prepare(&mut self, group: WorkstationGroup) -> Result<Workstation, String> {
        let mut prepared = (self.prepare)(self.sys.as_ref(), group, self.selected)?;
        prepared.hold(&self.held);
        Ok(prepared)
    }

    fn entries(&mut self, domain: &str, host: bool) -> Result<&plist::Entries, String> {
        if let Some(why) =
            aterm_machines::items::defaults::withheld(domain, self.sys.full_disk_access())
        {
            return Err(why);
        }
        let cache_key = (domain.to_owned(), host);
        if !self.domains.contains_key(&cache_key) {
            let mut export = Cmd::new(DEFAULTS);
            if host {
                export = export.arg("-currentHost");
            }
            let export = export.args(["export", domain, "-"]);
            let entries = aterm_machines::items::defaults::read_domain(self.sys.as_ref(), &export)?;
            self.domains.insert(cache_key.clone(), entries);
        }
        self.domains
            .get(&cache_key)
            .ok_or_else(|| "missing cached preference domain".into())
    }
}

impl Host for NativeHost {
    fn selection(&self) -> Selection {
        self.selected
    }
    fn full_disk_access(&self) -> Option<bool> {
        self.sys.full_disk_access()
    }
    fn select(&mut self, selected: Selection) {
        self.selected = selected.workstation();
        self.prepared = None;
    }
    fn attempts(&mut self) -> Vec<Attempt> {
        std::mem::take(&mut self.attempts)
    }
    fn settled(&mut self) -> Vec<String> {
        std::mem::take(&mut self.settled)
    }
    fn withheld(&mut self) -> Vec<String> {
        std::mem::take(&mut self.withheld)
    }
    fn hold(&mut self, items: &[(String, Hold)]) {
        self.held = items.to_vec();
    }
    fn held(&self) -> Vec<String> {
        self.held.iter().map(|(item, _)| item.clone()).collect()
    }
    fn journal(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        self.sys.journal()?.before_change(id, from, to, revert)
    }
    fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String> {
        use aterm_machines::items::defaults::Scope;
        use aterm_machines::model::Read;
        let mut prepared = self.prepare(group)?;
        let changes = prepared
            .preference_changes(self.sys.as_ref())?
            .into_iter()
            .map(|(target, read)| {
                if target.scope == Scope::System {
                    return Err(
                        "workstation repair unexpectedly targets a system preference".into(),
                    );
                }
                let value = match read {
                    Read::Absent => None,
                    Read::Value(v) => Some(v),
                    Read::Unusable(why) => return Err(why),
                };
                let target = Target {
                    domain: target.domain,
                    key: target.key,
                    host: target.scope == Scope::Host,
                };
                Ok(Saved {
                    granted: self.granted(&target),
                    target,
                    value,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.prepared = Some((group, prepared));
        Ok(changes)
    }
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String> {
        self.entries(&target.domain, target.host)?
            .iter()
            .find(|(key, _)| *key == target.key)
            .map(|(_, value)| value.clone())
            .transpose()
            .map_err(|e| format!("{}: {e}", target.describe()))
    }

    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
        if let Some(why) = withheld(&*self, target) {
            return Err(why);
        }
        self.domains.remove(&(target.domain.clone(), target.host));
        let verb = if value.is_some() { "write" } else { "delete" };
        let failed = |why: String| format!("{DEFAULTS} {verb} {}: {why}", target.describe());
        let ran = self
            .sys
            .run(&defaults_cmd(target, value))
            .map_err(|e| failed(e.to_string()))?;
        if ran.success() {
            Ok(())
        } else {
            Err(failed(ran.describe()))
        }
    }

    /// The shared items' own relaunch ([`aterm_machines::init_activation::relaunch_reading`]),
    /// journaled first by the caller (`refresh`).
    fn restart(&mut self, name: &str, reads: &[&Target]) -> Result<(), String> {
        let user = own_account(self.sys.euid());
        let keys: Vec<aterm_machines::items::defaults::Target> = reads
            .iter()
            .map(|target| {
                if target.host {
                    aterm_machines::items::defaults::Target::host(
                        &target.domain,
                        &target.key,
                        &user,
                    )
                } else {
                    aterm_machines::items::defaults::Target::user(
                        &target.domain,
                        &target.key,
                        &user,
                    )
                }
            })
            .collect();
        let answer = aterm_machines::init_activation::relaunch_reading(
            self.sys.as_ref(),
            &user,
            name,
            &keys,
        );
        // What it now holds is what it settled on, not what was read before it.
        for target in reads {
            self.domains.remove(&(target.domain.clone(), target.host));
        }
        answer.map_err(|why| format!("restart {name}: {why}"))
    }

    fn workstation(&mut self, group: WorkstationGroup, apply: bool) -> Result<Vec<Row>, String> {
        use aterm_machines::model::{Mode, Outcome};
        let mode = if apply { Mode::Apply } else { Mode::Plan };
        let prepared = match self.prepared.take() {
            Some((prepared_group, prepared)) if prepared_group == group => prepared,
            _ => self.prepare(group)?,
        };
        self.domains.clear();
        let rows = if apply {
            use aterm_machines::items::defaults::Scope;
            use aterm_machines::model::Read;
            let result = prepared.run_recording(self.sys.as_ref(), mode)?;
            let attempts = result
                .attempts
                .into_iter()
                .map(|attempt| {
                    let preferences = attempt
                        .preferences
                        .into_iter()
                        .map(|(target, read)| {
                            let value = match read {
                                Read::Absent => None,
                                Read::Value(value) => Some(value),
                                Read::Unusable(why) => return Err(why),
                            };
                            let target = Target {
                                domain: target.domain,
                                key: target.key,
                                host: target.scope == Scope::Host,
                            };
                            Ok(Saved {
                                granted: self.granted(&target),
                                target,
                                value,
                            })
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    Ok(Attempt {
                        item: attempt.id,
                        verified: attempt.verified,
                        preferences,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            self.attempts.extend(attempts);
            self.settled.extend(
                result
                    .rows
                    .iter()
                    .filter(|row| {
                        matches!(
                            row.outcome,
                            Outcome::Ok | Outcome::Changed | Outcome::NotApplicable
                        )
                    })
                    .map(|row| row.id.clone()),
            );
            self.withheld.extend(result.withheld);
            result.rows
        } else {
            prepared.run(self.sys.as_ref(), mode)?
        };
        self.domains.clear();
        Ok({
            rows.into_iter()
                .map(|row| {
                    let status = match row.outcome {
                        Outcome::Ok | Outcome::NotApplicable => "ok",
                        Outcome::Changed => "applied",
                        Outcome::Planned => "drift",
                        Outcome::Person | Outcome::Blocked | Outcome::Deferred => "blocked",
                        Outcome::Unknown => "unverified",
                        Outcome::Failed => "error",
                    };
                    Row {
                        name: row.title,
                        status: status.into(),
                        detail: row
                            .why
                            .or_else(|| match (row.from, row.to) {
                                (Some(from), Some(to)) => Some(format!("{from} → {to}")),
                                (None, Some(to)) => Some(to),
                                (Some(from), None) => Some(from),
                                (None, None) => None,
                            })
                            .unwrap_or_else(|| "workstation item completed".into()),
                    }
                })
                .collect()
        })
    }

    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<Row> {
        let mut rows = Vec::new();
        if let Some(original) = saved.iter().find(|s| {
            !s.target.host
                && s.target.domain == "-g"
                && s.target.key == "com.apple.swipescrolldirection"
        }) {
            let value = match original.value.as_ref() {
                Some(PValue::Bool(value)) => Some(*value),
                Some(PValue::Int(0)) => Some(false),
                Some(PValue::Int(1)) => Some(true),
                _ => None,
            };
            if let Some(value) = value {
                let shown = |natural: bool| {
                    format!("natural scrolling {}", if natural { "on" } else { "off" })
                };
                let now = self
                    .sys
                    .natural_scrolling()
                    .map_or_else(|_| "natural scrolling as this setup left it".into(), shown);
                let id = "doctor.restore.natural-scrolling";
                if let Err(why) = self.journal(
                    id,
                    &now,
                    &format!("{}, as before this setup", shown(value)),
                    None,
                ) {
                    rows.push(journal_refused(&[format!("{id}: {why}")]));
                }
                let result = self.sys.set_natural_scrolling(value).and_then(|()| {
                    if self.sys.natural_scrolling()? == value {
                        Ok(())
                    } else {
                        Err("the original scroll direction did not activate".into())
                    }
                });
                rows.push(match result {
                    Ok(()) => Row {
                        name: "Original scroll direction".into(),
                        status: "restored".into(),
                        detail: "saved and active scroll direction restored".into(),
                    },
                    Err(detail) => Row {
                        name: "Original scroll direction".into(),
                        status: "error".into(),
                        detail,
                    },
                });
            } else {
                rows.push(Row { name: "Original active scroll direction".into(), status: "blocked".into(), detail: "the previous implicit or non-boolean setting has no recorded runtime value; log out and back in to load its restored preference".into() });
            }
        }
        if saved.iter().any(|s| {
            s.target.host
                && s.target
                    .key
                    .starts_with("com.apple.keyboard.modifiermapping.")
        }) {
            rows.push(Row { name: "Original active keyboard mapping".into(), status: "blocked".into(), detail: "the live HID service mappings from before this setup were not recorded, so log out and back in to load the saved mappings".into() });
        }
        rows
    }
}

fn journal_refused(refused: &[String]) -> Row {
    Row {
        name: "Change journal".into(),
        status: "error".into(),
        detail: format!(
            "the change journal refused these records; the changes went ahead anyway: {}",
            refused.join("; ")
        ),
    }
}

#[derive(Clone)]
struct Saved {
    target: Target,
    value: Option<PValue>,
    /// Whether the value was read with access to the target domain.
    granted: bool,
}

fn load_backup(path: &Path) -> Result<Vec<Saved>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes).map_err(|e| format!("backup: {e}"))?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized doctor backup profile".into());
    }
    let keys = value["keys"]
        .as_array()
        .ok_or("doctor backup has no keys")?;
    keys.iter()
        .map(|entry| {
            let domain = entry["domain"].as_str().ok_or("backup domain is missing")?;
            let key = entry["key"].as_str().ok_or("backup key is missing")?;
            let host = entry["host"].as_bool().ok_or("backup scope is missing")?;
            let target = Target {
                domain: domain.into(),
                key: key.into(),
                host,
            };
            let value = if entry["xml"].is_null() {
                None
            } else {
                Some(plist::parse(
                    entry["xml"]
                        .as_str()
                        .ok_or("backup XML is invalid")?
                        .as_bytes(),
                )?)
            };
            let granted = match entry.get("granted") {
                None => !contained(&target) || value.is_some(),
                Some(granted) => granted.as_bool().ok_or("backup grant is invalid")?,
            };
            Ok(Saved {
                target,
                value,
                granted,
            })
        })
        .collect()
}

fn save_backup(path: &Path, keys: &[Saved]) -> Result<(), String> {
    let rows: Vec<Value> = keys
        .iter()
        .map(|s| {
            json!({
                "domain": s.target.domain, "key": s.target.key, "host": s.target.host,
                "xml": s.value.as_ref().map(plist::document), "granted": s.granted,
            })
        })
        .collect();
    write_json(path, &json!({ "profile": PROFILE, "keys": rows }))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    write_json_with_flush(path, value, durable_name)
}

fn durable_directory(directory: &Path) -> std::io::Result<()> {
    use aterm_machines::port::System as _;
    let sys = aterm_machines::port::Live::new();
    sys.durable_dir(directory)
}

/// Publish `entry`, a name in `directory`: flush the directory, unless this process already
/// flushed it while it was the same directory and `entry` named the same file there. A record
/// written again is a new file (a rename over it), and a directory put in place of another is
/// a new directory, so either is flushed again; a directory's other names are not this flush's
/// business. A name moved away and back in between is the one change this cannot see, and
/// nothing moves a doctor record or its directories. Every doctor pass reads and writes its
/// records through here: flushing each one's whole chain to `/` every time made the passes
/// queue on the shared directories' locks.
fn durable_name(directory: &Path, entry: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    type Identity = (u64, u64);
    static PUBLISHED: std::sync::Mutex<BTreeMap<PathBuf, (Identity, Identity)>> =
        std::sync::Mutex::new(BTreeMap::new());
    let published = || {
        PUBLISHED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let identity = |path: &Path| {
        std::fs::symlink_metadata(path)
            .ok()
            .map(|meta| (meta.dev(), meta.ino()))
    };
    // Read before the flush: a file named after it is not one the flush published.
    let named = identity(directory).zip(identity(entry));
    if named.is_some() && published().get(entry) == named.as_ref() {
        return Ok(());
    }
    durable_directory(directory)?;
    if let Some(named) = named {
        published().insert(entry.to_path_buf(), named);
    }
    Ok(())
}

/// Flush every directory from `path`'s parent to `/`, each with the name in it on the way to
/// `path`.
fn flush_record_with(
    path: &Path,
    mut flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("doctor record has no parent directory")?;
    let mut entry = path;
    for directory in parent.ancestors() {
        flush(directory, entry).map_err(|e| format!("{}: {e}", directory.display()))?;
        entry = directory;
    }
    Ok(())
}

fn write_json_with_flush(
    path: &Path,
    value: &Value,
    flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("doctor record has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    let bytes = aterm_json::to_vec(value).map_err(|e| e.to_string())?;
    aterm_machines::port::write_atomic(path, &bytes, 0o600)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    flush_record_with(path, flush)
}

fn append_workstation(
    host: &mut dyn Host,
    group: WorkstationGroup,
    apply: bool,
    report: &mut Report,
) {
    if group.selected(host.selection()).is_empty() {
        return;
    }
    let name = match group {
        WorkstationGroup::Accounts => "Messages and FaceTime sign-out",
        WorkstationGroup::Preferences => "Workstation preferences",
        WorkstationGroup::InputActivation => "Active keyboard and trackpad settings",
        WorkstationGroup::All => "Workstation settings",
        WorkstationGroup::Withheld => "Settings waiting for Full Disk Access",
    };
    match host.workstation(group, apply) {
        Ok(rows) if rows.is_empty() => report.rows.push(Row {
            name: name.into(),
            status: "unverified".into(),
            detail: "setup returned no settings to verify".into(),
        }),
        Ok(rows) => report.rows.extend(rows),
        Err(detail) => report.rows.push(Row {
            name: name.into(),
            status: "unverified".into(),
            detail,
        }),
    }
}

fn execute(host: &mut dyn Host, root: &Path, action: Action) -> Result<Report, String> {
    execute_pass(
        host,
        root,
        action,
        &aterm_types::version::build_id(),
        epoch_secs(std::time::SystemTime::now()),
    )
    .map(|pass| pass.report)
}

struct Pass {
    report: Report,
    unverified: Vec<String>,
    settled: Vec<String>,
}

impl From<Report> for Pass {
    fn from(report: Report) -> Self {
        Self {
            report,
            unverified: Vec::new(),
            settled: Vec::new(),
        }
    }
}

fn execute_pass(
    host: &mut dyn Host,
    root: &Path,
    action: Action,
    build: &str,
    now: u64,
) -> Result<Pass, String> {
    let backup = root.join("original-preferences.json");
    let mut report = Report {
        action,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup,
    };
    if action == Action::Restore {
        return restore(host, root, report).map(Pass::from);
    }
    if host.selection().is_empty() {
        if action == Action::Apply {
            write_json(
                &root.join("first-run.json"),
                &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
            )?;
        }
        report.rows.push(Row {
            name: "Selected Mac settings".into(),
            status: "ok".into(),
            detail: "no settings selected; macOS was left unchanged".into(),
        });
        return Ok(report.into());
    }
    let full_profile = host.selection().workstation() == Selection::all().workstation();
    let previous_activation = if full_profile {
        pending_refresh(root)?
    } else {
        Vec::new()
    };
    if action == Action::Check {
        let (left, due): (Vec<&Target>, Vec<&Target>) = previous_activation
            .iter()
            .partition(|target| withheld(host, target).is_some());
        if !due.is_empty() {
            report.rows.push(Row {
                name: "Preference activation".into(),
                status: "drift".into(),
                detail: "a previous pass still needs activation; run aterm doctor --apply to retry"
                    .into(),
            });
        }
        if let Some(why) = left.first().and_then(|target| withheld(host, target)) {
            report.rows.push(Row {
                name: "Preference activation".into(),
                status: "blocked".into(),
                detail: format!(
                    "a previous pass's relaunch for {} waits: {why}",
                    left.iter()
                        .map(|target| target.describe())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            });
        }
        append_workstation(host, WorkstationGroup::All, false, &mut report);
        return Ok(report.into());
    }
    let (mut unverified, mut settled) = apply_preferences(
        host,
        root,
        &mut report,
        WorkstationGroup::Preferences,
        full_profile,
        previous_activation,
        (build, now),
    )?;
    append_workstation(host, WorkstationGroup::Accounts, true, &mut report);
    unverified.extend(
        host.attempts()
            .into_iter()
            .filter(|attempt| !attempt.verified)
            .map(|attempt| attempt.item),
    );
    settled.extend(host.settled());
    if !report.needs_attention() {
        write_json(
            &root.join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
        )?;
    }
    Ok(Pass {
        report,
        unverified,
        settled,
    })
}

fn apply_preferences(
    host: &mut dyn Host,
    root: &Path,
    report: &mut Report,
    group: WorkstationGroup,
    full_profile: bool,
    previous_activation: Vec<Target>,
    turn: (&str, u64),
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut saved = load_backup(&report.backup)?;
    let before = if group.selected(host.selection()).is_empty() {
        Vec::new()
    } else {
        host.preference_changes(group)?
    };
    for setting in &before {
        match saved.iter_mut().find(|s| s.target == setting.target) {
            None => saved.push(setting.clone()),
            Some(old) if !old.granted && setting.granted => *old = setting.clone(),
            Some(_) => {}
        }
    }
    if !before.is_empty() {
        save_backup(&report.backup, &saved)?;
    }
    if group == WorkstationGroup::Preferences {
        write_json(
            &root.join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "pending" }),
        )?;
    }
    append_workstation(host, group, true, report);
    let attempts = host.attempts();
    let settled = host.settled();
    let held = host.held();
    let ran = |item: &&String| !held.contains(item);
    let left_alone: Vec<String> = host.withheld().iter().filter(ran).cloned().collect();
    let reached: Vec<String> = settled.iter().filter(ran).cloned().collect();
    let unverified: Vec<String> = attempts
        .iter()
        .filter(|attempt| !attempt.verified)
        .map(|attempt| attempt.item.clone())
        .collect();
    let activated: Vec<Target> = attempts
        .iter()
        .filter(|attempt| attempt.verified)
        .flat_map(|attempt| attempt.preferences.iter().map(|saved| saved.target.clone()))
        .collect();
    let rollback: Vec<Saved> = attempts
        .into_iter()
        .filter(|attempt| !attempt.verified)
        .flat_map(|attempt| attempt.preferences)
        .collect();
    let mut reload: Vec<Target> = previous_activation
        .into_iter()
        .filter(|target| !activated.contains(target))
        .collect();
    let mut put_back = Vec::new();
    // The rollback's `restored` row, by its place in the report, and the keys it names.
    let mut restored_row: Option<(usize, Vec<String>)> = None;
    if !rollback.is_empty() {
        let mut errors = Vec::new();
        if full_profile {
            let mut record = reload.clone();
            for previous in &rollback {
                if !record.contains(&previous.target) {
                    record.push(previous.target.clone());
                }
            }
            if let Err(why) = save_pending_refresh(root, &record) {
                errors.push(format!("record the relaunch after it: {why}"));
            }
        }
        let mut restored = Vec::new();
        let mut refused = Vec::new();
        let mut left = Vec::new();
        for previous in rollback.iter().rev() {
            if let Some(why) =
                withheld(host, &previous.target).or_else(|| unknown_original(previous))
            {
                left.push(why);
                continue;
            }
            let wrote = match restore_one(
                host,
                &previous.target,
                previous.value.as_ref(),
                &mut refused,
            ) {
                Ok(wrote) => {
                    if wrote {
                        restored.push(previous.target.describe());
                        put_back.push((previous.target.clone(), previous.value.clone()));
                    }
                    wrote
                }
                Err((wrote, error)) => {
                    errors.push(format!("rollback {}: {error}", previous.target.describe()));
                    wrote
                }
            };
            if wrote && !reload.contains(&previous.target) {
                reload.push(previous.target.clone());
            }
        }
        if !restored.is_empty() {
            restored.reverse();
            restored_row = Some((report.rows.len(), restored.clone()));
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "restored".into(),
                detail: rolled_back(&restored),
            });
        }
        if !left.is_empty() {
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "blocked".into(),
                detail: format!("left as this pass made it: {}", left.join("; ")),
            });
        }
        if !refused.is_empty() {
            report.rows.push(journal_refused(&refused));
        }
        report.rows.extend(host.restore_runtime(&rollback));
        if !errors.is_empty() {
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "error".into(),
                detail: errors.join("; "),
            });
        }
    }
    if refresh(host, &reload.iter().collect::<Vec<_>>(), report) && full_profile {
        settle_pending_refresh(host, root)?;
    }
    // Read the rollback back once each reader is up again: one that wrote its own value
    // over the original as it started did not keep it, and is named as that alone, not
    // also as back.
    let lost = not_retained(host, &put_back);
    if !lost.is_empty() {
        if let Some((at, restored)) = restored_row {
            let kept: Vec<String> = restored
                .into_iter()
                .filter(|name| !lost.contains(name))
                .collect();
            if kept.is_empty() {
                report.rows.remove(at);
            } else {
                report.rows[at].detail = rolled_back(&kept);
            }
        }
        report.rows.push(Row {
            name: "Preference rollback".into(),
            status: "error".into(),
            detail: format!("{NOT_RETAINED}: {}", lost.join(", ")),
        });
    }
    if let Err(detail) = record_withheld(root, &left_alone, &reached, &unverified, turn) {
        report.rows.push(Row {
            name: "Settings waiting for Full Disk Access".into(),
            status: "error".into(),
            detail,
        });
    }
    Ok((unverified, settled))
}

const WITHHELD_RECORD: &str = "withheld-preferences.json";

/// What [`WITHHELD_RECORD`] holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Withheld {
    /// The items that left keys alone, in run order.
    items: Vec<String>,
    /// The tries of a change that reached their keys and did not verify, per item and build.
    failed: Vec<Failed>,
}

fn load_withheld(root: &Path) -> Result<Withheld, String> {
    let path = root.join(WITHHELD_RECORD);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Withheld::default()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let record: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("withheld preferences: {e}"))?;
    if record["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized withheld preferences profile".into());
    }
    let bad = || "withheld preferences are unreadable".to_owned();
    let items = record["items"]
        .as_array()
        .ok_or_else(bad)?
        .iter()
        .map(|item| item.as_str().map(str::to_owned).ok_or_else(bad))
        .collect::<Result<_, _>>()?;
    let mut failed = current_tries(load_tries(&record["failed"], &bad)?, true);
    if record["revision"].as_u64() != initial::revision_at(root).ok() {
        failed.clear();
    }
    Ok(Withheld {
        items: current_ids(items, true),
        failed,
    })
}

fn save_withheld(root: &Path, record: &Withheld) -> Result<(), String> {
    if !record.items.is_empty() {
        return write_json(
            &root.join(WITHHELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": initial::revision_at(root).ok(),
                "items": record.items, "failed": tries_json(&record.failed),
            }),
        );
    }
    match std::fs::remove_file(root.join(WITHHELD_RECORD)) {
        Ok(()) => durable_directory(root).map_err(|e| format!("{}: {e}", root.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear withheld preferences: {e}")),
    }
}

fn record_withheld(
    root: &Path,
    left_alone: &[String],
    reached: &[String],
    unverified: &[String],
    turn: (&str, u64),
) -> Result<(), String> {
    if left_alone.is_empty() && reached.is_empty() && unverified.is_empty() {
        return Ok(());
    }
    let before = load_withheld(root)?;
    let mut record = before.clone();
    record.items.retain(|item| !reached.contains(item));
    for item in left_alone {
        if !record.items.contains(item) {
            record.items.push(item.clone());
        }
    }
    let tried: Vec<String> = unverified
        .iter()
        .filter(|item| record.items.contains(item) && !left_alone.contains(item))
        .cloned()
        .collect();
    let (build, now) = turn;
    count_tries(&mut record.failed, build, now, &tried);
    let items = &record.items;
    record.failed.retain(|f| items.contains(&f.item));
    if record == before {
        return Ok(());
    }
    save_withheld(root, &record)
}

fn follow_up_at(
    root: &Path,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    let record = load_withheld(root)?;
    if record.items.is_empty() || initial::stopped(root)? {
        return Ok(None);
    }
    if host.full_disk_access() != Some(true) {
        return Ok(None);
    }
    let chosen = authorized_selection(root)?;
    let mut selected = Selection::none();
    for item in &record.items {
        if let Some(setting) = aterm_machines::init_preferences::setting_of(item)
            && chosen
                .is_none_or(|chosen| chosen.workstation().is_empty() || chosen.contains(setting))
        {
            selected.set(setting, true);
        }
    }
    if selected.is_empty() {
        save_withheld(root, &Withheld::default())?;
        return Ok(None);
    }
    let mut report = Report {
        action: Action::Apply,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
    };
    let now = epoch_secs(now);
    let selection = host.selection();
    host.select(selected);
    host.hold(&held_back(&record.failed, build, now));
    let pass = apply_preferences(
        host,
        root,
        &mut report,
        WorkstationGroup::Withheld,
        false,
        Vec::new(),
        (build, now),
    );
    host.select(selection);
    host.hold(&[]);
    pass?;
    Ok(Some(report))
}

/// Apply any container preferences withheld from earlier passes once Full Disk Access is granted.
pub fn finish_withheld_preferences<S>(
    reserve: impl FnOnce() -> S,
) -> Result<Option<Report>, String> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return Ok(None);
    }
    follow_up_finished_at(
        &state_root()?,
        reserve,
        || {
            ensure_person_scope()?;
            Ok(NativeHost::new())
        },
        &aterm_types::version::build_id(),
        std::time::SystemTime::now(),
    )
}

fn follow_up_finished_at<S, H: Host>(
    root: &Path,
    reserve: impl FnOnce() -> S,
    host: impl FnOnce() -> Result<H, String>,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    if load_withheld(root)?.items.is_empty() || initial::stopped(root)? {
        return Ok(None);
    }
    let mut host = host()?;
    if host.full_disk_access() != Some(true) {
        return Ok(None);
    }
    let _slot = reserve();
    let Some(_lock) = HeldLock::try_take(root)? else {
        return Ok(None);
    };
    follow_up_at(root, &mut host, build, now)
}

fn follow_up_locked_at(root: &Path) -> Result<Option<Report>, String> {
    if load_withheld(root)?.items.is_empty() {
        return Ok(None);
    }
    follow_up_at(
        root,
        &mut NativeHost::new(),
        &aterm_types::version::build_id(),
        std::time::SystemTime::now(),
    )
}

fn unknown_original(saved: &Saved) -> Option<String> {
    (contained(&saved.target) && !saved.granted && saved.value.is_none()).then(|| {
        format!(
            "{} was backed up while macOS hid it from aterm (before aterm had Full Disk Access), \
             so its original is unknown; it is left as it is",
            saved.target.describe()
        )
    })
}

fn contained(target: &Target) -> bool {
    aterm_machines::items::defaults::contained(&target.domain)
}

fn settle_pending_refresh(host: &dyn Host, root: &Path) -> Result<(), String> {
    let kept: Vec<Target> = pending_refresh(root)?
        .into_iter()
        .filter(|target| withheld(host, target).is_some())
        .collect();
    if kept.is_empty() {
        clear_pending_refresh(root)
    } else {
        save_pending_refresh(root, &kept)
    }
}

fn withheld(host: &dyn Host, target: &Target) -> Option<String> {
    aterm_machines::items::defaults::withheld(&target.domain, host.full_disk_access())
}

fn restore_waits(target: &Target) -> String {
    format!(
        "{} is in another app's container, which macOS opens only to a process whose app holds \
         Full Disk Access; its original stays in the backup: run Restore again from System State \
         or an aterm tab once aterm has Full Disk Access, aterm doctor --restore in another \
         terminal app that has it, or over SSH once Remote Login's \"Allow full disk access for \
         remote users\" is on (System Settings > General > Sharing)",
        target.describe()
    )
}

fn restore_one(
    host: &mut dyn Host,
    target: &Target,
    value: Option<&PValue>,
    refused: &mut Vec<String>,
) -> Result<bool, (bool, String)> {
    let observed = host.read(target).map_err(|why| (false, why))?;
    if same_optional(observed.as_ref(), value) {
        return Ok(false);
    }
    let id = restore_id(target);
    if let Err(why) = host.journal(
        &id,
        &shown(target, observed.as_ref()),
        &shown(target, value),
        Some(&defaults_cmd(target, observed.as_ref()).display()),
    ) {
        refused.push(format!("{id}: {why}"));
    }
    host.write(target, value).map_err(|why| (true, why))?;
    let back = host.read(target).map_err(|why| (true, why))?;
    if same_optional(back.as_ref(), value) {
        Ok(true)
    } else {
        Err((true, "macOS did not retain the original value".into()))
    }
}

fn restore_id(target: &Target) -> String {
    format!(
        "doctor.restore.{}{}.{}",
        if target.host { "currentHost." } else { "" },
        target.domain,
        target.key
    )
}

fn defaults_cmd(target: &Target, value: Option<&PValue>) -> Cmd {
    let mut cmd = Cmd::new(DEFAULTS);
    if target.host {
        cmd = cmd.arg("-currentHost");
    }
    match value {
        Some(value) => cmd
            .args(["write", target.domain.as_str(), target.key.as_str()])
            .arg(plist::fragment(value)),
        None => cmd.args(["delete", target.domain.as_str(), target.key.as_str()]),
    }
}

fn shown(target: &Target, value: Option<&PValue>) -> String {
    format!(
        "{} = {}",
        target.describe(),
        value.map_or_else(|| "absent".into(), ToString::to_string)
    )
}

fn same_optional(a: Option<&PValue>, b: Option<&PValue>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => same_original(a, b),
        _ => false,
    }
}

fn same_original(a: &PValue, b: &PValue) -> bool {
    match (a, b) {
        (PValue::Array(a), PValue::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_original(a, b))
        }
        (PValue::Dict(a), PValue::Dict(b)) => {
            a.len() == b.len()
                && a.iter().all(|(key, value)| {
                    b.iter()
                        .find(|(k, _)| k == key)
                        .is_some_and(|(_, other)| same_original(value, other))
                })
        }
        _ => a == b,
    }
}

fn restore(host: &mut dyn Host, root: &Path, mut report: Report) -> Result<Report, String> {
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "restored" }),
    )?;
    save_withheld(root, &Withheld::default())?;
    let saved = load_backup(&report.backup)?;
    let mut activation = pending_refresh(root)?;
    for saved in &saved {
        if withheld(host, &saved.target).is_some() || unknown_original(saved).is_some() {
            continue;
        }
        if !activation.contains(&saved.target) {
            activation.push(saved.target.clone());
        }
    }
    save_pending_refresh(root, &activation)?;
    if saved.is_empty() {
        report.rows.push(Row {
            name: "Original preferences".into(),
            status: "ok".into(),
            detail: "no changed preferences were backed up".into(),
        });
    }
    let mut refused = Vec::new();
    let mut put_back = Vec::new();
    for item in saved.iter().rev() {
        let why = unknown_original(item)
            .or_else(|| withheld(host, &item.target).map(|_| restore_waits(&item.target)));
        if let Some(why) = why {
            report.rows.push(Row {
                name: item.target.describe(),
                status: "blocked".into(),
                detail: why,
            });
            continue;
        }
        match restore_one(host, &item.target, item.value.as_ref(), &mut refused) {
            Ok(wrote) => {
                if wrote {
                    put_back.push((item.target.clone(), item.value.clone()));
                }
                report.rows.push(Row {
                    name: item.target.describe(),
                    status: "restored".into(),
                    detail: "original value restored".into(),
                });
            }
            Err((_, detail)) => report.rows.push(Row {
                name: item.target.describe(),
                status: "error".into(),
                detail,
            }),
        }
    }
    if !refused.is_empty() {
        report.rows.push(journal_refused(&refused));
    }
    if refresh(host, &activation.iter().collect::<Vec<_>>(), &mut report) {
        settle_pending_refresh(host, root)?;
    }
    // Read back once each reader is up again: a reader that wrote its own value over the
    // original as it started did not keep it.
    for name in not_retained(host, &put_back) {
        if let Some(row) = report
            .rows
            .iter_mut()
            .find(|row| row.name == name && row.status == "restored")
        {
            row.status = "error".into();
            row.detail = NOT_RETAINED.into();
        }
    }
    report.rows.extend(host.restore_runtime(&saved));
    Ok(report)
}

fn pending_refresh(root: &Path) -> Result<Vec<Target>, String> {
    let path = root.join("pending-refresh.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value =
        aterm_json::from_slice(&bytes).map_err(|e| format!("pending activation: {e}"))?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized pending activation profile".into());
    }
    value["targets"]
        .as_array()
        .ok_or("pending activation has no targets")?
        .iter()
        .map(|entry| {
            Ok(Target {
                domain: entry["domain"]
                    .as_str()
                    .ok_or("pending activation domain is missing")?
                    .into(),
                key: entry["key"]
                    .as_str()
                    .ok_or("pending activation key is missing")?
                    .into(),
                host: entry["host"]
                    .as_bool()
                    .ok_or("pending activation scope is missing")?,
            })
        })
        .collect()
}

fn save_pending_refresh(root: &Path, targets: &[Target]) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    let targets: Vec<Value> = targets
        .iter()
        .map(|t| json!({ "domain": t.domain, "key": t.key, "host": t.host }))
        .collect();
    write_json(
        &root.join("pending-refresh.json"),
        &json!({ "profile": PROFILE, "targets": targets }),
    )
}

fn clear_pending_refresh(root: &Path) -> Result<(), String> {
    match std::fs::remove_file(root.join("pending-refresh.json")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("clear pending activation: {e}")),
    }
}

/// This process's own account, whose processes the doctor relaunches: its name from the
/// account database (its uid, which `killall -u` also takes, when none names it).
fn own_account(uid: u32) -> aterm_machines::model::User {
    aterm_machines::model::User {
        name: aterm_uds::account::user_name(uid).unwrap_or_else(|| uid.to_string()),
        uid,
        gid: 20,
        home: aterm_uds::account::home_of(uid)
            .map(|home| home.to_string_lossy().into_owned())
            .unwrap_or_default(),
        shell: String::new(),
        admin: false,
    }
}

/// Of `put_back` (each key and the value a rollback or a Restore wrote back), the keys
/// that no longer read that value once their readers were relaunched and up again
/// ([`refresh`]): a reader that wrote its own over it as it started.
fn not_retained(host: &mut dyn Host, put_back: &[(Target, Option<PValue>)]) -> Vec<String> {
    put_back
        .iter()
        .filter(|(target, value)| {
            host.read(target)
                .map_or(true, |now| !same_optional(now.as_ref(), value.as_ref()))
        })
        .map(|(target, _)| target.describe())
        .collect()
}

/// What the rollback's `restored` row says of the keys `restored` names.
fn rolled_back(restored: &[String]) -> String {
    format!(
        "the value before this pass is back for what did not verify: {}",
        restored.join(", ")
    )
}

/// What a key that did not keep the value put back says.
const NOT_RETAINED: &str =
    "written back, but its reader, once relaunched, no longer reads the original value";

fn refresh(host: &mut dyn Host, targets: &[&Target], report: &mut Report) -> bool {
    let targets: Vec<&Target> = targets
        .iter()
        .copied()
        .filter(|target| withheld(&*host, target).is_none())
        .collect();
    let mut refreshed = true;
    let mut refused = Vec::new();
    for process in aterm_machines::init_activation::RELAUNCHED {
        let reads: Vec<&Target> = targets
            .iter()
            .copied()
            .filter(|t| aterm_machines::init_activation::readers(&t.domain).contains(&process))
            .collect();
        if reads.is_empty() {
            continue;
        }
        let id = format!("doctor.relaunch.{process}");
        if let Err(why) = host.journal(
            &id,
            &format!("{process} running with the preferences it read at its start"),
            &format!("{process} relaunched to read its saved preferences"),
            None,
        ) {
            refused.push(format!("{id}: {why}"));
        }
        if let Err(detail) = host.restart(process, &reads) {
            refreshed = false;
            report.rows.push(Row {
                name: format!("Reload {process}"),
                status: "error".into(),
                detail,
            });
        }
    }
    if targets
        .iter()
        .any(|t| t.host || t.domain.contains("Trackpad") || t.domain.contains("trackpad"))
    {
        report.rows.push(Row { name: "Keyboard and trackpad activation".into(), status: "ok".into(), detail: "native preferences saved; log out and back in if an already connected device still uses its previous setting".into() });
    }
    if !refused.is_empty() {
        report.rows.push(journal_refused(&refused));
    }
    refreshed
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_machines::port::Ran;

    /// Fake doctor host backed by in-memory [`wanted`] keys.
    #[derive(Default)]
    struct Fake {
        selected: Option<Selection>,
        preference_passes: usize,
        values: BTreeMap<Target, PValue>,
        writes: Vec<Target>,
        /// The first item applied reports `error` after its write, once.
        fail_once: bool,
        /// Items whose read-back never verifies.
        failing: Vec<Target>,
        /// Items whose consumer writes the value before the change back over it.
        written_back: Vec<Target>,
        /// Items that stand blocked (a grant not given, a read that does not answer).
        blocked: Vec<Target>,
        accounts_blocked: bool,
        /// The account sign-out's apply runs and its read-back never verifies.
        accounts_fail: bool,
        fail_restart_once: bool,
        /// The journal refuses every line of the doctor's own (`doctor.…`).
        journal_refuses: bool,
        /// An apply pass of the preference group panics, as a process that dies there.
        dies_in_preferences: bool,
        /// An apply pass of the preference group panics once its items wrote.
        dies_after_item_writes: bool,
        /// The doctor's own writes (a rollback, a Restore) panic.
        dies_in_doctor_writes: bool,
        restarts: Vec<String>,
        account_passes: usize,
        attempts: Vec<Attempt>,
        settled: Vec<String>,
        held: Vec<(String, Hold)>,
        /// Every journal line, write and relaunch, in order.
        events: Vec<String>,
        /// Each journal line's id, from, to and revert.
        lines: Vec<(String, String, String, Option<String>)>,
        runtime_changed: bool,
        /// What the pass measured of its Full Disk Access.
        full_disk_access: Option<bool>,
        /// Whether the widgets row also clears Notification Center's widget list.
        notification_center: bool,
        /// The Window Manager's widget key is of Notification Center's item: an item that
        /// leaves one key for want of the grant and reaches another, as init's widgets row
        /// did before each catalog row became its own item, whose tries the doctor counts
        /// whichever pass makes them.
        bundles: bool,
        withheld: Vec<String>,
    }

    impl Fake {
        fn item(&self, target: &Target) -> String {
            if contained(target) || (self.bundles && target.domain == "com.apple.WindowManager") {
                WIDGETS.into()
            } else {
                target.describe()
            }
        }

        fn items(&self) -> Vec<(String, Vec<(Target, PValue)>)> {
            let mut items: Vec<(String, Vec<(Target, PValue)>)> = Vec::new();
            for (target, value) in self.wants() {
                let item = self.item(&target);
                match items.iter_mut().find(|(id, _)| *id == item) {
                    Some((_, keys)) => keys.push((target, value)),
                    None => items.push((item, vec![(target, value)])),
                }
            }
            items
        }

        fn setting(target: &Target) -> Setting {
            if target.domain == "com.apple.dock" {
                Setting::Dock
            } else {
                Setting::Widgets
            }
        }

        fn relaunches(&self, process: &str) -> usize {
            let event = format!("relaunch {process}");
            self.events.iter().filter(|e| **e == event).count()
        }

        fn wants(&self) -> Vec<(Target, PValue)> {
            let mut keys = wanted();
            if self.notification_center {
                keys.push((nc_widgets(), cleared()));
            }
            keys
        }

        fn reaches(&self, target: &Target) -> bool {
            !contained(target) || self.full_disk_access == Some(true)
        }

        fn changes(&self) -> usize {
            self.events
                .iter()
                .filter(|event| event.starts_with("write ") || event.starts_with("restart "))
                .count()
        }

        fn put(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
            self.writes.push(target.clone());
            self.events.push(format!("write {}", target.describe()));
            match value {
                Some(value) => {
                    self.values.insert(target.clone(), value.clone());
                }
                None => {
                    self.values.remove(target);
                }
            }
            Ok(())
        }

        fn hold_of(&self, item: &str) -> Option<Hold> {
            self.held
                .iter()
                .find(|(held, _)| held == item)
                .map(|(_, hold)| hold.clone())
        }
    }

    pub(super) fn restore_without_effects(root: &Path) -> Result<Report, String> {
        let mut host = Fake {
            runtime_changed: true,
            ..Fake::default()
        };
        let result = execute(&mut host, root, Action::Restore);
        assert!(host.writes.is_empty());
        assert!(host.restarts.is_empty());
        assert!(host.runtime_changed, "Restore reached runtime activation");
        result
    }

    /// Test harness where the Dock change never verifies.
    pub(super) struct NeverVerifies {
        host: Fake,
        now: std::time::SystemTime,
    }

    impl NeverVerifies {
        pub(super) fn dock() -> Self {
            Self {
                host: Fake {
                    failing: vec![wanted()[0].0.clone()],
                    ..Fake::default()
                },
                now: std::time::SystemTime::now(),
            }
        }

        /// What `apply_on_first_run_at` runs once the stage is due.
        pub(super) fn pass(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.now += std::time::Duration::from_secs(61);
            apply_on_first_run_with(
                root,
                &Inert::default(),
                &mut self.host,
                "1.0.0+gtest",
                self.now,
            )
        }

        /// Preference writes and relaunches so far.
        pub(super) fn changes(&self) -> usize {
            self.host.changes()
        }

        /// Relaunches so far.
        pub(super) fn relaunches(&self) -> usize {
            self.host.restarts.len()
        }
    }

    /// Test harness for Notification Center widget withholding and follow-up.
    pub(super) struct Withholding {
        host: Fake,
        now: std::time::SystemTime,
        build: &'static str,
    }

    impl Withholding {
        pub(super) fn widgets() -> Self {
            Self {
                host: Fake {
                    notification_center: true,
                    values: BTreeMap::from([(nc_widgets(), populated())]),
                    ..Fake::default()
                },
                now: std::time::SystemTime::now(),
                build: "1.0.0+gtest",
            }
        }

        /// [`Withholding::widgets`] with the Dock selected too, its change never verifying (the
        /// Dock wrote its own tiles back over it), so the Workstation stage stays pending.
        pub(super) fn widgets_and_a_dock_that_never_verifies() -> Self {
            let mut doctor = Self::widgets();
            doctor.host.failing = vec![wanted()[0].0.clone()];
            doctor
        }

        /// [`Withholding::widgets`] whose first change does not verify.
        pub(super) fn widgets_whose_first_try_does_not_verify() -> Self {
            let mut doctor = Self::widgets();
            doctor.host.bundles = true;
            doctor.host.fail_once = true;
            doctor
        }

        pub(super) fn pass(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.now += std::time::Duration::from_secs(61);
            apply_on_first_run_with(
                root,
                &Inert::default(),
                &mut self.host,
                self.build,
                self.now,
            )
        }

        pub(super) fn follow_up(&mut self, root: &Path) -> Result<Option<Report>, String> {
            follow_up_at(root, &mut self.host, self.build, self.now)
        }

        fn wait(&mut self, by: std::time::Duration) {
            self.now += by;
        }

        /// The time of the last pass, which its follow-up shares.
        pub(super) fn now(&self) -> std::time::SystemTime {
            self.now
        }

        /// macOS gives aterm Full Disk Access.
        pub(super) fn grant(&mut self) {
            self.host.full_disk_access = Some(true);
        }

        /// Notification Center never keeps a clear of its widget list (it is not up again in time).
        pub(super) fn notification_center_never_keeps(&mut self) {
            self.host.failing.push(nc_widgets());
        }

        /// The person shows their desktop widgets again.
        pub(super) fn show_desktop_widgets(&mut self) {
            self.host
                .values
                .insert(wanted()[1].0.clone(), PValue::Bool(false));
        }

        /// Whether the desktop widgets show, as the person set them.
        pub(super) fn desktop_widgets_shown(&self) -> bool {
            self.host
                .values
                .get(&wanted()[1].0)
                .is_some_and(|now| now.same(&PValue::Bool(false)))
        }

        /// Writes to the Window Manager's widget key so far.
        pub(super) fn window_manager_writes(&self) -> usize {
            self.host
                .writes
                .iter()
                .filter(|target| **target == wanted()[1].0)
                .count()
        }

        /// Relaunches of the Dock so far.
        pub(super) fn dock_relaunches(&self) -> usize {
            self.host.relaunches("Dock")
                + self.host.restarts.iter().filter(|p| *p == "Dock").count()
        }

        /// Relaunches of Notification Center so far.
        pub(super) fn notification_center_relaunches(&self) -> usize {
            self.host.relaunches("NotificationCenter")
                + self
                    .host
                    .restarts
                    .iter()
                    .filter(|p| *p == "NotificationCenter")
                    .count()
        }

        /// What the record of withheld items names.
        pub(super) fn withheld(root: &Path) -> Vec<String> {
            load_withheld(root).unwrap().items
        }

        /// Writes to Notification Center's widget list so far.
        pub(super) fn notification_center_writes(&self) -> usize {
            self.host
                .writes
                .iter()
                .filter(|target| contained(target))
                .count()
        }

        /// Whether Notification Center's widget list holds no widget.
        pub(super) fn notification_center_cleared(&self) -> bool {
            self.host
                .values
                .get(&nc_widgets())
                .is_some_and(|now| now.same(&cleared()))
        }
    }

    /// A [`System`] that runs nothing ([`NativeHost`]).
    #[derive(Default)]
    pub(super) struct Inert {
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// What it answers for this process's Full Disk Access.
        full_disk_access: Option<bool>,
        /// What it answers for the commands a test models, and which pids it says are alive.
        model: Option<std::sync::Arc<dyn Model>>,
    }

    /// The commands and processes a test models over an [`Inert`] system.
    pub(super) trait Model: Send + Sync {
        /// Its answer to `cmd`; `None` refuses it, as the inert system does.
        fn answer(&self, cmd: &Cmd) -> Option<Ran>;
        /// Whether `pid` is alive.
        fn alive(&self, pid: u32) -> bool;
    }

    struct InertJournal(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

    impl aterm_converge::Journal for InertJournal {
        fn before_change(
            &mut self,
            id: &str,
            _from: &str,
            _to: &str,
            _revert: Option<&str>,
        ) -> Result<(), String> {
            self.0.lock().unwrap().push(id.into());
            Ok(())
        }
    }

    mod inert {
        use super::Inert;
        use aterm_machines::port::{Cmd, Ran, RunFailed, Stat, System, User};
        use std::io;
        use std::path::Path;

        fn unsupported<T>() -> io::Result<T> {
            Err(io::ErrorKind::Unsupported.into())
        }

        impl System for Inert {
            fn run(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
                self.calls.lock().unwrap().push(cmd.display());
                if let Some(ran) = self.model.as_ref().and_then(|model| model.answer(cmd)) {
                    return Ok(ran);
                }
                Err(RunFailed::Refused(
                    "an inert test system runs nothing".into(),
                ))
            }
            fn run_complete_stdout(&self, cmd: &Cmd) -> Result<Ran, RunFailed> {
                self.run(cmd)
            }
            fn pid_alive(&self, pid: u32) -> bool {
                self.model.as_ref().is_some_and(|model| model.alive(pid))
            }
            fn run_tty(&self, cmd: &Cmd) -> Result<Option<i32>, RunFailed> {
                self.run(cmd).map(|ran| ran.status)
            }
            fn read_file(&self, _: &Path) -> io::Result<Vec<u8>> {
                unsupported()
            }
            fn write_file(&self, _: &Path, _: &[u8], _: u32) -> io::Result<()> {
                unsupported()
            }
            fn write_user_file(&self, _: &User, _: &Path, _: &[u8], _: u32) -> io::Result<()> {
                unsupported()
            }
            fn remove(&self, _: &Path) -> io::Result<()> {
                unsupported()
            }
            fn create_dir_all(&self, _: &Path, _: u32) -> io::Result<()> {
                unsupported()
            }
            fn chown(&self, _: &Path, _: u32, _: u32) -> io::Result<()> {
                unsupported()
            }
            fn stat(&self, _: &Path) -> io::Result<Stat> {
                unsupported()
            }
            fn list_dir(&self, _: &Path) -> io::Result<Vec<String>> {
                unsupported()
            }
            fn sha256(&self, _: &Path) -> io::Result<String> {
                unsupported()
            }
            fn copy_file(&self, _: &Path, _: &Path, _: u32) -> io::Result<()> {
                unsupported()
            }
            fn rename(&self, _: &Path, _: &Path) -> io::Result<()> {
                unsupported()
            }
            fn euid(&self) -> u32 {
                501
            }
            fn now(&self) -> u64 {
                0
            }
            fn full_disk_access(&self) -> Option<bool> {
                self.full_disk_access
            }
            fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + '_>, String> {
                Ok(Box::new(super::InertJournal(std::sync::Arc::clone(
                    &self.lines,
                ))))
            }
        }
    }

    const NC: &str = "com.apple.notificationcenterui";

    /// The shared widgets item's id.
    pub(super) const WIDGETS: &str = "consumer.widgets.notification-center@alice";

    fn touches_notification_center(call: &str) -> bool {
        call.contains(NC) || call.contains("NotificationCenter")
    }

    fn inert_host(
        full_disk_access: Option<bool>,
        items: fn() -> Vec<Box<dyn aterm_machines::items::Item>>,
    ) -> (NativeHost, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let inert = Inert {
            full_disk_access,
            ..Inert::default()
        };
        let calls = std::sync::Arc::clone(&inert.calls);
        let host = NativeHost::over(
            Box::new(inert),
            Box::new(move |_, group, _| {
                Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                    items()
                } else {
                    Vec::new()
                }))
            }),
        );
        (host, calls)
    }

    #[test]
    fn the_doctors_host_neither_reads_nor_writes_a_container_without_the_grant() {
        let widgets = Target {
            domain: NC.into(),
            key: "widgets".into(),
            host: false,
        };
        for unknown in [None, Some(false)] {
            let (mut host, calls) = inert_host(unknown, Vec::new);
            let read = host.read(&widgets).unwrap_err();
            let write = host.write(&widgets, None).unwrap_err();
            assert!(read.contains("Full Disk Access"), "{read}");
            assert!(write.contains("Full Disk Access"), "{write}");
            assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock());
        }
        let (mut host, calls) = inert_host(Some(true), Vec::new);
        assert!(
            host.read(&widgets).is_err(),
            "the inert system runs nothing"
        );
        assert_eq!(
            *calls.lock().unwrap(),
            [format!("{DEFAULTS} export {NC} -")]
        );
    }

    #[test]
    fn restore_leaves_a_container_before_image_alone_without_the_grant() {
        let saved = [
            Saved {
                target: Target {
                    domain: NC.into(),
                    key: "widgets".into(),
                    host: false,
                },
                value: Some(PValue::Dict(vec![(
                    "instances".into(),
                    PValue::Array(vec![PValue::Data(vec![42])]),
                )])),
                granted: true,
            },
            Saved {
                target: Target::new("com.apple.WindowManager", "StandardHideWidgets"),
                value: Some(PValue::Bool(false)),
                granted: true,
            },
        ];
        for full_disk_access in [None, Some(false), Some(true)] {
            let root = aterm_tempfile::tempdir().expect("scratch");
            save_backup(&root.path().join("original-preferences.json"), &saved).unwrap();
            let (mut host, calls) = inert_host(full_disk_access, Vec::new);
            let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
            let calls = calls.lock().unwrap().clone();
            assert!(
                calls
                    .iter()
                    .any(|call| call.contains("export com.apple.WindowManager")),
                "{calls:?}"
            );
            assert!(
                calls.iter().any(|call| call.ends_with(" Dock")),
                "{calls:?}"
            );
            let row = report
                .rows
                .iter()
                .find(|row| row.name == format!("{NC} widgets"))
                .unwrap();
            if full_disk_access == Some(true) {
                assert_eq!(row.status, "error", "the inert system runs nothing");
                assert!(
                    calls.contains(&format!("{DEFAULTS} export {NC} -")),
                    "{calls:?}"
                );
                assert!(
                    calls
                        .iter()
                        .any(|call| call.ends_with(" NotificationCenter")),
                    "{calls:?}"
                );
            } else {
                assert_eq!(
                    (row.status.as_str(), row.detail.as_str()),
                    ("blocked", restore_waits(&saved[0].target).as_str()),
                    "{}",
                    report.to_text()
                );
                assert!(
                    !calls.iter().any(|call| touches_notification_center(call)),
                    "{calls:?}"
                );
                assert_eq!(
                    load_backup(&root.path().join("original-preferences.json"))
                        .unwrap()
                        .len(),
                    2,
                    "the backup keeps it"
                );
            }
        }
    }

    fn setup_records(root: &Path) -> BTreeMap<String, Vec<u8>> {
        std::fs::read_dir(root)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.file_type().unwrap().is_file())
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    std::fs::read(entry.path()).unwrap(),
                )
            })
            .filter(|(name, _)| {
                !["original-preferences.json", WITHHELD_RECORD].contains(&name.as_str())
            })
            .collect()
    }

    /// A Dock under management: once relaunched it takes a second to start, then shows the
    /// pins it is given whatever was written, as the 2026-10-21 managed Dock did. `defaults`
    /// reads and writes its one domain, and `pgrep`, `launchctl print`, `killall` and
    /// `sleep` act on it.
    #[derive(Default)]
    struct ManagedDock(std::sync::Mutex<ManagedState>);

    #[derive(Default)]
    struct ManagedState {
        prefs: Vec<(String, PValue)>,
        pid: u32,
        /// The sleeps left until a relaunched Dock is past its start.
        starting: Option<u32>,
        kills: usize,
    }

    /// One pin of `label`, with the GUID a Dock gives a tile it loaded.
    fn loaded_pin(label: &str, guid: i64) -> PValue {
        PValue::Dict(vec![
            ("GUID".into(), PValue::Int(guid)),
            (
                "tile-data".into(),
                PValue::Dict(vec![("file-label".into(), PValue::Str(label.into()))]),
            ),
        ])
    }

    /// The pins management gives the Dock.
    fn managed_pins() -> PValue {
        PValue::Array(vec![loaded_pin("Managed", 7)])
    }

    impl Model for ManagedDock {
        fn answer(&self, cmd: &Cmd) -> Option<Ran> {
            let args: Vec<String> = cmd
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let mut dock = self.0.lock().unwrap();
            match (cmd.program.to_str()?, args.as_slice()) {
                (DEFAULTS, ["export", "com.apple.dock", "-"]) => {
                    let entries = dock.prefs.clone();
                    Some(Ran::ok(&plist::document(&PValue::Dict(entries))))
                }
                (DEFAULTS, ["write", "com.apple.dock", key, fragment]) => {
                    let value = plist::parse(fragment.as_bytes()).ok()?;
                    dock.prefs.retain(|(k, _)| k != key);
                    dock.prefs.push(((*key).into(), value));
                    Some(Ran::ok(""))
                }
                ("/usr/bin/pgrep", ["-u", _, "-x", "Dock"]) => {
                    Some(Ran::ok(&format!("{}\n", dock.pid)))
                }
                ("/bin/launchctl", ["print", _]) => Some(Ran::ok("")),
                ("/usr/bin/killall", ["-u", _, "Dock"]) => {
                    dock.pid += 1;
                    dock.starting = Some(1);
                    dock.kills += 1;
                    Some(Ran::ok(""))
                }
                ("/bin/sleep", ["1"]) => {
                    if let Some(left) = dock.starting.as_mut() {
                        *left = left.saturating_sub(1);
                        if *left == 0 {
                            dock.starting = None;
                            dock.prefs.retain(|(k, _)| k != "persistent-apps");
                            dock.prefs.push(("persistent-apps".into(), managed_pins()));
                        }
                    }
                    Some(Ran::ok(""))
                }
                _ => None,
            }
        }

        fn alive(&self, pid: u32) -> bool {
            self.0.lock().unwrap().pid == pid
        }
    }

    /// The doctor's relaunch after it puts a value back waits, as the shared items' own
    /// does, until the relaunched reader runs again and what it reads has settled, so the
    /// read-back after it reads what the reader kept: a managed Dock that shows its own pins
    /// over the original a second into its start is reported as not keeping it. Negative
    /// control: a read right after the bare kill the doctor's relaunch was races the Dock's
    /// start and reads the original back, as if it were kept.
    #[test]
    fn a_restores_read_back_waits_for_the_relaunched_reader_to_settle() {
        let pins = Target::new("com.apple.dock", "persistent-apps");
        let original = PValue::Array(vec![loaded_pin("Mail", 1)]);
        let dock = std::sync::Arc::new(ManagedDock::default());
        {
            let mut state = dock.0.lock().unwrap();
            state.pid = 100;
            state.prefs.push((
                "persistent-apps".into(),
                PValue::Array(vec![loaded_pin("aterm", 2)]),
            ));
        }
        let root = aterm_tempfile::tempdir().expect("scratch");
        save_backup(
            &root.path().join("original-preferences.json"),
            &[Saved {
                target: pins.clone(),
                value: Some(original.clone()),
                granted: true,
            }],
        )
        .unwrap();
        let inert = Inert {
            full_disk_access: Some(true),
            model: Some(std::sync::Arc::clone(&dock) as std::sync::Arc<dyn Model>),
            ..Inert::default()
        };
        let mut host = NativeHost::over(
            Box::new(inert),
            Box::new(|_, _, _| Ok(Workstation::of(Vec::new()))),
        );
        let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
        let row = report
            .rows
            .iter()
            .find(|row| row.name == pins.describe())
            .unwrap();
        assert_eq!(
            (row.status.as_str(), row.detail.as_str()),
            ("error", NOT_RETAINED),
            "{}",
            report.to_text()
        );
        assert!(
            !report.rows.iter().any(|row| row.name == "Reload Dock"),
            "{}",
            report.to_text()
        );
        assert_eq!(dock.0.lock().unwrap().kills, 1);

        let racing = ManagedDock::default();
        racing.0.lock().unwrap().pid = 100;
        let write = defaults_cmd(&pins, Some(&original));
        racing.answer(&write).unwrap();
        let kill = Cmd::new("/usr/bin/killall").args(["-u", "501", "Dock"]);
        racing.answer(&kill).unwrap();
        let export = Cmd::new(DEFAULTS).args(["export", "com.apple.dock", "-"]);
        let read = plist::parse_domain(&racing.answer(&export).unwrap().stdout).unwrap();
        assert_eq!(read, [("persistent-apps".to_owned(), Ok(original))]);
    }

    /// A shared item that pins the Dock and never verifies, whose before-image of the
    /// Dock's pins, `.0`, the doctor's rollback puts back.
    struct Repinned(PValue);

    impl aterm_machines::items::Item for Repinned {
        fn id(&self) -> String {
            "init.test.repinned".into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            "init.test.repinned".into()
        }
        fn probe(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> aterm_machines::model::Probe {
            aterm_machines::model::Probe::Change {
                from: "a".into(),
                to: "b".into(),
            }
        }
        fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
            Ok(())
        }
        fn preference_changes(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> Result<
            Vec<(
                aterm_machines::items::defaults::Target,
                aterm_machines::model::Read<PValue>,
            )>,
            String,
        > {
            let alice = aterm_machines::model::User {
                name: "alice".into(),
                uid: 501,
                gid: 20,
                home: "/Users//alice".into(),
                shell: "/bin/zsh".into(),
                admin: false,
            };
            Ok(vec![(
                aterm_machines::items::defaults::Target::user(
                    "com.apple.dock",
                    "persistent-apps",
                    &alice,
                ),
                aterm_machines::model::Read::Value(self.0.clone()),
            )])
        }
    }

    /// The rollback of a change that did not verify reads each key it put back once its
    /// reader is up again: a managed Dock that shows its own pins over the original a second
    /// into its start is reported, in one `error` row, as not keeping them, and the pins
    /// are not also listed as back. Negative control: without that read-back the report
    /// says the pins are back and nothing more.
    #[test]
    fn a_rollback_its_relaunched_reader_did_not_keep_is_reported_once_as_not_kept() {
        let pins = Target::new("com.apple.dock", "persistent-apps");
        let original = PValue::Array(vec![loaded_pin("Mail", 1)]);
        let dock = std::sync::Arc::new(ManagedDock::default());
        {
            let mut state = dock.0.lock().unwrap();
            state.pid = 100;
            state.prefs.push((
                "persistent-apps".into(),
                PValue::Array(vec![loaded_pin("aterm", 2)]),
            ));
        }
        let root = aterm_tempfile::tempdir().expect("scratch");
        let inert = Inert {
            full_disk_access: Some(true),
            model: Some(std::sync::Arc::clone(&dock) as std::sync::Arc<dyn Model>),
            ..Inert::default()
        };
        let before = original.clone();
        let mut host = NativeHost::over(
            Box::new(inert),
            Box::new(move |_, group, _| {
                Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                    vec![Box::new(Repinned(before.clone())) as Box<dyn aterm_machines::items::Item>]
                } else {
                    Vec::new()
                }))
            }),
        );
        let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
        let rollback: Vec<(&str, &str)> = report
            .rows
            .iter()
            .filter(|row| row.name == "Preference rollback")
            .map(|row| (row.status.as_str(), row.detail.as_str()))
            .collect();
        let lost = format!("{NOT_RETAINED}: {}", pins.describe());
        assert_eq!(rollback, [("error", lost.as_str())], "{}", report.to_text());
        assert_eq!(dock.0.lock().unwrap().kills, 1, "{}", report.to_text());
    }

    #[test]
    fn notification_center_waits_for_the_grant_and_is_cleared_once_silently_when_it_arrives() {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);

        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), widgets).unwrap();
        let mut doctor = Withholding::widgets();
        let report = doctor
            .pass(root.path())
            .unwrap()
            .expect("the setup pass runs");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(doctor.notification_center_writes(), 0);
        assert!(!doctor.notification_center_cleared());
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert!(
            Automatic::load(root.path(), initial::revision_at(root.path()).unwrap())
                .unwrap()
                .settled
                .contains(&WIDGETS.to_owned()),
            "settled for the keys it reached"
        );
        doctor.show_desktop_widgets();
        let docks = doctor.dock_relaunches();
        for unknown in [None, Some(false), None] {
            doctor.host.full_disk_access = unknown;
            assert!(doctor.follow_up(root.path()).unwrap().is_none());
        }
        assert_eq!(doctor.notification_center_writes(), 0);
        let records = setup_records(root.path());

        doctor.grant();
        let follow_up = doctor
            .follow_up(root.path())
            .unwrap()
            .expect("the grant is held");
        assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
        assert!(doctor.notification_center_cleared());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert_eq!(doctor.notification_center_relaunches(), 1);
        assert_eq!(doctor.dock_relaunches(), docks);
        assert!(
            doctor.desktop_widgets_shown(),
            "the person's change stays theirs"
        );
        assert_eq!(setup_records(root.path()), records);
        assert!(Withholding::withheld(root.path()).is_empty());
        let backup = load_backup(&root.path().join("original-preferences.json")).unwrap();
        let saved = backup.iter().find(|s| s.target == nc_widgets()).unwrap();
        assert!(saved.granted && saved.value.as_ref().is_some_and(|v| v.same(&populated())));
        assert!(doctor.follow_up(root.path()).unwrap().is_none(), "once");
        assert_eq!(doctor.notification_center_writes(), 1);

        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), widgets).unwrap();
        let mut doctor = Withholding::widgets();
        doctor.grant();
        doctor
            .pass(root.path())
            .unwrap()
            .expect("the setup pass runs");
        assert!(doctor.notification_center_cleared());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert!(Withholding::withheld(root.path()).is_empty());
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
    }

    /// aterm 0.118.0 and older recorded their one widgets item, `init.widgets@<user>`: for the
    /// follow-up, which with the grant still reads it as Notification Center's row, clears
    /// that list once and drops the record; and as settled, which holds each of the three
    /// widget rows as it is. Negative control: loading the follow-up record's ids as written
    /// runs no follow-up, and drops the record with the list as it was.
    #[test]
    fn a_follow_up_an_older_aterm_recorded_still_clears_notification_center() {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), widgets).unwrap();
        record_withheld(
            root.path(),
            &["init.widgets@alice".to_owned()],
            &[],
            &[],
            (BUILD, 0),
        )
        .unwrap();
        let mut doctor = Withholding::widgets();
        doctor.grant();
        let follow_up = doctor
            .follow_up(root.path())
            .unwrap()
            .expect("the grant is held");
        assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
        assert!(doctor.notification_center_cleared());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert!(Withholding::withheld(root.path()).is_empty());
        let revision = initial::revision_at(root.path()).unwrap();
        write_json(
            &root.path().join(HELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": revision,
                "settled": ["init.widgets@alice"], "failed": [],
            }),
        )
        .unwrap();
        assert_eq!(
            Automatic::load(root.path(), revision).unwrap().settled,
            [
                "consumer.widgets.desktop@alice",
                "consumer.widgets.stage-manager@alice",
                WIDGETS,
            ]
        );
    }

    #[test]
    fn the_follow_up_record_keeps_an_item_that_ran_but_left_a_key_alone_again() {
        let root = aterm_tempfile::tempdir().unwrap();
        let ids = |names: &[&str]| -> Vec<String> {
            names.iter().map(|name| (*name).to_owned()).collect()
        };
        let turn = (BUILD, 60);
        record_withheld(root.path(), &ids(&["a", "b"]), &[], &[], turn).unwrap();
        record_withheld(
            root.path(),
            &ids(&["a", "c"]),
            &ids(&["a", "b"]),
            &ids(&["a"]),
            turn,
        )
        .unwrap();
        let record = load_withheld(root.path()).unwrap();
        assert_eq!(record.items, ["a", "c"]);
        assert!(record.failed.is_empty(), "{record:?}");
        record_withheld(root.path(), &[], &[], &ids(&["c", "d"]), turn).unwrap();
        let record = load_withheld(root.path()).unwrap();
        assert_eq!(record.items, ["a", "c"]);
        assert_eq!(
            held_back(&record.failed, BUILD, 61),
            [("c".to_owned(), Hold::Back(WAITING.into()))]
        );
        record_withheld(root.path(), &[], &ids(&["a", "c"]), &[], turn).unwrap();
        assert!(!root.path().join(WITHHELD_RECORD).exists());
    }

    #[test]
    fn a_retry_with_the_grant_leaves_the_settled_row_and_the_follow_up_reaches_what_it_left() {
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        for granted in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_automatic_setup_at(root.path(), selected).unwrap();
            let mut doctor = Withholding::widgets_and_a_dock_that_never_verifies();
            doctor
                .pass(root.path())
                .unwrap()
                .expect("the setup pass runs");
            assert_eq!(doctor.notification_center_writes(), 0);
            if granted {
                doctor.grant();
            }
            doctor.pass(root.path()).unwrap().expect("the retry runs");
            assert_eq!(doctor.notification_center_writes(), 0);
            assert_eq!(doctor.window_manager_writes(), 1);
            assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
            assert_eq!(doctor.follow_up(root.path()).unwrap().is_some(), granted);
            assert_eq!(doctor.notification_center_cleared(), granted);
            assert_eq!(doctor.notification_center_writes(), usize::from(granted));
            assert_eq!(doctor.window_manager_writes(), 1);
            assert_eq!(Withholding::withheld(root.path()).is_empty(), granted);
        }
    }

    #[test]
    fn a_follow_up_that_does_not_verify_stays_recorded_and_is_tried_twice_per_build() {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), widgets).unwrap();
        let mut doctor = Withholding::widgets();
        doctor
            .pass(root.path())
            .unwrap()
            .expect("the setup pass runs");
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        doctor.grant();
        doctor.host.failing = vec![nc_widgets()];
        let tried = |doctor: &mut Withholding, root: &Path| {
            let report = doctor.follow_up(root).unwrap().expect("the follow-up runs");
            assert!(report.needs_attention(), "{}", report.to_text());
            assert!(!doctor.notification_center_cleared(), "rolled back");
            assert_eq!(Withholding::withheld(root), [WIDGETS]);
        };
        let held = |doctor: &mut Withholding, root: &Path, why: &str| {
            let events = doctor.host.events.len();
            let report = doctor.follow_up(root).unwrap().expect("the follow-up runs");
            let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
            assert_eq!((row.status.as_str(), row.detail.as_str()), ("blocked", why));
            assert!(
                doctor.host.events[events..].is_empty(),
                "nothing written, relaunched or journaled: {:?}",
                &doctor.host.events[events..]
            );
            assert_eq!(Withholding::withheld(root), [WIDGETS]);
        };
        tried(&mut doctor, root.path());
        assert_eq!(
            doctor.notification_center_writes(),
            2,
            "the try and its rollback"
        );
        doctor.wait(std::time::Duration::from_secs(5 * 60));
        held(&mut doctor, root.path(), WAITING);
        doctor.wait(SECOND_TRY_AFTER);
        tried(&mut doctor, root.path());
        assert_eq!(doctor.notification_center_writes(), 4);
        for _ in 0..3 {
            doctor.wait(SECOND_TRY_AFTER);
            held(&mut doctor, root.path(), HELD);
        }
        doctor.build = "1.0.1+gtest";
        doctor.host.failing.clear();
        let report = doctor
            .follow_up(root.path())
            .unwrap()
            .expect("the follow-up runs");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
    }

    #[test]
    fn a_finished_setups_follow_up_holds_the_busy_slot_while_it_may_hold_the_profile_lock() {
        use std::cell::{Cell, RefCell};
        struct Slot<'a> {
            root: &'a Path,
            released: &'a RefCell<Vec<(bool, bool)>>,
        }
        impl Drop for Slot<'_> {
            fn drop(&mut self) {
                let free = HeldLock::try_take(self.root).is_ok_and(|lock| lock.is_some());
                let ran = load_withheld(self.root).is_ok_and(|record| record.items.is_empty());
                self.released.borrow_mut().push((free, ran));
            }
        }
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(root.path(), widgets).unwrap();
        let taken = RefCell::new(Vec::new());
        let released = RefCell::new(Vec::new());
        let reserve = || {
            taken
                .borrow_mut()
                .push(HeldLock::try_take(root.path()).is_ok_and(|lock| lock.is_some()));
            Slot {
                root: root.path(),
                released: &released,
            }
        };
        let built = Cell::new(0);
        let probed = RefCell::new(Vec::new());
        let host = |full_disk_access| {
            built.set(built.get() + 1);
            probed.borrow_mut().push((
                taken.borrow().len(),
                HeldLock::try_take(root.path()).is_ok_and(|lock| lock.is_some()),
            ));
            Ok(Fake {
                notification_center: true,
                values: BTreeMap::from([(nc_widgets(), populated())]),
                full_disk_access,
                ..Fake::default()
            })
        };
        let now = std::time::SystemTime::now();
        assert!(
            follow_up_finished_at(root.path(), reserve, || host(Some(true)), BUILD, now)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            (taken.borrow().len(), built.get()),
            (0, 0),
            "nothing owed: no slot, no grant measured"
        );
        record_withheld(root.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
        for full_disk_access in [None, Some(false)] {
            assert!(
                follow_up_finished_at(root.path(), reserve, || host(full_disk_access), BUILD, now)
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(taken.borrow().len(), 0, "no grant: no slot");
        let report = follow_up_finished_at(root.path(), reserve, || host(Some(true)), BUILD, now)
            .unwrap()
            .expect("the follow-up runs");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(*taken.borrow(), [true], "the slot, before the lock");
        assert_eq!(
            *probed.borrow(),
            [(0, true); 3],
            "the grant measured before the slot and the lock"
        );
        assert_eq!(
            *released.borrow(),
            [(true, true)],
            "the slot, after the follow-up and its lock"
        );
        record_withheld(root.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
        cancel_automatic_setup_at(root.path()).unwrap();
        let unbuilt = || -> Result<Fake, String> { panic!("a stopped setup measures nothing") };
        assert!(
            follow_up_finished_at(root.path(), reserve, unbuilt, BUILD, now)
                .unwrap()
                .is_none()
        );
        assert_eq!(taken.borrow().len(), 1, "a stopped setup takes no slot");
        assert_eq!(
            Withholding::withheld(root.path()),
            [WIDGETS],
            "the record stays"
        );
        let done = aterm_tempfile::tempdir().unwrap();
        write_json(
            &done.path().join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
        )
        .unwrap();
        assert!(first_run_complete(done.path()).unwrap());
        record_withheld(done.path(), &[WIDGETS.to_owned()], &[], &[], (BUILD, 0)).unwrap();
        let report = follow_up_finished_at(done.path(), || (), || host(Some(true)), BUILD, now)
            .unwrap()
            .expect("a completed setup's follow-up runs");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(Withholding::withheld(done.path()).is_empty());
    }

    #[test]
    fn nothing_is_followed_up_after_setup_was_restored_or_cancelled() {
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        let mut dock = Selection::none();
        dock.set(Setting::Dock, true);
        for stop in ["restore", "cancel", "unchecked", "none"] {
            let root = aterm_tempfile::tempdir().unwrap();
            authorize_automatic_setup_at(root.path(), selected).unwrap();
            let mut doctor = Withholding::widgets();
            doctor.host.failing = vec![wanted()[0].0.clone()];
            doctor
                .pass(root.path())
                .unwrap()
                .expect("the setup pass runs");
            assert!(!first_run_complete(root.path()).unwrap());
            match stop {
                "restore" => {
                    execute(&mut doctor.host, root.path(), Action::Restore).unwrap();
                    assert!(Withholding::withheld(root.path()).is_empty());
                }
                "cancel" => cancel_automatic_setup_at(root.path()).unwrap(),
                "unchecked" => save_automatic_plan(root.path(), dock).unwrap(),
                _ => {}
            }
            doctor.grant();
            let follow_up = doctor.follow_up(root.path()).unwrap();
            assert_eq!(follow_up.is_some(), stop == "none", "{stop}");
            assert_eq!(
                doctor.notification_center_cleared(),
                stop == "none",
                "{stop}"
            );
        }
    }

    #[test]
    fn a_restore_never_deletes_what_a_read_without_the_grant_saved_as_absent() {
        let backup_of = |widgets: Value| {
            json!({ "profile": PROFILE, "keys": [
                widgets,
                { "domain": "com.apple.WindowManager", "key": "StandardHideWidgets",
                  "host": false, "xml": plist::document(&PValue::Bool(false)) },
            ] })
        };
        let legacy =
            |xml: Value| json!({ "domain": NC, "key": "widgets", "host": false, "xml": xml });
        let root = aterm_tempfile::tempdir().unwrap();
        let backup = root.path().join("original-preferences.json");
        write_json(
            &backup,
            &backup_of(legacy(json!(plist::document(&populated())))),
        )
        .unwrap();
        assert!(
            load_backup(&backup)
                .unwrap()
                .iter()
                .all(|saved| saved.granted)
        );
        for (granted, full_disk_access) in [
            (false, None),
            (false, Some(false)),
            (false, Some(true)),
            (true, Some(true)),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let backup = root.path().join("original-preferences.json");
            let widgets = if granted {
                json!({ "domain": NC, "key": "widgets", "host": false, "xml": null, "granted": true })
            } else {
                legacy(Value::Null)
            };
            write_json(&backup, &backup_of(widgets)).unwrap();
            let saved = load_backup(&backup).unwrap();
            assert_eq!(saved[0].granted, granted);
            assert!(saved[1].granted);
            let (mut host, calls) = inert_host(full_disk_access, Vec::new);
            let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
            let calls = calls.lock().unwrap().clone();
            assert!(
                calls
                    .iter()
                    .any(|call| call.contains("export com.apple.WindowManager")),
                "{calls:?}"
            );
            let row = report
                .rows
                .iter()
                .find(|row| row.name == format!("{NC} widgets"))
                .unwrap();
            if granted {
                assert_eq!(row.status, "error", "the inert system runs nothing");
                assert!(
                    calls.iter().any(|call| touches_notification_center(call)),
                    "{calls:?}"
                );
            } else {
                assert_eq!(
                    (row.status.as_str(), Some(row.detail.clone())),
                    ("blocked", unknown_original(&saved[0])),
                    "{full_disk_access:?}"
                );
                assert!(
                    !calls.iter().any(|call| touches_notification_center(call)),
                    "{calls:?}"
                );
            }
        }
    }

    #[test]
    fn the_first_read_with_the_grant_replaces_a_before_image_the_denial_wrote() {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let other = PValue::Dict(vec![("vers".into(), PValue::Int(2))]);
        for (old, original) in [
            (
                Saved {
                    target: nc_widgets(),
                    value: None,
                    granted: false,
                },
                populated(),
            ),
            (
                Saved {
                    target: nc_widgets(),
                    value: Some(other.clone()),
                    granted: true,
                },
                other.clone(),
            ),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let backup = root.path().join("original-preferences.json");
            save_backup(&backup, &[old]).unwrap();
            let mut host = Fake {
                selected: Some(widgets),
                notification_center: true,
                full_disk_access: Some(true),
                values: BTreeMap::from([(nc_widgets(), populated())]),
                ..Fake::default()
            };
            let report = execute(&mut host, root.path(), Action::Apply).unwrap();
            assert!(!report.needs_attention(), "{}", report.to_text());
            assert!(
                host.values
                    .get(&nc_widgets())
                    .is_some_and(|now| now.same(&cleared()))
            );
            let saved = load_backup(&backup).unwrap();
            let nc = saved.iter().find(|s| s.target == nc_widgets()).unwrap();
            assert!(nc.granted);
            assert!(nc.value.as_ref().is_some_and(|v| v.same(&original)));
            execute(&mut host, root.path(), Action::Restore).unwrap();
            assert!(
                host.values
                    .get(&nc_widgets())
                    .is_some_and(|now| now.same(&original))
            );
        }
    }

    #[test]
    fn a_granted_absence_is_kept_by_the_backup_and_restored_by_deleting_the_key() {
        let mut widgets = Selection::none();
        widgets.set(Setting::Widgets, true);
        let root = aterm_tempfile::tempdir().unwrap();
        let backup = root.path().join("original-preferences.json");
        save_backup(
            &backup,
            &[Saved {
                target: nc_widgets(),
                value: None,
                granted: true,
            }],
        )
        .unwrap();
        let mut host = Fake {
            selected: Some(widgets),
            notification_center: true,
            full_disk_access: Some(true),
            values: BTreeMap::from([(nc_widgets(), populated())]),
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        let saved = load_backup(&backup).unwrap();
        let nc = saved.iter().find(|s| s.target == nc_widgets()).unwrap();
        assert!(
            nc.granted && nc.value.is_none(),
            "the recorded absence stays"
        );
        let report = execute(&mut host, root.path(), Action::Restore).unwrap();
        let row = report
            .rows
            .iter()
            .find(|row| row.name == nc_widgets().describe())
            .unwrap();
        assert_eq!(row.status, "restored", "{}", report.to_text());
        assert!(!host.values.contains_key(&nc_widgets()), "deleted");
    }

    #[test]
    fn a_notification_center_relaunch_owed_waits_for_the_grant_instead_of_being_dropped() {
        let activation_rows = |report: &Report| -> Vec<String> {
            report
                .rows
                .iter()
                .filter(|row| row.name == "Preference activation")
                .map(|row| row.status.clone())
                .collect()
        };
        for unknown in [None, Some(false)] {
            let root = aterm_tempfile::tempdir().unwrap();
            save_pending_refresh(root.path(), &[nc_widgets()]).unwrap();
            let mut host = Fake {
                full_disk_access: unknown,
                ..Fake::default()
            };
            let check = execute(&mut host, root.path(), Action::Check).unwrap();
            assert_eq!(activation_rows(&check), ["blocked"], "{}", check.to_text());
            execute(&mut host, root.path(), Action::Apply).unwrap();
            execute(&mut host, root.path(), Action::Restore).unwrap();
            assert!(
                !host.restarts.iter().any(|p| p == "NotificationCenter"),
                "{:?}",
                host.restarts
            );
            assert_eq!(pending_refresh(root.path()).unwrap(), [nc_widgets()]);

            host.full_disk_access = Some(true);
            let check = execute(&mut host, root.path(), Action::Check).unwrap();
            assert_eq!(activation_rows(&check), ["drift"], "{}", check.to_text());
            execute(&mut host, root.path(), Action::Apply).unwrap();
            assert_eq!(
                host.restarts
                    .iter()
                    .filter(|p| *p == "NotificationCenter")
                    .count(),
                1,
                "{:?}",
                host.restarts
            );
            assert!(pending_refresh(root.path()).unwrap().is_empty());
        }
    }

    struct Contained;

    impl aterm_machines::items::Item for Contained {
        fn id(&self) -> String {
            "init.test.contained".into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            "init.test.contained".into()
        }
        fn probe(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> aterm_machines::model::Probe {
            aterm_machines::model::Probe::Change {
                from: "a".into(),
                to: "b".into(),
            }
        }
        fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
            Ok(())
        }
        fn preference_changes(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> Result<
            Vec<(
                aterm_machines::items::defaults::Target,
                aterm_machines::model::Read<PValue>,
            )>,
            String,
        > {
            let alice = aterm_machines::model::User {
                name: "alice".into(),
                uid: 501,
                gid: 20,
                home: "/Users//alice".into(),
                shell: "/bin/zsh".into(),
                admin: false,
            };
            Ok(vec![(
                aterm_machines::items::defaults::Target::user(NC, "widgets", &alice),
                aterm_machines::model::Read::Absent,
            )])
        }
    }

    struct LeftAlone(bool);

    impl aterm_machines::items::Item for LeftAlone {
        fn id(&self) -> String {
            "init.test.left-alone".into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            "init.test.left-alone".into()
        }
        fn probe(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> aterm_machines::model::Probe {
            aterm_machines::model::Probe::Ok { now: "b".into() }
        }
        fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
            Ok(())
        }
        fn withheld(&self) -> Option<String> {
            self.0.then(|| "waits for Full Disk Access".into())
        }
    }

    #[test]
    fn a_row_that_left_a_key_alone_is_settled_and_recorded_for_the_follow_up() {
        let item = "init.test.left-alone".to_owned();
        for left_alone in [true, false] {
            let root = aterm_tempfile::tempdir().expect("scratch");
            let items: fn() -> Vec<Box<dyn aterm_machines::items::Item>> = if left_alone {
                || vec![Box::new(LeftAlone(true))]
            } else {
                || vec![Box::new(LeftAlone(false))]
            };
            let (mut host, _) = inert_host(None, items);
            let pass =
                execute_pass(&mut host, root.path(), Action::Apply, BUILD, 0).expect("apply");
            let row = pass
                .report
                .rows
                .iter()
                .find(|row| row.name == item)
                .unwrap();
            assert_eq!(row.status, "ok", "{}", pass.report.to_text());
            assert!(pass.settled.contains(&item));
            assert_eq!(
                load_withheld(root.path()).unwrap().items.contains(&item),
                left_alone
            );
        }
    }

    /// Fake widgets item with separate Window Manager and Notification Center state.
    #[derive(Clone, Default)]
    struct Bundled {
        hidden: std::sync::Arc<std::sync::atomic::AtomicBool>,
        applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        granted: std::sync::Arc<std::sync::atomic::AtomicBool>,
        cleared: std::sync::Arc<std::sync::atomic::AtomicBool>,
        contained_only: bool,
    }

    impl Bundled {
        fn on(flag: &std::sync::atomic::AtomicBool) -> bool {
            flag.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl aterm_machines::items::Item for Bundled {
        fn id(&self) -> String {
            WIDGETS.into()
        }
        fn family(&self) -> &'static str {
            "widgets"
        }
        fn title(&self) -> String {
            WIDGETS.into()
        }
        fn probe(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> aterm_machines::model::Probe {
            use aterm_machines::model::Probe;
            let mut from = Vec::new();
            if !self.contained_only && !Self::on(&self.hidden) {
                from.push("com.apple.WindowManager StandardHideWidgets".to_owned());
            }
            if Self::on(&self.granted) && !Self::on(&self.cleared) {
                from.push(format!("{NC} widgets"));
            }
            if from.is_empty() {
                Probe::Ok {
                    now: "hidden".into(),
                }
            } else {
                Probe::Change {
                    from: from.join(", "),
                    to: "hidden".into(),
                }
            }
        }
        fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
            if !self.contained_only {
                self.applies
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.hidden.store(true, std::sync::atomic::Ordering::SeqCst);
            }
            if Self::on(&self.granted) {
                self.cleared
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(())
        }
        fn withheld(&self) -> Option<String> {
            (!Self::on(&self.granted))
                .then(|| "Notification Center's wait for Full Disk Access".into())
        }
    }

    #[test]
    fn a_withheld_row_is_left_as_the_person_set_it_by_every_later_automatic_pass() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let widgets = Bundled::default();
        let never = Scripted::new("init.test.never", &[false]);
        let items = (widgets.clone(), never.clone());
        let mut host = NativeHost::over(
            Box::new(Inert::default()),
            Box::new(move |_, group, _| {
                Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                    vec![
                        Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                        Box::new(items.1.clone()),
                    ]
                } else {
                    Vec::new()
                }))
            }),
        );
        let tick = |host: &mut NativeHost, minute: u64| -> Report {
            apply_on_first_run_with(root.path(), &Inert::default(), host, BUILD, at(minute))
                .expect("tick")
                .expect("setup still runs")
        };
        tick(&mut host, 0);
        assert_eq!(widgets.applies.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
        widgets
            .hidden
            .store(false, std::sync::atomic::Ordering::SeqCst);
        for minute in 1..30 {
            let report = tick(&mut host, minute);
            let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
            assert_eq!(row.status, "ok", "minute {minute}: {}", report.to_text());
            assert!(row.detail.ends_with(SETTLED), "{}", row.detail);
        }
        assert_eq!(
            widgets.applies.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the person's change stays theirs"
        );
        assert!(!widgets.hidden.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(
            never.applies(),
            2,
            "the Dock's two tries, and setup still retries"
        );
        assert!(!first_run_complete(root.path()).unwrap());
        assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
    }

    #[test]
    fn the_grant_after_a_withheld_row_settled_leaves_it_owed_to_the_follow_up_and_says_so() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let widgets = Bundled::default();
        let never = Scripted::new("init.test.never", &[false]);
        let host = |full_disk_access: Option<bool>| {
            let items = (widgets.clone(), never.clone());
            NativeHost::over(
                Box::new(Inert {
                    full_disk_access,
                    ..Inert::default()
                }),
                Box::new(move |_, group, _| {
                    Ok(Workstation::of(match group {
                        WorkstationGroup::Preferences => vec![
                            Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                            Box::new(items.1.clone()),
                        ],
                        WorkstationGroup::Withheld => vec![Box::new(Bundled {
                            contained_only: true,
                            ..items.0.clone()
                        })],
                        _ => Vec::new(),
                    }))
                }),
            )
        };
        let row = |full_disk_access: Option<bool>, minute: u64| -> (String, String) {
            let report = apply_on_first_run_with(
                root.path(),
                &Inert::default(),
                &mut host(full_disk_access),
                BUILD,
                at(minute),
            )
            .expect("tick")
            .expect("setup still runs");
            let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
            (row.status.clone(), row.detail.clone())
        };
        let applies = || widgets.applies.load(std::sync::atomic::Ordering::SeqCst);
        let window_manager = "com.apple.WindowManager StandardHideWidgets";
        let notification_center = format!("{NC} widgets");

        row(None, 0);
        assert_eq!(applies(), 1);
        assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
        widgets
            .granted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            row(Some(true), 1),
            (
                "ok".into(),
                format!("{notification_center}; left as it is: {SETTLED_OWED}")
            )
        );
        assert_eq!(
            load_withheld(root.path()).unwrap().items,
            [WIDGETS],
            "a held row is not reached: the follow-up still owes Notification Center"
        );
        widgets
            .hidden
            .store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            row(Some(true), 2),
            (
                "ok".into(),
                format!("{window_manager}, {notification_center}; left as it is: {SETTLED_OWED}")
            )
        );
        assert_eq!(applies(), 1, "the person's change stays theirs");

        let follow_up = follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(3))
            .unwrap()
            .expect("the grant is held");
        assert!(!follow_up.needs_attention(), "{}", follow_up.to_text());
        assert!(Bundled::on(&widgets.cleared));
        assert!(!Bundled::on(&widgets.hidden));
        assert!(load_withheld(root.path()).unwrap().items.is_empty());
        assert_eq!(
            row(Some(true), 4),
            (
                "ok".into(),
                format!("{window_manager}; left as it is: {SETTLED}")
            )
        );
        assert_eq!(applies(), 1);
        assert_eq!(
            never.applies(),
            1,
            "the Dock's first try; its second is not due"
        );
    }

    #[test]
    fn a_pass_without_the_grant_never_records_again_a_row_it_held() {
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        selected.set(Setting::Dock, true);
        for cleared_by in ["a pass with the grant", "the follow-up"] {
            let root = aterm_tempfile::tempdir().expect("scratch");
            authorize_automatic_setup_at(root.path(), selected).unwrap();
            let widgets = Bundled::default();
            let never = Scripted::new("init.test.never", &[false]);
            let host = |full_disk_access: Option<bool>| {
                widgets.granted.store(
                    full_disk_access == Some(true),
                    std::sync::atomic::Ordering::SeqCst,
                );
                let items = (widgets.clone(), never.clone());
                NativeHost::over(
                    Box::new(Inert {
                        full_disk_access,
                        ..Inert::default()
                    }),
                    Box::new(move |_, group, _| {
                        Ok(Workstation::of(match group {
                            WorkstationGroup::Preferences => vec![
                                Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                                Box::new(items.1.clone()),
                            ],
                            WorkstationGroup::Withheld => vec![Box::new(Bundled {
                                contained_only: true,
                                ..items.0.clone()
                            })],
                            _ => Vec::new(),
                        }))
                    }),
                )
            };
            let tick = |full_disk_access: Option<bool>, build: &str, minute: u64| {
                apply_on_first_run_with(
                    root.path(),
                    &Inert::default(),
                    &mut host(full_disk_access),
                    build,
                    at(minute),
                )
                .expect("tick")
                .expect("setup still runs")
            };
            if cleared_by == "the follow-up" {
                tick(None, BUILD, 0);
                assert_eq!(load_withheld(root.path()).unwrap().items, [WIDGETS]);
                follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(1))
                    .unwrap()
                    .expect("the grant is held");
            } else {
                tick(Some(true), BUILD, 0);
            }
            assert!(Bundled::on(&widgets.cleared), "{cleared_by}");
            assert!(load_withheld(root.path()).unwrap().items.is_empty());
            widgets
                .cleared
                .store(false, std::sync::atomic::Ordering::SeqCst);

            let report = tick(None, "1.0.1+gtest", 2);
            let row = report.rows.iter().find(|row| row.name == WIDGETS).unwrap();
            assert_eq!(row.status, "ok", "{cleared_by}: {}", report.to_text());
            assert!(
                load_withheld(root.path()).unwrap().items.is_empty(),
                "{cleared_by}: a held row is not recorded again"
            );
            assert!(
                follow_up_at(root.path(), &mut host(Some(true)), BUILD, at(3))
                    .unwrap()
                    .is_none(),
                "{cleared_by}"
            );
            tick(Some(true), BUILD, 4);
            assert!(
                !Bundled::on(&widgets.cleared),
                "{cleared_by}: the person's widget stays theirs"
            );
            assert_eq!(widgets.applies.load(std::sync::atomic::Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn the_rollback_leaves_a_container_before_image_alone_without_the_grant() {
        for full_disk_access in [None, Some(false), Some(true)] {
            let root = aterm_tempfile::tempdir().expect("scratch");
            let (mut host, calls) = inert_host(full_disk_access, || vec![Box::new(Contained)]);
            let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
            let item = report
                .rows
                .iter()
                .find(|row| row.name == "init.test.contained")
                .unwrap();
            assert_eq!(item.status, "error", "{}", report.to_text());
            let calls = calls.lock().unwrap().clone();
            if full_disk_access == Some(true) {
                assert!(
                    calls.contains(&format!("{DEFAULTS} export {NC} -")),
                    "{calls:?}"
                );
            } else {
                assert!(
                    report
                        .rows
                        .iter()
                        .any(|row| row.name == "Preference rollback"
                            && row.status == "blocked"
                            && row.detail.contains("Full Disk Access")),
                    "{}",
                    report.to_text()
                );
                assert!(
                    !calls.iter().any(|call| touches_notification_center(call)),
                    "{calls:?}"
                );
            }
        }
    }

    /// A shared item whose probes answer from a list (the last one repeats), counting its applies.
    #[derive(Clone)]
    struct Scripted {
        id: &'static str,
        probes: std::sync::Arc<std::sync::Mutex<Vec<aterm_machines::model::Probe>>>,
        applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Scripted {
        fn new(id: &'static str, wanted: &[bool]) -> Self {
            use aterm_machines::model::Probe;
            let probes = wanted
                .iter()
                .map(|wanted| {
                    if *wanted {
                        Probe::Ok { now: "b".into() }
                    } else {
                        Probe::Change {
                            from: "a".into(),
                            to: "b".into(),
                        }
                    }
                })
                .collect();
            Self {
                id,
                probes: std::sync::Arc::new(std::sync::Mutex::new(probes)),
                applies: std::sync::Arc::default(),
            }
        }

        fn applies(&self) -> usize {
            self.applies.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl aterm_machines::items::Item for Scripted {
        fn id(&self) -> String {
            self.id.into()
        }
        fn family(&self) -> &'static str {
            "test"
        }
        fn title(&self) -> String {
            self.id.into()
        }
        fn probe(
            &self,
            _: &dyn System,
            _: &aterm_machines::items::Ctx,
        ) -> aterm_machines::model::Probe {
            let mut probes = self.probes.lock().unwrap();
            if probes.len() > 1 {
                probes.remove(0)
            } else {
                probes[0].clone()
            }
        }
        fn apply(&self, _: &dyn System, _: &aterm_machines::items::Ctx) -> Result<(), String> {
            self.applies
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn the_worker_holds_through_the_doctors_own_host() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut selected = Selection::none();
        selected.set(Setting::Dock, true);
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        let never = Scripted::new("init.test.never", &[false]);
        let drifts = Scripted::new("init.test.drifts", &[false, true, false]);
        let applies = || (never.applies(), drifts.applies());
        let inert = Inert::default();
        let (calls, lines) = (
            std::sync::Arc::clone(&inert.calls),
            std::sync::Arc::clone(&inert.lines),
        );
        let items = (never.clone(), drifts.clone());
        let mut host = NativeHost::over(
            Box::new(inert),
            Box::new(move |_, group, _| {
                Ok(Workstation::of(if group == WorkstationGroup::Preferences {
                    vec![
                        Box::new(items.0.clone()) as Box<dyn aterm_machines::items::Item>,
                        Box::new(items.1.clone()),
                    ]
                } else {
                    Vec::new()
                }))
            }),
        );
        let tick = |host: &mut NativeHost, minute: u64, build: &str| -> Report {
            apply_on_first_run_with(root.path(), &Inert::default(), host, build, at(minute))
                .expect("tick")
                .expect("setup still runs")
        };
        let row = |report: &Report, name: &str| {
            let row = report.rows.iter().find(|row| row.name == name).unwrap();
            (row.status.clone(), row.detail.clone())
        };
        let first = tick(&mut host, 0, BUILD);
        assert_eq!(
            row(&first, "init.test.never").0,
            "error",
            "{}",
            first.to_text()
        );
        assert_eq!(row(&first, "init.test.drifts").0, "applied");
        assert_eq!(applies(), (1, 1));
        let quiet = |host: &mut NativeHost, minutes: std::ops::Range<u64>, why: &str| {
            for minute in minutes {
                let report = tick(host, minute, BUILD);
                assert_eq!(
                    row(&report, "init.test.never"),
                    ("blocked".into(), format!("a; not changed: {why}")),
                    "minute {minute}"
                );
                assert_eq!(
                    row(&report, "init.test.drifts"),
                    ("ok".into(), format!("a; left as it is: {SETTLED}")),
                    "minute {minute}"
                );
            }
        };
        quiet(&mut host, 1..10, WAITING);
        assert_eq!(applies(), (1, 1));
        tick(&mut host, 10, BUILD);
        assert_eq!(applies(), (2, 1), "the second try");
        quiet(&mut host, 11..20, HELD);
        assert_eq!(applies(), (2, 1));
        tick(&mut host, 21, "1.0.1+gbbbb");
        assert_eq!(
            applies(),
            (3, 1),
            "a new build's try, and the setting left as it is"
        );
        assert!(calls.lock().unwrap().is_empty(), "{:?}", calls.lock());
        assert_eq!(
            *lines.lock().unwrap(),
            [
                "init.test.never",
                "init.test.drifts",
                "init.test.never",
                "init.test.never"
            ]
        );
    }

    /// Notification Center's widget list, a key of another app's container.
    pub(super) fn nc_widgets() -> Target {
        Target::new(NC, "widgets")
    }

    fn populated() -> PValue {
        PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            (
                "instances".into(),
                PValue::Array(vec![PValue::Data(vec![42])]),
            ),
        ])
    }

    fn cleared() -> PValue {
        PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            ("instances".into(), PValue::Array(Vec::new())),
        ])
    }

    fn wanted() -> Vec<(Target, PValue)> {
        vec![
            (
                Target::new("com.apple.dock", "show-recents"),
                PValue::Bool(false),
            ),
            (
                Target::new("com.apple.WindowManager", "StandardHideWidgets"),
                PValue::Bool(true),
            ),
        ]
    }

    impl Host for Fake {
        fn selection(&self) -> Selection {
            self.selected.unwrap_or_default()
        }
        fn full_disk_access(&self) -> Option<bool> {
            self.full_disk_access
        }
        fn select(&mut self, selected: Selection) {
            self.selected = Some(selected.workstation());
        }
        fn attempts(&mut self) -> Vec<Attempt> {
            std::mem::take(&mut self.attempts)
        }
        fn settled(&mut self) -> Vec<String> {
            std::mem::take(&mut self.settled)
        }
        fn withheld(&mut self) -> Vec<String> {
            std::mem::take(&mut self.withheld)
        }
        fn hold(&mut self, items: &[(String, Hold)]) {
            self.held = items.to_vec();
        }
        fn held(&self) -> Vec<String> {
            self.held.iter().map(|(item, _)| item.clone()).collect()
        }
        fn journal(
            &mut self,
            id: &str,
            from: &str,
            to: &str,
            revert: Option<&str>,
        ) -> Result<(), String> {
            if self.journal_refuses && id.starts_with("doctor.") {
                self.events.push(format!("refused {id}"));
                return Err("the journal is full".into());
            }
            self.events.push(format!("journal {id}"));
            self.lines
                .push((id.into(), from.into(), to.into(), revert.map(str::to_owned)));
            Ok(())
        }
        fn restore_runtime(&mut self, _saved: &[Saved]) -> Vec<Row> {
            self.runtime_changed = false;
            Vec::new()
        }
        fn read(&mut self, target: &Target) -> Result<Option<PValue>, String> {
            Ok(self.values.get(target).cloned())
        }
        fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String> {
            assert!(
                !self.dies_in_doctor_writes,
                "the process ends among the doctor's own writes"
            );
            self.put(target, value)
        }
        fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String> {
            self.preference_passes += 1;
            self.attempts.clear();
            self.settled.clear();
            self.withheld.clear();
            Ok(self
                .wants()
                .into_iter()
                .filter(|(target, _)| group != WorkstationGroup::Withheld || contained(target))
                .filter(|(target, _)| self.reaches(target))
                .filter(|(target, _)| self.selection().contains(Self::setting(target)))
                .filter(|(target, _)| self.hold_of(&self.item(target)).is_none())
                .filter(|(t, v)| !self.values.get(t).is_some_and(|now| now.same(v)))
                .map(|(target, _)| Saved {
                    value: self.values.get(&target).cloned(),
                    granted: self.reaches(&target),
                    target,
                })
                .collect())
        }
        fn restart(&mut self, name: &str, _reads: &[&Target]) -> Result<(), String> {
            self.restarts.push(name.into());
            self.events.push(format!("restart {name}"));
            if self.fail_restart_once {
                self.fail_restart_once = false;
                return Err("injected restart failure".into());
            }
            Ok(())
        }
        fn workstation(
            &mut self,
            group: WorkstationGroup,
            apply: bool,
        ) -> Result<Vec<Row>, String> {
            let mut rows = Vec::new();
            assert!(
                !(apply && group == WorkstationGroup::Preferences && self.dies_in_preferences),
                "the process ends in its preference pass"
            );
            if group != WorkstationGroup::Accounts {
                for (item, keys) in self.items() {
                    let keys: Vec<(Target, PValue)> = keys
                        .into_iter()
                        .filter(|(target, _)| {
                            self.selection().contains(Self::setting(target))
                                && (group != WorkstationGroup::Withheld || contained(target))
                        })
                        .collect();
                    if keys.is_empty() {
                        continue;
                    }
                    let left_alone = keys.iter().any(|(target, _)| !self.reaches(target));
                    if apply && left_alone {
                        self.withheld.push(item.clone());
                    }
                    let unmet: Vec<(Target, PValue)> = keys
                        .into_iter()
                        .filter(|(target, value)| {
                            self.reaches(target)
                                && !self.values.get(target).is_some_and(|now| now.same(value))
                        })
                        .collect();
                    let mut status = if unmet.is_empty() { "ok" } else { "drift" };
                    let mut detail = if left_alone {
                        "Notification Center's wait for Full Disk Access".to_owned()
                    } else {
                        "injected shared item".to_owned()
                    };
                    if apply && !unmet.is_empty() {
                        match self.hold_of(&item) {
                            Some(Hold::Back(why)) => {
                                status = "blocked";
                                detail = why;
                            }
                            Some(Hold::AsIs(why)) => {
                                status = "ok";
                                let from: Vec<String> =
                                    unmet.iter().map(|(target, _)| target.describe()).collect();
                                detail = format!("{}; left as it is: {why}", from.join(", "));
                            }
                            None if unmet
                                .iter()
                                .any(|(target, _)| self.blocked.contains(target)) =>
                            {
                                status = "blocked";
                            }
                            None => {
                                self.events.push(format!("journal {item}"));
                                let mut failed = std::mem::take(&mut self.fail_once);
                                let mut preferences = Vec::new();
                                for (target, value) in &unmet {
                                    let before = self.values.get(target).cloned();
                                    self.put(target, Some(value))?;
                                    if self.written_back.contains(target) {
                                        failed = true;
                                        match &before {
                                            Some(before) => {
                                                self.values.insert(target.clone(), before.clone());
                                            }
                                            None => {
                                                self.values.remove(target);
                                            }
                                        }
                                    }
                                    failed |= self.failing.contains(target);
                                    preferences.push(Saved {
                                        target: target.clone(),
                                        value: before,
                                        granted: true,
                                    });
                                }
                                for process in aterm_machines::init_activation::RELAUNCHED {
                                    if unmet.iter().any(|(target, _)| {
                                        aterm_machines::init_activation::readers(&target.domain)
                                            .contains(&process)
                                    }) {
                                        self.events.push(format!("relaunch {process}"));
                                    }
                                }
                                self.runtime_changed = true;
                                status = if failed { "error" } else { "applied" };
                                self.attempts.push(Attempt {
                                    item: item.clone(),
                                    verified: !failed,
                                    preferences,
                                });
                            }
                        }
                    }
                    if apply && matches!(status, "ok" | "applied") {
                        self.settled.push(item.clone());
                    }
                    rows.push(Row {
                        name: item,
                        status: status.into(),
                        detail,
                    });
                }
                assert!(
                    !(apply
                        && group == WorkstationGroup::Preferences
                        && self.dies_after_item_writes),
                    "the process ends after its items' writes, before they relaunch their readers"
                );
            }
            if matches!(group, WorkstationGroup::Accounts | WorkstationGroup::All)
                && !WorkstationGroup::Accounts
                    .selected(self.selection())
                    .is_empty()
            {
                self.account_passes += 1;
                let item = "Account sign-out".to_owned();
                let mut detail = "injected account result".to_owned();
                let status = if self.accounts_blocked {
                    "blocked"
                } else if !self.accounts_fail {
                    "ok"
                } else if !apply {
                    "drift"
                } else {
                    match self.hold_of(&item) {
                        Some(Hold::Back(why)) => {
                            detail = why;
                            "blocked"
                        }
                        Some(Hold::AsIs(why)) => {
                            detail = why;
                            "ok"
                        }
                        None => {
                            self.events.push(format!("journal {item}"));
                            self.attempts.push(Attempt {
                                item: item.clone(),
                                verified: false,
                                preferences: Vec::new(),
                            });
                            "error"
                        }
                    }
                };
                if apply && status == "ok" {
                    self.settled.push(item.clone());
                }
                rows.push(Row {
                    name: item,
                    status: status.into(),
                    detail,
                });
            }
            Ok(rows)
        }
    }

    #[test]
    fn selected_preferences_leave_unchecked_accounts_and_recovery_targets_untouched() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut selected = Selection::none();
        selected.set(Setting::Widgets, true);
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        assert_eq!(authorized_selection(root.path()).unwrap(), Some(selected));
        let dock = wanted()[0].0.clone();
        save_pending_refresh(root.path(), std::slice::from_ref(&dock)).unwrap();
        let old_activation = std::fs::read(root.path().join("pending-refresh.json")).unwrap();
        let mut host = Fake {
            selected: Some(selected),
            accounts_blocked: true,
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(
            host.account_passes, 0,
            "unchecked account operations never run"
        );
        assert_eq!(host.writes, [wanted()[1].0.clone()]);
        assert!(!host.values.contains_key(&dock));
        assert!(
            host.restarts.is_empty(),
            "do not activate an unchecked old Dock obligation"
        );
        assert_eq!(
            std::fs::read(root.path().join("pending-refresh.json")).unwrap(),
            old_activation
        );
        let saved = load_backup(&report.backup).unwrap();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].target, wanted()[1].0);
    }

    #[test]
    fn account_only_and_empty_selections_never_probe_preferences() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut selected = Selection::none();
        selected.set(Setting::Messages, true);
        let mut host = Fake {
            selected: Some(selected),
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention());
        assert_eq!(host.preference_passes, 0);
        assert_eq!(host.account_passes, 1);
        assert!(host.writes.is_empty());
        assert!(!report.backup.exists());

        let empty_root = aterm_tempfile::tempdir().unwrap();
        authorize_automatic_setup_at(empty_root.path(), Selection::none()).unwrap();
        assert!(first_run_complete(empty_root.path()).unwrap());
        assert_eq!(
            authorized_selection(empty_root.path()).unwrap(),
            Some(Selection::none())
        );
        let mut host = Fake {
            selected: Some(Selection::none()),
            ..Fake::default()
        };
        let report = execute(&mut host, empty_root.path(), Action::Apply).unwrap();
        assert!(!report.needs_attention());
        assert_eq!(host.preference_passes, 0);
        assert_eq!(host.account_passes, 0);
        assert!(host.writes.is_empty());
        assert!(host.restarts.is_empty());
        assert!(!report.backup.exists());
    }

    #[test]
    fn authorization_survives_reopen_and_missing_or_unknown_plans_are_refused() {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(authorized_selection(root.path()).unwrap(), None);
        let mut selected = Selection::all();
        selected.set(Setting::Messages, false);
        selected.set(Setting::FaceTime, false);
        authorize_automatic_setup_at(root.path(), selected).unwrap();
        assert_eq!(authorized_selection(root.path()).unwrap(), Some(selected));
        assert!(!first_run_complete(root.path()).unwrap());
        save_automatic_plan(root.path(), Selection::all()).unwrap();
        assert_eq!(
            authorized_selection(root.path()).unwrap(),
            Some(Selection::all())
        );
        write_json(
            &root.path().join("automatic-plan.json"),
            &json!({"profile": PROFILE, "selection": 65535}),
        )
        .unwrap();
        assert!(authorized_selection(root.path()).is_err());
    }

    #[test]
    fn stale_automatic_authorization_preserves_every_completed_decision() {
        let root = aterm_tempfile::tempdir().unwrap();
        let marker_path = root.path().join("first-run.json");
        let mut original = Selection::none();
        original.set(Setting::Widgets, true);
        save_automatic_plan(root.path(), original).unwrap();
        let plan = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
        for marker in [
            json!({"profile": PROFILE, "state": "cancelled"}),
            json!({"profile": PROFILE, "state": "restored"}),
            json!({"profile": PROFILE, "state": "applied", "accounts_verified": true}),
        ] {
            write_json(&marker_path, &marker).unwrap();
            let before = std::fs::read(&marker_path).unwrap();
            let _lock = HeldLock::take(root.path()).unwrap();
            authorize_automatic_setup_at(root.path(), Selection::all()).unwrap();
            assert_eq!(std::fs::read(&marker_path).unwrap(), before);
            assert_eq!(
                std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
                plan
            );
        }
    }

    #[test]
    fn visible_finish_and_authorization_markers_must_discharge_failed_namespace_flushes() {
        let root = aterm_tempfile::tempdir().unwrap();
        let marker = root.path().join("first-run.json");
        for value in [
            json!({"profile": PROFILE, "state": "cancelled"}),
            json!({"profile": PROFILE, "state": "restored"}),
            json!({"profile": PROFILE, "state": "applied", "accounts_verified": true}),
        ] {
            assert!(
                write_json_with_flush(&marker, &value, |_, _| Err(std::io::Error::other(
                    "failed directory flush"
                )))
                .is_err()
            );
            assert!(marker.exists(), "the rename is visible even on failure");
            assert!(
                first_run_complete_with_flush(root.path(), |_, _| Err(std::io::Error::other(
                    "still not durable"
                )))
                .is_err()
            );
            let mut reached_root = false;
            assert!(
                first_run_complete_with_flush(root.path(), |directory, _| {
                    reached_root |= directory.parent().is_none();
                    Ok(())
                })
                .unwrap()
            );
            assert!(
                reached_root,
                "the retry flushes the complete namespace chain"
            );
        }
        let plan = root.path().join("automatic-plan.json");
        assert!(
            write_json_with_flush(
                &plan,
                &json!({"profile":PROFILE,"selection":Selection::all().bits()}),
                |_, _| Err(std::io::Error::other("failed plan flush"))
            )
            .is_err()
        );
        assert!(
            authorized_selection_with_flush(root.path(), |_, _| Err(std::io::Error::other(
                "still not durable"
            )))
            .is_err()
        );
        assert_eq!(
            authorized_selection_with_flush(root.path(), |_, _| Ok(())).unwrap(),
            Some(Selection::all())
        );
    }

    #[test]
    fn shared_apply_is_idempotent_and_restore_preserves_unowned_keys() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        let unowned = Target::new("com.apple.dock", "orientation");
        host.values
            .insert(unowned.clone(), PValue::Str("left".into()));
        let first = execute(&mut host, root.path(), Action::Apply).expect("apply");
        assert!(!first.needs_attention(), "{}", first.to_text());
        assert!(first_run_complete(root.path()).expect("complete marker"));
        let count = host.writes.len();
        let backup = std::fs::read(&first.backup).expect("backup");
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("reapply")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), count);
        assert_eq!(
            backup,
            std::fs::read(&first.backup).expect("same originals")
        );
        host.values
            .insert(unowned.clone(), PValue::Str("right".into()));
        assert!(
            !execute(&mut host, root.path(), Action::Restore)
                .expect("restore")
                .needs_attention()
        );
        assert_eq!(host.values.len(), 1);
        assert_eq!(host.values[&unowned], PValue::Str("right".into()));
        assert!(first_run_complete(root.path()).expect("restore disables automatic setup"));
    }

    #[test]
    fn cancellation_is_durable_until_explicit_apply_and_check_leaves_it_alone() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        assert!(!first_run_complete(root.path()).expect("new profile"));
        cancel_automatic_setup_at(root.path()).expect("cancel");
        assert!(first_run_complete(root.path()).expect("cancelled profile"));
        let marker_path = root.path().join("first-run.json");
        let cancelled = std::fs::read(&marker_path).expect("durable cancellation");
        assert_eq!(
            aterm_json::from_slice::<Value>(&cancelled).unwrap()["state"].as_str(),
            Some("cancelled")
        );
        let mut host = Fake {
            accounts_blocked: true,
            ..Fake::default()
        };
        execute(&mut host, root.path(), Action::Check).expect("check cancellation");
        assert!(host.writes.is_empty());
        assert_eq!(std::fs::read(&marker_path).unwrap(), cancelled);
        assert!(!root.path().join("original-preferences.json").exists());

        let report = execute(&mut host, root.path(), Action::Apply).expect("explicit apply");
        assert!(report.needs_attention(), "blocked account remains visible");
        assert!(
            !host.writes.is_empty(),
            "explicit apply supersedes cancellation"
        );
        assert!(!first_run_complete(root.path()).expect("pending explicit apply"));
        assert_eq!(
            aterm_json::from_slice::<Value>(&std::fs::read(marker_path).unwrap()).unwrap()["state"]
                .as_str(),
            Some("pending")
        );
    }

    #[test]
    fn cancellation_preserves_completed_decisions_and_cannot_race_an_apply() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let marker_path = root.path().join("first-run.json");
        for marker in [
            json!({ "profile": PROFILE, "state": "restored" }),
            json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true }),
        ] {
            write_json(&marker_path, &marker).expect("completed state");
            let before = std::fs::read(&marker_path).unwrap();
            cancel_automatic_setup_at(root.path()).expect("already finished");
            assert_eq!(std::fs::read(&marker_path).unwrap(), before);
        }

        write_json(
            &marker_path,
            &json!({ "profile": PROFILE, "state": "pending" }),
        )
        .expect("pending state");
        let held = HeldLock::take(root.path()).expect("active apply");
        assert!(cancel_automatic_setup_at(root.path()).is_err());
        assert!(!first_run_complete(root.path()).expect("cancellation refused"));
        drop(held);
        cancel_automatic_setup_at(root.path()).expect("cancel after apply");
        assert!(first_run_complete(root.path()).expect("cancelled after release"));
    }

    #[test]
    fn a_revived_legacy_profile_finishes_only_after_successful_explicit_apply() {
        for stopped in ["cancelled", "restored"] {
            for blocked in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                write_json(
                    &root.path().join("first-run.json"),
                    &json!({"profile":PROFILE,"state":stopped}),
                )
                .unwrap();
                let _profile = HeldLock::take(root.path()).unwrap();
                initial::save_explicit_workstation_plan(root.path()).unwrap();
                let revision = initial::revision_at(root.path()).unwrap();
                assert_ne!(revision, 0);
                let mut host = Fake {
                    accounts_blocked: blocked,
                    ..Fake::default()
                };
                let report = execute(&mut host, root.path(), Action::Apply).unwrap();
                assert_eq!(report.needs_attention(), blocked);
                if !report.needs_attention() {
                    initial::complete_explicit_workstation_plan(root.path()).unwrap();
                }
                assert_eq!(initial::finished_at(root.path()).unwrap(), !blocked);
                assert_eq!(initial::revision_at(root.path()).unwrap(), revision);
                assert_eq!(
                    claim_automatic_setup_at(root.path()).unwrap().is_none(),
                    !blocked
                );
            }
        }
    }

    #[test]
    fn automatic_worker_claim_is_exclusive_and_does_not_hold_the_profile_lock() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let claim = claim_automatic_setup_at(root.path())
            .expect("claim worker")
            .expect("first worker");
        assert!(claim_automatic_setup_at(root.path()).unwrap().is_none());
        let profile = HeldLock::take(root.path()).expect("explicit apply remains available");
        drop(profile);
        cancel_automatic_setup_at(root.path()).expect("cancel during countdown");
        drop(claim);
        assert!(
            claim_automatic_setup_at(root.path()).unwrap().is_none(),
            "a cancelled setup never starts another countdown"
        );

        write_json(
            &root.path().join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "pending" }),
        )
        .expect("explicit retry pending");
        let claim = claim_automatic_setup_at(root.path())
            .unwrap()
            .expect("retry worker");
        drop(claim);
        let replacement = claim_automatic_setup_at(root.path())
            .unwrap()
            .expect("released claim can be acquired again");
        let mut host = Fake::default();
        execute(&mut host, root.path(), Action::Apply).expect("explicit apply during countdown");
        drop(replacement);
        assert!(
            claim_automatic_setup_at(root.path()).unwrap().is_none(),
            "a completed explicit apply ends automatic setup too"
        );
    }

    #[test]
    fn failed_shared_preference_apply_rolls_back_partial_mutations() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
        let mut host = Fake {
            fail_once: true,
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("failure report");
        assert!(report.needs_attention());
        assert!(
            !host.values.contains_key(&dock),
            "even a failed write may already have mutated a preference"
        );
        assert!(
            host.values.contains_key(&widgets),
            "the item that verified keeps its change"
        );
        assert!(
            !host.runtime_changed,
            "rollback must recover active settings as well as saved preferences"
        );
        let restored: Vec<&Row> = report
            .rows
            .iter()
            .filter(|row| row.status == "restored")
            .collect();
        assert_eq!(restored.len(), 1, "{}", report.to_text());
        assert!(
            restored[0].detail.contains(&dock.describe())
                && !restored[0].detail.contains(&widgets.describe()),
            "recovery claims only what it put back: {}",
            report.to_text()
        );
        assert_eq!(
            report
                .rows
                .iter()
                .find(|row| row.name == widgets.describe())
                .map(|row| row.status.as_str()),
            Some("applied"),
            "a verified change is not relabelled"
        );
        assert_eq!(host.restarts, ["Dock"]);
        assert_eq!(host.account_passes, 1);
        assert!(!first_run_complete(root.path()).expect("failed pass remains pending"));
        assert!(report.backup.exists());
    }

    fn at(minutes: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000 + minutes * 60)
    }

    const BUILD: &str = "1.0.0+gaaaa";

    #[test]
    fn a_standing_failure_is_tried_twice_and_then_never_again_by_the_automatic_worker() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
        let mut host = Fake {
            failing: vec![dock.clone()],
            accounts_fail: true,
            ..Fake::default()
        };
        let first = automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
        assert!(first.needs_attention());
        assert!(
            !host.values.contains_key(&dock),
            "the failed change is undone"
        );
        assert!(host.values.contains_key(&widgets), "the verified one stays");
        assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
        let sign_outs = |host: &Fake| {
            host.events
                .iter()
                .filter(|event| *event == "journal Account sign-out")
                .count()
        };
        assert_eq!(sign_outs(&host), 1);
        let quiet = |host: &mut Fake, minutes: std::ops::Range<u64>, why: &str| {
            let events = host.events.len();
            for minute in minutes {
                let report =
                    automatic_pass(host, root.path(), BUILD, at(minute)).expect("retry pass");
                assert!(report.needs_attention(), "minute {minute}");
                for name in [dock.describe(), "Account sign-out".into()] {
                    let row = report.rows.iter().find(|row| row.name == name).unwrap();
                    assert_eq!(
                        (row.status.as_str(), row.detail.as_str()),
                        ("blocked", why),
                        "minute {minute}"
                    );
                }
            }
            assert!(
                host.events[events..].is_empty(),
                "nothing changed, relaunched or journaled: {:?}",
                &host.events[events..]
            );
        };
        quiet(&mut host, 1..10, WAITING);
        automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
        assert_eq!(host.restarts, ["Dock", "Dock"]);
        assert_eq!(sign_outs(&host), 2);
        quiet(&mut host, 11..40, HELD);
        assert!(host.values.contains_key(&widgets));
        assert!(!first_run_complete(root.path()).unwrap());
        automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(41)).expect("new build");
        assert_eq!(host.restarts.len(), 3);
        assert_eq!(sign_outs(&host), 3);
        let record = Automatic::load(root.path(), 0).unwrap();
        assert_eq!(record.settled, [widgets.describe()], "{record:?}");
        assert_eq!(
            record.failed.len(),
            4,
            "two items in two builds: {record:?}"
        );
        assert_eq!(
            Automatic::load(root.path(), 1).unwrap(),
            Automatic::default()
        );
        host.held.clear();
        execute(&mut host, root.path(), Action::Apply).expect("explicit apply");
        assert_eq!(host.restarts.len(), 4);
    }

    #[test]
    fn two_builds_taking_turns_each_spend_only_their_own_tries() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            failing: vec![wanted()[0].0.clone()],
            ..Fake::default()
        };
        let builds = ["1.0.0+gaaaa", "1.0.0+gbbbb"];
        let turns = |host: &mut Fake, minutes: std::ops::Range<u64>| {
            for (turn, minute) in minutes.enumerate() {
                automatic_pass(host, root.path(), builds[turn % 2], at(minute)).expect("pass");
            }
        };
        turns(&mut host, 0..8);
        assert_eq!(host.restarts, ["Dock", "Dock"], "one try each");
        turns(&mut host, 20..40);
        assert_eq!(host.restarts.len(), 4, "a second try each, and no more");
    }

    #[test]
    fn the_held_record_keeps_the_builds_that_tried_last() {
        let failed = || Pass {
            report: Report {
                action: Action::Apply,
                rows: Vec::new(),
                manual_steps: Vec::new(),
                backup: PathBuf::new(),
            },
            unverified: vec!["dock".to_owned()],
            settled: Vec::new(),
        };
        let build = |n: usize| format!("1.0.{n}+g0000");
        let mut record = Automatic::default();
        let mut now = 0;
        for n in 0..HELD_BUILDS {
            now += 1;
            assert!(record.add(&build(n), now, &failed()));
        }
        now += 1;
        record.add(&build(0), now, &failed());
        now += 1;
        record.add(&build(HELD_BUILDS), now, &failed());
        let builds: Vec<&str> = record.failed.iter().map(|f| f.build.as_str()).collect();
        assert_eq!(builds.len(), HELD_BUILDS, "{builds:?}");
        assert!(!builds.contains(&build(1).as_str()), "{builds:?}");
        assert_eq!(
            record.holds(&build(0), now, &Withheld::default()),
            [("dock".to_owned(), Hold::Back(HELD.into()))],
            "{builds:?}"
        );
        assert!(
            record
                .holds(&build(1), now, &Withheld::default())
                .is_empty()
        );
    }

    #[test]
    fn only_the_settled_item_the_follow_up_owes_is_held_with_its_words() {
        let record = Automatic {
            settled: vec!["dock".to_owned(), WIDGETS.to_owned()],
            failed: Vec::new(),
        };
        let owed = Withheld {
            items: vec![WIDGETS.to_owned()],
            failed: Vec::new(),
        };
        assert_eq!(
            record.holds(BUILD, 0, &owed),
            [
                ("dock".to_owned(), Hold::AsIs(SETTLED.into())),
                (WIDGETS.to_owned(), Hold::AsIs(SETTLED_OWED.into())),
            ]
        );
        assert_eq!(
            record.holds(BUILD, 0, &Withheld::default()),
            [
                ("dock".to_owned(), Hold::AsIs(SETTLED.into())),
                (WIDGETS.to_owned(), Hold::AsIs(SETTLED.into())),
            ]
        );
    }

    #[test]
    fn the_follow_ups_tries_hold_an_automatic_pass_that_reaches_its_keys() {
        let tried = |item: &str, tries: u32, at: u64| Failed {
            item: item.to_owned(),
            build: BUILD.to_owned(),
            tries,
            at,
        };
        let back = |item: &str, why: &str| (item.to_owned(), Hold::Back(why.into()));
        let now = SECOND_TRY_AFTER.as_secs() * 2;
        let recent = now - 60;
        let record = Automatic {
            settled: vec!["settled".to_owned()],
            failed: vec![
                tried("waits here, spent there", 1, recent),
                tried("spent here, waits there", TRIES, 0),
                tried("due here, waits there", 1, 0),
                tried("due here, owed nothing", 1, 0),
            ],
        };
        let owed = Withheld {
            items: [
                "settled",
                "waits here, spent there",
                "spent here, waits there",
                "due here, waits there",
                "waits there",
                "due here, owed nothing",
            ]
            .map(str::to_owned)
            .to_vec(),
            failed: vec![
                tried("waits here, spent there", TRIES, 0),
                tried("spent here, waits there", 1, recent),
                tried("due here, waits there", 1, recent),
                tried("waits there", 1, recent),
                tried("settled", TRIES, recent),
                tried("no longer named", 1, recent),
            ],
        };
        assert_eq!(
            record.holds(BUILD, now, &owed),
            [
                ("settled".to_owned(), Hold::AsIs(SETTLED_OWED.into())),
                back("waits here, spent there", HELD),
                back("spent here, waits there", HELD),
                back("due here, waits there", WAITING),
                back("waits there", WAITING),
            ]
        );
        assert_eq!(
            record.holds(BUILD, now, &Withheld::default()),
            [
                ("settled".to_owned(), Hold::AsIs(SETTLED.into())),
                back("waits here, spent there", WAITING),
                back("spent here, waits there", HELD),
            ]
        );
    }

    #[test]
    fn a_setting_changed_after_setup_made_it_is_left_as_the_person_set_it() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let widgets = wanted()[1].0.clone();
        let mut host = Fake {
            failing: vec![wanted()[0].0.clone()],
            ..Fake::default()
        };
        automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
        assert!(host.values.contains_key(&widgets));
        host.values.remove(&widgets);
        for minute in 1..30 {
            let report = automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
            let row = report
                .rows
                .iter()
                .find(|row| row.name == widgets.describe())
                .unwrap();
            assert_eq!(row.status, "ok", "minute {minute}: {}", report.to_text());
            assert!(row.detail.ends_with(SETTLED), "{}", row.detail);
        }
        assert_eq!(
            host.writes
                .iter()
                .filter(|target| **target == widgets)
                .count(),
            1,
            "{:?}",
            host.events
        );
        assert!(!host.values.contains_key(&widgets));
        host.failing.clear();
        let report = automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(31)).expect("done");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(first_run_complete(root.path()).unwrap());
        assert!(!host.values.contains_key(&widgets));
    }

    #[test]
    fn a_failure_that_clears_gets_its_second_try_and_setup_completes() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            fail_once: true,
            ..Fake::default()
        };
        automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
        assert!(!first_run_complete(root.path()).unwrap());
        let changes = host.changes();
        for minute in 1..10 {
            automatic_pass(&mut host, root.path(), BUILD, at(minute)).expect("retry");
        }
        assert_eq!(host.changes(), changes, "{:?}", host.events);
        let report = automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(first_run_complete(root.path()).unwrap());
    }

    #[test]
    fn a_held_retry_that_dies_part_way_leaves_nothing_to_relaunch() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            failing: vec![wanted()[0].0.clone()],
            ..Fake::default()
        };
        automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("first pass");
        assert_eq!(host.restarts, ["Dock"]);
        host.dies_in_preferences = true;
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            automatic_pass(&mut host, root.path(), BUILD, at(1))
        }));
        assert!(died.is_err(), "the pass died in its preference group");
        assert!(
            pending_refresh(root.path()).unwrap().is_empty(),
            "{:?}",
            pending_refresh(root.path())
        );
        host.dies_in_preferences = false;
        automatic_pass(&mut host, root.path(), BUILD, at(2)).expect("the next process");
        assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
    }

    #[test]
    fn a_pass_that_ends_after_its_items_wrote_leaves_their_relaunch_to_them() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            dies_after_item_writes: true,
            ..Fake::default()
        };
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&mut host, root.path(), Action::Apply)
        }));
        assert!(died.is_err(), "the pass died after its items' writes");
        assert!(
            pending_refresh(root.path()).unwrap().is_empty(),
            "{:?}",
            pending_refresh(root.path())
        );
        host.dies_after_item_writes = false;
        let report = execute(&mut host, root.path(), Action::Apply).expect("the next process");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(host.restarts.is_empty(), "{:?}", host.events);
    }

    #[test]
    fn a_pass_that_ends_in_its_rollback_relaunches_their_readers_on_the_next() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let dock = wanted()[0].0.clone();
        let mut host = Fake {
            failing: vec![dock.clone()],
            dies_in_doctor_writes: true,
            ..Fake::default()
        };
        let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            execute(&mut host, root.path(), Action::Apply)
        }));
        assert!(died.is_err(), "the pass died in its rollback");
        assert_eq!(pending_refresh(root.path()).unwrap(), [dock]);
        assert!(host.restarts.is_empty());
        host.dies_in_doctor_writes = false;
        host.failing.clear();
        let report = execute(&mut host, root.path(), Action::Apply).expect("the next process");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(host.restarts, ["Dock"], "{:?}", host.events);
        assert!(pending_refresh(root.path()).unwrap().is_empty());
    }

    #[test]
    fn a_recorded_key_an_item_rewrites_and_verifies_is_not_relaunched_for_again() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let dock = wanted()[0].0.clone();
        save_pending_refresh(root.path(), std::slice::from_ref(&dock)).unwrap();
        let mut host = Fake::default();
        let report = execute(&mut host, root.path(), Action::Apply).expect("apply");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(host.writes.contains(&dock));
        assert!(host.restarts.is_empty(), "{:?}", host.events);
        assert!(pending_refresh(root.path()).unwrap().is_empty());
    }

    #[test]
    fn a_refused_journal_line_never_keeps_an_original_from_coming_back() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let dock = wanted()[0].0.clone();
        let mut host = Fake {
            failing: vec![dock.clone()],
            journal_refuses: true,
            ..Fake::default()
        };
        let report = automatic_pass(&mut host, root.path(), BUILD, at(0)).expect("failed pass");
        assert!(!host.values.contains_key(&dock), "rolled back anyway");
        assert_eq!(host.restarts, ["Dock"], "relaunched anyway");
        let refused = |report: &Report| {
            report
                .rows
                .iter()
                .filter(|row| row.name == "Change journal" && row.status == "error")
                .map(|row| row.detail.clone())
                .collect::<Vec<_>>()
                .join("; ")
        };
        let said = refused(&report);
        assert!(
            said.contains(&restore_id(&dock)) && said.contains("doctor.relaunch.Dock"),
            "{}",
            report.to_text()
        );
        let report = execute(&mut host, root.path(), Action::Restore).expect("restore");
        assert!(
            host.values.is_empty(),
            "every original is back: {:?}",
            host.values
        );
        assert_eq!(host.restarts, ["Dock", "Dock"]);
        assert!(
            refused(&report).contains("doctor.restore.com.apple.WindowManager"),
            "{}",
            report.to_text()
        );
    }

    #[test]
    fn a_rollback_with_nothing_to_put_back_relaunches_nothing() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let dock = wanted()[0].0.clone();
        let mut host = Fake {
            written_back: vec![dock.clone()],
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("failed pass");
        assert!(report.needs_attention());
        assert!(!host.values.contains_key(&dock));
        assert!(host.restarts.is_empty(), "{:?}", host.events);
        assert_eq!(
            host.writes.iter().filter(|target| **target == dock).count(),
            1,
            "the apply's write, and no rollback write"
        );
    }

    #[test]
    fn one_failing_item_leaves_the_applied_ones_applied_and_the_next_pass_changes_nothing() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let (dock, widgets) = (wanted()[0].0.clone(), wanted()[1].0.clone());
        let mut host = Fake {
            blocked: vec![dock.clone()],
            ..Fake::default()
        };
        let first = execute(&mut host, root.path(), Action::Apply).expect("first pass");
        assert!(
            first.needs_attention(),
            "the blocked row still needs the person"
        );
        assert_eq!(
            host.writes,
            std::slice::from_ref(&widgets),
            "{:?}",
            host.events
        );
        assert!(host.values.contains_key(&widgets));
        assert!(
            host.restarts.is_empty(),
            "nothing was rolled back to reload"
        );
        let changes = host.changes();
        for _ in 0..3 {
            let report = execute(&mut host, root.path(), Action::Apply).expect("next pass");
            assert_eq!(
                report
                    .rows
                    .iter()
                    .find(|row| row.name == widgets.describe())
                    .map(|row| row.status.as_str()),
                Some("ok")
            );
        }
        assert_eq!(host.changes(), changes, "{:?}", host.events);
        assert!(host.values.contains_key(&widgets));
        host.blocked.clear();
        let report = execute(&mut host, root.path(), Action::Apply).expect("unblocked");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(&host.writes[1..], [dock]);
        assert!(first_run_complete(root.path()).unwrap());
    }

    #[test]
    fn every_rollback_restore_write_and_relaunch_is_journaled_before_it_is_made() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            fail_once: true,
            ..Fake::default()
        };
        execute(&mut host, root.path(), Action::Apply).expect("failed pass");
        execute(&mut host, root.path(), Action::Apply).expect("retry");
        execute(&mut host, root.path(), Action::Restore).expect("restore");
        assert!(host.restarts.len() >= 2, "{:?}", host.events);
        assert!(host.writes.len() >= 4, "{:?}", host.events);
        for (at, event) in host.events.iter().enumerate() {
            let wanted = if let Some(target) = event.strip_prefix("write ") {
                let target = wanted()
                    .into_iter()
                    .map(|(target, _)| target)
                    .find(|t| t.describe() == target)
                    .unwrap();
                vec![host.item(&target), restore_id(&target)]
            } else if let Some(process) = event.strip_prefix("restart ") {
                vec![format!("doctor.relaunch.{process}")]
            } else {
                continue;
            };
            let line = at
                .checked_sub(1)
                .and_then(|before| host.events[before].strip_prefix("journal "));
            assert!(
                line.is_some_and(|id| wanted.iter().any(|want| want == id)),
                "{event} at {at} has no journal line before it: {:?}",
                host.events
            );
        }
        let restores: Vec<_> = host
            .lines
            .iter()
            .filter(|(id, ..)| id.starts_with("doctor.restore."))
            .collect();
        assert!(restores.len() >= 2, "{:?}", host.lines);
        for (id, from, to, revert) in restores {
            let revert = revert.as_deref().unwrap_or_default();
            assert!(
                revert.starts_with("/usr/bin/defaults write ")
                    || revert.starts_with("/usr/bin/defaults delete "),
                "{id}: {revert}"
            );
            assert!(
                from.ends_with(" = true") || from.ends_with(" = false"),
                "{id}: {from}"
            );
            assert!(to.ends_with(" = absent"), "{id}: {to}");
        }
    }

    #[test]
    fn blocked_accounts_keep_preference_progress_but_not_completion() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake {
            accounts_blocked: true,
            ..Fake::default()
        };
        let report = execute(&mut host, root.path(), Action::Apply).expect("blocked");
        assert!(report.needs_attention());
        assert!(!first_run_complete(root.path()).expect("blocked accounts remain pending"));
        assert!(!host.values.is_empty());
        let writes = host.writes.len();
        host.accounts_blocked = false;
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("permission retry")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), writes);
        assert!(first_run_complete(root.path()).expect("fully verified"));
    }

    #[test]
    fn failed_activation_is_reported_and_retried_without_preference_writes() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        execute(&mut host, root.path(), Action::Apply).expect("initial apply");
        save_pending_refresh(root.path(), &[wanted()[0].0.clone()])
            .expect("interrupted activation");
        assert!(
            execute(&mut host, root.path(), Action::Check)
                .expect("pending check")
                .needs_attention()
        );
        let writes = host.writes.len();
        host.fail_restart_once = true;
        assert!(
            execute(&mut host, root.path(), Action::Apply)
                .expect("failed activation")
                .needs_attention()
        );
        assert!(root.path().join("pending-refresh.json").exists());
        assert!(
            !execute(&mut host, root.path(), Action::Apply)
                .expect("retry activation")
                .needs_attention()
        );
        assert_eq!(host.writes.len(), writes);
        assert!(!root.path().join("pending-refresh.json").exists());
    }

    #[test]
    fn read_only_and_bad_recovery_records_never_write_preferences() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let mut host = Fake::default();
        let report = execute(&mut host, root.path(), Action::Check).expect("check");
        assert!(report.needs_attention());
        assert!(host.writes.is_empty());
        assert!(!report.backup.exists());
        std::fs::write(&report.backup, b"broken").expect("bad backup");
        assert!(execute(&mut host, root.path(), Action::Apply).is_err());
        assert!(host.writes.is_empty());
        std::fs::write(root.path().join("pending-refresh.json"), b"broken").expect("bad pending");
        assert!(execute(&mut host, root.path(), Action::Check).is_err());
        assert!(execute(&mut host, root.path(), Action::Restore).is_err());
        assert!(host.writes.is_empty());

        let record = root
            .path()
            .join("new-state")
            .join("doctor")
            .join("backup.json");
        let value = json!({"profile": PROFILE, "keys": []});
        let mut attempted = Vec::new();
        let refused = write_json_with_flush(&record, &value, |directory, _| {
            attempted.push(directory.to_path_buf());
            if directory == root.path() {
                Err(std::io::Error::other("injected ancestor flush refusal"))
            } else {
                Ok(())
            }
        });
        assert!(refused.is_err(), "a visible rename is not a durable backup");
        assert_eq!(attempted.last().map(PathBuf::as_path), Some(root.path()));
        assert_eq!(
            aterm_json::from_slice::<Value>(&std::fs::read(&record).unwrap()).unwrap(),
            value,
            "failure after publication is retained as failure"
        );
        let mut reached_root = false;
        write_json_with_flush(&record, &value, |directory, _| {
            reached_root |= directory.parent().is_none();
            Ok(())
        })
        .expect("a retry discharges the complete namespace chain");
        assert!(
            reached_root,
            "existing ancestor entries also need publication"
        );
    }

    #[test]
    fn a_published_name_is_flushed_again_only_once_it_or_its_directory_is_another() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let doctor = root.path().join("doctor");
        let record = doctor.join("first-run.json");
        std::fs::create_dir(&doctor).expect("doctor dir");
        std::fs::write(&record, b"{}").expect("record");
        let flushes = aterm_durable::seam::Recording::start();
        flush_record_with(&record, durable_name).expect("first");
        assert!(
            flushes.take().len() >= 3,
            "the record's, the doctor dir's and the scratch dir's names are new"
        );
        flush_record_with(&record, durable_name).expect("again");
        assert_eq!(flushes.take(), [], "every name on the way is as published");
        std::fs::write(doctor.join("next"), b"{}").expect("next");
        std::fs::rename(doctor.join("next"), &record).expect("rewrite");
        flush_record_with(&record, durable_name).expect("rewritten");
        assert_eq!(
            flushes.take().len(),
            1,
            "a record written again: its directory"
        );
        std::fs::rename(&doctor, root.path().join("old")).expect("move away");
        std::fs::create_dir(&doctor).expect("new doctor dir");
        std::fs::rename(root.path().join("old").join("first-run.json"), &record)
            .expect("the same record, in the new directory");
        flush_record_with(&record, durable_name).expect("replaced");
        assert_eq!(
            flushes.take().len(),
            2,
            "a directory put in place of another: it, and the scratch dir naming it"
        );
    }

    #[test]
    fn recovery_restores_plist_types_and_old_marker_does_not_skip_accounts() {
        let mut host = Fake::default();
        let target = wanted()[0].0.clone();
        host.values.insert(target.clone(), PValue::Bool(true));
        restore_one(&mut host, &target, Some(&PValue::Int(1)), &mut Vec::new())
            .expect("typed original");
        assert_eq!(host.values[&target], PValue::Int(1));
        let root = aterm_tempfile::tempdir().expect("scratch");
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile": PROFILE, "state": "applied"}),
        )
        .expect("old marker");
        assert!(!first_run_complete(root.path()).expect("not verified"));
    }

    #[test]
    fn automatic_permission_retries_do_not_repeat_on_every_launch() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let then = std::time::SystemTime::now();
        assert!(automatic_retry_due(root.path(), Some(false), then).expect("first attempt"));
        write_json(
            &root.path().join("automatic-attempt.json"),
            &json!({"profile": PROFILE, "accessibility_ready": false}),
        )
        .expect("attempt");
        let now = std::time::SystemTime::now();
        assert!(!automatic_retry_due(root.path(), Some(false), now).expect("same permission"));
        assert!(automatic_retry_due(root.path(), Some(true), now).expect("new permission"));
        let later = now + std::time::Duration::from_secs(61);
        assert!(automatic_retry_due(root.path(), Some(false), later).expect("after cooldown"));
    }

    #[test]
    fn a_real_home_may_change_the_profile_and_a_scratch_home_never_may() {
        let root = aterm_tempfile::tempdir().expect("scratch");
        let account = root.path().join("account");
        let scratch = root.path().join("scratch");
        let link = root.path().join("link");
        std::fs::create_dir(&account).expect("account home");
        std::fs::create_dir(&scratch).expect("scratch home");
        std::os::unix::fs::symlink(&account, &link).expect("a link to the account home");
        let own = || -> Result<PathBuf, String> { Ok(account.clone()) };
        for home in [&account, &link] {
            assert_eq!(
                scope_of(false, 501, Some(home), own),
                PersonScope::Person,
                "{}",
                home.display()
            );
            assert_eq!(scope_of(false, 501, Some(home), own).lasting(), None);
        }
        for scope in [
            scope_of(false, 501, Some(&scratch), own),
            scope_of(true, 501, Some(&account), own),
            scope_of(false, 0, Some(&account), own),
        ] {
            assert!(matches!(scope, PersonScope::Refused(_)), "{scope:?}");
            assert!(scope.lasting().is_some());
        }
        let unread = || -> Result<PathBuf, String> { Err("getpwuid_r: 5".into()) };
        for scope in [
            scope_of(false, 501, None, own),
            scope_of(false, 501, Some(&account), unread),
            scope_of(false, 501, Some(&root.path().join("missing")), own),
        ] {
            assert!(matches!(scope, PersonScope::Unresolved(_)), "{scope:?}");
            assert_eq!(scope.lasting(), None);
        }
    }
}
