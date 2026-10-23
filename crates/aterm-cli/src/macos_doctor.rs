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

/// Whether this process holds Full Disk Access, measured once per pass and without a
/// prompt (`aterm_containment::probe_fda`: a child of this process opens the TCC database,
/// closed unread). The probe is fenced to an `.app`, so a test binary or a bare build reads
/// `None`, not measured, which counts as no grant: Notification Center's widgets then wait
/// for it, and no `defaults` read or write of its container is made
/// ([`aterm_machines::items::defaults::withheld`]). `aterm init` is handed this too.
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

/// What it says between its first automatic try and its last ([`SECOND_TRY_AFTER`]): the
/// second is setup's first try once that has passed or, for the follow-up of a setup that has
/// finished ([`follow_up_at`]), aterm's first launch after it.
const WAITING: &str = "aterm tried this by itself and it did not take; it tries once more on its own when ten minutes have passed, on setup's next try or at aterm's next launch; Apply in System State, or run aterm doctor --apply, to try now";

/// What a setting says that an automatic pass of this run found as wanted, and that has
/// changed since: the person's change, which setup leaves alone.
const SETTLED: &str = "aterm set this once in this setup and it has changed since, so setup leaves it; Apply in System State, or run aterm doctor --apply, to set it again";

/// What a setting says that an automatic pass of this run settled for the keys it reached,
/// with a key it left for want of Full Disk Access ([`WITHHELD_RECORD`]: Notification
/// Center's widget list) still owed, once the pass holds the grant and so reads that key too:
/// aterm never wrote it, and nobody changed it since; it is the follow-up's
/// ([`follow_up_at`]), whose row in the same report says how that went.
const SETTLED_OWED: &str = "Notification Center's widget list is left to setup's own step for it, whose row says how that went; aterm set the rest once in this setup and leaves it as it is now; Apply in System State, or run aterm doctor --apply, to set it all now";

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
/// holds nothing. Once the pass holds Full Disk Access it reaches the keys an earlier pass
/// left for want of it, which the follow-up owes ([`WITHHELD_RECORD`]): a settled item that
/// owes one reads that key too, and is held as it is with [`SETTLED_OWED`], not [`SETTLED`];
/// an item whose change to that key did not verify, made by an automatic pass or by the
/// follow-up, is held by that record's tries as well, so one budget of tries covers that key
/// whichever of them makes the try.
fn automatic_pass(
    host: &mut dyn Host,
    root: &Path,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Report, String> {
    let revision = initial::revision_at(root)?;
    let now = epoch_secs(now);
    let mut record = Automatic::load(root, revision)?;
    // Without the grant the pass reads only the keys a settled item reached, which aterm set
    // ([`SETTLED`] is true of them), and none of the keys the follow-up's tries are spent on.
    // An unreadable record is the follow-up's to report.
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

/// `now` in seconds since the epoch, as the records keep a try's time.
fn epoch_secs(now: std::time::SystemTime) -> u64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The tries a record keeps ([`Failed`]) in its `entries`.
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

/// `failed`, as a record keeps it.
fn tries_json(failed: &[Failed]) -> Vec<Value> {
    failed
        .iter()
        .map(|f| json!({ "item": f.item, "build": f.build, "tries": f.tries, "at": f.at }))
        .collect()
}

/// What `failed` holds back in a pass of `build` at `now`: an item it tried [`TRIES`] times
/// ([`HELD`]), and one it tried less than [`SECOND_TRY_AFTER`] ago ([`WAITING`]).
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

/// Count a try of each of `items` by `build` at `now` in `failed`, and keep the tries of
/// the [`HELD_BUILDS`] builds that tried last.
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
    // The builds that tried last keep their record.
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
        let failed = load_tries(&record["failed"], &bad)?;
        Ok(Self { settled, failed })
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

    /// What a pass of `build` at `now` leaves alone, and how. `owed` is the follow-up's record
    /// as a pass that reaches its keys reads it (empty for one that does not). A settled item
    /// is left as it is, with [`SETTLED_OWED`] when `owed` names it, else [`SETTLED`]. Any
    /// other item is held back by its tries in this record and, for one `owed` names, by
    /// that record's too ([`held_back`]): HELD over WAITING, since the engine takes an id's
    /// first hold.
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

    /// Add what a pass of `build` at `now` did; whether the record changed.
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
    /// What the pass measured of this process's Full Disk Access, once
    /// ([`aterm_machines::port::System::full_disk_access`]); `None` counts as no grant.
    fn full_disk_access(&self) -> Option<bool>;
    /// Make later passes run the settings `selected` checks (its workstation part).
    fn select(&mut self, selected: Selection);
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String>;
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String>;
    /// The before-images of what `group`'s pass (the selected preferences, or a follow-up's
    /// [`WorkstationGroup::Withheld`] keys) would change, prepared for its apply.
    fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String>;
    /// The items whose change the apply passes since the last call reached, in run order.
    fn attempts(&mut self) -> Vec<Attempt>;
    /// The items the apply passes since the last call left as wanted: already so, made
    /// so, not applicable here, or held as they are ([`Host::hold`]); one that left a key
    /// alone for want of Full Disk Access ([`Host::withheld`]) too, by what it did with the
    /// keys it reached, so a later automatic pass of the run holds it as it is (it writes
    /// none of its keys, which the person may have set since) and the follow-up
    /// ([`follow_up_at`]) alone writes the key it left.
    fn settled(&mut self) -> Vec<String>;
    /// The items the apply passes since the last call left keys alone in, in another app's
    /// container, for want of Full Disk Access, held or not
    /// ([`aterm_machines::init::WorkstationRun::withheld`]); the record adds only one that
    /// ran ([`record_withheld`]).
    fn withheld(&mut self) -> Vec<String>;
    /// Leave `items` alone in later passes, each as its [`Hold`] says: nothing of one is
    /// changed, backed up, relaunched or journaled.
    fn hold(&mut self, items: &[(String, Hold)]);
    /// The items later passes leave alone ([`Host::hold`]), by id.
    fn held(&self) -> Vec<String>;
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
    /// The pass [`Host::preference_changes`] prepared, and its group.
    prepared: Option<(WorkstationGroup, Workstation)>,
    attempts: Vec<Attempt>,
    settled: Vec<String>,
    withheld: Vec<String>,
    held: Vec<(String, Hold)>,
}

impl NativeHost {
    /// The profile on this Mac, with this process's Full Disk Access measured once for the
    /// pass: its shared items read it from their context, its own reads and writes from
    /// the port ([`withheld`]).
    fn new() -> Self {
        Self::over(
            Box::new(aterm_machines::port::Live::new().with_full_disk_access(full_disk_access())),
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
            withheld: Vec::new(),
            held: Vec::new(),
        }
    }

    /// Whether this pass reads `target` where macOS shows it ([`Saved::granted`]).
    fn granted(&self, target: &Target) -> bool {
        !contained(target) || self.sys.full_disk_access() == Some(true)
    }

    /// `group`'s pass, leaving alone what this host holds.
    fn prepare(&mut self, group: WorkstationGroup) -> Result<Workstation, String> {
        let mut prepared = (self.prepare)(self.sys.as_ref(), group, self.selected)?;
        prepared.hold(&self.held);
        Ok(prepared)
    }

    fn entries(&mut self, domain: &str, host: bool) -> Result<&plist::Entries, String> {
        // Never an export of another app's container without the grant: macOS would tell
        // the person aterm tried to read another app's data, and print an empty domain.
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
        // The journal the shared items write through: one run, one record.
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
            // A row that left a key alone for want of Full Disk Access is settled by what it
            // did, so a later automatic pass holds the whole item as it is and writes none of
            // its keys again; the key it left is the follow-up's ([`follow_up_at`]), which no
            // hold of the run touches, through the record of what was withheld. A later pass
            // with the grant reads that key too, and says it is the follow-up's
            // ([`SETTLED_OWED`]).
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
    /// Whether the value was read where macOS shows it: always for an open domain; for a
    /// key of another app's container ([`contained`]), only by a pass that held Full Disk
    /// Access. Without it, macOS showed that pass an empty domain, so its absent value was
    /// the denial, not the person's ([`unknown_original`]). A backup written before this
    /// field reads it as held for a value and not for an absence.
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
            // An earlier aterm wrote no grant: a value it read was there to read, and an
            // absence in another app's container may have been macOS hiding it.
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
    write_json_with_flush(path, value, durable_directory)
}

fn durable_directory(directory: &Path) -> std::io::Result<()> {
    use aterm_machines::port::System as _;
    let sys = aterm_machines::port::Live::new();
    sys.durable_dir(directory)
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
        WorkstationGroup::Withheld => "Settings waiting for Full Disk Access",
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

/// A pass a person asked for (`doctor --apply`, `--check`, `--restore`), as this build, now.
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

/// A profile pass of `build` at `now` (seconds since the epoch): what a change that did not
/// verify spends its tries against ([`TRIES`]).
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
    // Subset passes leave the legacy full-profile activation obligation intact:
    // explicit doctor --apply/--restore handles it without restarting unchecked
    // settings here. Selected modern items own precise per-family markers.
    let previous_activation = if full_profile {
        pending_refresh(root)?
    } else {
        Vec::new()
    };
    if action == Action::Check {
        // A relaunch owed for a key this pass may not reach waits for the grant: it is
        // not drift a person can apply away.
        let (left, due): (Vec<&Target>, Vec<&Target>) = previous_activation
            .iter()
            .partition(|target| withheld(host, target).is_some());
        if !due.is_empty() {
            report.rows.push(Row {
                name: "Preference activation".into(),
                status: "drift".into(),
                detail: "a previous pass still needs activation; run doctor --apply to retry"
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

/// The preference part of an apply pass over `group`: the selected rows
/// ([`WorkstationGroup::Preferences`], after which first-run.json reads pending until the
/// pass ends well), or a follow-up's keys ([`WorkstationGroup::Withheld`], which writes no
/// setup record but its own). It backs up, applies, rolls back what did not verify,
/// relaunches, and records which items wait for Full Disk Access ([`WITHHELD_RECORD`]), a try
/// of one whose change reached that key and did not verify spent by `turn` (the build, and
/// the time in seconds since the epoch). The items whose change did not verify, and those
/// left as wanted.
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
            // An earlier aterm saved this while macOS hid the app's container from it: its
            // absence was the denial, not the person's value. This read holds the grant.
            Some(old) if !old.granted && setting.granted => *old = setting.clone(),
            Some(_) => {}
        }
    }
    // The shared item set supplies exact typed before-images. The recovery
    // record is durable before any preference changes, without another policy.
    // The shared items' own writes need no relaunch record here: each relaunches what
    // reads it under its own durable marker (aterm-machines' `init_activation`), so a
    // pass that ends between those writes and that relaunch makes the next one relaunch
    // each reader once, not once for the marker and again for this record.
    if !before.is_empty() {
        save_backup(&report.backup, &saved)?;
    }
    if group == WorkstationGroup::Preferences {
        // An explicit repair also supersedes an earlier completion/restoration.
        // Failed account work must remain retryable until the shared pass verifies it.
        write_json(
            &root.join("first-run.json"),
            &json!({ "profile": PROFILE, "state": "pending" }),
        )?;
    }
    append_workstation(host, group, true, report);
    // Each item settles alone. One whose change did not verify puts back only its own
    // keys; one that verified keeps its change, so the next pass finds it done instead
    // of making it again. A row that changed nothing (blocked, deferred, unreadable,
    // held) undoes nothing. Undoing every item for one failure made each retry re-apply
    // the rest and relaunch their consumers: the pass never settled.
    let attempts = host.attempts();
    let settled = host.settled();
    // An item this pass held ran nothing, its keys untouched: it reached no key as wanted
    // and left none alone, so the record neither drops nor adds it (one recorded keeps its
    // place). Else a pass without the grant that holds as it is an item a pass with the
    // grant settled would record it, and the follow-up would write over the person's change
    // since. One that ran and left a key alone again stays recorded.
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
    // An older pass's activation record, and the keys this rollback rewrote: an item
    // that verified activated its own writes, and nothing else changed. A key of the
    // older record that an item of this pass wrote again and verified was relaunched for
    // after that write, which read whatever the record was kept for.
    let mut reload: Vec<Target> = previous_activation
        .into_iter()
        .filter(|target| !activated.contains(target))
        .collect();
    if !rollback.is_empty() {
        // The rollback's writes are the doctor's own: recorded before they are made, so
        // a pass that ends among them relaunches their readers on the next. A record that
        // cannot be saved does not keep the originals from coming back: this pass still
        // relaunches their readers, and the report says so.
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
    // After the rollback: a record that cannot be kept does not keep originals away.
    if let Err(detail) = record_withheld(root, &left_alone, &reached, &unverified, turn) {
        report.rows.push(Row {
            name: "Settings waiting for Full Disk Access".into(),
            status: "error".into(),
            detail,
        });
    }
    Ok((unverified, settled))
}

/// The record of the items an apply pass left keys alone in, in another app's container,
/// for want of Full Disk Access (Notification Center's widget list): a pass that reaches
/// them as wanted with the grant drops them, and [`follow_up_at`] reaches them alone once the
/// grant is held. A change that reached those keys and did not verify keeps its item, and
/// spends one of its tries ([`TRIES`] per build, [`SECOND_TRY_AFTER`] apart), whichever pass
/// made it. The tries belong to the setup run that spent them, as [`HELD_RECORD`]'s do: a new
/// run of the plan (System State's Apply, or its answer to the startup review) starts them
/// afresh, and keeps what the record owes. A Restore drops it.
const WITHHELD_RECORD: &str = "withheld-preferences.json";

/// What [`WITHHELD_RECORD`] holds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Withheld {
    /// The items that left keys alone, in run order.
    items: Vec<String>,
    /// The tries of a change that reached their keys and did not verify, per item and build.
    failed: Vec<Failed>,
}

/// What [`WITHHELD_RECORD`] holds; nothing when there is no record, and no tries when another
/// setup run spent them ([`initial::revision_at`]).
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
    let mut failed = load_tries(&record["failed"], &bad)?;
    if record["revision"].as_u64() != initial::revision_at(root).ok() {
        failed.clear();
    }
    Ok(Withheld { items, failed })
}

/// Save [`WITHHELD_RECORD`] as `record`, its tries the current setup run's, or remove it when
/// it names no item.
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

/// Drop from [`WITHHELD_RECORD`] the items a pass reached as wanted (`reached`) and add those
/// it ran and left keys alone in (`left_alone`): an item in both stays, and one the pass held
/// is in neither, so it keeps whatever place the record gives it. Each recorded item whose
/// change did not verify (`unverified`) and that reached its keys there (it left none alone)
/// spends one of its tries, as `turn` (the build, and the time in seconds since the epoch).
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

/// Reach, once this process holds Full Disk Access, what earlier apply passes left alone in
/// another app's container for want of it ([`WITHHELD_RECORD`]: Notification Center's
/// widget list): those keys alone ([`WorkstationGroup::Withheld`]), backed up first, its
/// readers alone relaunched (never the Dock); no other key is read, so a person's change to
/// them since stays theirs. It runs whether or not the Workstation stage is done: the setup
/// run's automatic passes hold the item as it is once it settled for the keys it reached, so
/// this is what reaches the key it left. Nothing runs without a record or without the grant,
/// nor after the person cancelled or restored setup, nor for a row unchecked in a setup
/// choice that still runs Mac settings (one that runs none, as the vault's remedy asks,
/// withdraws nothing); it writes no setup record (no offer, countdown or question follows
/// from it).
/// A follow-up that reached its keys as wanted drops them from the record, so it runs once;
/// one that could not (away from the console) is left for the next; one whose change did not
/// verify is rolled back and stays, and is tried again as an automatic pass's change is,
/// [`TRIES`] times per build (`build`, at `now`), the second once [`SECOND_TRY_AFTER`] has
/// passed, held in between and after, writing and relaunching nothing for it then; an
/// automatic pass with the grant spends and honours the same tries ([`automatic_pass`]).
/// That second try is the first follow-up after it is due: setup's next worker pass while
/// setup runs, or, once setup has finished, aterm's next launch
/// ([`finish_withheld_preferences`]). The caller holds the profile lock. `None`: nothing
/// ran.
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
    // The current setup choice: a row the person unchecked since stays as it is. A choice
    // with every Mac settings row off (the vault's remedy) runs none of them again, and is
    // no answer about this one.
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
        // Nothing still chosen that this build knows names those items: nothing to follow up.
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
    // A follow-up is no automatic retry of the setup run's changes, and none of its holds
    // applies; its record's tries hold an item whose change to its keys did not verify,
    // whichever pass made it.
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

/// Clear, once this aterm holds Full Disk Access, what earlier passes of the profile left
/// alone in another app's container for want of it (Notification Center's widget list),
/// silently ([`follow_up_at`]): what a window whose setup has finished calls at launch (a
/// setup still running reaches it after each pass of its stages). It reads only aterm's
/// record until there is one, and then measures the grant without a prompt. `reserve` takes
/// the caller's in-process slot for profile work (System State's busy slot), held from
/// before the profile lock until after it is released ([`follow_up_finished_at`]). `None`:
/// nothing ran.
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

/// [`finish_withheld_preferences`] of `root`, over the host `host` makes, for `build` at
/// `now`. With a record that names something to reach, a setup the person has not stopped,
/// and the grant (measured before either is taken), it takes `reserve`'s slot, then the
/// profile lock, and releases the lock before the slot: this process's System State, which
/// serializes its operations on that slot, shows the follow-up busy and its setup worker
/// waits for it, instead of failing on, or backing off from, a lock nobody they can see
/// holds. Otherwise it takes neither, so a launch with nothing it could reach never shows
/// busy; [`follow_up_at`] checks the stop and the grant again under the lock. Another pass
/// holding the lock: nothing runs, and the next launch tries.
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

/// [`follow_up_at`] for a caller that holds the profile lock: a running setup's worker,
/// after its stages. It measures the grant only when a record names something to reach.
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

/// Why a saved before-image is not the original and is left as it is: a key of another
/// app's container ([`contained`]) recorded as absent by a pass that held no Full Disk
/// Access (an earlier aterm read it so while macOS hid the container from it), whose
/// absence was the denial. Nothing is deleted or relaunched for it.
fn unknown_original(saved: &Saved) -> Option<String> {
    (contained(&saved.target) && !saved.granted && saved.value.is_none()).then(|| {
        format!(
            "{} was saved before aterm had Full Disk Access, while macOS hid it from aterm, \
             so its original is unknown; it is left as it is",
            saved.target.describe()
        )
    })
}

/// Whether `target` is in another app's sandbox container
/// ([`aterm_machines::items::defaults::CONTAINED`]).
fn contained(target: &Target) -> bool {
    aterm_machines::items::defaults::contained(&target.domain)
}

/// After a relaunch of what pending-refresh.json records: keep in it only what this pass
/// left alone for want of Full Disk Access ([`withheld`]), for a pass with the grant to
/// relaunch; remove it when nothing is left.
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

/// Why this pass leaves `target` alone: it is in another app's sandbox container and the
/// pass holds no Full Disk Access ([`aterm_machines::items::defaults::withheld`]). A
/// before-image of one is not read, put back or relaunched for, in a rollback or a Restore,
/// and the backup keeps it for a Restore the person runs again with the grant (which puts it
/// back unless it is an [`unknown_original`]; nothing reruns a Restore, so its row says so,
/// [`restore_waits`]); a relaunch owed for one stays in pending-refresh.json for a pass with
/// the grant ([`settle_pending_refresh`]).
fn withheld(host: &dyn Host, target: &Target) -> Option<String> {
    aterm_machines::items::defaults::withheld(&target.domain, host.full_disk_access())
}

/// What a Restore without Full Disk Access says of a key in another app's container it
/// leaves as it is ([`withheld`]), whose before-image is the original (not an
/// [`unknown_original`], which a Restore with the grant leaves too): setup is stopped and
/// nothing runs a Restore again by itself, so the row names the step that puts it back,
/// however this one ran: a Restore again from an app that holds the grant, which is the app
/// macOS holds responsible (aterm for System State or a command in an aterm tab, the
/// terminal app for one in another terminal, and over SSH `sshd-keygen-wrapper`, which
/// Remote Login's switch grants).
fn restore_waits(target: &Target) -> String {
    format!(
        "{} is in another app's container, which macOS opens only to a process whose app holds \
         Full Disk Access; its original stays in the backup: run Restore again from an app that \
         holds it: from aterm's System State on this Mac, or a command in an aterm tab, once \
         aterm has Full Disk Access; aterm doctor --restore in another terminal once that \
         terminal app has it; over SSH, once Remote Login's \"Allow full disk access for remote \
         users\" is on (System Settings > General > Sharing)",
        target.describe()
    )
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
    // Nothing an earlier pass left alone for want of Full Disk Access is followed up after
    // a Restore: setup is stopped, and the person's preferences are theirs.
    save_withheld(root, &Withheld::default())?;
    let saved = load_backup(&report.backup)?;
    let mut activation = pending_refresh(root)?;
    for saved in &saved {
        // Nothing is relaunched for a key this Restore does not put back.
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
    for item in saved.iter().rev() {
        // A before-image that is no original stays unknown whatever the grant: no Restore
        // with it would put anything back, so the row names no step for one.
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
        settle_pending_refresh(host, root)?;
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
    // Nothing is relaunched for a key of another app's container without the grant: this
    // pass neither read nor wrote it ([`withheld`]).
    let targets: Vec<&Target> = targets
        .iter()
        .copied()
        .filter(|target| withheld(&*host, target).is_none())
        .collect();
    let mut refreshed = true;
    let mut refused = Vec::new();
    // The processes that read each target's domain: the table the shared items' own
    // relaunch reads ([`aterm_machines::init_activation::readers`]).
    for process in aterm_machines::init_activation::RELAUNCHED {
        let affected = targets
            .iter()
            .any(|t| aterm_machines::init_activation::readers(&t.domain).contains(&process));
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
    /// the key's [`Target::describe`]), and whose account group is one sign-out row. When
    /// it models Notification Center ([`Fake::notification_center`]), the widgets row is one
    /// item of the Window Manager's key and Notification Center's widget list, as the shared
    /// widgets item is (`init.widgets@alice`). Each row is its item's, by its id. It holds
    /// what it is told to as the shared engine does ([`Hold`]; a row held as it is names the
    /// keys it found changed, as the engine's does); the engine's own hold is pinned in
    /// aterm-machines, and [`NativeHost`]'s by the tests over [`Inert`]. An item's own
    /// relaunch of what reads its writes is an event (`relaunch <process>`), apart from the
    /// doctor's (`restart <process>`).
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
        /// An apply pass of the preference group panics once its items wrote, as a process
        /// that dies before they relaunch what reads them.
        dies_after_item_writes: bool,
        /// The doctor's own writes (a rollback, a Restore) panic, as a process that dies
        /// among them.
        dies_in_doctor_writes: bool,
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
        /// What the pass measured of its Full Disk Access; `None` counts as no grant.
        full_disk_access: Option<bool>,
        /// The widgets row also clears Notification Center's widget list ([`NC`]), a key
        /// of another app's container, in one item with the Window Manager's key: without
        /// the grant that key is left alone, the row says so and reads as its other key does,
        /// and the item is withheld ([`Host::withheld`]).
        notification_center: bool,
        withheld: Vec<String>,
    }

    impl Fake {
        /// The item a wanted key belongs to, as the fake's rows and attempts name it:
        /// Notification Center's key is the shared widgets row's, and so is the Window
        /// Manager's when it models Notification Center.
        fn item(&self, target: &Target) -> String {
            if contained(target)
                || (self.notification_center && target.domain == "com.apple.WindowManager")
            {
                WIDGETS.into()
            } else {
                target.describe()
            }
        }

        /// The items in run order, each with its keys and what each should hold.
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

        /// The setting whose row writes `target`.
        fn setting(target: &Target) -> Setting {
            if target.domain == "com.apple.dock" {
                Setting::Dock
            } else {
                Setting::Widgets
            }
        }

        /// How many times an item's own activation relaunched `process`.
        fn relaunches(&self, process: &str) -> usize {
            let event = format!("relaunch {process}");
            self.events.iter().filter(|e| **e == event).count()
        }

        /// The keys this host's items want: [`wanted`], and Notification Center's cleared
        /// widget list when it models it.
        fn wants(&self) -> Vec<(Target, PValue)> {
            let mut keys = wanted();
            if self.notification_center {
                keys.push((nc_widgets(), cleared()));
            }
            keys
        }

        /// Whether this pass reaches `target`: one of another app's container only with
        /// the grant, as the shared items do.
        fn reaches(&self, target: &Target) -> bool {
            !contained(target) || self.full_disk_access == Some(true)
        }

        /// Writes and relaunches, the changes a pass makes outside aterm's own records.
        fn changes(&self) -> usize {
            self.events
                .iter()
                .filter(|event| event.starts_with("write ") || event.starts_with("restart "))
                .count()
        }

        /// Write `target` as a shared item or the doctor does.
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

    /// The doctor of a selected widgets row whose Notification Center key (a widget in its
    /// list) waits for Full Disk Access, the row one item with the Window Manager's key, as
    /// the shared widgets item is, for the stage tests in `initial`: [`Withholding::pass`] is
    /// what one worker retry of the Workstation stage runs, a minute after the last, and
    /// [`Withholding::follow_up`] what it runs after its stages.
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

        /// [`Withholding::widgets`] whose first change does not verify (the Window Manager
        /// key's reader, the Dock, was not up again in time): that try is rolled back, and
        /// the row tried again once its tries allow.
        pub(super) fn widgets_whose_first_try_does_not_verify() -> Self {
            let mut doctor = Self::widgets();
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

        /// `by` goes by.
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

        /// Notification Center never keeps a clear of its widget list (it is not up again
        /// in time): each try reads back as it was, and is rolled back.
        pub(super) fn notification_center_never_keeps(&mut self) {
            self.host.failing.push(nc_widgets());
        }

        /// The person shows their desktop widgets again: the Window Manager's key reads as
        /// they set it.
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

        /// Relaunches of the Dock so far: the items' own and the doctor's.
        pub(super) fn dock_relaunches(&self) -> usize {
            self.host.relaunches("Dock")
                + self.host.restarts.iter().filter(|p| *p == "Dock").count()
        }

        /// Relaunches of Notification Center so far: the items' own and the doctor's.
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

    /// A [`System`] that runs nothing: each command is recorded and refused, every file
    /// operation is unsupported, and its journal keeps each line's id. A [`NativeHost`]
    /// over it never reaches this Mac's preferences, processes or journal.
    #[derive(Default)]
    pub(super) struct Inert {
        calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
        /// What it answers for this process's Full Disk Access; `None`, not measured.
        full_disk_access: Option<bool>,
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

    /// Notification Center's widget list, which lives in its app container.
    const NC: &str = "com.apple.notificationcenterui";

    /// The shared widgets item's id: the Window Manager's widget keys and Notification
    /// Center's widget list.
    pub(super) const WIDGETS: &str = "init.widgets@alice";

    /// Whether `call` reads, writes or relaunches Notification Center or its preferences.
    fn touches_notification_center(call: &str) -> bool {
        call.contains(NC) || call.contains("NotificationCenter")
    }

    /// A doctor host over an [`Inert`] system whose Full Disk Access is `full_disk_access`,
    /// its shared preference items `items`, and the commands it was asked to run.
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

    /// The doctor's own reads and writes of another app's container wait for the grant: no
    /// `defaults` runs for it, and the refusal names Full Disk Access. Negative control:
    /// with the grant the same read runs the export.
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

    /// Restore puts back what the backup holds, except a key of another app's container
    /// while the pass holds no Full Disk Access: that row is `blocked` and names the step that
    /// puts it back, the grant and a Restore run again ([`restore_waits`]; nothing runs one by
    /// itself), and nothing reads, writes or relaunches Notification Center, while the Window
    /// Manager's key is still read for and the Dock still relaunched. The backup keeps the key
    /// for that Restore. Negative control: with the grant the Restore reads Notification
    /// Center's domain and relaunches it.
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
                // Nothing runs a Restore again by itself: the row carries what names that step.
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

    /// The files of `root` a follow-up must leave as they are: every record but the backup
    /// and the follow-up's own, so no offer, countdown or question can follow from it.
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

    /// A setup pass without Full Disk Access leaves Notification Center's widgets alone and
    /// records the widgets row for the follow-up, settled for the keys it reached; nothing
    /// touches Notification Center while the grant is missing, however often it is asked.
    /// Once the grant is held, the follow-up clears it and relaunches Notification Center
    /// once, reads no other key (a person's change since stays theirs), relaunches no Dock and
    /// writes no setup record, then has nothing left to do. Negative control: a pass that
    /// held the grant clears them itself and leaves nothing to follow up.
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
        // The person turns the Window Manager's key back since.
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

        // Negative control: the grant held from the first pass.
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

    /// The follow-up record drops what a pass reached as wanted and adds what it left a key
    /// alone in: an item it ran that left one alone again (the widgets row writing the Window
    /// Manager's keys without the grant) stays, and the record goes once nothing waits. A
    /// recorded item whose change reached its keys and did not verify stays and spends a try
    /// of its build; one left alone in that pass spends none. Negative control: an item a pass
    /// reached as wanted is dropped, with its tries.
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

    /// A setup retry with the grant leaves the widgets row a pass without it settled as it
    /// is (the Dock never verifies, so setup still retries): the retry writes neither the
    /// Window Manager's key nor Notification Center's list, and the record still names the
    /// row; the follow-up after it clears Notification Center once and drops the record.
    /// Negative control: without the grant the follow-up reaches nothing.
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

    /// A follow-up whose Notification Center relaunch does not settle is rolled back and keeps
    /// the row in the record, held (nothing written, relaunched or journaled for it) until
    /// [`SECOND_TRY_AFTER`] has passed; its second try fails too, and it is held for good in
    /// that build, while a new build tries once more and, verifying, drops it. Negative
    /// controls: counting an attempt that did not verify as reached drops the record after
    /// the first failure; a follow-up that holds nothing tries on every call.
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
        // A new build's try, which verifies.
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

    /// A finished setup's follow-up at launch holds the caller's busy slot for as long as it
    /// may hold the profile lock: it takes the slot only once the record names something to
    /// reach, setup is not stopped and the grant is held, the grant measured with neither the
    /// slot nor the lock held; it takes the slot before the lock, and releases it after the
    /// lock, once the follow-up ran. Negative controls: a slot taken with nothing owed, after a
    /// Cancel or without the grant is counted (and a Cancel's host is never built); a grant
    /// measured under the slot or the lock is seen so; one taken after the lock finds the lock
    /// held; one dropped at once, or before the lock is released, finds the follow-up not yet
    /// run or the lock still held.
    #[test]
    fn a_finished_setups_follow_up_holds_the_busy_slot_while_it_may_hold_the_profile_lock() {
        use std::cell::{Cell, RefCell};
        /// The slot: at its release, whether the profile lock was free and the record empty.
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
        // At each measure of the grant: how many slots were taken, and whether the lock was free.
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
        // A cancelled setup's record is never followed up: no host, no slot.
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
        // A completed setup, which is when a launch calls it, is followed up.
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

    /// Nothing is followed up once the person restored or cancelled setup (still running:
    /// the Dock never verifies), or unchecked the widgets row in a later choice, even with
    /// the grant; a Restore drops the record. Negative control: without any of them, the
    /// same follow-up clears the widgets.
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

    /// A before-image an earlier aterm saved of Notification Center's widget list without a
    /// grant field reads as granted only when it holds a value: an absence it saved may be
    /// macOS hiding the container. A Restore, with the grant or without, neither deletes such
    /// a key nor relaunches Notification Center for it, and its row says its original is
    /// unknown ([`unknown_original`]), never to grant the app and run Restore again
    /// ([`restore_waits`]), which would put nothing back. Negative controls: an absence read
    /// with the grant is the original, and the Restore reaches Notification Center; asking
    /// for the grant before the record (the old order in `restore`) sends the person
    /// without it to a Restore that cannot work.
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

    /// The first read with the grant replaces a before-image saved without it, so a Restore
    /// puts back the widgets the person had. Negative control: a before-image read with the
    /// grant is the original, and no later read replaces it.
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

    /// A before-image of Notification Center's widget list read with the grant and found
    /// absent is the original, and the backup says it was read with the grant: the next
    /// apply with the grant keeps that absence (it does not take the list it reads now for
    /// the original), and a Restore with the grant deletes the key, as `defaults delete`.
    /// Negative control: a backup that does not keep the grant reads that absence as macOS
    /// hiding the container, so the apply replaces it and the Restore puts the widgets back.
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

    /// A relaunch of Notification Center an earlier pass owed (pending-refresh.json) waits
    /// for the grant: a pass or a Restore without it does not relaunch it and keeps the
    /// record, and a check says it waits rather than calling it drift. Negative control: the
    /// first pass with the grant relaunches Notification Center and clears the record.
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

    /// A shared item that changes Notification Center's widget list and never verifies,
    /// whose before-image the doctor's rollback would put back.
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

    /// A shared item already as wanted that, when `.0`, left a key of another app's
    /// container alone for want of Full Disk Access ([`aterm_machines::items::Item::withheld`]).
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

    /// The doctor counts settled a row that left a key alone for want of Full Disk Access,
    /// for the keys it reached, and records it for the follow-up. Negative control: the same
    /// row with every key reached is settled and leaves nothing to follow up.
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

    /// A widgets row as the shared item is one: the Window Manager's key and Notification
    /// Center's list in one item, which without Full Disk Access reaches the Window
    /// Manager's alone and says it left the other ([`aterm_machines::items::Item::withheld`]),
    /// and with it reads and clears both; a follow-up's ([`WorkstationGroup::Withheld`])
    /// reaches Notification Center's alone. Its clones share the keys (whether the desktop
    /// widgets are hidden, whether Notification Center's list is cleared) and the grant, and
    /// count the applies that hid the desktop widgets.
    #[derive(Clone, Default)]
    struct Bundled {
        hidden: std::sync::Arc<std::sync::atomic::AtomicBool>,
        applies: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        granted: std::sync::Arc<std::sync::atomic::AtomicBool>,
        cleared: std::sync::Arc<std::sync::atomic::AtomicBool>,
        contained_only: bool,
    }

    impl Bundled {
        /// Whether `flag` is set.
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

    /// The worker's own entry over the doctor's own host ([`NativeHost`], on an [`Inert`]
    /// system without Full Disk Access): a widgets row that left Notification Center alone is
    /// settled for the Window Manager's key it hid, so while setup keeps retrying (the Dock
    /// never verifies) no retry hides the desktop widgets again once the person shows them,
    /// and its row says it is left as the person set it; the record still names the row for
    /// the follow-up. Negative control: not counting a withheld row settled
    /// (`NativeHost::workstation`) hides them again on every retry.
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
        // The person shows their desktop widgets again.
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

    /// Over the doctor's own host, each tick's anew as the worker's is (its grant measured
    /// again): the grant arrives after a pass without it settled the widgets row, while setup
    /// keeps retrying (the Dock never verifies). The next pass holds the row as it is and
    /// reads Notification Center's list too: the row is `ok` and says that list is the
    /// follow-up's ([`SETTLED_OWED`]), not that aterm set it and it changed since, and the
    /// record still names the row, since a pass that held it did not reach it as wanted;
    /// nothing hides the desktop widgets again once the person shows them. The follow-up then
    /// clears Notification Center alone and drops the record, and the row says [`SETTLED`],
    /// true of the Window Manager's key the person changed. Negative controls: holding every
    /// settled item with [`SETTLED`] (`Automatic::holds` ignoring what is owed) fails the
    /// row's words; `NativeHost::held` naming nothing drops the record on the granted tick.
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
        // The person shows their desktop widgets again.
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

    /// Over the doctor's own host, whose engine lists a row that leaves Notification Center
    /// alone as withheld whether or not the pass holds it: once Notification Center's widget
    /// list was cleared (by a pass with the grant, or by the follow-up) and the person added
    /// a widget back, a pass without the grant (another build taking a turn, say, or a grant
    /// read as not measured) holds the settled row as it is and does not record it again,
    /// so the next follow-up with the grant has nothing to do and the person's widget stays,
    /// as does the next pass's. Negative control: recording every withheld row, held or not,
    /// has that follow-up clear the person's widget.
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
            // The person adds a widget back.
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

    /// The rollback of a change that did not verify skips a before-image of another app's
    /// container while the pass holds no Full Disk Access: a `blocked` rollback row names
    /// the grant, and nothing reads, writes or relaunches Notification Center. Negative
    /// control: with the grant the rollback reads Notification Center's domain.
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

    /// Notification Center's widget list, a key of another app's container.
    pub(super) fn nc_widgets() -> Target {
        Target::new(NC, "widgets")
    }

    /// A widget list with a widget in it.
    fn populated() -> PValue {
        PValue::Dict(vec![
            ("vers".into(), PValue::Int(1)),
            (
                "instances".into(),
                PValue::Array(vec![PValue::Data(vec![42])]),
            ),
        ])
    }

    /// A widget list with no widget.
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
                // A follow-up reaches another app's container alone, and only with the grant.
                .filter(|(target, _)| group != WorkstationGroup::Withheld || contained(target))
                .filter(|(target, _)| self.reaches(target))
                .filter(|(target, _)| self.selection().contains(Self::setting(target)))
                // A held item changes nothing, so nothing of it is backed up.
                .filter(|(target, _)| self.hold_of(&self.item(target)).is_none())
                .filter(|(t, v)| !self.values.get(t).is_some_and(|now| now.same(v)))
                .map(|(target, _)| Saved {
                    value: self.values.get(&target).cloned(),
                    granted: self.reaches(&target),
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
                for (item, keys) in self.items() {
                    // A follow-up reaches the keys in another app's container alone.
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
                    // A key it may not reach is left alone, and said so; the item is withheld,
                    // whatever its row reads, and its row reads as its other keys do.
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
                            // The shared engine's holds: nothing of it runs.
                            Some(Hold::Back(why)) => {
                                status = "blocked";
                                detail = why;
                            }
                            // What it found changed, and why it leaves it, as the engine's.
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
                                // The engine journals each item before its apply.
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
                                // Its own activation relaunches what reads its writes, once.
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

    /// Only a settled item the follow-up still owes says that key is the follow-up's
    /// ([`SETTLED_OWED`]); every other settled item keeps [`SETTLED`], even while something
    /// is owed, and nothing is owed to a pass that does not read the follow-up's keys.
    /// Negative control: choosing the words on whether anything is owed gives the Dock the
    /// Notification Center wording.
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

    /// An automatic pass that reaches the follow-up's keys is held by that record's tries
    /// too, for an item it owes: one either record holds waits, one only that record holds
    /// included; spent tries in either outrank a wait in the other, whichever is read first;
    /// an item neither holds runs; a settled item stays as it is whatever the follow-up's
    /// tries; and tries of an item the record no longer names hold nothing. Negative
    /// control: holding by this record's tries alone lets the pass make, minutes after the
    /// follow-up's try, the one that is that key's second, before it is due.
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
    /// not make the next process relaunch the Dock for a key nobody wrote. No pass records
    /// its items' keys for a relaunch (their own markers do,
    /// `a_pass_that_ends_after_its_items_wrote_leaves_their_relaunch_to_them`), and a held
    /// item's keys are not backed up either (aterm-machines'
    /// `a_held_item_has_no_before_images`).
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

    /// A pass that ends after its items wrote and before they relaunch what reads them
    /// leaves that relaunch to the items: each keeps a durable marker from before its
    /// first write, so the next pass's item relaunches each reader once. The doctor keeps
    /// no record of those keys to relaunch for again. Negative control: recording every
    /// backed-up key before the items run makes the next pass relaunch the Dock a second
    /// time.
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

    /// The doctor's own rollback writes are recorded before they are made: a pass that
    /// ends among them makes the next relaunch what reads the keys it was putting back.
    /// Negative control: no record before the rollback leaves the Dock unrelaunched.
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

    /// A key of an older pass's relaunch record that an item of this pass writes again
    /// and verifies was relaunched for after that write, by the item: the doctor does not
    /// relaunch its reader again, and the record is cleared. Negative control: reloading
    /// the whole record relaunches the Dock a second time.
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
