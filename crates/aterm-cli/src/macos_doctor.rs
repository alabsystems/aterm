// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The owner's macOS workstation profile. Preference changes go through cfprefsd's
//! `defaults` client; the recovery journal contains only the keys this profile owns.

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
    authorize_initial_setup_observed, automatic_plan, check_initial_setup,
    check_initial_setup_observed, initial_setup_admission, initial_setup_finished,
    initial_setup_observation, initial_setup_revision, initial_setup_snapshot,
    initial_setup_status, restart_initial_setup, restart_initial_setup_observed,
    validate_initial_setup_plan,
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
    fn name(self) -> &'static str {
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
    /// `ok`, `drift`, `applied`, `restored`, `blocked`, `unverified`, or `error`.
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
    /// Supported UI steps that require the account owner.
    pub manual_steps: Vec<String>,
    /// Original per-key values retained for `--restore`.
    pub backup: PathBuf,
}

impl Report {
    /// Whether a setting or account remains out of policy, blocked, or unverified.
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

/// Check, apply, or restore the macOS workstation profile for the actual user.
/// An isolated instance never changes the login user's machine preferences.
pub fn run(action: Action) -> Result<Report, String> {
    run_observed(action).result
}

/// Observe the state belonging to a result before releasing its profile lock.
/// Check instead verifies that its whole read-only pass observed stable records.
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
/// Reads only aterm's state and establishes the durability of an existing plan.
pub fn automatic_selection() -> Result<Selection, String> {
    Ok(authorized_selection(&state_root()?)?.unwrap_or_default())
}

/// Persist the person's exact choice before any OS probes or changes. An empty
/// selection finishes setup immediately; retrying this operation is idempotent.
/// A completed decision made while the checklist was open takes precedence.
pub fn authorize_automatic_setup(selected: Selection) -> Result<(), String> {
    ensure_person_scope()?;
    let root = state_root()?;
    let _lock = HeldLock::take(&root)?;
    authorize_automatic_setup_at(&root, selected)
}

fn authorize_automatic_setup_at(root: &Path, selected: Selection) -> Result<(), String> {
    // The public caller holds the profile lock: an explicit apply or restore may
    // have finished since the coordinator last checked the marker.
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
    authorized_selection_with_flush(root, durable_directory)
}

fn authorized_selection_with_flush(
    root: &Path,
    flush: impl FnMut(&Path) -> std::io::Result<()>,
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

/// Exclusive ownership of the automatic countdown and its subsequent retry
/// worker. Keep this alive until the worker ends; dropping it permits another
/// aterm process to offer setup. Explicit doctor operations use a separate lock.
#[must_use = "keep the claim alive until the automatic setup worker ends"]
pub struct AutomaticSetupClaim {
    _lock: HeldLock,
}

/// Claim the automatic countdown without inspecting preferences or accounts.
/// Returns `None` when another process owns it or setup has already ended.
pub fn claim_automatic_setup() -> Result<Option<AutomaticSetupClaim>, String> {
    ensure_person_scope()?;
    claim_automatic_setup_at(&state_root()?)
}

/// Claim an explicit System State rerun, including a plan that already ended.
/// Acquire this before restarting the plan so an older worker cannot retain its
/// password and continue against the new selection.
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

/// Apply after the first-launch offer authorizes setup. A cancelled,
/// restored or successfully configured machine is left alone on later launches;
/// explicit apply supersedes cancellation and repairs drift.
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

/// [`apply_on_first_run_at`] on `sys` and `host`, as `build` at `now`: what each tick of
/// the System State worker runs.
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
        // An old pending marker or a new launch alone cannot authorize changes.
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
        // Record the request before showing native consent. Crashes and the
        // retry worker must not keep reopening the same permission prompt.
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

/// Permanently cancel automatic setup until the owner explicitly runs doctor
/// with `--apply`. This writes only aterm's marker; it does not inspect or alter
/// preferences, account registration, or permissions. A profile pass that has
/// already started holds the same lock, so cancellation then returns an error.
pub fn cancel_automatic_setup() -> Result<(), String> {
    InitialSetupCancellation::default().persist().map(|_| ())
}

#[cfg(test)]
fn cancel_automatic_setup_at(root: &Path) -> Result<(), String> {
    InitialSetupCancellation::default()
        .persist_at(root)
        .map(|_| ())
}

/// A retained Cancel choice, bound to the first readable setup run. Capturing
/// its revision does not take automatic.lock or wait behind a profile pass.
/// If the initial read fails, persistence remains pending until a run can be bound.
#[derive(Default)]
pub struct InitialSetupCancellation {
    revision: Option<u64>,
}

impl InitialSetupCancellation {
    /// Persist this choice through transient errors. False means a newer run
    /// superseded it; true includes an already finished decision in the same run.
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
/// Reads aterm's marker and confirms its namespace durability, before a countdown
/// or any preference/account probes.
pub fn first_run_finished() -> Result<bool, String> {
    first_run_complete(&state_root()?)
}

fn first_run_complete(root: &Path) -> Result<bool, String> {
    first_run_complete_with_flush(root, durable_directory)
}

fn first_run_complete_with_flush(
    root: &Path,
    flush: impl FnMut(&Path) -> std::io::Result<()>,
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
        // A previous writer can have published this marker but failed to flush
        // its namespace. Visibility alone never certifies a durable decision.
        flush_record_with(&path, flush)?;
    }
    Ok(finished)
}

// A permission refusal should not reopen account settings on each launch. An
// explicit --apply always retries; automatic setup retries after a short crash
// cooldown, or immediately when Accessibility permission changed.
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

/// What a held item's row says once its automatic tries are spent ([`Host::hold`]).
const HELD: &str = "aterm tried this by itself and it did not take, so it will not try again on its own; Apply in System State, or run aterm doctor --apply, to try again";

/// What it says between its first automatic try and its last ([`SECOND_TRY_AFTER`]).
const WAITING: &str = "aterm tried this by itself and it did not take; it tries once more on its own ten minutes later; Apply in System State, or run aterm doctor --apply, to try now";

/// What a setting says that an automatic pass of this run found as wanted, and that has
/// changed since: the person's change, which setup leaves alone.
const SETTLED: &str = "aterm set this once in this setup and it has changed since, so setup leaves it; Apply in System State, or run aterm doctor --apply, to set it again";

/// How many times an automatic pass of one build makes a change that does not verify.
const TRIES: u32 = 2;

/// How long after a try that did not verify the next one may come: a failure that clears
/// by itself (an app slow to quit, a read that timed out) gets its second try, and one
/// that stands disrupts the person twice, not every minute.
const SECOND_TRY_AFTER: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// How many builds' failed tries the record keeps: two aterm builds taking turns on one
/// state root (an installed app and a dev bundle, an update beside its predecessor) each
/// spend their own tries, and neither re-arms the other's.
const HELD_BUILDS: usize = 16;

/// The record of what this run's automatic passes did by themselves ([`automatic_pass`]).
const HELD_RECORD: &str = "automatic-held.json";

/// One unattended pass. An item an earlier automatic pass of this setup run found as
/// wanted, made or not, is left as it is, so a change the person made since is theirs.
/// An item whose automatic change did not verify is made at most [`TRIES`] times per
/// build, the second [`SECOND_TRY_AFTER`] the first, and held in between and after. A
/// retry on the worker's timer then makes no change it made before. A new run (System
/// State's Apply, a restart of the plan) starts afresh; an explicit `doctor --apply`
/// holds nothing.
fn automatic_pass(
    host: &mut dyn Host,
    root: &Path,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Report, String> {
    let revision = initial::revision_at(root)?;
    let now = now
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut record = Automatic::load(root, revision)?;
    host.hold(&record.holds(build, now));
    let pass = execute_pass(host, root, Action::Apply)?;
    if record.add(build, now, &pass) {
        record.save(root, revision)?;
    }
    Ok(pass.report)
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
    /// The record of setup run `revision`; an empty one for another run.
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
        let failed = record["failed"]
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
            .collect::<Result<_, String>>()?;
        Ok(Self { settled, failed })
    }

    fn save(&self, root: &Path, revision: u64) -> Result<(), String> {
        let failed: Vec<Value> = self
            .failed
            .iter()
            .map(|f| json!({ "item": f.item, "build": f.build, "tries": f.tries, "at": f.at }))
            .collect();
        write_json(
            &root.join(HELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": revision,
                "settled": self.settled, "failed": failed,
            }),
        )
    }

    /// What a pass of `build` at `now` leaves alone, and how.
    fn holds(&self, build: &str, now: u64) -> Vec<(String, Hold)> {
        let mut holds: Vec<(String, Hold)> = self
            .settled
            .iter()
            .map(|item| (item.clone(), Hold::AsIs(SETTLED.into())))
            .collect();
        for failed in self.failed.iter().filter(|f| f.build == build) {
            let why = if failed.tries >= TRIES {
                HELD
            } else if now < failed.at.saturating_add(SECOND_TRY_AFTER.as_secs()) {
                WAITING
            } else {
                continue;
            };
            holds.push((failed.item.clone(), Hold::Back(why.into())));
        }
        holds
    }

    /// Add what a pass of `build` at `now` did; whether the record changed.
    fn add(&mut self, build: &str, now: u64, pass: &Pass) -> bool {
        let before = (self.settled.len(), self.failed.clone());
        for item in &pass.settled {
            if !self.settled.contains(item) {
                self.settled.push(item.clone());
            }
        }
        for item in &pass.unverified {
            match self
                .failed
                .iter_mut()
                .find(|f| f.item == *item && f.build == build)
            {
                Some(failed) => {
                    failed.tries = failed.tries.saturating_add(1);
                    failed.at = now;
                }
                None => self.failed.push(Failed {
                    item: item.clone(),
                    build: build.to_owned(),
                    tries: 1,
                    at: now,
                }),
            }
        }
        let settled = &self.settled;
        self.failed.retain(|f| !settled.contains(&f.item));
        // The builds that tried last keep their record.
        let mut builds: Vec<(u64, &str)> = Vec::new();
        for failed in &self.failed {
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
            self.failed.retain(|f| kept.contains(&f.build));
        }
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
    /// It never may: an isolated instance, sudo, or a HOME that is not the
    /// account's. Each is fixed for the life of the process.
    Refused(String),
    /// The account home or HOME could not be read just now. A later pass may.
    Unresolved(String),
}

impl PersonScope {
    /// The refusal fixed for the life of the process ([`lasting_scope_refusal`]).
    fn lasting(self) -> Option<String> {
        match self {
            Self::Refused(why) => Some(why),
            Self::Person | Self::Unresolved(_) => None,
        }
    }
}

// `defaults` selects its account from the process uid, not $HOME. In particular,
// a lab's scratch HOME is not a sandbox around the real person's preferences.
fn person_scope() -> PersonScope {
    let uid = aterm_uds::peer::our_uid();
    scope_of(
        aterm_types::dirs::runs_under_private_state_root(),
        uid,
        aterm_types::dirs::home_dir().as_deref(),
        || account_home(uid),
    )
}

/// The pure half of [`person_scope`]: `private` whether the process runs under a
/// private state root, `uid` its uid, `home` its `$HOME`, and `account_home` the
/// account record's home, read only once the rest admits the process.
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

/// Why this process may never change the person's macOS profile: an isolated
/// instance, sudo, or a HOME that is not the account's, each fixed for the life
/// of the process, so a caller that gets one need not ask again in it. `None`
/// when it may, and when the account home or HOME cannot be read just now: that
/// passing failure is the profile pass's own error, which it reports and retries.
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
    /// Sample an existing profile pass without creating state or waiting for it.
    /// Release immediately: a status read must not hold up a profile writer.
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
    /// Make later passes run the settings `selected` checks (its workstation part).
    fn select(&mut self, selected: Selection);
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String>;
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String>;
    fn preference_changes(&mut self) -> Result<Vec<Saved>, String>;
    /// The items whose change the apply passes since the last call reached, in run order.
    fn attempts(&mut self) -> Vec<Attempt>;
    /// The items the apply passes since the last call left as wanted: already so, made
    /// so, or not applicable here.
    fn settled(&mut self) -> Vec<String>;
    /// Leave `items` alone in later passes, each as its [`Hold`] says: nothing of one is
    /// changed, backed up, relaunched or journaled.
    fn hold(&mut self, items: &[(String, Hold)]);
    /// Record a change in the run's converge journal before it is made (design §3.3 of
    /// docs/DESIGN-init-system-2026-10-20.md). The caller says what a refusal means.
    fn journal(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String>;
    fn restart(&mut self, name: &str) -> Result<(), String>;
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

/// The shared items of one group's pass: [`aterm_machines::init::prepare_workstation_selected`]
/// on a Mac, a test's own items in a test.
type Prepare =
    Box<dyn FnMut(&dyn System, WorkstationGroup, Selection) -> Result<Workstation, String>>;

/// The `defaults` client every preference read and write of the doctor's own runs.
const DEFAULTS: &str = "/usr/bin/defaults";

/// The doctor's host: the shared engine's items, and the doctor's own reads, writes,
/// relaunches and journal lines, all on one [`System`].
struct NativeHost {
    sys: Box<dyn System>,
    prepare: Prepare,
    selected: Selection,
    domains: BTreeMap<(String, bool), plist::Entries>,
    prepared: Option<Workstation>,
    attempts: Vec<Attempt>,
    settled: Vec<String>,
    held: Vec<(String, Hold)>,
}

impl NativeHost {
    /// The profile on this Mac.
    fn new() -> Self {
        Self::over(
            Box::new(aterm_machines::port::Live::new()),
            Box::new(aterm_machines::init::prepare_workstation_selected),
        )
    }

    /// The settings `selected` checks, on this Mac.
    fn selected(selected: Selection) -> Self {
        let mut host = Self::new();
        host.selected = selected.workstation();
        host
    }

    /// The profile on `sys`, its items from `prepare`.
    fn over(sys: Box<dyn System>, prepare: Prepare) -> Self {
        Self {
            sys,
            prepare,
            selected: Selection::all().workstation(),
            domains: BTreeMap::new(),
            prepared: None,
            attempts: Vec::new(),
            settled: Vec::new(),
            held: Vec::new(),
        }
    }

    /// `group`'s pass, leaving alone what this host holds.
    fn prepare(&mut self, group: WorkstationGroup) -> Result<Workstation, String> {
        let mut prepared = (self.prepare)(self.sys.as_ref(), group, self.selected)?;
        prepared.hold(&self.held);
        Ok(prepared)
    }

    fn entries(&mut self, domain: &str, host: bool) -> Result<&plist::Entries, String> {
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
    fn hold(&mut self, items: &[(String, Hold)]) {
        self.held = items.to_vec();
    }
    fn journal(
        &mut self,
        id: &str,
        from: &str,
        to: &str,
        revert: Option<&str>,
    ) -> Result<(), String> {
        // The journal the shared items write through: one run, one record.
        self.sys.journal()?.before_change(id, from, to, revert)
    }
    fn preference_changes(&mut self) -> Result<Vec<Saved>, String> {
        use aterm_machines::items::defaults::Scope;
        use aterm_machines::model::Read;
        let mut prepared = self.prepare(WorkstationGroup::Preferences)?;
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
                Ok(Saved {
                    target: Target {
                        domain: target.domain,
                        key: target.key,
                        host: target.scope == Scope::Host,
                    },
                    value,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        self.prepared = Some(prepared);
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
        // A timed-out or failed client may already have changed cfprefsd's
        // value. Rollback must reread the machine, never the before-image cache.
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

    fn restart(&mut self, name: &str) -> Result<(), String> {
        let uid = aterm_uds::peer::our_uid().to_string();
        // An absent process needs no restart. killall's only expected failure is 1.
        // The caller journals the relaunch first (`refresh`).
        let killall = Cmd::new("/usr/bin/killall").args(["-u", uid.as_str(), name]);
        match self.sys.run(&killall) {
            Ok(ran) if ran.success() || ran.status == Some(1) => Ok(()),
            Ok(ran) => Err(format!("restart {name}: {}", ran.describe())),
            Err(e) => Err(format!("restart {name}: {e}")),
        }
    }

    fn workstation(&mut self, group: WorkstationGroup, apply: bool) -> Result<Vec<Row>, String> {
        use aterm_machines::model::{Mode, Outcome};
        let mode = if apply { Mode::Apply } else { Mode::Plan };
        let prepared = if group == WorkstationGroup::Preferences
            && let Some(prepared) = self.prepared.take()
        {
            prepared
        } else {
            self.prepare(group)?
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
                            Ok(Saved {
                                target: Target {
                                    domain: target.domain,
                                    key: target.key,
                                    host: target.scope == Scope::Host,
                                },
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
                            .or(row.to)
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
                // A live setting has no command that undoes it; its saved preference
                // (restored and journaled before this) is what a revert rewrites. A
                // refused line does not keep the original from coming back.
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
            rows.push(Row { name: "Original active keyboard mapping".into(), status: "blocked".into(), detail: "saved mappings restored; the previous live HID service mappings were not recorded, so log out and back in to reload them".into() });
        }
        rows
    }
}

/// The row that says the journal refused the lines of changes made anyway: each puts back
/// an original, or relaunches what reads one, and leaving it undone would keep aterm's
/// own change in place.
fn journal_refused(refused: &[String]) -> Row {
    Row {
        name: "Change journal".into(),
        status: "error".into(),
        detail: format!(
            "the journal refused the record of what was put back or relaunched anyway: {}",
            refused.join("; ")
        ),
    }
}

#[derive(Clone)]
struct Saved {
    target: Target,
    value: Option<PValue>,
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
            Ok(Saved { target, value })
        })
        .collect()
}

fn save_backup(path: &Path, keys: &[Saved]) -> Result<(), String> {
    let rows: Vec<Value> = keys
        .iter()
        .map(|s| {
            json!({
                "domain": s.target.domain, "key": s.target.key, "host": s.target.host,
                "xml": s.value.as_ref().map(plist::document),
            })
        })
        .collect();
    write_json(path, &json!({ "profile": PROFILE, "keys": rows }))
}

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    write_json_with_flush(path, value, durable_directory)
}

#[cfg(not(test))]
fn durable_directory(directory: &Path) -> std::io::Result<()> {
    use aterm_machines::port::System as _;
    let sys = aterm_machines::port::Live::new();
    sys.durable_dir(directory)
}

/// [`durable_directory`] under this crate's own unit tests: a plain `fsync(2)`
/// ([`aterm_durable::seam::durable_fsync`], which the counting seam records as durable)
/// in place of `F_FULLFSYNC`. It is still a real flush of the same directory at the same
/// point, so a test sees the same files, renames and errors; what it drops is the
/// drive-cache barrier, which only orders the disk against a power loss and holds up
/// most flushes, creates and renames on the volume while it runs
/// (`docs/measured/flush-wait-2026-10-21.md`). A record's flush walks every ancestor of
/// its directory, so the setup tests' fault sweeps made one full flush per ancestor per
/// write: alone, `every_owned_write_boundary_recovers_the_same_revision_including_mismatched_prefixes`
/// took 19.3 s wall for 0.39 s CPU, and the whole `--lib` run 14.1-14.2 s.
#[cfg(test)]
fn durable_directory(directory: &Path) -> std::io::Result<()> {
    aterm_durable::seam::durable_fsync(&std::fs::File::open(directory)?)
}

fn flush_record_with(
    path: &Path,
    mut flush: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or("doctor record has no parent directory")?;
    for directory in parent.ancestors() {
        flush(directory).map_err(|e| format!("{}: {e}", directory.display()))?;
    }
    Ok(())
}

// The backup precedes writes outside this filesystem. Every namespace entry,
// including one left by an interrupted earlier attempt, must be published too.
// A post-rename flush error may leave the new record visible, but is not success.
fn write_json_with_flush(
    path: &Path,
    value: &Value,
    flush: impl FnMut(&Path) -> std::io::Result<()>,
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
    };
    match host.workstation(group, apply) {
        Ok(rows) if rows.is_empty() => report.rows.push(Row {
            name: name.into(),
            status: "unverified".into(),
            detail: "the shared setup engine returned no settings to verify".into(),
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
    execute_pass(host, root, action).map(|pass| pass.report)
}

/// A profile pass's report, the items whose change it reached without verifying, and
/// the items it left as wanted.
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

fn execute_pass(host: &mut dyn Host, root: &Path, action: Action) -> Result<Pass, String> {
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
    // Subset passes leave the legacy full-profile activation obligation intact:
    // explicit doctor --apply/--restore handles it without restarting unchecked
    // settings here. Selected modern items own precise per-family markers.
    let previous_activation = if full_profile {
        pending_refresh(root)?
    } else {
        Vec::new()
    };
    if action == Action::Check {
        if !previous_activation.is_empty() {
            report.rows.push(Row {
                name: "Preference activation".into(),
                status: "drift".into(),
                detail: "a previous pass still needs activation; run doctor --apply to retry"
                    .into(),
            });
        }
        append_workstation(host, WorkstationGroup::All, false, &mut report);
        return Ok(report.into());
    }
    let mut saved = load_backup(&report.backup)?;
    let before = if WorkstationGroup::Preferences
        .selected(host.selection())
        .is_empty()
    {
        Vec::new()
    } else {
        host.preference_changes()?
    };
    for setting in &before {
        if !saved.iter().any(|s| s.target == setting.target) {
            saved.push(setting.clone());
        }
    }
    // The shared item set supplies exact typed before-images. The recovery
    // record is durable before any preference changes, without another policy.
    if !before.is_empty() {
        save_backup(&report.backup, &saved)?;
    }
    let mut activation = previous_activation.clone();
    for setting in &before {
        if !activation.contains(&setting.target) {
            activation.push(setting.target.clone());
        }
    }
    if full_profile {
        save_pending_refresh(root, &activation)?;
    }
    // An explicit repair also supersedes an earlier completion/restoration.
    // Failed account work must remain retryable until the shared pass verifies it.
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "pending" }),
    )?;
    append_workstation(host, WorkstationGroup::Preferences, true, &mut report);
    // Each item settles alone. One whose change did not verify puts back only its own
    // keys; one that verified keeps its change, so the next pass finds it done instead
    // of making it again. A row that changed nothing (blocked, deferred, unreadable,
    // held) undoes nothing. Undoing every item for one failure made each retry re-apply
    // the rest and relaunch their consumers: the pass never settled.
    let attempts = host.attempts();
    let mut settled = host.settled();
    let mut unverified: Vec<String> = attempts
        .iter()
        .filter(|attempt| !attempt.verified)
        .map(|attempt| attempt.item.clone())
        .collect();
    let rollback: Vec<Saved> = attempts
        .into_iter()
        .filter(|attempt| !attempt.verified)
        .flat_map(|attempt| attempt.preferences)
        .collect();
    // An older pass's activation record, and the keys this rollback rewrote: an item
    // that verified activated its own writes, and nothing else changed.
    let mut reload = previous_activation;
    if !rollback.is_empty() {
        let mut errors = Vec::new();
        let mut restored = Vec::new();
        let mut refused = Vec::new();
        for previous in rollback.iter().rev() {
            let wrote = match restore_one(
                host,
                &previous.target,
                previous.value.as_ref(),
                &mut refused,
            ) {
                Ok(wrote) => {
                    if wrote {
                        restored.push(previous.target.describe());
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
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "restored".into(),
                detail: format!(
                    "the value before this pass is back for what did not verify: {}",
                    restored.join(", ")
                ),
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
    if refresh(host, &reload.iter().collect::<Vec<_>>(), &mut report) && full_profile {
        clear_pending_refresh(root)?;
    }
    // Account sign-out does not wait on an unrelated preference: a held Dock would
    // otherwise keep the selected accounts signed in until a person acted on the Dock.
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

/// Put `target` back to `value`, journaled before the write. `Ok(false)`: it already
/// read `value`, and nothing was written; `Ok(true)`: it was rewritten and reads back.
/// `Err((wrote, why))`: `wrote` says whether a write was made, which may have taken
/// effect, so its consumer reloads either way. A journal that refuses the line does not
/// stop the write, which puts an original back: refusing it would leave aterm's own
/// change in place, and the backup it reads from is the record. The refusal goes to
/// `refused` for the report.
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

/// The journal id of a restore of `target`.
fn restore_id(target: &Target) -> String {
    format!(
        "doctor.restore.{}{}.{}",
        if target.host { "currentHost." } else { "" },
        target.domain,
        target.key
    )
}

/// The `defaults` command that sets `target` to `value`, or deletes it for `None`: what
/// [`NativeHost::write`] runs, and so the revert of a restore that replaces `value`.
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

/// `target` and the value it holds, as a journal line names it.
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

// Policy comparison accepts macOS's equivalent bool/int values. Recovery has
// the stronger contract: restore the original plist type, recursively.
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
    // Stop automatic convergence before any fallible recovery preflight. A
    // corrupt backup or activation record cannot leave automatic setup active.
    // This records the decision, not successful restoration of preferences.
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "restored" }),
    )?;
    let saved = load_backup(&report.backup)?;
    let mut activation = pending_refresh(root)?;
    for saved in &saved {
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
    for item in saved.iter().rev() {
        match restore_one(host, &item.target, item.value.as_ref(), &mut refused) {
            Ok(_) => report.rows.push(Row {
                name: item.target.describe(),
                status: "restored".into(),
                detail: "original value restored".into(),
            }),
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
        clear_pending_refresh(root)?;
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

/// Relaunch the consumers of `targets`, each journaled first. Each relaunch makes a
/// consumer read what is saved (an original put back, or an earlier pass's change), so a
/// journal that refuses its line does not stop it; the refusal is reported.
fn refresh(host: &mut dyn Host, targets: &[&Target], report: &mut Report) -> bool {
    let mut refreshed = true;
    let mut refused = Vec::new();
    for (process, affected) in [
        (
            "Dock",
            targets
                .iter()
                .any(|t| t.domain == "com.apple.dock" || t.domain == "com.apple.WindowManager"),
        ),
        (
            "Finder",
            targets.iter().any(|t| t.domain == "com.apple.finder"),
        ),
        (
            "NotificationCenter",
            targets
                .iter()
                .any(|t| t.domain == "com.apple.notificationcenterui"),
        ),
    ] {
        if !affected {
            continue;
        }
        // A relaunch has nothing to undo: the line records that it was made, and why.
        let id = format!("doctor.relaunch.{process}");
        if let Err(why) = host.journal(
            &id,
            &format!("{process} running with the preferences it read at its start"),
            &format!("{process} relaunched to read its saved preferences"),
            None,
        ) {
            refused.push(format!("{id}: {why}"));
        }
        if let Err(detail) = host.restart(process) {
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

    /// A doctor host whose shared items are the [`wanted`] keys, one item each (its id is
    /// the key's [`Target::describe`]), and whose account group is one sign-out row. It
    /// holds what it is told to as the shared engine does ([`Hold`]); the engine's own
    /// hold is pinned in aterm-machines, and [`NativeHost`]'s by the tests over [`Inert`].
    #[derive(Default)]
    struct Fake {
        selected: Option<Selection>,
        preference_passes: usize,
        values: BTreeMap<Target, PValue>,
        writes: Vec<Target>,
        /// The first item applied reports `error` after its write, once.
        fail_once: bool,
        /// Items whose read-back never verifies: each apply writes and reports `error`,
        /// as the Dock did when it wrote its own tiles back over the change.
        failing: Vec<Target>,
        /// Items whose consumer writes the value before the change back over it: each
        /// apply writes, the key reads as it did, and the row reports `error`.
        written_back: Vec<Target>,
        /// Items that stand blocked (a grant not given, a read that does not answer):
        /// their apply changes nothing and reports `blocked`.
        blocked: Vec<Target>,
        accounts_blocked: bool,
        /// The account sign-out's apply runs and its read-back never verifies.
        accounts_fail: bool,
        fail_restart_once: bool,
        /// The journal refuses every line of the doctor's own (`doctor.…`).
        journal_refuses: bool,
        /// An apply pass of the preference group panics, as a process that dies there.
        dies_in_preferences: bool,
        restarts: Vec<String>,
        account_passes: usize,
        attempts: Vec<Attempt>,
        settled: Vec<String>,
        held: Vec<(String, Hold)>,
        /// Every journal line, write and relaunch, in order: `journal <id>`,
        /// `refused <id>`, `write <target>`, `restart <process>`.
        events: Vec<String>,
        /// Each journal line's id, from, to and revert.
        lines: Vec<(String, String, String, Option<String>)>,
        runtime_changed: bool,
    }

    impl Fake {
        /// The item a wanted key belongs to, as the fake's rows and attempts name it.
        fn item(target: &Target) -> String {
            target.describe()
        }

        /// Writes and relaunches, the changes a pass makes outside aterm's own records.
        fn changes(&self) -> usize {
            self.events
                .iter()
                .filter(|event| event.starts_with("write ") || event.starts_with("restart "))
                .count()
        }

        /// How a pass holds `item`, if it does.
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

    /// The doctor of a selected Dock and widgets whose Dock change never verifies (the
    /// Dock wrote its own tiles back over it), for the stage tests in `initial`: each
    /// [`NeverVerifies::pass`] is what one worker retry runs, a minute after the last.
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

        /// What `apply_on_first_run_at` runs once the stage is due, a minute after the
        /// last, past the crash cooldown.
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

    /// A [`System`] that runs nothing: each command is recorded and refused, every file
    /// operation is unsupported, and its journal keeps each line's id. A [`NativeHost`]
    /// over it never reaches this Mac's preferences, processes or journal.
    #[derive(Default)]
    pub(super) struct Inert {
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    /// [`Inert`]'s journal.
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
                Err(RunFailed::Refused(
                    "an inert test system runs nothing".into(),
                ))
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
            fn journal(&self) -> Result<Box<dyn aterm_converge::Journal + '_>, String> {
                Ok(Box::new(super::InertJournal(std::sync::Arc::clone(
                    &self.lines,
                ))))
            }
        }
    }

    /// A shared item whose probes answer from a list (the last one repeats), counting its
    /// applies; its clones share both, so a pass prepared anew sees the same item.
    #[derive(Clone)]
    struct Scripted {
        id: &'static str,
        probes: std::sync::Arc<std::sync::Mutex<Vec<aterm_machines::model::Probe>>>,
        applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Scripted {
        /// `wanted[i]`: whether the `i`th probe reads as wanted.
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

    /// The worker's own entry (`apply_on_first_run_with`, what each System State tick
    /// runs) over the doctor's own host ([`NativeHost`], on an [`Inert`] system and the
    /// test's shared items) holds what its record says: an item whose change never takes
    /// is applied twice per build, ten minutes apart, and one that took and was changed
    /// since is left as it is, while the host runs no command and journals only the
    /// applies. Negative controls: `NativeHost::prepare` without its `hold`, or the entry
    /// calling `execute` in place of `automatic_pass`, applies both on every tick.
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
        fn select(&mut self, selected: Selection) {
            self.selected = Some(selected.workstation());
        }
        fn attempts(&mut self) -> Vec<Attempt> {
            std::mem::take(&mut self.attempts)
        }
        fn settled(&mut self) -> Vec<String> {
            std::mem::take(&mut self.settled)
        }
        fn hold(&mut self, items: &[(String, Hold)]) {
            self.held = items.to_vec();
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
        fn preference_changes(&mut self) -> Result<Vec<Saved>, String> {
            self.preference_passes += 1;
            self.attempts.clear();
            self.settled.clear();
            Ok(wanted()
                .into_iter()
                .filter(|(target, _)| {
                    self.selection()
                        .contains(if target.domain == "com.apple.dock" {
                            Setting::Dock
                        } else {
                            Setting::Widgets
                        })
                })
                // A held item changes nothing, so nothing of it is backed up.
                .filter(|(target, _)| self.hold_of(&Self::item(target)).is_none())
                .filter(|(t, v)| !self.values.get(t).is_some_and(|now| now.same(v)))
                .map(|(target, _)| Saved {
                    value: self.values.get(&target).cloned(),
                    target,
                })
                .collect())
        }
        fn restart(&mut self, name: &str) -> Result<(), String> {
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
                for (target, value) in wanted() {
                    if !self
                        .selection()
                        .contains(if target.domain == "com.apple.dock" {
                            Setting::Dock
                        } else {
                            Setting::Widgets
                        })
                    {
                        continue;
                    }
                    let correct = self.values.get(&target).is_some_and(|now| now.same(&value));
                    let mut status = if correct { "ok" } else { "drift" };
                    let item = Self::item(&target);
                    let mut detail = "injected shared item".to_owned();
                    if apply && !correct {
                        match self.hold_of(&item) {
                            // The shared engine's holds: nothing of it runs.
                            Some(Hold::Back(why)) => {
                                status = "blocked";
                                detail = why;
                            }
                            Some(Hold::AsIs(why)) => {
                                status = "ok";
                                detail = why;
                            }
                            None if self.blocked.contains(&target) => status = "blocked",
                            None => {
                                let before = self.values.get(&target).cloned();
                                // The engine journals each item before its apply.
                                self.events.push(format!("journal {item}"));
                                self.write(&target, Some(&value))?;
                                self.runtime_changed = true;
                                let written_back = self.written_back.contains(&target);
                                if written_back {
                                    match &before {
                                        Some(before) => {
                                            self.values.insert(target.clone(), before.clone());
                                        }
                                        None => {
                                            self.values.remove(&target);
                                        }
                                    }
                                }
                                let failed = std::mem::take(&mut self.fail_once)
                                    || written_back
                                    || self.failing.contains(&target);
                                status = if failed { "error" } else { "applied" };
                                self.attempts.push(Attempt {
                                    item: item.clone(),
                                    verified: !failed,
                                    preferences: vec![Saved {
                                        target: target.clone(),
                                        value: before,
                                    }],
                                });
                            }
                        }
                    }
                    if apply && matches!(status, "ok" | "applied") {
                        self.settled.push(item);
                    }
                    rows.push(Row {
                        name: target.describe(),
                        status: status.into(),
                        detail,
                    });
                }
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
                            // The sign-out drives the app's own controls; it changes no key.
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
                write_json_with_flush(&marker, &value, |_| Err(std::io::Error::other(
                    "failed directory flush"
                )))
                .is_err()
            );
            assert!(marker.exists(), "the rename is visible even on failure");
            assert!(
                first_run_complete_with_flush(root.path(), |_| Err(std::io::Error::other(
                    "still not durable"
                )))
                .is_err()
            );
            let mut reached_root = false;
            assert!(
                first_run_complete_with_flush(root.path(), |directory| {
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
                |_| Err(std::io::Error::other("failed plan flush"))
            )
            .is_err()
        );
        assert!(
            authorized_selection_with_flush(root.path(), |_| Err(std::io::Error::other(
                "still not durable"
            )))
            .is_err()
        );
        assert_eq!(
            authorized_selection_with_flush(root.path(), |_| Ok(())).unwrap(),
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
        // Sign-out does not wait on an unrelated preference that did not verify.
        assert_eq!(host.account_passes, 1);
        assert!(!first_run_complete(root.path()).expect("failed pass remains pending"));
        assert!(report.backup.exists());
    }

    /// The worker's clock, `minutes` after a fixed start.
    fn at(minutes: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_800_000_000 + minutes * 60)
    }

    const BUILD: &str = "1.0.0+gaaaa";

    /// The overnight loop of 2026-10-22: the Dock wrote its own tiles back over the
    /// change, so the item's read-back failed on every attempt, and each retry of the
    /// worker re-made the change, relaunched the Dock, rolled back and relaunched it
    /// again, every minute for hours. An automatic pass now makes a change that does not
    /// verify twice per build and setup run, the second ten minutes after the first:
    /// every other retry holds the item, so it writes, journals and relaunches nothing,
    /// while the item that verified stays applied. Negative control: a pass that holds
    /// nothing (`host.hold(&[])` in `automatic_pass`) writes and relaunches on every retry.
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
        // Every retry it holds the two items: no write, relaunch or journal line.
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
        // Ten minutes after the first, the second try; then each is held for good.
        automatic_pass(&mut host, root.path(), BUILD, at(10)).expect("second try");
        assert_eq!(host.restarts, ["Dock", "Dock"]);
        assert_eq!(sign_outs(&host), 2);
        quiet(&mut host, 11..40, HELD);
        assert!(host.values.contains_key(&widgets));
        assert!(!first_run_complete(root.path()).unwrap());
        // A new build may try each once more.
        automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(41)).expect("new build");
        assert_eq!(host.restarts.len(), 3);
        assert_eq!(sign_outs(&host), 3);
        // The record is the run's: a new run (System State's Apply) holds nothing of it.
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
        // An explicit apply is a person asking: its new host holds nothing.
        host.held.clear();
        execute(&mut host, root.path(), Action::Apply).expect("explicit apply");
        assert_eq!(host.restarts.len(), 4);
    }

    /// Two aterm builds taking turns on one state root (the installed app and a dev
    /// bundle, an update beside its predecessor) each spend their own tries: neither
    /// re-arms the other's, so a change that never takes disrupts at most twice per
    /// build. Negative control: a record of one build's tries, which the other build
    /// reads as none, re-arms the item on every alternate pass.
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

    /// The record keeps the [`HELD_BUILDS`] builds that tried last: a build past the
    /// bound drops the one whose last try is oldest, not one that tried again since,
    /// so a build still in use keeps its spent tries however many builds came between.
    /// Negative control: keeping the builds whose last try is oldest (sorting ascending
    /// in `Automatic::add`) drops the build that tried last and re-arms it.
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
        // Build 0 tries again last, spending its tries; then one build more comes.
        now += 1;
        record.add(&build(0), now, &failed());
        now += 1;
        record.add(&build(HELD_BUILDS), now, &failed());
        let builds: Vec<&str> = record.failed.iter().map(|f| f.build.as_str()).collect();
        assert_eq!(builds.len(), HELD_BUILDS, "{builds:?}");
        assert!(!builds.contains(&build(1).as_str()), "{builds:?}");
        assert_eq!(
            record.holds(&build(0), now),
            [("dock".to_owned(), Hold::Back(HELD.into()))],
            "{builds:?}"
        );
        assert!(record.holds(&build(1), now).is_empty());
    }

    /// What an automatic pass found as wanted is left as it is afterwards: a setting it
    /// made that the person changed since is not made again while setup waits on another
    /// item, and its row needs nothing. Before, each retry re-applied it over the
    /// person's change, relaunching its consumer, for as long as any row kept setup
    /// pending. Once the rest verifies, setup completes with the person's value standing.
    /// Negative control: holding only what failed (no `settled` holds in
    /// `Automatic::holds`) writes the widgets key again on the next retry.
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
        // The person shows their widgets again.
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
        // The Dock takes on a new build's try, and setup completes as the person left it.
        host.failing.clear();
        let report = automatic_pass(&mut host, root.path(), "1.0.1+gbbbb", at(31)).expect("done");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert!(first_run_complete(root.path()).unwrap());
        assert!(!host.values.contains_key(&widgets));
    }

    /// A failure that clears by itself (an app slow to quit, a read that timed out) gets
    /// its second try ten minutes after the first, and unattended setup completes; the
    /// retries in between change nothing. Negative control: one try per build
    /// (`TRIES = 1`) leaves setup pending for good.
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

    /// A held retry changes nothing, so it leaves no activation record naming the held
    /// item's keys: a pass that dies part way (a quit, a crash, an update relaunch) does
    /// not make the next process relaunch the Dock for a key nobody wrote. Negative
    /// control: backing up a held item's keys (the hold dropped from the fake's
    /// `preference_changes`, as from `Workstation::preference_changes`, which
    /// aterm-machines' `a_held_item_has_no_before_images` pins) leaves the Dock key there.
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

    /// A journal that refuses a line never keeps an original from coming back: a rollback
    /// and a Restore put each value back and relaunch its consumer anyway, and the report
    /// says the journal refused, since refusing would leave aterm's own change in place.
    /// Negative control: returning on the refusal in `restore_one` (in `refresh`) leaves
    /// the Dock key changed (the Dock not relaunched).
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

    /// A rollback relaunches only the consumers whose keys it rewrote: when the consumer
    /// already wrote the original back over the change (as the Dock did), nothing is put
    /// back and nothing is relaunched for it. Negative control: dropping the `wrote` gate
    /// of the rollback in `execute_pass` relaunches the Dock for a key nobody rewrote.
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

    /// A standing row (a grant not given, a read that does not answer) changes nothing,
    /// so it undoes nothing either: the item that verified keeps its change, and the next
    /// pass finds it done instead of re-applying it and relaunching its consumer. The
    /// whole-pass rollback this replaces undid every attempted item whenever any row
    /// needed attention, so each retry re-applied them. Negative control: rolling back
    /// every attempt (dropping the `!attempt.verified` filter of `rollback` in
    /// `execute_pass`) undoes the widgets change and makes the second pass write again.
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
        // Once the blocked condition lifts, the next pass applies just that item.
        host.blocked.clear();
        let report = execute(&mut host, root.path(), Action::Apply).expect("unblocked");
        assert!(!report.needs_attention(), "{}", report.to_text());
        assert_eq!(&host.writes[1..], [dock]);
        assert!(first_run_complete(root.path()).unwrap());
    }

    /// Every change the doctor makes itself, a rollback's or a Restore's preference write
    /// and each consumer relaunch, is journaled immediately before it is made, a write
    /// naming the value it replaces and the one it writes, with the `defaults` command
    /// that reverts it. Negative control: dropping the `host.journal` call of
    /// `restore_one` (or of `refresh`) leaves a write (a relaunch) with no line before it.
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
                vec![Fake::item(&target), restore_id(&target)]
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
            // The values: aterm's (`true`, `false`), and the original put back (absent).
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

        // The record may already be visible when its namespace flush fails.
        // That failure must not authorize the external preference writes.
        let record = root
            .path()
            .join("new-state")
            .join("doctor")
            .join("backup.json");
        let value = json!({"profile": PROFILE, "keys": []});
        let mut attempted = Vec::new();
        let refused = write_json_with_flush(&record, &value, |directory| {
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
        write_json_with_flush(&record, &value, |directory| {
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

    /// A real first run keeps its setup: HOME is the account's own home, as
    /// written or through a link, so the profile may change and the launch's
    /// startup gate (`first_run_doctor_eligible` in crates/aterm) has no lasting
    /// refusal to stop it. A scratch HOME, a private state root and sudo are
    /// refused for the life of the process, so such a launch starts no setup
    /// worker and says nothing (crates/aterm/tests/session_launch_silence.rs). A
    /// home that cannot be read just now is no lasting refusal: the worker
    /// reports it and retries.
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
