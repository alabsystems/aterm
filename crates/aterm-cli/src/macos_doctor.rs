// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! macOS workstation profile, checks, and `defaults` recovery journal.

use aterm_json::{Value, json};
use aterm_machines::init::{Hold, Workstation, WorkstationGroup, WorkstationRun};
use aterm_machines::port::{Cmd, System};
use aterm_machines::{model::PValue, plist};
use aterm_types::macos_setup::{
    PassOutcome, PutBack, RestoreOutcome, Selection, Setting, Subject, Unfinished,
};

#[cfg(test)]
mod accounts_visibility_tests;
#[cfg(any(test, debug_assertions))]
mod follow_up_stop_test;
#[cfg(any(test, debug_assertions))]
pub use follow_up_stop_test::InitialSetupWithheldStopTest;
mod initial;
#[cfg(any(test, debug_assertions))]
pub use initial::background_workstation_of_reply;
pub use initial::{
    BackgroundAnswer, BackgroundVaultAuthorization, BroughtForward, DisplayHistory, Fence,
    InitialSetupAdmission, InitialSetupAuthorization, InitialSetupObservation,
    InitialSetupObserved, InitialSetupParkMark, InitialSetupRestart, InitialSetupSnapshot,
    InitialSetupSnapshotError, InitialSetupUpgrade, InputServiceSet, InputSetupScope, LastRead,
    ObservationRead, PASS_RETRY_SCHEDULE, ParkWatch, PassRetry, PassWorker, PermissionSteps,
    SETUP_BUSY, SETUP_RESUME_ENTRY, SETUP_RESUME_LABEL, SetAside, SetupHistoryStore, SetupHooks,
    SetupNext, SetupRecordsWatch, SetupResume, SetupUpgrade, SetupWindowHold, StageEvent,
    VAULT_SETTINGS_UNREAD, Watches, activate_input_attachments, alab_mac, apply_initial_setup,
    apply_initial_setup_observed, ask_background_setup, automatic_plan,
    automatic_vault_bootstrap_allowed, bring_forward_restored_setup,
    bring_forward_restored_setup_observing, check_background_workstation, check_initial_setup,
    check_initial_setup_observed, end_initial_setup_resume, forget_set_aside_setup,
    hold_initial_setup_window, initial_setup_access_revision, initial_setup_admission,
    initial_setup_finished, initial_setup_holds, initial_setup_keeps_vault_sync,
    initial_setup_observation, initial_setup_owes_secrets_stage, initial_setup_park_mark,
    initial_setup_pass_retry, initial_setup_pending_repositories,
    initial_setup_pending_workstation_revision, initial_setup_records, initial_setup_resumable,
    initial_setup_review_revision, initial_setup_revision, initial_setup_snapshot,
    initial_setup_snapshot_read_only, initial_setup_window_open, input_setup_scope, journal_fence,
    keep_initial_setup_resume, look_for_the_vault_again, note_initial_setup_pass_retry,
    note_person_asked_for_access, note_setup_interrupted, observe_initial_setup,
    provisional_setup_widened, provisional_setup_widens, put_back_held_initial_setup,
    remember_private_read_back, remembered_private_read_back, request_initial_setup_stop,
    restart_initial_setup, restart_initial_setup_observed, retry_parked_setup_stages,
    run_background_setup_pass, run_initial_setup_pass, set_aside_unreadable_setup,
    set_aside_unreadable_setup_observing, setup_is_newer, setup_ledger_environment,
    setup_records_held, take_back_setup_interrupted, validate_initial_setup_plan,
    vault_config_unread, wait_for_initial_setup_windows, with_current_input_scope,
    with_initial_setup_vault_sync,
};
#[cfg(any(test, debug_assertions))]
pub use initial::{
    InitialSetupAdmissionFault, InitialSetupAdmissionStopTest, ProvisionalSetupWatchTest,
    initial_setup_observation_at, initial_setup_snapshot_at, input_setup_scope_at,
    set_aside_unreadable_setup_at, set_aside_unreadable_setup_at_observing,
    set_aside_unreadable_setup_at_observing_before_checkpoint,
    set_aside_unreadable_setup_at_observing_done_error, with_initial_setup_vault_sync_at,
};
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

/// A row's status for a setting that is back as it was saved but loads only at the next login:
/// nothing failed, and the person logs out and back in for it (its detail says so).
pub const LOGIN: &str = "login";

/// A row's status for a change the engine put off ([`aterm_machines::model::Outcome::Deferred`]):
/// the person's console, the Mac going idle or the next launch moves it, never a retry a minute
/// later. Nothing failed and nobody is needed now, so it is no attention ([`Report::needs_attention`]),
/// but the change is not made either ([`Report::complete`]).
pub const DEFERRED: &str = "deferred";

/// A row's status for a grant its stage leaves to someone else and waits for nobody on: the
/// person (a switch only they turn on), an administrator, setup's one Mac password, or macOS with
/// a step this revision handed it that no reading confirms. Its reading is not in place, and its
/// detail opens with that reading. Nothing failed and nothing waits for it, so it is no attention
/// ([`Report::needs_attention`]) and the stage completes on it ([`Report::complete`]); it is
/// never `ok`, which only a reading in place says.
pub const OFFERED: &str = "offered";

/// A row's status for a grant this process could not read (an unhardened probe, a list only
/// readable with Full Disk Access): aterm takes no step on it and waits for nothing, so the stage
/// completes on it as on [`OFFERED`], and it is never said in place, nor failed.
pub const UNREAD: &str = "unread";

/// A row's status for a setting this setup set and read back, then set again once it read
/// otherwise, that reads otherwise again since, by whoever changed it (nothing tells): something
/// holds it so, and the pass leaves it as it is (design §6.6); and for a sign-out signed in again
/// since aterm did it, which takes a person's Apple Account. Its detail opens with what it reads
/// now, and the stage completes on it, since no pass of setup's sets it again. It needs no
/// attention, and it is never `ok`, which only a reading in place says. Restore, turning its
/// group off and on, or Set up now sets it again.
pub const CHANGED: &str = "changed";

/// The longest [`Report::attention_line`], in bytes: an info record keeps 512
/// (`aterm_log::MAX_RECORD_BYTES`), and the setup worker's line says a little around it.
pub const ATTENTION_LINE: usize = 400;

/// One preference's observed outcome.
#[derive(Clone, Debug)]
pub struct Row {
    /// Human-readable setting name.
    pub name: String,
    /// `ok`, `drift`, `applied`, `restored`, [`LOGIN`], [`DEFERRED`], [`OFFERED`], [`UNREAD`],
    /// [`CHANGED`], `blocked`, `waiting`, `unreached`, `absent`, `unverified` or `error`; a setup
    /// snapshot's rows also `pending`, `running` or `stopped`.
    pub status: String,
    /// Action or failure detail, without account identifiers.
    pub detail: String,
    /// The wall time that produced the row, in milliseconds (the `--json` row's `elapsed_ms`):
    /// a shared engine item's own ([`aterm_converge::ItemReport::elapsed_ms`]), else the act
    /// or stage it reports ([`RowClock`]). `None` until one of them times it.
    pub elapsed_ms: Option<u64>,
}

impl Row {
    /// Whether the setting or account is out of policy, blocked, waiting, unreached, absent
    /// (`initial::ABSENT`: a vault whose address answered that none is there, which the stage
    /// parks on as on one nobody answered), unverified or failed ([`Report::needs_attention`]).
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        matches!(
            self.status.as_str(),
            "drift" | "error" | "blocked" | "waiting" | "unreached" | "absent" | "unverified"
        )
    }

    /// Whether aterm's own step failed or could not be confirmed: what an unattended pass warns
    /// about. A row that waits on a person, a vault or a service starting is a not-yet.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(self.status.as_str(), "drift" | "error" | "unverified")
    }
}

/// A pass's clock for the rows it adds to a report: each [`RowClock::lap`] gives every row its
/// producer did not time the milliseconds since the lap before, so a row carries the time of
/// the act or step that produced it. A step that keeps its own clock ([`append_workstation`],
/// [`refresh`]) is followed by a lap that only restarts the caller's.
pub(crate) struct RowClock(std::time::Instant);

impl RowClock {
    pub(crate) fn start() -> Self {
        Self(std::time::Instant::now())
    }

    /// Time `rows` that have no time yet, and restart.
    pub(crate) fn lap(&mut self, rows: &mut [Row]) {
        let took = aterm_converge::millis(self.0.elapsed());
        for row in rows.iter_mut().filter(|row| row.elapsed_ms.is_none()) {
            row.elapsed_ms = Some(took);
        }
        self.0 = std::time::Instant::now();
    }
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
    /// A Restore's outcome by System State row, including one a setup pass finishes.
    /// The page uses its row names where `rows` names each preference key.
    pub restore: Option<RestoreRows>,
    /// A pass's rows for the page (design §3.4, §3.6): what it did to the Mac settings in
    /// System State's row names, where `rows` names each preference key, keyboard and process
    /// for `aterm doctor`; `None` where `rows` are in the page's terms already.
    pub page: Option<PassPage>,
}

/// What a Restore did, for System State (design §3.4).
#[derive(Clone, Debug, Default)]
pub struct RestoreRows {
    /// How each row's saved preferences came out. A relaunch that failed after they were
    /// written leaves the rows its process reads to the next login, never a row of its own:
    /// the page names no process.
    pub outcome: aterm_types::macos_setup::RestoreOutcome,
}

/// A pass's rows for System State (design §3.4, §3.6).
#[derive(Clone, Debug, Default)]
pub struct PassPage {
    /// What the pass did to the Mac settings that the page says in its own words: a change
    /// that failed or could not be confirmed, by row, and what its rollback put back.
    pub outcome: PassOutcome,
    /// The pass's other rows, about no one setting's keys, which the page shows as they are.
    pub rows: Vec<Row>,
    /// Native facts already read by this pass; never serialized with operation rows.
    pub readback: WorkstationReadback,
}

/// An owned native readback, independent of journal outcomes or setup completion: each a
/// complete item cohort's reading, dated from before its run read anything. Only the native
/// producers construct nonempty values: the Mac settings and sign-out runs
/// ([`run_workstation_readback`]) and the Spotlight stage's.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct WorkstationReadback(Vec<aterm_types::macos_setup::Observation>);

impl std::fmt::Debug for WorkstationReadback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WorkstationReadback(..)")
    }
}

impl WorkstationReadback {
    /// No owned native facts were supplied.
    pub const EMPTY: Self = Self(Vec::new());

    /// Current facts as the native producer observed them, without another read.
    #[must_use]
    pub fn observations(&self) -> &[aterm_types::macos_setup::Observation] {
        &self.0
    }

    /// Whether no native facts are available for this pass.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Transfer custody without cloning its facts.
    #[must_use]
    pub fn take(&mut self) -> Self {
        std::mem::take(self)
    }

    /// Keep only the settings the owning stage can publish.
    pub fn retain(&mut self, selected: Selection) {
        self.0.retain(|observation| match observation.subject {
            Subject::Setting(setting) => selected.contains(setting),
            _ => false,
        });
    }

    /// Merge a later complete setting readback, replacing its earlier value.
    pub fn append(&mut self, later: Self) {
        for observation in later.0 {
            match self
                .0
                .iter_mut()
                .find(|old| old.subject == observation.subject)
            {
                Some(old) => *old = observation,
                None => self.0.push(observation),
            }
        }
    }
}

/// Run one already-prepared native pass and retain its complete post-apply facts
/// before converting operation rows for display, each dated from before the run reads
/// anything, so its age is never told as less than it is (design §3.5). Its canonical
/// preparation must match `group`, a group of whole settings (every preference, the
/// preferences, the sign-outs); attachment-only, deferred-container and manual passes
/// prove no whole setting.
///
/// # Errors
/// The native pass cannot resolve its journal or execute its prepared items.
pub fn run_workstation_readback(
    prepared: Workstation,
    sys: &dyn System,
    group: WorkstationGroup,
    mode: aterm_machines::model::Mode,
) -> Result<(WorkstationRun, WorkstationReadback), String> {
    let selected = prepared
        .prepared_scope()
        .and_then(|(prepared_group, selected)| {
            (mode == aterm_machines::model::Mode::Apply
                && prepared_group == group
                && matches!(
                    group,
                    WorkstationGroup::All
                        | WorkstationGroup::Preferences
                        | WorkstationGroup::Accounts
                ))
            .then_some(selected)
        });
    let read_at = sys.now();
    let run = prepared.run_recording(sys, mode)?;
    let readback = selected.map_or(WorkstationReadback::EMPTY, |selected| {
        WorkstationReadback(initial::read_back(&run.tagged(), selected, read_at))
    });
    Ok((run, readback))
}

impl PassPage {
    /// Add a later part of the same pass (a follow-up, a later stage's report).
    pub fn merge(&mut self, later: Self) {
        self.outcome.merge(later.outcome);
        self.rows.extend(later.rows);
        self.readback.append(later.readback);
    }

    /// Say that `setting`'s step failed for `reason`, the page's words its step decided
    /// (`GitHub can't be reached`), in place of the words a change that did not take carries.
    /// A page with no failure of `setting` is left as it is.
    pub fn failed_because(&mut self, setting: Setting, reason: &str) {
        let failed = (setting, Unfinished::Failed);
        let unfinished = self.outcome.unfinished.contains(&failed);
        self.outcome.unfinished.retain(|each| *each != failed);
        match self.rows.iter_mut().find(|row| {
            row.name == setting.name() && matches!(row.status.as_str(), "error" | "failed")
        }) {
            Some(row) => row.detail = reason.into(),
            None if unfinished => self.rows.push(Row {
                name: setting.name().into(),
                status: "error".into(),
                detail: reason.into(),
                elapsed_ms: None,
            }),
            None => {}
        }
    }
}

impl Report {
    /// A report of `action` at `root` with no row yet.
    fn empty(action: Action, root: &Path) -> Self {
        Self {
            action,
            rows: Vec::new(),
            manual_steps: Vec::new(),
            backup: root.join("original-preferences.json"),
            restore: None,
            page: None,
        }
    }

    /// The line naming each change this pass or Restore made and could not log, with why, when
    /// the change journal refused one ([`journal_refused`]).
    #[must_use]
    pub fn journal_refused(&self) -> Option<&str> {
        self.rows
            .iter()
            .find(|row| row.name == JOURNAL_ROW && row.status == "error")
            .map(|row| row.detail.as_str())
    }

    /// The rows a pass left as they are, [`CHANGED`], because they changed after this setup had
    /// them as wanted ([`SETTLED`], under a record a newer aterm wrote), again after aterm set
    /// them again ([`initial::kept::KEPT`]), or were signed in again after aterm signed them out
    /// ([`initial::kept::SIGNED_IN`]): drift after a row read back ok, which no automatic pass
    /// rewrites.
    #[must_use]
    pub fn changed_since(&self) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|row| row.status == CHANGED)
            .map(|row| row.name.as_str())
            .collect()
    }

    /// Whether a setting or account remains out of policy, blocked, unverified or failed. A
    /// [`DEFERRED`] change is none of these: nobody is needed for it now.
    #[must_use]
    pub fn needs_attention(&self) -> bool {
        self.rows.iter().any(Row::needs_attention)
    }

    /// Whether a row says aterm's own step failed or could not be confirmed ([`Row::failed`]).
    #[must_use]
    pub fn failed(&self) -> bool {
        self.rows.iter().any(Row::failed)
    }

    /// Whether every change is made or in place: nothing [`Self::needs_attention`] and nothing is
    /// [`DEFERRED`]. An [`OFFERED`] or [`UNREAD`] grant, or a setting [`CHANGED`] since aterm set
    /// it, is no change of aterm's to make, so it holds nothing back, and the report still names
    /// it. What records a pass as applied, or a plan as complete.
    #[must_use]
    pub fn complete(&self) -> bool {
        !self.needs_attention() && !self.rows.iter().any(|row| row.status == DEFERRED)
    }

    /// One line for the log: the verdict and each row that needs attention, in that order and
    /// cut to [`ATTENTION_LINE`] bytes, so the first row that blocks is named however many rows
    /// are in place, and no row in place is.
    #[must_use]
    pub fn attention_line(&self) -> String {
        let mut line = self.verdict();
        let attention: Vec<&Row> = self.rows.iter().filter(|r| r.needs_attention()).collect();
        for (at, row) in attention.iter().enumerate() {
            let left = attention.len() - at - 1;
            let tail = if left > 0 {
                format!("; and {left} more")
            } else {
                String::new()
            };
            let mut part = format!("; {}: {} — {}", row.name, row.status, row.detail);
            if line.len() + part.len() + tail.len() > ATTENTION_LINE {
                if at > 0 {
                    line.push_str(&format!("; and {} more", left + 1));
                    break;
                }
                // The first row that needs attention is named whatever its detail's length.
                let mut cut = ATTENTION_LINE
                    .saturating_sub(line.len() + tail.len() + '\u{2026}'.len_utf8())
                    .min(part.len());
                while !part.is_char_boundary(cut) {
                    cut -= 1;
                }
                part.truncate(cut);
                part.push('\u{2026}');
            }
            line.push_str(&part);
        }
        line
    }

    /// The report's first line: the action, how many of its rows need attention, and how many
    /// grants are [`OFFERED`] or [`UNREAD`] and settings [`CHANGED`] since aterm set them, which
    /// need no attention and are not in place either.
    fn verdict(&self) -> String {
        let total = self.rows.len();
        let count = |status: &str| self.rows.iter().filter(|row| row.status == status).count();
        let apart: Vec<String> = [
            (count(OFFERED), "offered"),
            (count(UNREAD), "not read"),
            (count(CHANGED), "changed since setup"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, word)| format!("{n} {word}"))
        .collect();
        let apart = if apart.is_empty() {
            String::new()
        } else {
            format!("; {}", apart.join(", "))
        };
        match self.rows.iter().filter(|r| r.needs_attention()).count() {
            0 => format!(
                "macOS workstation profile ({}): nothing needs attention{apart} ({total} rows)",
                self.action.name()
            ),
            n => format!(
                "macOS workstation profile ({}): {n} of {total} rows need attention{apart}",
                self.action.name()
            ),
        }
    }

    /// A compact JSON report, suitable for fleet automation.
    #[must_use]
    pub fn to_json(&self) -> String {
        let rows: Vec<Value> = self
            .rows
            .iter()
            .map(|r| {
                let mut row = json!({
                    "name": r.name, "status": r.status, "detail": r.detail,
                });
                if let (Some(ms), Some(fields)) = (r.elapsed_ms, row.as_object_mut()) {
                    fields.insert("elapsed_ms".into(), json!(ms));
                }
                row
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

    /// The same report for a terminal: the verdict, then each row not in place (what changed,
    /// what waits, what failed). [`Self::to_text_verbose`] lists every row.
    #[must_use]
    pub fn to_text(&self) -> String {
        self.text(false)
    }

    /// [`Self::to_text`] with every row, the ones in place too, and, for a Check, the backup's
    /// path, which an Apply or a Restore always gives (`aterm doctor --check --verbose`).
    #[must_use]
    pub fn to_text_verbose(&self) -> String {
        self.text(true)
    }

    fn text(&self, verbose: bool) -> String {
        let mut text = format!("{}\n", self.verdict());
        for row in self.rows.iter().filter(|row| verbose || row.status != "ok") {
            text.push_str(&format!(
                "  {}: {} — {}\n",
                row.name, row.status, row.detail
            ));
        }
        for step in &self.manual_steps {
            text.push_str(&format!("  owner action: {step}\n"));
        }
        // An Apply or a Restore says where the originals are kept; a Check only when asked.
        if verbose || self.action != Action::Check {
            text.push_str(&format!("  backup: {}\n", self.backup.display()));
        }
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

/// `aterm init`'s share of this Mac's setup records (INIT-8): `profile.lock`, waited for behind
/// a pass another setup runs, and for a pass of the whole profile the plan and completion
/// `aterm doctor --apply` records ([`run_mutation`]), so the first launch neither runs nor
/// offers what init did. Under a private state root (a lab instance) there is nothing to share.
pub struct InitRecords;

impl aterm_machines::init::Records for InitRecords {
    fn hold(
        &self,
        pass: aterm_machines::init::Pass,
        wait: Option<&dyn Fn(&str)>,
    ) -> Result<Box<dyn aterm_machines::init::Held>, aterm_machines::init::Unheld> {
        if aterm_types::dirs::runs_under_private_state_root() {
            return Ok(Box::new(aterm_machines::init::NoRecords));
        }
        ensure_person_scope()?;
        Ok(Box::new(init_hold_at(
            &state_root()?,
            pass,
            initial::alab_mac(),
            wait,
            initial::complete_explicit_workstation_plan,
        )?))
    }

    fn holds(&self) -> aterm_machines::init::Holds {
        initial::initial_setup_holds()
    }

    fn kept(&self) -> Result<Vec<String>, String> {
        if aterm_types::dirs::runs_under_private_state_root() {
            return Ok(Vec::new());
        }
        kept_items_at(&state_root()?)
    }
}

/// The items whose keys changed after aterm set them, as the record at `root` says
/// ([`initial::kept::Kept`]): `aterm init` leaves each as it is.
///
/// # Errors
/// The record cannot be read.
fn kept_items_at(root: &Path) -> Result<Vec<String>, String> {
    Ok(initial::kept::Kept::load(root)?
        .kept
        .into_iter()
        .map(|key| key.item)
        .collect())
}

/// What `aterm init` hears while another setup holds the records ([`init_hold_at`]).
pub const INIT_WAITS: &str =
    "waiting while System State, or another setup pass, changes this Mac's settings\u{2026}";

/// How often `aterm init` asks again for the records another setup holds.
const INIT_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// How long `aterm init` waits for the records another setup holds: far past any one pass over
/// the Mac's settings (a Dock relaunch and the sign-outs take well under a minute), so a pass
/// that never lets go ends init's wait with words rather than holding it for good.
const INIT_WAIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Why `aterm init`'s preferences and sign-outs stood aside once its wait for the records ended
/// ([`INIT_WAIT_LIMIT`]): each row so worded changed nothing, and the run's other rows stand.
pub const INIT_WAITED: &str = "left as it is: System State, or another setup pass, was \
                               changing this Mac's settings for 10 minutes; run aterm init again \
                               once that setup ends";

/// Why `aterm init --no-wait`'s preferences and sign-outs stood aside at once.
pub const INIT_NO_WAIT: &str = "left as it is: System State, or another setup pass, is changing \
                                this Mac's settings now, and --no-wait waits for nothing; run \
                                aterm init again once that setup ends";

/// [`InitRecords::hold`] at `root`: the profile lock, asked for again every [`INIT_POLL`]
/// while another setup's pass holds it (`wait` hears [`INIT_WAITS`] once), up to
/// [`INIT_WAIT_LIMIT`] (with no `wait`, `--no-wait`'s, asked once), then, for a pass of the
/// whole profile ([`aterm_machines::init::Pass::Whole`]) that no Stop of the person's holds, the
/// explicit plan `aterm doctor --apply` saves: the Mac settings the pass sets, less the groups
/// the person left as is, and the clones where the pass runs them on a Mac aterm's own plan
/// scopes them to (`alab`). A pass under an older aterm's Cancel, which holds every Mac setting,
/// saves no plan, so the Cancel stands; and a Stop pressed as the plan is saved holds the pass as
/// one on record does.
fn init_hold_at(
    root: &Path,
    pass: aterm_machines::init::Pass,
    alab: bool,
    wait: Option<&dyn Fn(&str)>,
    complete: initial::CompleteExplicit,
) -> Result<InitHold, aterm_machines::init::Unheld> {
    init_hold_within(root, pass, alab, wait, INIT_WAIT_LIMIT, complete)
}

/// [`init_hold_at`], waiting at most `limit`.
fn init_hold_within(
    root: &Path,
    pass: aterm_machines::init::Pass,
    alab: bool,
    wait: Option<&dyn Fn(&str)>,
    limit: std::time::Duration,
    complete: initial::CompleteExplicit,
) -> Result<InitHold, aterm_machines::init::Unheld> {
    let lock = init_lock_within(root, wait, limit)?;
    let holds = initial::holds_at(root);
    // A Stop, or records a newer aterm saved, hold the pass: it runs only what its flags name,
    // and records no run over them. A group the person left as is stays so in the plan a whole
    // pass saves; a flag naming it sets it up this once.
    let mut on = Selection::none();
    let whole = pass.whole() && !holds_the_run(&holds);
    if let aterm_machines::init::Pass::Whole { clones } = pass
        && whole
    {
        for setting in Setting::ALL {
            on.set(
                setting,
                setting.is_workstation() && !holds.off.contains(setting),
            );
        }
        // aterm's own plan on an ALab Mac holds the clones too (`scope::scoped`), where the pass
        // runs them (`--no-clone` leaves them out).
        on.set(
            Setting::Repositories,
            alab && clones && !holds.off.contains(Setting::Repositories),
        );
    }
    let run = if !whole || (on.workstation().is_empty() && initial::cancelled_at(root)?) {
        None
    } else {
        match initial::save_explicit_workstation_plan(root, on) {
            Ok(run) => Some(run),
            // The person pressed Stop as the plan was saved: it holds the pass from now on.
            Err(_) if holds_the_run(&initial::holds_at(root)) => None,
            Err(why) => return Err(why.into()),
        }
    };
    // What names the run the pass saved, so a run restarted while the records are let go is not
    // its own to record done ([`aterm_machines::init::Held::again`]).
    let marked = if run.is_some() {
        initial::run_in_force(root)?
    } else {
        None
    };
    Ok(InitHold {
        lock: Some(lock),
        root: root.to_path_buf(),
        run,
        marked,
        off: holds.off,
        limit,
        complete,
    })
}

/// Whether the person's choices in System State hold a whole pass: their Stop, or records a newer
/// aterm saved.
fn holds_the_run(holds: &aterm_machines::init::Holds) -> bool {
    holds.newer || holds.stopped.is_some()
}

/// The profile lock at `root`, waited for as [`init_hold_within`] waits.
fn init_lock_within(
    root: &Path,
    wait: Option<&dyn Fn(&str)>,
    limit: std::time::Duration,
) -> Result<HeldLock, aterm_machines::init::Unheld> {
    use aterm_machines::init::Unheld;
    let deadline = std::time::Instant::now() + limit;
    let mut said = false;
    loop {
        if let Some(lock) = HeldLock::try_take(root)? {
            return Ok(lock);
        }
        let Some(wait) = wait else {
            return Err(Unheld::Busy(INIT_NO_WAIT.into()));
        };
        if std::time::Instant::now() >= deadline {
            return Err(Unheld::Busy(INIT_WAITED.into()));
        }
        if !std::mem::replace(&mut said, true) {
            wait(INIT_WAITS);
        }
        std::thread::sleep(INIT_POLL);
    }
}

/// `aterm init`'s hold on the records ([`init_hold_at`]), let go when dropped, and between the
/// preferences and the sign-outs ([`aterm_machines::init::Held::release`]).
struct InitHold {
    /// The profile lock, `None` while the run's other lanes finish.
    lock: Option<HeldLock>,
    root: PathBuf,
    /// The explicit plan of a pass of the whole profile.
    run: Option<initial::ExplicitRun>,
    /// The revision and plan of the run that plan started ([`initial::run_in_force`]).
    marked: Option<(u64, [u8; 32])>,
    /// The groups the person had left as is when the records were first held.
    off: Selection,
    /// How long [`aterm_machines::init::Held::again`] waits.
    limit: std::time::Duration,
    /// How [`aterm_machines::init::Held::verified`] completes the window's run.
    complete: initial::CompleteExplicit,
}

impl aterm_machines::init::Held for InitHold {
    fn release(&mut self) {
        self.lock = None;
    }

    fn again(&mut self, wait: Option<&dyn Fn(&str)>) -> Result<(), aterm_machines::init::Unheld> {
        if self.lock.is_none() {
            self.lock = Some(init_lock_within(&self.root, wait, self.limit)?);
            // A choice the person made while the records were let go (a Stop, or a group they
            // left as is since) holds the run from now on: it records no whole pass over it. A
            // run Set up now restarted meanwhile (a new revision, or a new plan) is that run's:
            // init records nothing of it done.
            let holds = initial::holds_at(&self.root);
            if holds_the_run(&holds)
                || Setting::ALL.into_iter().any(|setting| {
                    setting.is_workstation()
                        && holds.off.contains(setting)
                        && !self.off.contains(setting)
                })
                || initial::run_in_force(&self.root).ok().flatten() != self.marked
            {
                self.run = None;
            }
        }
        Ok(())
    }

    fn withheld(&self, ids: &[String]) -> Result<(), String> {
        if self.run.is_none() {
            return Ok(());
        }
        // Only a whole profile admits automatic follow-up work. Merge its debt
        // before the Desktop lane runs, without spending retries or Restore state.
        record_withheld(&self.root, ids, &[], &[], ("", 0))
    }

    fn checkpoint(&self, settings: Selection) -> Result<(), String> {
        if self.run.is_some() {
            initial::record_applied(&self.root, settings)?;
        }
        Ok(())
    }

    fn verified(self: Box<Self>) -> Result<(), String> {
        let Some(run) = self.run else {
            return Ok(());
        };
        initial::record_applied(&self.root, Selection::all().workstation())?;
        // A vault or checkout stage the completion reads again and reopens is not this pass's
        // to report: init's own pass said its vault and repository rows, and setup's next pass
        // runs that stage again.
        (self.complete)(&self.root, run).map(drop)
    }
}

/// aterm's macOS grants as `aterm init`'s Permissions phase reads them, without a prompt
/// ([`aterm_machines::init::Grants`]).
#[must_use]
pub fn init_grants(
    sign_out: bool,
) -> Vec<(
    aterm_types::macos_setup::Access,
    aterm_machines::init::Grant,
)> {
    initial::init_grants(sign_out)
}

/// Check, apply, or restore the macOS workstation profile for the actual user.
pub fn run(action: Action) -> Result<Report, String> {
    run_observed(action).result
}

/// Observe the state belonging to a result before releasing its profile lock.
pub fn run_observed(action: Action) -> InitialSetupObserved<Report> {
    if action == Action::Check {
        return initial::observe_read_only(state_root(), |root| {
            let (judged, unjudged) = check_scope(
                root,
                initial::alab_mac,
                aterm_machines::init::ways_back_run_here(),
            );
            let mut report = execute(&mut NativeHost::selected(judged), root, action)?;
            report.rows.extend(unjudged);
            Ok(report)
        });
    }
    initial::observe_person(|root, before| {
        mutation_observed(root, before, action, initial::choice_lock_wait(), |root| {
            run_mutation(&mut NativeHost::new(), root, action)
        })
    })
}

/// What `aterm doctor --check` judges at `root`: the Mac settings the plan in force selects (the
/// person's, or with none aterm's own for this Mac's scope, `alab`: every one on an ALab Mac,
/// none elsewhere), and one row for each other: [`OFF`] where the person's choices in System
/// State hold it (a group left as is, their Stop, or records a newer aterm saved), in the words
/// `aterm doctor --apply` says of it ([`left_as_set`]), and [`NOT_IN_SCOPE`] where this Mac's
/// scope leaves it out. Neither is a failure. A way back that names `aterm init` is named only
/// where it runs (`at_console`, [`aterm_machines::init::ways_back_run_here`]).
fn check_scope(
    root: &Path,
    alab: impl FnOnce() -> bool,
    at_console: bool,
) -> (Selection, Vec<Row>) {
    let holds = initial::holds_at(root);
    let in_scope = initial::plan_in_scope(root, alab);
    let mut judged = Selection::none();
    let mut rows = Vec::new();
    for setting in Setting::ALL.into_iter().filter(|s| s.is_workstation()) {
        if holds.holds(setting) {
            rows.extend(holds.line(&[setting], at_console).map(|line| Row {
                name: setting.name().into(),
                status: OFF.into(),
                detail: line,
                elapsed_ms: None,
            }));
        } else if in_scope.contains(setting) {
            judged.set(setting, true);
        } else {
            rows.push(not_in_scope(setting, at_console));
        }
    }
    (judged, rows)
}

/// The row a pass says of `setting` where this Mac's scope leaves it out (the ALab profile on a
/// Mac that is not ALab's): not judged and not set up, and no failure; only `aterm init` naming
/// its group sets it up here, which it names for a person at this Mac's console (`at_console`).
fn not_in_scope(setting: Setting, at_console: bool) -> Row {
    let way = if at_console {
        format!(
            "; {} sets it up here",
            aterm_machines::init::naming(setting)
        )
    } else {
        String::new()
    };
    Row {
        name: setting.name().into(),
        status: NOT_IN_SCOPE.into(),
        detail: format!(
            "not part of this Mac's setup: the ALab profile is set up on ALab's Macs{way}"
        ),
        elapsed_ms: None,
    }
}

/// System State's Restore previous settings, as [`run_observed`] runs a Restore, saying `waits`
/// the moment its own attempt finds the profile lock another setup holds, so the page shows it
/// waiting from then (design §3.4), and asking `holding` once it holds the lock, before its
/// first write: the page shows it restoring from then, and `false` (a group press overtook it
/// meanwhile) lets it go having changed nothing, `Ok(None)`.
pub fn restore_observed(
    waits: impl FnMut(),
    holding: impl FnOnce() -> bool,
) -> InitialSetupObserved<Option<Report>> {
    initial::observe_person(|root, before| {
        restore_at(
            root,
            before,
            initial::choice_lock_wait(),
            waits,
            holding,
            |root| run_mutation(&mut NativeHost::new(), root, Action::Restore),
        )
    })
}

/// [`restore_observed`] at `root`, `wait` waiting for the profile lock another holds as a
/// choice does ([`mutation_observed`]). Construct the mutation host inside `restore`, after
/// the lease and the final `holding` decision, so its FDA reading belongs to this operation.
fn restore_at(
    root: &Path,
    before: Option<InitialSetupObservation>,
    wait: impl FnMut() -> bool,
    waits: impl FnMut(),
    holding: impl FnOnce() -> bool,
    restore: impl FnOnce(&Path) -> Result<Report, String>,
) -> InitialSetupObserved<Option<Report>> {
    initial::observe_choice_at(root, before, initial::noting_first(waits, wait), |root| {
        if !holding() {
            return Ok(None);
        }
        restore(root).map(Some)
    })
}

/// [`run_observed`]'s Apply or Restore at `root`, under the profile lock. A Restore is a
/// person's press, as a choice is: it waits for the lock another holds for one short write (the
/// ALab watch widening a plan, a Stop's fold), asking again while `wait` says so, and is refused
/// in the person's words ([`SETUP_BUSY`]) only past that wait, which System State then queues
/// (design §3.4). `aterm doctor --apply` is refused at once, as it always was.
/// `operation` constructs its host only after the observer owns the profile lease.
fn mutation_observed(
    root: &Path,
    before: Option<InitialSetupObservation>,
    action: Action,
    wait: impl FnMut() -> bool,
    operation: impl FnOnce(&Path) -> Result<Report, String>,
) -> InitialSetupObserved<Report> {
    if action == Action::Restore {
        initial::observe_choice_at(root, before, wait, operation)
    } else {
        initial::observe_mutation_at(root, before, Err(SETTING_UP.into()), operation)
    }
}

/// Why a change to the Mac's settings is refused while another pass holds the profile lock: the
/// window's setup, most often (its Set up now, a group's switch, the ALab watch).
pub const SETTING_UP: &str = "System State, or another setup pass, is changing this Mac's \
                              settings now, so nothing was changed; try again once it ends";

/// The action body after its caller admitted the person and took the profile lock. An Apply
/// sets up this Mac's scope, as `aterm doctor --check` judges it ([`initial::plan_in_scope`]):
/// the ALab profile and its sign-outs, which nothing undoes, only on an ALab Mac or where the
/// plan in force names them, each other setting one [`NOT_IN_SCOPE`] row that names `aterm
/// init`'s flag for it. It keeps what the person's choices in System State hold (System State
/// design principle 4): the settings they left as they are, and every setting while their Stop
/// stands, each said in one row ([`left_as_set`]); only the page's switch, or `aterm init`
/// naming the group, turns one on.
fn run_mutation(host: &mut dyn Host, root: &Path, action: Action) -> Result<Report, String> {
    let holds = if action == Action::Apply {
        initial::holds_at(root)
    } else {
        aterm_machines::init::Holds::default()
    };
    let held: Vec<Setting> = Setting::ALL
        .into_iter()
        .filter(|setting| setting.is_workstation() && holds.holds(*setting))
        .collect();
    if action == Action::Apply && (holds.stopped.is_some() || holds.newer) {
        let mut report = Report::empty(action, root);
        report.rows.extend(left_as_set(&holds, &held));
        return Ok(report);
    }
    let mut outside = Vec::new();
    if action == Action::Apply {
        let in_scope = initial::plan_in_scope(root, || host.alab());
        let mut selection = Selection::none();
        for setting in Setting::ALL.into_iter().filter(|s| s.is_workstation()) {
            if held.contains(&setting) {
                continue;
            }
            if in_scope.contains(setting) {
                selection.set(setting, true);
            } else {
                outside.push(not_in_scope(setting, true));
            }
        }
        host.select(selection);
    }
    // A pass under an older aterm's Cancel, which holds every group, records nothing: no plan,
    // no decision (whose write would lift the Cancel), no stage done, and the keys the person
    // changed stay theirs. Their Cancel stands as they made it.
    if action == Action::Apply
        && host.selection().is_empty()
        && !held.is_empty()
        && initial::cancelled_at(root)?
    {
        let mut report = Report::empty(action, root);
        report.rows.extend(left_as_set(&holds, &held));
        report.rows.extend(outside);
        return Ok(report);
    }
    // A person's Restore or Apply sets every key again: what changed since aterm set it is
    // forgotten first, so a pass after either never leaves it as it was changed (design §6.6).
    if matches!(action, Action::Apply | Action::Restore) {
        initial::kept::forget_at(root, None)?;
    }
    let explicit = if action == Action::Apply {
        let mut on = host.selection();
        // aterm's own plan on an ALab Mac holds the clones too (`scope::scoped`), unless the
        // person's choices hold them.
        on.set(
            Setting::Repositories,
            host.alab() && !holds.holds(Setting::Repositories),
        );
        Some(initial::save_explicit_workstation_plan(root, on)?)
    } else {
        None
    };
    let mut report = execute(host, root, action)?;
    report.rows.extend(left_as_set(&holds, &held));
    report.rows.extend(outside);
    if let Some(run) = explicit
        && report.complete()
    {
        // A vault or checkout stage an earlier pass finished and that no longer reads ready is
        // said here, so the pass never reads as the run's completion while it is not.
        let reopened = (host.complete_explicit())(root, run)?;
        report.rows.extend(reopened);
    }
    if action == Action::Restore {
        // A person's Restore: the note that aterm set its records aside goes with it. One left
        // only keeps that calm note up.
        let _ = initial::forget_set_aside_at(root);
    }
    Ok(report)
}

/// The one row a pass says of the settings the person's choices in System State kept from it
/// (`held`, [`aterm_machines::init::Holds::line`]); none when it kept none. Nothing failed, so
/// it needs no attention.
fn left_as_set(holds: &aterm_machines::init::Holds, held: &[Setting]) -> Option<Row> {
    holds.line(held, true).map(|line| Row {
        name: "System State".into(),
        status: OFF.into(),
        detail: line,
        elapsed_ms: None,
    })
}

/// A row's status for a setting the person turned off in System State, or held with their Stop:
/// left as they set it, which is no failure and needs nobody ([`Row::needs_attention`]).
pub const OFF: &str = "off";

/// A row's status for a setting outside this Mac's scope (the ALab profile on a Mac that is not
/// ALab's) that no choice of the person's selected: not judged, which is no failure.
pub const NOT_IN_SCOPE: &str = "not in scope";

/// What opens the error of a Restore that cannot read the settings aterm saved before setup
/// changed them (`original-preferences.json`): pressing Restore again reads the same record,
/// so System State names `aterm doctor` for why instead of asking for another press.
pub const BACKUP_UNREAD: &str = "the settings saved before setup changed them cannot be read";

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
        &initial::stamped(if selected.is_empty() {
            json!({ "profile": PROFILE, "state": "applied", "accounts_verified": true })
        } else {
            json!({ "profile": PROFILE, "state": "pending" })
        }),
    )
}

fn save_automatic_plan(root: &Path, selected: Selection) -> Result<(), String> {
    write_json(
        &root.join("automatic-plan.json"),
        &initial::stamped(json!({ "profile": PROFILE, "selection": selected.bits() })),
    )
}

fn authorized_selection(root: &Path) -> Result<Option<Selection>, String> {
    authorized_selection_with_flush(root, durable_name)
}

pub(in crate::macos_doctor) fn read_record_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
    aterm_uds::atomic_file::read_regular(path, aterm_uds::atomic_file::MAX_CONFIG_BYTES)
        .map(|(bytes, _)| bytes)
}

pub(in crate::macos_doctor) fn read_record_text(path: &Path) -> std::io::Result<String> {
    aterm_uds::atomic_file::read_regular_text(path, aterm_uds::atomic_file::MAX_CONFIG_BYTES)
}

fn authorized_selection_with_flush(
    root: &Path,
    flush: impl FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<Option<Selection>, String> {
    let path = root.join("automatic-plan.json");
    let bytes = match read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let plan: Value = aterm_json::from_slice(&bytes).map_err(|e| format!("automatic plan: {e}"))?;
    if plan["profile"].as_str() != Some(PROFILE) {
        return Err("unrecognized automatic setup profile".into());
    }
    initial::known_format(&plan)?;
    let selected =
        initial::known_selection(&plan["selection"], "unrecognized automatic setup selection")?;
    flush_record_with(&path, flush)?;
    Ok(Some(selected))
}

/// Exclusive ownership of the automatic offer and its subsequent retry worker.
#[must_use = "keep the claim alive until the automatic setup worker ends"]
pub struct AutomaticSetupClaim {
    _lock: HeldLock,
}

/// Claim the automatic offer without inspecting preferences or accounts.
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

/// [`claim_initial_setup_rerun`] in `root`, a test's scratch folder: the same
/// lock, never the person's state root.
#[cfg(any(test, debug_assertions))]
pub fn claim_initial_setup_rerun_at(root: &Path) -> Result<Option<AutomaticSetupClaim>, String> {
    Ok(HeldLock::try_take_named(root, "automatic.lock")?
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
    apply_part_at(root, Part::All)
}

/// The Workstation stage of a setup run: the selected preferences, without the
/// sign-outs, which are the Accounts stage's and run last.
fn apply_preferences_at(
    root: &Path,
    only: Option<aterm_types::macos_setup::Selection>,
) -> Result<Option<Report>, String> {
    apply_part_at(root, Part::Preferences { only })
}

/// The Accounts stage of a setup run: the selected Messages and FaceTime sign-outs. A sign-out
/// drives those apps and needs Accessibility, which the window's permission walk asks for
/// (`aterm_gui`'s `permission_walk`, with macOS's Automation question for System Events beside
/// it), and nothing of a person beyond it: accounts already signed out finish here, one waiting
/// on a grant stands aside until it is on, and each sign-out clicks only into this login's own
/// screen, unlocked, with the keys at rest, read right before its clicks
/// ([`aterm_machines::port::System::session_reach`]), else it stands aside and its next pass
/// looks again by itself ([`LOOK_AGAIN`]).
fn apply_accounts_at(root: &Path) -> Result<Option<Report>, String> {
    apply_part_at(root, Part::Accounts)
}

/// Which part of the authorized selection one unattended pass applies: all of it
/// (the first-run entry), or one stage of a System State setup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Part {
    All,
    /// The selected preferences, or `only` these where a choice narrowed the stage
    /// to the groups it turned on.
    Preferences {
        only: Option<aterm_types::macos_setup::Selection>,
    },
    Accounts,
}

fn apply_part_at(root: &Path, part: Part) -> Result<Option<Report>, String> {
    let sys = aterm_machines::port::Live::new();
    apply_part_with(
        root,
        &sys,
        &mut NativeHost::new(),
        &aterm_stamp::build_id(),
        std::time::SystemTime::now(),
        part,
    )
}

/// [`apply_on_first_run_at`] on `sys` and `host`, as `build` at `now`.
#[cfg(test)]
fn apply_on_first_run_with(
    root: &Path,
    sys: &dyn System,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    apply_part_with(root, sys, host, build, now, Part::All)
}

/// One unattended pass over `part` of the authorized selection, on `sys` and `host`, as `build`
/// at `now`: what each stage of the System State worker runs.
fn apply_part_with(
    root: &Path,
    sys: &dyn System,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
    part: Part,
) -> Result<Option<Report>, String> {
    let selected = match part {
        Part::All => {
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
            selected
        }
        Part::Preferences { .. } | Part::Accounts => {
            let Some(selected) = authorized_selection(root)? else {
                return Ok(None);
            };
            let selected = match part {
                Part::Preferences { only } => {
                    let mut selected = initial::preferences(selected);
                    if let Some(only) = only {
                        for setting in aterm_types::macos_setup::Setting::ALL {
                            if !only.contains(setting) {
                                selected.set(setting, false);
                            }
                        }
                    }
                    selected
                }
                _ => initial::accounts(selected),
            };
            if selected.is_empty() {
                // A scope a choice narrowed to nothing owes nothing: the stage is done.
                if matches!(part, Part::Preferences { only: Some(_) }) {
                    return Ok(Some(Report {
                        action: Action::Apply,
                        rows: vec![Row {
                            name: "Mac settings".into(),
                            status: "ok".into(),
                            detail: "nothing left to set in the groups turned to Set up".into(),
                            elapsed_ms: Some(0),
                        }],
                        manual_steps: Vec::new(),
                        backup: root.join("original-preferences.json"),
                        restore: None,
                        page: None,
                    }));
                }
                return Ok(None);
            }
            selected
        }
    };
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
        &json!({
            "profile": PROFILE,
            "accessibility_ready": accessibility,
            "at": epoch_secs(now),
        }),
    )?;
    // Accessibility is the window's permission walk's to ask for, beside its System Settings
    // page: a sign-out without it stands aside (`StoodAside::Permission`), and the grant's
    // access-record line wakes this run again.
    host.select(selected);
    let report = automatic_pass(host, root, build, now)?;
    Ok(Some(report))
}

/// The record an older aterm's Cancel left in `root`.
#[cfg(test)]
fn write_legacy_cancel_at(root: &Path) {
    write_json(
        &root.join("first-run.json"),
        &json!({ "profile": PROFILE, "state": "cancelled" }),
    )
    .expect("an older aterm's Cancel record");
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
    let bytes = match read_record_bytes(&path) {
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

/// The least pause between two automatic passes ([`automatic_retry_due`]), so a burst of
/// launches or wakes passes once.
const AUTOMATIC_PAUSE_SECS: u64 = 60;

/// Whether an automatic pass may start at `now`: none on record, Accessibility read otherwise
/// than at the last (`accessibility`), or [`AUTOMATIC_PAUSE_SECS`] since the last pass's start,
/// in the whole unix second that pass's own records were dated by (`at`, [`epoch_secs`]), so a
/// look the Automatic record times from that second ([`LOOK_AGAIN`]'s first, a minute on) is due
/// at the gate's own second. Timed by the file's write instead, a fraction of a second later, the
/// gate held such a look back about half the time, its row reading blocked. A last pass dated
/// after `now` (the clock set back) or with no date leaves the gate open.
fn automatic_retry_due(
    root: &Path,
    accessibility: Option<bool>,
    now: std::time::SystemTime,
) -> Result<bool, String> {
    let path = root.join("automatic-attempt.json");
    let bytes =
        match aterm_uds::atomic_file::read_regular(&path, aterm_uds::atomic_file::MAX_CONFIG_BYTES)
        {
            Ok((bytes, _)) => bytes,
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
    let Some(at) = marker["at"].as_u64() else {
        return Ok(true);
    };
    let now = epoch_secs(now);
    Ok(now < at || now >= at.saturating_add(AUTOMATIC_PAUSE_SECS))
}

/// The second the automatic gate opens again after the pass `root`'s attempt file dates
/// ([`automatic_retry_due`]), for a test that steps past the pause.
#[cfg(test)]
fn automatic_gate_opens(root: &Path) -> std::time::SystemTime {
    let bytes = std::fs::read(root.join("automatic-attempt.json")).expect("an attempt file");
    let marker: Value = aterm_json::from_slice(&bytes).expect("an attempt record");
    let at = marker["at"].as_u64().expect("the pass's own second");
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(at + AUTOMATIC_PAUSE_SECS)
}

// Each remedy names the narrowest action that changes its row: the row's own group, turned off
// and on in System State, which sets that group again and nothing else (no other group, no
// sign-out).
/// What an item whose change did not take says while its next try waits ([`RETRY`]).
const LATER: &str = "aterm tried this by itself and it did not take; it tries again by itself, less often after each try that does not take, and at once on Check again in System State; turn its group off and on in System State to try now";

/// Why a pass leaves as it is an item this run had as wanted that reads otherwise now, where a newer
/// aterm wrote the record of the keys aterm set again ([`initial::kept::Kept::frozen`]): this
/// build notes none, so it sets none again by itself.
const SETTLED: &str = "this setup had it as wanted once and it has changed since; turn its group off and on in System State to set it again";

const SETTLED_OWED: &str = "Notification Center's widget list is left to setup's own step for it, whose row says how that went; aterm set the rest once in this setup and leaves it as it is now; turn its group off and on in System State to set it all now";

/// What an item held as it is says ahead of the hold's words when its probe found a change
/// (`aterm_converge::left_as_is`): the words before it are what it reads now.
const LEFT_AS_IS: &str = "; left as it is: ";

/// Whether a row held as it is that reads otherwise (`why`, its words) is left because it changed
/// since this setup had it as wanted ([`SETTLED`]), again since aterm set it again
/// ([`initial::kept::KEPT`]), or was signed in again since aterm signed it out
/// ([`initial::kept::SIGNED_IN`]): its status is [`CHANGED`]. A row held for the Notification
/// Center step setup still owes ([`SETTLED_OWED`]) is not: that step's own row says how it went.
fn changed(why: &str) -> bool {
    why.ends_with(SETTLED)
        || why.ends_with(initial::kept::KEPT)
        || why.ends_with(initial::kept::SIGNED_IN)
}

/// The pause after each automatic try of an item whose change did not take, before this build
/// tries it again by itself ([`next_try`]): ten minutes after the first, twice as long after each
/// later one, never more than a day, so a setting that will not take is tried once a day, quietly,
/// and never given up on. The tries in a row and the last one's time are on record
/// ([`HELD_RECORD`], [`WITHHELD_RECORD`]), so a relaunch or a reboot keeps the pause, and the
/// park's timer wakes the worker when it ends ([`mac_settings_retry_ahead`]).
const RETRY: aterm_types::retry_schedule::Backoff = aterm_types::retry_schedule::Backoff::new(
    std::time::Duration::from_secs(10 * 60),
    std::time::Duration::from_secs(24 * 60 * 60),
);

/// The pause before an item that stood aside on what passes by itself is looked at again
/// ([`Host::waits`]: a sign-out behind a locked screen, another login or the person's keys, or
/// one macOS's Automation question has had no answer for): a minute after the first such wait,
/// twice as long after each later one in a row, never more than a quarter hour. A look that
/// finds the wait still holding clicks and asks nothing, and no look counts a failed try
/// ([`RETRY`]). The waits in a row
/// and the last one's time are on record ([`HELD_RECORD`]), so a relaunch or a reboot keeps the
/// pause, and the park's timer wakes the worker when it ends ([`mac_settings_retry_ahead`]); a
/// window gaining focus ends the park sooner ([`initial::InitialSetupParkMark::waits_on_settings`]).
const LOOK_AGAIN: aterm_types::retry_schedule::Backoff = aterm_types::retry_schedule::Backoff::new(
    std::time::Duration::from_secs(60),
    std::time::Duration::from_secs(15 * 60),
);

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
    // What aterm set, set again, and left as it changed since, across every run and build
    // (design §6.6): unreadable, nothing is set rather than a key something holds otherwise.
    let mut kept = initial::kept::Kept::load(root)?;
    let owed = if host.full_disk_access() == Some(true) {
        load_withheld(root)?
    } else {
        Withheld::default()
    };
    let owes = |item: &str| owed.items.iter().any(|owes| owes == item);
    let mut holds = record.holds(build, now, &owed, kept.frozen());
    kept.hold(&mut holds, &|item| owes(item).then_some(SETTLED_OWED));
    host.hold(&holds);
    let pass = execute_pass(host, root, Action::Apply, build, now)?;
    if record.add(build, now, &pass) {
        record.save(root, revision)?;
    }
    let found = initial::kept::Found {
        settled: &pass.settled,
        readings: &pass.readings,
        drifted: &pass.drifted,
        changed: &pass.changed,
        new_targets: &pass.new_targets,
        owed: &owes,
    };
    if kept.note(&found, &|item| host.item_setting(item)) {
        kept.save(root)?;
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

/// When this build tries `failed`'s item again by itself ([`RETRY`]), in seconds since the epoch,
/// read at `now`: at once for a last try recorded after `now`, the clock set back since, as a
/// pause that cannot be measured ([`aterm_types::retry_schedule::Backoff::due_by`]).
fn next_try(failed: &Failed, now: u64) -> u64 {
    RETRY.due_by(failed.at, failed.tries, now)
}

/// The items of `failed` this build's pass at `now` puts off ([`Hold::Later`]): each whose last
/// try's pause has not ended ([`next_try`]).
fn put_off(failed: &[Failed], build: &str, now: u64) -> Vec<(String, Hold)> {
    failed
        .iter()
        .filter(|f| f.build == build && now < next_try(f, now))
        .map(|failed| (failed.item.clone(), Hold::Later(LATER.into())))
        .collect()
}

/// A next automatic Mac settings try is due in this run and build. Return
/// the latest due deadline so an older, still-blocked item cannot hide another
/// item's deadline. Reading this never spends or restarts an attempt.
fn mac_settings_retry_due(
    root: &Path,
    revision: u64,
    build: &str,
    now: u64,
) -> Result<Option<u64>, String> {
    mac_settings_next_try(root, revision, build, now, |dues| {
        dues.filter(|due| *due <= now).max()
    })
}

/// The earliest next automatic Mac settings try in this run and build that is not due yet
/// ([`mac_settings_retry_due`]'s deadlines still ahead of `now`): when a parked worker looks
/// again by itself, with no timer before it.
fn mac_settings_retry_ahead(
    root: &Path,
    revision: u64,
    build: &str,
    now: u64,
) -> Result<Option<u64>, String> {
    mac_settings_next_try(root, revision, build, now, |dues| {
        dues.filter(|due| *due > now).min()
    })
}

/// The deadline `pick` takes from each next automatic Mac settings try this run and build owes,
/// read at `now` ([`next_try`]): every item whose change did not take, however often, has one;
/// an entry that counts no try names none.
fn mac_settings_next_try(
    root: &Path,
    revision: u64,
    build: &str,
    now: u64,
    pick: impl Fn(&mut dyn Iterator<Item = u64>) -> Option<u64>,
) -> Result<Option<u64>, String> {
    let automatic = Automatic::load(root, revision).map(|record| {
        let owed = |failed: &&Failed| {
            failed.build == build && failed.tries > 0 && !record.settled.contains(&failed.item)
        };
        pick(
            &mut record
                .failed
                .iter()
                .filter(owed)
                .map(|failed| next_try(failed, now))
                .chain(
                    record
                        .waits
                        .iter()
                        .filter(owed)
                        .map(|waits| LOOK_AGAIN.due_by(waits.at, waits.tries, now)),
                ),
        )
    });
    // A separate Full Disk Access follow-up can fail without an ordinary pass
    // recording that attempt. Its scope includes approved debt from an earlier
    // stage, even when this run queues only another group of Mac settings.
    let withheld = (|| {
        let record = load_withheld_reading_revision(root, || Ok(revision))?;
        let Some(chosen) = initial::withheld_selection(root)? else {
            return Ok(None);
        };
        Ok(pick(
            &mut record
                .failed
                .iter()
                .filter(|failed| {
                    failed.build == build
                        && failed.tries > 0
                        && record.items.contains(&failed.item)
                        && aterm_machines::init_preferences::setting_of(&failed.item)
                            .is_some_and(|setting| chosen.contains(setting))
                })
                .map(|failed| next_try(failed, now)),
        ))
    })();
    if initial::revision_at(root)? != revision {
        return Ok(None);
    }
    // One unreadable cache must not hide a known deadline from the other, or
    // create a deadline where neither record establishes one.
    match (automatic, withheld) {
        (Ok(a), Ok(b)) => Ok(match (a, b) {
            (Some(a), Some(b)) => pick(&mut [a, b].into_iter()),
            (one, None) | (None, one) => one,
        }),
        (known @ Ok(Some(_)), Err(_)) | (Err(_), known @ Ok(Some(_))) => known,
        (Err(why), _) | (_, Err(why)) => Err(why),
    }
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
    /// The waits in a row on what passes by itself ([`Host::waits`]), per item and build: no
    /// try of the item ran, and its next look comes on [`LOOK_AGAIN`].
    waits: Vec<Failed>,
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
        let bytes = match read_record_bytes(&path) {
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
        // An older aterm's record keeps no waits: none is under way.
        let waits = match &record["waits"] {
            Value::Null => Vec::new(),
            waits => load_tries(waits, &bad)?,
        };
        Ok(Self {
            settled: current_ids(settled, false),
            failed: current_tries(failed, false),
            waits,
        })
    }

    fn save(&self, root: &Path, revision: u64) -> Result<(), String> {
        write_json(
            &root.join(HELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": revision,
                "settled": self.settled, "failed": tries_json(&self.failed),
                "waits": tries_json(&self.waits),
            }),
        )
    }

    /// What this run's pass at `now` in `build` leaves alone: each settled item the Full Disk
    /// Access follow-up still owes (`owed`), as it is, and each item whose last try's pause has
    /// not ended, put off. Any other settled item is the record of the keys aterm set's to hold
    /// ([`initial::kept::Kept::hold`]), which sets it again once should it read otherwise; only
    /// where a newer aterm wrote that record (`frozen`), which notes nothing this build sets, is
    /// each held as it is.
    fn holds(&self, build: &str, now: u64, owed: &Withheld, frozen: bool) -> Vec<(String, Hold)> {
        let mut holds: Vec<(String, Hold)> = self
            .settled
            .iter()
            .filter_map(|item| {
                let why = if owed.items.contains(item) {
                    SETTLED_OWED
                } else if frozen {
                    SETTLED
                } else {
                    return None;
                };
                Some((item.clone(), Hold::AsIs(why.into())))
            })
            .collect();
        let owed_tries = put_off(&owed.failed, build, now)
            .into_iter()
            .filter(|(item, _)| owed.items.contains(item));
        for (item, hold) in put_off(&self.failed, build, now)
            .into_iter()
            .chain(owed_tries)
        {
            if !holds.iter().any(|(held, _)| *held == item) {
                holds.push((item, hold));
            }
        }
        holds
    }

    /// Note `pass`, made in `build` at `now`: an item it settled is settled, and one whose change
    /// did not take counts a try and is settled no more, though an earlier pass had it as wanted
    /// (it changed since, and the try to set it again did not take), so its next try waits on
    /// its pause; an item it found waiting on what passes by itself counts a wait in a row, whose
    /// next look comes on [`LOOK_AGAIN`]. Whether the record changed.
    fn add(&mut self, build: &str, now: u64, pass: &Pass) -> bool {
        let before = (
            self.settled.clone(),
            self.failed.clone(),
            self.waits.clone(),
        );
        self.settled.retain(|item| !pass.unverified.contains(item));
        for item in &pass.settled {
            if !self.settled.contains(item) {
                self.settled.push(item.clone());
            }
        }
        count_tries(&mut self.failed, build, now, &pass.unverified);
        let settled = &self.settled;
        self.failed.retain(|f| !settled.contains(&f.item));
        // A wait in a row ends with the first pass that looks at the item and finds it no longer
        // waiting; a pass that never looked at it (another stage's) leaves it as it was.
        self.waits
            .retain(|w| pass.waits.contains(&w.item) || !pass.looked.contains(&w.item));
        count_tries(&mut self.waits, build, now, &pass.waits);
        (&self.settled, &self.failed, &self.waits) != (&before.0, &before.1, &before.2)
    }
}

/// A person pressed Check again ([`initial::retry_parked_setup_stages`]): run `revision`'s Mac
/// settings that did not take start their tries afresh, so its next pass tries each now, with no
/// pause and no try counted. What was set and read back stays settled; another run's records
/// are left as they are.
///
/// # Errors
/// A record cannot be read or written.
fn mac_settings_afresh(root: &Path, revision: u64) -> Result<(), String> {
    let mut automatic = Automatic::load(root, revision)?;
    if !automatic.failed.is_empty() || !automatic.waits.is_empty() {
        automatic.failed.clear();
        automatic.waits.clear();
        automatic.save(root, revision)?;
    }
    let mut withheld = load_withheld_reading_revision(root, || Ok(revision))?;
    if !withheld.failed.is_empty() && initial::revision_at(root)? == revision {
        withheld.failed.clear();
        save_withheld_reading_revision(root, &withheld, || Ok(revision))?;
    }
    Ok(())
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
    fn ready(self) -> Result<bool, String> {
        match self {
            Self::Person => Ok(true),
            Self::Refused(_) => Ok(false),
            Self::Unresolved(why) => Err(why),
        }
    }

    fn lasting(self) -> Option<String> {
        match self {
            Self::Refused(why) => Some(why),
            Self::Person | Self::Unresolved(_) => None,
        }
    }
}

/// Read scope readiness from scratch account paths without consulting this process's account.
#[cfg(any(test, debug_assertions))]
pub fn initial_setup_scope_ready_at(home: &Path, account_home: &Path) -> Result<bool, String> {
    scope_of(false, 501, Some(home), || Ok(account_home.to_path_buf())).ready()
}

/// [`lasting_scope_refusal`] from scratch account paths, without consulting this process's
/// account.
#[cfg(any(test, debug_assertions))]
pub fn lasting_scope_refusal_at(home: &Path, account_home: &Path) -> Option<String> {
    scope_of(false, 501, Some(home), || Ok(account_home.to_path_buf())).lasting()
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

/// Whether startup may bind this account's setup records. A lasting refusal is `false`;
/// an unresolved account or home is an error to retry before observing those records.
pub fn initial_setup_scope_ready() -> Result<bool, String> {
    person_scope().ready()
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
        let file: std::fs::File = match aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                .open(root.join("profile.lock"))
        }) {
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
        Self::try_take(root)?.ok_or_else(|| SETTING_UP.into())
    }

    fn try_take(root: &Path) -> Result<Option<Self>, String> {
        let held = Self::try_take_named(root, "profile.lock")?;
        if let Some(held) = &held {
            held.mark_holder();
        }
        Ok(held)
    }

    /// Write a fresh mark of this hold in profile.lock, which a setup stage's begin records and
    /// a reading of the run compares (`initial::ledger`, design §6.5): a stage is alive only
    /// while the hold that began it holds the lock, never while another does (a choice's save,
    /// `aterm doctor --apply`). The mark carries this process's id in its top 32 bits, so a quit
    /// notes only the stages its own process began (`initial::ledger::hold_process`). Best
    /// effort: a mark not written leaves the last one, so a stage reads alive no more often than
    /// when nothing was marked. An older aterm's hold writes no mark, which leaves the last one
    /// too.
    fn mark_holder(&self) {
        use std::os::unix::fs::FileExt as _;
        static TAKES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let take = TAKES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| {
                u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
            });
        let mark =
            (u128::from(std::process::id()) << 96) | (u128::from(nanos) << 32) | u128::from(take);
        let text = format!("{mark:032x}\n");
        let _ = self
            .0
            .set_len(0)
            .and_then(|()| self.0.write_all_at(text.as_bytes(), 0));
    }

    /// The mark the latest hold of profile.lock wrote in it ([`Self::mark_holder`]), if one reads.
    fn profile_holder(root: &Path) -> Option<u128> {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
            .open(root.join("profile.lock"))
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.file_type().is_file() {
            return None;
        }
        let mut text = String::with_capacity(
            usize::try_from(meta.len().min(64))
                .unwrap_or(0)
                .saturating_add(1),
        );
        file.take(64).read_to_string(&mut text).ok()?;
        let trimmed = text.trim();
        if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        u128::from_str_radix(trimmed, 16)
            .ok()
            .filter(|mark| *mark != 0)
    }

    fn try_take_named(root: &Path, name: &str) -> Result<Option<Self>, String> {
        let file = Self::open_named(root, name)?;
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

    /// Wait for a shared hold of `name`: one of the holders that each keep a lasting fact true
    /// (a window that runs setup), which an exclusive take waits out.
    fn take_shared_named(root: &Path, name: &str) -> Result<Self, String> {
        let file = Self::open_named(root, name)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "HeldLock::drop releases by LOCK_UN before closing, including child-spawn paths"
        )]
        file.lock_shared()
            .map_err(|error| format!("doctor lock: {error}"))?;
        Ok(Self(file))
    }

    /// Wait for `name`: a short record protocol, never the profile's OS work, or a shared
    /// holder's lasting fact ([`Self::take_shared_named`]) to end.
    fn take_named(root: &Path, name: &str) -> Result<Self, String> {
        let file = Self::open_named(root, name)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "HeldLock::drop releases by LOCK_UN before closing, including child-spawn paths"
        )]
        file.lock()
            .map_err(|error| format!("doctor lock: {error}"))?;
        Ok(Self(file))
    }

    fn open_named(root: &Path, name: &str) -> Result<std::fs::File, String> {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A test takes its file system place before it holds the doctor lock this opens: a wait
        // for one under the lock would hold up each of the test's threads that takes it.
        #[cfg(test)]
        aterm_spec::fs_lane::join();
        std::fs::create_dir_all(root).map_err(|e| format!("{}: {e}", root.display()))?;
        let file = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
                .open(root.join(name))
        })
        .map_err(|e| format!("doctor lock: {e}"))?;
        if !file
            .metadata()
            .map_err(|e| format!("doctor lock: {e}"))?
            .file_type()
            .is_file()
        {
            return Err("doctor lock is not a regular file".into());
        }
        Ok(file)
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
    /// Whether this is an ALab Mac ([`initial::alab_mac`]): the scope `aterm doctor --apply`
    /// sets up where aterm's own plan is in force ([`initial::plan_in_scope`]).
    fn alab(&self) -> bool;
    fn full_disk_access(&self) -> Option<bool>;
    fn select(&mut self, selected: Selection);
    fn read(&mut self, target: &Target) -> Result<Option<PValue>, String>;
    fn write(&mut self, target: &Target, value: Option<&PValue>) -> Result<(), String>;
    fn preference_changes(&mut self, group: WorkstationGroup) -> Result<Vec<Saved>, String>;
    fn attempts(&mut self) -> Vec<Attempt>;
    fn settled(&mut self) -> Vec<String>;
    /// The items an apply pass found waiting on what passes by itself since the last take
    /// ([`aterm_machines::init_accounts::looks_again`]): no try of theirs ran, and the next
    /// pass looks at them again on [`LOOK_AGAIN`].
    fn waits(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// The items an apply pass reported a row for since the last take: a wait of one of them
    /// not among [`Self::waits`] is over.
    fn looked(&mut self) -> Vec<String> {
        Vec::new()
    }
    fn withheld(&mut self) -> Vec<String>;
    fn hold(&mut self, items: &[(String, Hold)]);
    fn held(&self) -> Vec<String>;
    /// The items an apply pass left as they were ([`Hold::AsIs`], or [`Hold::ReadBack`] once set
    /// again) although they read otherwise than it sets them, since the last take.
    fn drifted(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// What each item an apply pass settled reads now, in its row's words, since the last take.
    fn readings(&mut self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// Each item an apply pass changed and read back as wanted, with what it read before the
    /// change in its row's words, since the last take.
    fn changed(&mut self) -> Vec<(String, String)> {
        Vec::new()
    }
    /// The items held as read back ([`Hold::ReadBack`]) whose change an apply pass made only to
    /// targets their read-back did not name, since the last take.
    fn new_targets(&mut self) -> Vec<String> {
        Vec::new()
    }
    /// Preferences whose complete tagged item cohort this pass verified as wanted. A held,
    /// unread or absent cohort supplies no new completion evidence.
    fn verified_settings(&mut self) -> Selection {
        Selection::none()
    }
    /// Native facts paid for by the last workstation pass, transferred once to its report.
    fn take_readback(&mut self) -> WorkstationReadback {
        WorkstationReadback::EMPTY
    }
    /// The System State row the item `item` was built for.
    fn item_setting(&self, item: &str) -> Option<Setting> {
        aterm_machines::init_preferences::setting_of(item)
    }
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
    /// Run `group`'s items, applying them when `apply`: each row with the System State row its
    /// item was built for, which the page names in place of the item's own title.
    fn workstation(
        &mut self,
        group: WorkstationGroup,
        apply: bool,
    ) -> Result<Vec<(Option<Setting>, Row)>, String>;
    /// Put the live settings `saved` holds back as well as the saved preferences: `saved` is
    /// only what was put back and kept. A setting whose saved preference is back but loads only
    /// at the next login is [`LOGIN`], named as its System State row is, with a note to log
    /// out: `blocked` means Restore could not put a preference back.
    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<RuntimeRow>;
    /// How an apply that set every row completes the window's run under `root`
    /// ([`initial::CompleteExplicit`]): it reads this Mac's vault and checkouts again first.
    fn complete_explicit(&self) -> initial::CompleteExplicit;
}

/// One row [`Host::restore_runtime`] gives, and how it says the System State row it is about
/// came out.
struct RuntimeRow {
    row: Row,
    /// The row and how it came out; `None` for the one row about none, the change journal's
    /// refusal of a record ([`journal_refused`]).
    put_back: Option<(Setting, PutBack)>,
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
    /// [`Host::waits`].
    waits: Vec<String>,
    /// [`Host::looked`].
    looked: Vec<String>,
    withheld: Vec<String>,
    held: Vec<(String, Hold)>,
    /// [`Host::drifted`], [`Host::readings`], [`Host::changed`] and [`Host::new_targets`], as the
    /// pass's rows said them.
    drifted: Vec<String>,
    readings: Vec<(String, String)>,
    changed: Vec<(String, String)>,
    new_targets: Vec<String>,
    /// Each item's System State row, as the passes' tagged rows said ([`Host::item_setting`]).
    tagged: Vec<(String, Setting)>,
    /// Complete preference cohorts verified by this pass, independent of other settings.
    verified: Selection,
    /// How an apply that set every row completes the window's run ([`Host::complete_explicit`]).
    complete: initial::CompleteExplicit,
    /// What the last workstation run read of its rows ([`Host::take_readback`]).
    readback: WorkstationReadback,
}

impl NativeHost {
    fn new() -> Self {
        Self::over(
            Box::new(aterm_machines::port::Live::new().with_full_disk_access(full_disk_access())),
            Box::new(aterm_machines::init::prepare_workstation_selected),
            initial::complete_explicit_workstation_plan,
        )
    }

    fn selected(selected: Selection) -> Self {
        let mut host = Self::new();
        host.selected = selected.workstation();
        host
    }

    /// A host over `sys`, preparing its passes with `prepare`, whose explicit apply completes the
    /// window's run with `complete`.
    fn over(sys: Box<dyn System>, prepare: Prepare, complete: initial::CompleteExplicit) -> Self {
        Self {
            sys,
            prepare,
            selected: Selection::all().workstation(),
            domains: BTreeMap::new(),
            prepared: None,
            attempts: Vec::new(),
            settled: Vec::new(),
            waits: Vec::new(),
            looked: Vec::new(),
            withheld: Vec::new(),
            held: Vec::new(),
            drifted: Vec::new(),
            readings: Vec::new(),
            changed: Vec::new(),
            new_targets: Vec::new(),
            tagged: Vec::new(),
            verified: Selection::none(),
            complete,
            readback: WorkstationReadback::EMPTY,
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
    fn alab(&self) -> bool {
        initial::alab_mac()
    }
    fn complete_explicit(&self) -> initial::CompleteExplicit {
        self.complete
    }
    fn selection(&self) -> Selection {
        self.selected
    }
    fn full_disk_access(&self) -> Option<bool> {
        self.sys.full_disk_access()
    }
    fn select(&mut self, selected: Selection) {
        self.selected = selected.workstation();
        self.prepared = None;
        self.verified = Selection::none();
    }
    fn attempts(&mut self) -> Vec<Attempt> {
        std::mem::take(&mut self.attempts)
    }
    fn settled(&mut self) -> Vec<String> {
        std::mem::take(&mut self.settled)
    }
    fn waits(&mut self) -> Vec<String> {
        std::mem::take(&mut self.waits)
    }
    fn looked(&mut self) -> Vec<String> {
        std::mem::take(&mut self.looked)
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
    fn drifted(&mut self) -> Vec<String> {
        std::mem::take(&mut self.drifted)
    }
    fn readings(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.readings)
    }
    fn changed(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.changed)
    }
    fn new_targets(&mut self) -> Vec<String> {
        std::mem::take(&mut self.new_targets)
    }
    fn verified_settings(&mut self) -> Selection {
        std::mem::replace(&mut self.verified, Selection::none())
    }
    fn take_readback(&mut self) -> WorkstationReadback {
        self.readback.take()
    }
    fn item_setting(&self, item: &str) -> Option<Setting> {
        self.tagged
            .iter()
            .find(|(id, _)| id == item)
            .map(|(_, setting)| *setting)
            .or_else(|| aterm_machines::init_preferences::setting_of(item))
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
            aterm_machines::items::defaults::Session::Ask,
            name,
            &keys,
        );
        // What it now holds is what it settled on, not what was read before it.
        for target in reads {
            self.domains.remove(&(target.domain.clone(), target.host));
        }
        answer.map_err(|why| format!("restart {name}: {why}"))
    }

    fn workstation(
        &mut self,
        group: WorkstationGroup,
        apply: bool,
    ) -> Result<Vec<(Option<Setting>, Row)>, String> {
        use aterm_machines::model::{Mode, Outcome};
        self.readback = WorkstationReadback::EMPTY;
        let mode = if apply { Mode::Apply } else { Mode::Plan };
        let prepared = match self.prepared.take() {
            Some((prepared_group, prepared)) if prepared_group == group => prepared,
            _ => self.prepare(group)?,
        };
        self.domains.clear();
        let (result, readback) =
            run_workstation_readback(prepared, self.sys.as_ref(), group, mode)?;
        self.readback = readback;
        let settings: Vec<(String, Setting)> = result
            .tagged()
            .into_iter()
            .map(|(setting, row)| (row.id.clone(), setting))
            .collect();
        // An item held as it is whose probe found a change reads its value in its row's words
        // ahead of the hold's (`aterm_converge::left_as_is`): it drifted, a check's reading as an
        // apply's. One left as it is because it changed since this setup set it, or again since
        // aterm set it again ([`changed`]), says so ([`CHANGED`]), never `ok`.
        let drifted: Vec<(String, String)> = result
            .rows
            .iter()
            .filter(|row| {
                row.outcome == Outcome::NotApplicable
                    && self.held.iter().any(|(item, hold)| {
                        *item == row.id && matches!(hold, Hold::AsIs(_) | Hold::ReadBack { .. })
                    })
            })
            .filter_map(|row| {
                let (from, _) = row.why.as_deref()?.split_once(LEFT_AS_IS)?;
                Some((row.id.clone(), from.to_owned()))
            })
            .collect();
        let rows = if apply {
            use aterm_machines::items::defaults::Scope;
            use aterm_machines::model::Read;
            let tagged = result.tagged();
            let selected = initial::preferences(group.selected(self.selected));
            for setting in Setting::ALL {
                if !selected.contains(setting) {
                    continue;
                }
                let cohort: Vec<_> = tagged
                    .iter()
                    .filter(|(member, _)| *member == setting)
                    .map(|(_, row)| *row)
                    .collect();
                if !cohort.is_empty()
                    && cohort
                        .iter()
                        .all(|row| matches!(row.outcome, Outcome::Ok | Outcome::Changed))
                {
                    self.verified.set(setting, true);
                }
            }
            if let Ok(root) = state_root() {
                initial::note_at_login(&root, initial::changed_at_login(result.tagged()));
            }
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
            self.waits.extend(
                result
                    .rows
                    .iter()
                    .filter(|row| aterm_machines::init_accounts::looks_again(row))
                    .map(|row| row.id.clone()),
            );
            self.looked
                .extend(result.rows.iter().map(|row| row.id.clone()));
            // What each item the pass changed read before: an item set again reads, beside its
            // read-back, what it changed to.
            self.changed.extend(
                result
                    .rows
                    .iter()
                    .filter(|row| row.outcome == Outcome::Changed)
                    .filter_map(|row| Some((row.id.clone(), row.from.clone()?))),
            );
            self.new_targets.extend(result.new_targets.iter().cloned());
            // A drifted item reads what it reads now; a set or verified one its value.
            for row in &result.rows {
                let reads =
                    match row.outcome {
                        Outcome::NotApplicable => drifted
                            .iter()
                            .find(|(item, _)| *item == row.id)
                            .map(|(item, from)| {
                                self.drifted.push(item.clone());
                                from.clone()
                            }),
                        Outcome::Ok | Outcome::Changed => row.to.clone(),
                        _ => None,
                    };
                if let Some(reads) = reads {
                    self.readings.push((row.id.clone(), reads));
                }
            }
            self.tagged.extend(settings.iter().cloned());
            self.withheld.extend(result.withheld);
            result.rows
        } else {
            result.rows
        };
        self.domains.clear();
        Ok({
            rows.into_iter()
                .map(|row| {
                    let setting = settings
                        .iter()
                        .find(|(id, _)| *id == row.id)
                        .map(|(_, setting)| *setting);
                    let status = if drifted.iter().any(|(item, _)| *item == row.id)
                        && changed(row.why.as_deref().unwrap_or_default())
                    {
                        CHANGED
                    } else {
                        initial::row_status(&row)
                    };
                    let row = Row {
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
                        elapsed_ms: row.elapsed_ms,
                    };
                    (setting, row)
                })
                .collect()
        })
    }

    fn restore_runtime(&mut self, saved: &[Saved]) -> Vec<RuntimeRow> {
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
                    rows.push(RuntimeRow {
                        row: journal_refused(&[format!("{id}: {why}")]),
                        put_back: None,
                    });
                }
                let result = self.sys.set_natural_scrolling(value).and_then(|()| {
                    if self.sys.natural_scrolling()? == value {
                        Ok(())
                    } else {
                        Err("the original scroll direction did not activate".into())
                    }
                });
                rows.push(match result {
                    Ok(()) => RuntimeRow {
                        row: Row {
                            name: "Original scroll direction".into(),
                            status: "restored".into(),
                            detail: "saved and active scroll direction restored".into(),
                            elapsed_ms: None,
                        },
                        put_back: Some((Setting::NaturalScrolling, PutBack::Back)),
                    },
                    // The saved direction is back, and macOS loads it at the next login.
                    Err(detail) => RuntimeRow {
                        row: Row {
                            name: "Original scroll direction".into(),
                            status: "error".into(),
                            detail,
                            elapsed_ms: None,
                        },
                        put_back: Some((Setting::NaturalScrolling, PutBack::AtLogin)),
                    },
                });
            } else {
                rows.push(RuntimeRow {
                    row: Row {
                        name: Setting::NaturalScrolling.name().into(),
                        status: LOGIN.into(),
                        detail: "Log out and back in to use the restored scroll direction.".into(),
                        elapsed_ms: None,
                    },
                    put_back: Some((Setting::NaturalScrolling, PutBack::AtLogin)),
                });
            }
        }
        if saved.iter().any(|s| {
            s.target.host
                && s.target
                    .key
                    .starts_with("com.apple.keyboard.modifiermapping.")
        }) {
            rows.push(RuntimeRow {
                row: Row {
                    name: Setting::CapsLock.name().into(),
                    status: LOGIN.into(),
                    detail: "Log out and back in to use the restored keyboard settings.".into(),
                    elapsed_ms: None,
                },
                put_back: Some((Setting::CapsLock, PutBack::AtLogin)),
            });
        }
        // Trackpad scrolling's keys are saved preferences macOS loads only at the next login
        // (their items' effect), and nothing here relaunches a reader of them: back as saved,
        // and in use from that login.
        if saved.iter().any(|s| {
            aterm_machines::init_preferences::setting_of_preference(&s.target.domain, &s.target.key)
                == Some(Setting::TrackpadScrolling)
        }) {
            rows.push(RuntimeRow {
                row: Row {
                    name: Setting::TrackpadScrolling.name().into(),
                    status: LOGIN.into(),
                    detail: "Log out and back in to use the restored trackpad settings.".into(),
                    elapsed_ms: None,
                },
                put_back: Some((Setting::TrackpadScrolling, PutBack::AtLogin)),
            });
        }
        rows
    }
}

/// The name of the row that says the change journal refused records ([`journal_refused`]).
pub const JOURNAL_ROW: &str = "Change journal";

fn journal_refused(refused: &[String]) -> Row {
    Row {
        name: JOURNAL_ROW.into(),
        status: "error".into(),
        detail: format!(
            "the change journal refused these records; the changes went ahead anyway: {}",
            refused.join("; ")
        ),
        elapsed_ms: None,
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
    let bytes = match read_record_bytes(path) {
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
            let xml = entry.get("xml").ok_or("backup XML is missing")?;
            let value = if xml.is_null() {
                None
            } else {
                Some(plist::parse(
                    xml.as_str().ok_or("backup XML is invalid")?.as_bytes(),
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

/// Originals a Restore still owes, including those waiting for a queued setup choice. A pass
/// that already set a row again retired its old put-back and needs no original for it.
fn required_restore_originals(root: &Path) -> Result<Vec<Target>, String> {
    Ok(restore_waiting(root)?
        .into_iter()
        .filter(|target| initial::restore_owed(root, row_of(target)) != initial::RestoreOwed::LetGo)
        .collect())
}

fn backup_unread(why: String) -> String {
    if why.starts_with(BACKUP_UNREAD) {
        why
    } else {
        format!("{BACKUP_UNREAD}: {why}")
    }
}

/// Validate the complete pending set before any preference is captured or changed. An absent
/// backup is valid only when no retained put-back needs an original from it.
fn load_backup_preserving_restore(path: &Path, required: &[Target]) -> Result<Vec<Saved>, String> {
    let saved = load_backup(path).map_err(|why| {
        if required.is_empty() {
            why
        } else {
            backup_unread(why)
        }
    })?;
    for target in required {
        if !saved
            .iter()
            .any(|saved| saved.target == *target && unknown_original(saved).is_none())
        {
            return Err(backup_unread(format!(
                "the original for {} is missing or unknown",
                target.describe()
            )));
        }
    }
    Ok(saved)
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
/// flushed it while it was the same directory (device and inode) and `entry` named the same file
/// there, so the doctor's passes do not queue on the shared directories' locks. A record written
/// again (a rename over it) or a directory replaced is new and flushed again; a name moved away
/// and back in between is the one change this cannot see, and nothing moves a doctor record or
/// its directories.
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
    // A test whose first file system work is a record write takes its place here; one that took
    // a doctor lock through `HeldLock::open_named` holds it already.
    #[cfg(test)]
    aterm_spec::fs_lane::join();
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
    let selected = group.selected(host.selection());
    if selected.is_empty() {
        return;
    }
    // The page says a step that failed as a whole under its rows (design §3.6): the group's
    // selected rows, and of a follow-up only those it withholds without Full Disk Access.
    let covers = match group {
        WorkstationGroup::Withheld => aterm_machines::init_preferences::withheld_settings(selected),
        _ => selected,
    };
    let name = match group {
        WorkstationGroup::Accounts => "Messages and FaceTime sign-out",
        WorkstationGroup::Preferences => "Workstation preferences",
        WorkstationGroup::InputActivation => "Active keyboard and trackpad settings",
        WorkstationGroup::All => "Workstation settings",
        WorkstationGroup::Withheld => "Settings waiting for Full Disk Access",
    };
    let mut clock = RowClock::start();
    match host.workstation(group, apply) {
        Ok(rows) if rows.is_empty() => as_step(
            report,
            Row {
                name: name.into(),
                status: "unverified".into(),
                detail: "setup returned no settings to verify".into(),
                elapsed_ms: None,
            },
            covers,
        ),
        Ok(rows) => {
            for (setting, row) in rows {
                // An item's line names its keys or keyboard: the page names its row, and says a
                // change that failed or could not be read back in its own words (design §3.6).
                match setting {
                    Some(setting) => {
                        let how = match row.status.as_str() {
                            "error" => Some(Unfinished::Failed),
                            "unverified" => Some(Unfinished::Unconfirmed),
                            _ => None,
                        };
                        if let Some(how) = how {
                            page(report).outcome.unfinished(setting, how);
                        }
                        report.rows.push(row);
                    }
                    None => as_is(report, row),
                }
            }
        }
        Err(detail) => as_step(
            report,
            Row {
                name: name.into(),
                status: "unverified".into(),
                detail,
                elapsed_ms: None,
            },
            covers,
        ),
    }
    let readback = host.take_readback();
    if !readback.is_empty() {
        page(report).readback.append(readback);
    }
    clock.lap(&mut report.rows);
}

/// `report`'s rows for System State's page ([`Report::page`]), begun where none were.
fn page(report: &mut Report) -> &mut PassPage {
    report.page.get_or_insert_with(PassPage::default)
}

/// Add `row`, which is about no one setting's keys, to `report` and to its page as it is.
fn as_is(report: &mut Report, row: Row) {
    page(report).rows.push(row.clone());
    report.rows.push(row);
}

/// Add `row`, a row about a whole step that `aterm doctor` names in its own words (`Workstation
/// preferences`, `Settings waiting for Full Disk Access`), to `report`, and to its page under
/// the page's rows for `settings`, the System State rows the step covers ([`step_page`]).
fn as_step(report: &mut Report, row: Row, settings: Selection) {
    page(report).merge(step_page(
        &row,
        Setting::ALL
            .into_iter()
            .filter(|setting| settings.contains(*setting))
            .map(Subject::Setting),
    ));
    report.rows.push(row);
}

/// The page's rows for `row`, a row about a whole step that `aterm doctor` names in its own
/// words: one for each of `subjects`, the System State rows the step covers, named as the page
/// names it, with `row`'s status and no detail, since the page says a step that failed or could
/// not be confirmed in its own words (design §3.6) and `aterm doctor` and the log keep the
/// step's.
fn step_page(row: &Row, subjects: impl IntoIterator<Item = Subject>) -> PassPage {
    PassPage {
        outcome: PassOutcome::default(),
        readback: WorkstationReadback::EMPTY,
        rows: subjects
            .into_iter()
            .map(|subject| Row {
                name: subject.name().into(),
                status: row.status.clone(),
                detail: String::new(),
                elapsed_ms: None,
            })
            .collect(),
    }
}

fn execute(host: &mut dyn Host, root: &Path, action: Action) -> Result<Report, String> {
    execute_pass(
        host,
        root,
        action,
        &aterm_stamp::build_id(),
        epoch_secs(std::time::SystemTime::now()),
    )
    .map(|pass| pass.report)
}

struct Pass {
    report: Report,
    unverified: Vec<String>,
    settled: Vec<String>,
    /// The items that waited on what passes by itself ([`Host::waits`]).
    waits: Vec<String>,
    /// The items the pass reported a row for ([`Host::looked`]).
    looked: Vec<String>,
    /// Of `settled`, the items left as they were although they read otherwise ([`Host::drifted`]).
    drifted: Vec<String>,
    /// What the settled items read ([`Host::readings`]).
    readings: Vec<(String, String)>,
    /// Of `settled`, the items changed, with what each read before ([`Host::changed`]).
    changed: Vec<(String, String)>,
    /// Of `changed`, those changed only in targets their read-back did not name
    /// ([`Host::new_targets`]).
    new_targets: Vec<String>,
}

impl From<Report> for Pass {
    fn from(report: Report) -> Self {
        Self {
            report,
            unverified: Vec::new(),
            settled: Vec::new(),
            waits: Vec::new(),
            looked: Vec::new(),
            drifted: Vec::new(),
            readings: Vec::new(),
            changed: Vec::new(),
            new_targets: Vec::new(),
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
    // A failed earlier pass can end before its staging is drained. Only this pass's complete
    // item cohorts can checkpoint the current selection.
    let _ = host.verified_settings();
    let backup = root.join("original-preferences.json");
    let mut report = Report {
        action,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup,
        restore: None,
        page: None,
    };
    if action == Action::Restore {
        return restore(host, root, report).map(Pass::from);
    }
    let mut clock = RowClock::start();
    if host.selection().is_empty() {
        if action == Action::Apply {
            initial::record_applied(root, host.selection())?;
        }
        as_is(
            &mut report,
            Row {
                name: "Selected Mac settings".into(),
                status: "ok".into(),
                detail: "no settings selected; macOS was left unchanged".into(),
                elapsed_ms: None,
            },
        );
        clock.lap(&mut report.rows);
        return Ok(report.into());
    }
    // The sign-outs are their own stage: a pass of every preference is the full profile's, with
    // or without them.
    let full_profile =
        initial::preferences(host.selection()) == initial::preferences(Selection::all());
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
                detail: "a previous pass still needs activation; turn its group off and on in \
                         System State to retry"
                    .into(),
                elapsed_ms: None,
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
                elapsed_ms: None,
            });
        }
        clock.lap(&mut report.rows);
        // A key kept as it changed since aterm set it is left as it is (design §6.6): System
        // State reads it "Changed since aterm set it", and the check says it `changed`, left as
        // it is, never drift nor in place. With the record unreadable each key is judged as it
        // reads.
        let kept = initial::kept::Kept::load(root)
            .map(|kept| kept.kept_holds())
            .unwrap_or_default();
        if !kept.is_empty() {
            host.hold(&kept);
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
    let verified = host.verified_settings();
    if !verified.is_empty() {
        initial::record_verified_preferences(root, verified)?;
    }
    if report.complete() {
        initial::record_applied(root, host.selection())?;
    }
    Ok(Pass {
        report,
        unverified,
        settled,
        waits: host.waits(),
        looked: host.looked(),
        drifted: host.drifted(),
        readings: host.readings(),
        changed: host.changed(),
        new_targets: host.new_targets(),
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
    let mut clock = RowClock::start();
    let required = required_restore_originals(root)?;
    let mut saved = load_backup_preserving_restore(&report.backup, &required)?;
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
        // Pending, keeping what a Restore keeps: this pass may set one group only.
        initial::record_pending(root)?;
    }
    append_workstation(host, group, true, report);
    clock.lap(&mut report.rows);
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
    // How the rollback left each key's System State row, for the page (`PassPage`).
    let mut rolled: Vec<(Option<Setting>, PutBack)> = Vec::new();
    let mut journal = false;
    // The rollback's `restored` row, by its place in the report, and the keys it names.
    let mut restored_row: Option<(usize, Vec<String>)> = None;
    if !rollback.is_empty() {
        // Restoring any preference can invalidate the earlier native cohort. The
        // ordinary observer must read the state left by rollback before publication.
        page(report).readback = WorkstationReadback::EMPTY;
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
                rolled.push((row_of(&previous.target), PutBack::Unknown));
                continue;
            }
            let wrote = match restore_one(
                host,
                &previous.target,
                previous.value.as_ref(),
                &mut refused,
            ) {
                // A key whose saved value already reads the original (its reader wrote it back
                // over the change) is back as much as one the rollback wrote, as Restore counts
                // it, and its live value is put back the same way; only a write relaunches its
                // reader.
                Ok(wrote) => {
                    restored.push(previous.target.describe());
                    put_back.push((previous.target.clone(), previous.value.clone()));
                    wrote
                }
                Err((wrote, error)) => {
                    errors.push(format!("rollback {}: {error}", previous.target.describe()));
                    rolled.push((row_of(&previous.target), PutBack::Failed));
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
                elapsed_ms: None,
            });
        }
        if !left.is_empty() {
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "blocked".into(),
                detail: format!("left as this pass made it: {}", left.join("; ")),
                elapsed_ms: None,
            });
        }
        if !refused.is_empty() {
            journal = true;
            report.rows.push(journal_refused(&refused));
        }
        if !errors.is_empty() {
            report.rows.push(Row {
                name: "Preference rollback".into(),
                status: "error".into(),
                detail: errors.join("; "),
                elapsed_ms: None,
            });
        }
    }
    clock.lap(&mut report.rows);
    let relaunched = report.rows.len();
    let unread = refresh(host, &reload.iter().collect::<Vec<_>>(), report);
    if !unread.is_empty() {
        page(report).readback = WorkstationReadback::EMPTY;
    }
    clock.lap(&mut report.rows);
    journal |= report.rows[relaunched..]
        .iter()
        .any(|row| row.name == JOURNAL_ROW);
    if unread.is_empty() && full_profile {
        settle_pending_refresh(host, root)?;
    }
    // A key whose relaunched reader did not keep the rollback is named as lost alone, not also
    // as back.
    let lost = not_retained(host, &put_back);
    if !lost.is_empty() {
        page(report).readback = WorkstationReadback::EMPTY;
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
            elapsed_ms: None,
        });
    }
    clock.lap(&mut report.rows);
    if !rollback.is_empty() {
        for (target, _) in &put_back {
            rolled.push((row_of(target), put_back_as(target, &lost, &unread)));
        }
        // Only what the rollback put back, and its reader kept, has a live value to put back
        // or loads at the next login.
        let kept: Vec<Saved> = rollback
            .iter()
            .filter(|item| {
                put_back.iter().any(|(target, _)| *target == item.target)
                    && !lost.contains(&item.target.describe())
            })
            .cloned()
            .collect();
        for runtime in host.restore_runtime(&kept) {
            match runtime.put_back {
                Some((setting, put)) => rolled.push((Some(setting), put)),
                None => journal = true,
            }
            report.rows.push(runtime.row);
        }
        // The page says what the rollback did in one line, in its row names, never a key, a
        // keyboard or a process (`rows` keeps each for `aterm doctor`).
        let mut outcome = RestoreOutcome::of(rolled);
        outcome.journal_refused = journal;
        page(report).outcome.merge(PassOutcome {
            rolled_back: Some(outcome),
            unfinished: Vec::new(),
        });
    }
    if let Err(detail) = record_withheld(root, &left_alone, &reached, &unverified, turn) {
        as_step(
            report,
            Row {
                name: "Settings waiting for Full Disk Access".into(),
                status: "error".into(),
                detail,
                elapsed_ms: None,
            },
            aterm_machines::init_preferences::withheld_settings(group.selected(host.selection())),
        );
    }
    clock.lap(&mut report.rows);
    Ok((unverified, settled))
}

/// The System State row whose items write `target` (`None` for a key no item of this build
/// writes, such as one an older aterm backed up).
fn row_of(target: &Target) -> Option<Setting> {
    aterm_machines::init_preferences::setting_of_preference(&target.domain, &target.key)
}

/// How a key put back and verified came out once its readers were relaunched: back, or, where
/// a reader of it did not relaunch (`unread`, [`refresh`]), back as saved and in use from the
/// next login; failed where its reader, once relaunched, no longer read it (`lost`).
fn put_back_as(target: &Target, lost: &[String], unread: &[&str]) -> PutBack {
    if lost.contains(&target.describe()) {
        PutBack::Failed
    } else if aterm_machines::init_activation::readers(&target.domain)
        .iter()
        .any(|process| unread.contains(process))
    {
        PutBack::AtLogin
    } else {
        PutBack::Back
    }
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
    load_withheld_reading_revision(root, || initial::revision_at(root))
}

fn load_withheld_reading_revision(
    root: &Path,
    revision: impl FnOnce() -> Result<u64, String>,
) -> Result<Withheld, String> {
    let path = root.join(WITHHELD_RECORD);
    let bytes = match read_record_bytes(&path) {
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
    // Only a known different run resets its retry budget. An unreadable current run
    // must not make a spent attempt available again.
    if record["revision"].as_u64() != Some(revision()?) {
        failed.clear();
    }
    Ok(Withheld {
        items: current_ids(items, true),
        failed,
    })
}

fn save_withheld(root: &Path, record: &Withheld) -> Result<(), String> {
    save_withheld_reading_revision(root, record, || initial::revision_at(root))
}

fn save_withheld_reading_revision(
    root: &Path,
    record: &Withheld,
    revision: impl FnOnce() -> Result<u64, String>,
) -> Result<(), String> {
    if !record.items.is_empty() {
        let revision = revision()?;
        return write_json(
            &root.join(WITHHELD_RECORD),
            &json!({
                "profile": PROFILE, "revision": revision,
                "items": record.items, "failed": tries_json(&record.failed),
            }),
        );
    }
    remove_record(root, WITHHELD_RECORD, "clear withheld preferences")
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

/// Whether a follow-up with Full Disk Access has anything to do: a Restore's put-back that
/// waits for the grant, or a change setup left for want of it while no Stop holds the run.
fn follow_up_owed(root: &Path) -> Result<bool, String> {
    Ok(!restore_waiting(root)?.is_empty()
        || (!load_withheld(root)?.items.is_empty() && !initial::stopped(root)?))
}

/// The settings a follow-up ([`follow_up_at`]) would change now: what a Restore or setup left
/// in another app's container for want of Full Disk Access, once aterm has it (`granted`,
/// asked only when something is owed); none without the grant. A setup pass marks them busy
/// while its follow-up runs, as it marks a stage's (`initial::InitialHost::withheld_owed`).
pub(super) fn owed_with_full_disk_access(
    root: &Path,
    granted: impl FnOnce() -> Option<bool>,
) -> Vec<Setting> {
    let mut owed = Selection::none();
    for item in load_withheld(root)
        .map(|record| record.items)
        .unwrap_or_default()
    {
        if let Some(setting) = aterm_machines::init_preferences::setting_of(&item) {
            owed.set(setting, true);
        }
    }
    for target in restore_waiting(root).unwrap_or_default() {
        if let Some(setting) =
            aterm_machines::init_preferences::setting_of_preference(&target.domain, &target.key)
        {
            owed.set(setting, true);
        }
    }
    if owed.is_empty() || granted() != Some(true) {
        return Vec::new();
    }
    Setting::ALL
        .into_iter()
        .filter(|setting| owed.contains(*setting))
        .collect()
}

/// A follow-up once aterm has Full Disk Access: a Restore's put-back that waited for it
/// ([`finish_waiting_restore`]), then what setup left for want of it ([`follow_up_setup_at`]).
fn follow_up_at(
    root: &Path,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    follow_up_at_unless_stopped(root, host, build, now, |_| false)
}

fn follow_up_at_unless_stopped(
    root: &Path,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
    stopped: impl Fn(u64) -> bool,
) -> Result<Option<Report>, String> {
    let restored = finish_waiting_restore(root, host)?;
    let began = std::time::Instant::now();
    // The window's Stop is immediate; its durable save can still be queued
    // behind this worker. Check the current run after taking profile.lock,
    // while leaving the Restore above free to finish putting originals back.
    let set = (|| {
        if !load_withheld(root)?.items.is_empty() && stopped(initial::revision_at(root)?) {
            Ok(None)
        } else {
            follow_up_setup_at(root, host, build, now)
        }
    })();
    match (restored, set) {
        (Some(mut restored), Err(detail)) => {
            // Its waiting debt is already retired. Keep the completed Restore
            // result alongside the separate setup failure, so the page cannot
            // retain an old waiting result that no later follow-up can replace.
            let elapsed_ms = Some(aterm_converge::millis(began.elapsed()));
            restored.rows.push(Row {
                name: "Setup".into(),
                status: "error".into(),
                detail,
                elapsed_ms,
            });
            page(&mut restored).rows.push(Row {
                name: "Setup".into(),
                status: "unverified".into(),
                detail: String::new(),
                elapsed_ms,
            });
            Ok(Some(restored))
        }
        (None, Err(why)) => Err(why),
        (Some(mut restored), Ok(Some(set))) => {
            restored.rows.extend(set.rows);
            Ok(Some(restored))
        }
        (restored, Ok(set)) => Ok(restored.or(set)),
    }
}

fn follow_up_setup_at(
    root: &Path,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    follow_up_setup_reading_revision(root, host, build, now, || initial::revision_at(root))
}

fn follow_up_setup_reading_revision(
    root: &Path,
    host: &mut dyn Host,
    build: &str,
    now: std::time::SystemTime,
    revision: impl FnOnce() -> Result<u64, String>,
) -> Result<Option<Report>, String> {
    let record = load_withheld_reading_revision(root, revision)?;
    if record.items.is_empty() || initial::stopped(root)? {
        return Ok(None);
    }
    if host.full_disk_access() != Some(true) {
        return Ok(None);
    }
    let Some(chosen) = initial::withheld_selection(root)? else {
        return Ok(None);
    };
    let mut selected = Selection::none();
    for item in &record.items {
        if let Some(setting) = aterm_machines::init_preferences::setting_of(item)
            && chosen.contains(setting)
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
        restore: None,
        page: None,
    };
    let now = epoch_secs(now);
    let selection = host.selection();
    host.select(selected);
    host.hold(&put_off(&record.failed, build, now));
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

/// A finite deferred-preference attempt. A held lease or unavailable grant retains the
/// request in its owner; it is never mistaken for completed recovery.
#[derive(Debug)]
pub enum WithheldFollowUp<S> {
    /// No effect ran. The existing owner may try again after its ordinary backoff.
    Held,
    /// The attempt ran to its result ([`FollowedUp`]).
    Finished(Box<FollowedUp<S>>),
}

/// What a finished deferred-preference attempt left ([`WithheldFollowUp::Finished`]).
#[derive(Debug)]
pub struct FollowedUp<S> {
    /// Its result, read under the profile lock, which is already released.
    pub observed: InitialSetupObserved<Option<Report>>,
    /// Its setup-slot reservation, kept until publication; no operation needs no reservation.
    pub reservation: Option<S>,
    /// How long after the attempt the first change it put off for that change's own pause
    /// ([`DEFERRED`], [`next_try`]) comes due: when the request's owner tries again, rather than
    /// on its ordinary backoff, for a result whose only unfinished rows are put off. `None` where
    /// no change waits out a pause, or the record could not be read.
    pub put_off_for: Option<std::time::Duration>,
}

impl<S> WithheldFollowUp<S> {
    fn finished(
        result: Result<Option<Report>, String>,
        observation: Option<InitialSetupObservation>,
        reservation: Option<S>,
    ) -> Self {
        Self::put_off(result, observation, reservation, None)
    }

    fn put_off(
        result: Result<Option<Report>, String>,
        observation: Option<InitialSetupObservation>,
        reservation: Option<S>,
        put_off_for: Option<std::time::Duration>,
    ) -> Self {
        Self::Finished(Box::new(FollowedUp {
            observed: InitialSetupObserved::new(result, observation),
            reservation,
            put_off_for,
        }))
    }
}

/// How long from `now` until the first of the follow-up's changes in this build that waits out
/// its own pause comes due ([`next_try`]): what its owner waits before it tries again.
fn withheld_put_off_for(root: &Path, build: &str, now: u64) -> Option<std::time::Duration> {
    let record = load_withheld(root).ok()?;
    record
        .failed
        .iter()
        .filter(|failed| {
            failed.build == build && failed.tries > 0 && record.items.contains(&failed.item)
        })
        .map(|failed| next_try(failed, now))
        .filter(|due| *due > now)
        .min()
        .map(|due| std::time::Duration::from_secs(due - now))
}

/// Put back waiting Restore originals, then finish still-authorized deferred setup changes.
/// Each attempt tries the setup slot before the profile lock without waiting on either.
/// The grant and current time are captured only with both leases held. Stop, the current
/// revision and Restore remain the effect boundary's own checks.
pub fn try_finish_withheld_preferences_unless_stopped<S>(
    reserve: impl FnMut() -> Option<S>,
    stopped: impl Fn(u64) -> bool,
) -> WithheldFollowUp<S> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return WithheldFollowUp::finished(Ok(None), None, None);
    }
    let root = match state_root() {
        Ok(root) => root,
        Err(why) => {
            return WithheldFollowUp::finished(Err(why), None, None);
        }
    };
    follow_up_attempt_at(
        &root,
        reserve,
        || {
            ensure_person_scope()?;
            Ok(NativeHost::new())
        },
        &aterm_stamp::build_id(),
        std::time::SystemTime::now,
        stopped,
    )
}

#[cfg(test)]
fn follow_up_finished_at<S, H: Host>(
    root: &Path,
    reserve: impl FnMut() -> S,
    host: impl FnOnce() -> Result<H, String>,
    build: &str,
    now: std::time::SystemTime,
) -> Result<Option<Report>, String> {
    let (observed, slot) = follow_up_finished_observed_at(root, reserve, host, build, now);
    drop(slot);
    observed.result
}

#[cfg(test)]
fn follow_up_finished_observed_at<S, H: Host>(
    root: &Path,
    mut reserve: impl FnMut() -> S,
    host: impl FnOnce() -> Result<H, String>,
    build: &str,
    now: std::time::SystemTime,
) -> (InitialSetupObserved<Option<Report>>, Option<S>) {
    match follow_up_attempt_at(root, || Some(reserve()), host, build, || now, |_| false) {
        WithheldFollowUp::Held => (InitialSetupObserved::new(Ok(None), None), None),
        WithheldFollowUp::Finished(finished) => (finished.observed, finished.reservation),
    }
}

fn follow_up_attempt_at<S, H: Host>(
    root: &Path,
    mut reserve: impl FnMut() -> Option<S>,
    host: impl FnOnce() -> Result<H, String>,
    build: &str,
    now: impl FnOnce() -> std::time::SystemTime,
    stopped: impl Fn(u64) -> bool,
) -> WithheldFollowUp<S> {
    match follow_up_owed(root) {
        Ok(false) => {
            // A Restore can hold the profile while its waiting debt is not published yet.
            // Only an empty observation under that lease can retire the grant's request.
            let empty = match HeldLock::try_take(root) {
                Ok(Some(_read)) => follow_up_owed(root).map(|owed| !owed),
                Ok(None) => return WithheldFollowUp::Held,
                Err(why) => return WithheldFollowUp::finished(Err(why), None, None),
            };
            match empty {
                Ok(true) => return WithheldFollowUp::finished(Ok(None), None, None),
                Err(why) => return WithheldFollowUp::finished(Err(why), None, None),
                Ok(false) => {}
            }
            // The read-only lease above is gone before acquiring the effect slot.
        }
        Err(why) => {
            return WithheldFollowUp::finished(Err(why), None, None);
        }
        Ok(true) => {}
    }
    let Some(slot) = reserve() else {
        return WithheldFollowUp::Held;
    };
    let lock = match HeldLock::try_take(root) {
        Ok(Some(lock)) => lock,
        Ok(None) => return WithheldFollowUp::Held,
        Err(why) => {
            return WithheldFollowUp::finished(Err(why), None, None);
        }
    };
    let mut grant_held = false;
    let mut ran_at = None;
    let result = (|| {
        if !follow_up_owed(root)? {
            return Ok(None);
        }
        let mut host = host()?;
        if host.full_disk_access() != Some(true) {
            grant_held = true;
            return Ok(None);
        }
        let at = now();
        ran_at = Some(epoch_secs(at));
        follow_up_at_unless_stopped(root, &mut host, build, at, &stopped)
    })();
    if grant_held {
        return WithheldFollowUp::Held;
    }
    let observation = initial::observation_at(root).ok();
    // Read under the same lease as the result: no other pass counts a try between the two.
    let put_off_for = ran_at.and_then(|at| withheld_put_off_for(root, build, at));
    // The caller publishes while retaining the slot, after releasing the profile lock,
    // so no newer operation's report can be replaced by this one.
    drop(lock);
    WithheldFollowUp::put_off(result, observation, Some(slot), put_off_for)
}

fn follow_up_locked_at(root: &Path) -> Result<Option<Report>, String> {
    if load_withheld(root)?.items.is_empty() && restore_waiting(root)?.is_empty() {
        return Ok(None);
    }
    follow_up_at(
        root,
        &mut NativeHost::new(),
        &aterm_stamp::build_id(),
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
         or an aterm tab once aterm has Full Disk Access, or aterm doctor --restore at this \
         Mac's console in another terminal app that has it",
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
    let mut clock = RowClock::start();
    // The stopping intent below resets the Restore baseline. Keep which earlier originals
    // were still owed before it, while saving that intent even when preflight then fails.
    let required = required_restore_originals(root);
    initial::save_restore(root)?;
    let required = required?;
    let saved = load_backup_preserving_restore(&report.backup, &required).map_err(backup_unread)?;
    save_withheld(root, &Withheld::default())?;
    // What this Restore cannot put back until aterm has Full Disk Access, which a follow-up
    // then puts back by itself ([`finish_waiting_restore`]).
    let waiting: Vec<Target> = saved
        .iter()
        .filter(|item| unknown_original(item).is_none() && withheld(host, &item.target).is_some())
        .map(|item| item.target.clone())
        .collect();
    // Keep an earlier Restore's put-back owed until this pass handles it, including when
    // preflight fails or the process ends before a write. Newly blocked keys must also be
    // durable before any other key changes.
    let previous_waiting = restore_waiting(root)?;
    let mut retained_waiting = previous_waiting.clone();
    for target in &waiting {
        if !retained_waiting.contains(target) {
            retained_waiting.push(target.clone());
        }
    }
    if retained_waiting != previous_waiting {
        save_restore_waiting(root, &retained_waiting)?;
    }
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
            elapsed_ms: None,
        });
    }
    // Each key's System State row, and how it came out, for the page (`RestoreRows`).
    let mut outcomes: Vec<(Option<Setting>, PutBack)> = Vec::new();
    let mut refused = Vec::new();
    let mut put_back = Vec::new();
    for item in saved.iter().rev() {
        clock.lap(&mut report.rows);
        let why = unknown_original(item)
            .map(|why| (why, PutBack::Unknown))
            .or_else(|| {
                withheld(host, &item.target).map(|_| (restore_waits(&item.target), PutBack::Waits))
            });
        if let Some((why, left)) = why {
            outcomes.push((row_of(&item.target), left));
            report.rows.push(Row {
                name: item.target.describe(),
                status: "blocked".into(),
                detail: why,
                elapsed_ms: None,
            });
            continue;
        }
        match restore_one(host, &item.target, item.value.as_ref(), &mut refused) {
            Ok(_) => {
                // An earlier pass may have written this original before exiting. Its
                // reader is still relaunched below and must retain it this time too.
                put_back.push((item.target.clone(), item.value.clone()));
                report.rows.push(Row {
                    name: item.target.describe(),
                    status: "restored".into(),
                    detail: "original value restored".into(),
                    elapsed_ms: None,
                });
            }
            Err((_, detail)) => {
                outcomes.push((row_of(&item.target), PutBack::Failed));
                report.rows.push(Row {
                    name: item.target.describe(),
                    status: "error".into(),
                    detail,
                    elapsed_ms: None,
                });
            }
        }
    }
    let mut journal = !refused.is_empty();
    if journal {
        report.rows.push(journal_refused(&refused));
    }
    clock.lap(&mut report.rows);
    let relaunched = report.rows.len();
    let unread = refresh(host, &activation.iter().collect::<Vec<_>>(), &mut report);
    clock.lap(&mut report.rows);
    if unread.is_empty() {
        settle_pending_refresh(host, root)?;
    }
    journal |= report.rows[relaunched..]
        .iter()
        .any(|row| row.name == JOURNAL_ROW);
    let lost = not_retained(host, &put_back);
    for name in &lost {
        if let Some(row) = report
            .rows
            .iter_mut()
            .find(|row| row.name == *name && row.status == "restored")
        {
            row.status = "error".into();
            row.detail = NOT_RETAINED.into();
        }
    }
    // A relaunch that failed after the preferences were written leaves what its process reads
    // to the next login: back as saved, in use from then, which the page says in its login
    // sentence, never as the process (`aterm doctor --restore` keeps its line).
    for (target, _) in &put_back {
        outcomes.push((row_of(target), put_back_as(target, &lost, &unread)));
    }
    // Only what is back, and stayed back, has a live value to put back or loads at the next
    // login: a key Restore could not write, or that its reader wrote over, does neither.
    let kept: Vec<Saved> = saved
        .iter()
        .filter(|item| {
            put_back.iter().any(|(target, _)| *target == item.target)
                && !lost.contains(&item.target.describe())
        })
        .cloned()
        .collect();
    for runtime in host.restore_runtime(&kept) {
        match runtime.put_back {
            Some((setting, put)) => outcomes.push((Some(setting), put)),
            None => journal = true,
        }
        report.rows.push(runtime.row);
    }
    // This pass handled the earlier keys. A reported write failure still needs a manual
    // Restore retry; only keys left for want of the grant remain automatic obligations.
    save_restore_waiting(root, &waiting)?;
    // The groups Restore put back are left as is from now on (design §3.4).
    initial::leave_restored_as_is(root)?;
    clock.lap(&mut report.rows);
    let mut outcome = RestoreOutcome::of(outcomes);
    outcome.journal_refused = journal;
    report.restore = Some(RestoreRows { outcome });
    Ok(report)
}

fn pending_refresh(root: &Path) -> Result<Vec<Target>, String> {
    targets_record(root, "pending-refresh.json", "pending activation")
}

/// The keys a record of `name` lists, `what` naming it in an error; none when it is absent.
fn targets_record(root: &Path, name: &str, what: &str) -> Result<Vec<Target>, String> {
    let path = root.join(name);
    let bytes = match read_record_bytes(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let value: Value = aterm_json::from_slice(&bytes).map_err(|e| format!("{what}: {e}"))?;
    if value["profile"].as_str() != Some(PROFILE) {
        return Err(format!("unrecognized {what} profile"));
    }
    value["targets"]
        .as_array()
        .ok_or_else(|| format!("{what} has no targets"))?
        .iter()
        .map(|entry| {
            Ok(Target {
                domain: entry["domain"]
                    .as_str()
                    .ok_or_else(|| format!("{what} domain is missing"))?
                    .into(),
                key: entry["key"]
                    .as_str()
                    .ok_or_else(|| format!("{what} key is missing"))?
                    .into(),
                host: entry["host"]
                    .as_bool()
                    .ok_or_else(|| format!("{what} scope is missing"))?,
            })
        })
        .collect()
}

fn save_targets_record(root: &Path, name: &str, targets: &[Target]) -> Result<(), String> {
    let targets: Vec<Value> = targets
        .iter()
        .map(|t| json!({ "domain": t.domain, "key": t.key, "host": t.host }))
        .collect();
    write_json(
        &root.join(name),
        &json!({ "profile": PROFILE, "targets": targets }),
    )
}

fn save_pending_refresh(root: &Path, targets: &[Target]) -> Result<(), String> {
    if targets.is_empty() {
        return Ok(());
    }
    save_targets_record(root, "pending-refresh.json", targets)
}

/// The record of the keys a Restore could not put back for want of Full Disk Access
/// (`PutBack::Waits`): their saved values are in another app's container.
const RESTORE_WAITING: &str = "restore-waiting.json";

fn restore_waiting(root: &Path) -> Result<Vec<Target>, String> {
    targets_record(root, RESTORE_WAITING, "waiting restore")
}

/// Record `targets` as what a Restore still owes once aterm has Full Disk Access; none
/// removes the record.
fn save_restore_waiting(root: &Path, targets: &[Target]) -> Result<(), String> {
    if !targets.is_empty() {
        return save_targets_record(root, RESTORE_WAITING, targets);
    }
    remove_record(root, RESTORE_WAITING, "clear the waiting restore")
}

/// Put back what a Restore left for want of Full Disk Access, once aterm has it (System State
/// design §3.4), with no press: each key whose row still stands restored
/// ([`initial::restore_owed`]), its readers relaunched as a Restore relaunches them. A key a
/// choice since queued to set again waits for that pass; one a pass set since, or whose
/// Restore a run replaced, is let go. `None` when nothing waits or aterm still lacks the grant.
fn finish_waiting_restore(root: &Path, host: &mut dyn Host) -> Result<Option<Report>, String> {
    let waiting = restore_waiting(root)?;
    if waiting.is_empty() || host.full_disk_access() != Some(true) {
        return Ok(None);
    }
    let backup = root.join("original-preferences.json");
    let required = required_restore_originals(root)?;
    let saved = if required.is_empty() {
        Vec::new()
    } else {
        load_backup_preserving_restore(&backup, &required)?
    };
    let activation = pending_refresh(root)?;
    let mut report = Report {
        action: Action::Restore,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup,
        restore: None,
        page: None,
    };
    let (mut still, mut put_back, mut outcomes, mut refused) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut clock = RowClock::start();
    for target in &waiting {
        clock.lap(&mut report.rows);
        match initial::restore_owed(root, row_of(target)) {
            initial::RestoreOwed::Wait => {
                still.push(target.clone());
                continue;
            }
            initial::RestoreOwed::LetGo => continue,
            initial::RestoreOwed::PutBack => {}
        }
        let Some(item) = saved
            .iter()
            .find(|item| item.target == *target && unknown_original(item).is_none())
        else {
            continue;
        };
        if withheld(host, target).is_some() {
            still.push(target.clone());
            continue;
        }
        match restore_one(host, target, item.value.as_ref(), &mut refused) {
            Ok(_) => {
                put_back.push((target.clone(), item.value.clone()));
                report.rows.push(Row {
                    name: target.describe(),
                    status: "restored".into(),
                    detail: "original value restored".into(),
                    elapsed_ms: None,
                });
            }
            Err((_, detail)) => {
                outcomes.push((row_of(target), PutBack::Failed));
                report.rows.push(Row {
                    name: target.describe(),
                    status: "error".into(),
                    detail,
                    elapsed_ms: None,
                });
            }
        }
    }
    if !refused.is_empty() {
        report.rows.push(journal_refused(&refused));
    }
    clock.lap(&mut report.rows);
    let relaunched = report.rows.len();
    let unread = refresh(
        host,
        &put_back
            .iter()
            .map(|(target, _)| target)
            .collect::<Vec<_>>(),
        &mut report,
    );
    let journal = !refused.is_empty()
        || report.rows[relaunched..]
            .iter()
            .any(|row| row.name == JOURNAL_ROW);
    let lost = not_retained(host, &put_back);
    // A reader that did not relaunch leaves its rows to the next login, as a Restore does: the
    // page names no process.
    for (target, _) in &put_back {
        if lost.contains(&target.describe())
            && let Some(row) = report
                .rows
                .iter_mut()
                .find(|row| row.name == target.describe() && row.status == "restored")
        {
            row.status = "error".into();
            row.detail = NOT_RETAINED.into();
        }
        outcomes.push((row_of(target), put_back_as(target, &lost, &unread)));
    }
    // This follow-up may also fulfil activation left by an earlier Restore. Only
    // retire targets it actually put back and whose readers all relaunched; other
    // activation remains owed. Publish this first so an I/O refusal leaves the
    // waiting Restore available for recovery.
    let remaining: Vec<Target> = activation
        .iter()
        .filter(|target| {
            !put_back.iter().any(|(restored, _)| restored == *target)
                || aterm_machines::init_activation::readers(&target.domain)
                    .iter()
                    .any(|process| unread.contains(process))
        })
        .cloned()
        .collect();
    if remaining != activation {
        if remaining.is_empty() {
            clear_pending_refresh(root)?;
        } else {
            save_pending_refresh(root, &remaining)?;
        }
    }
    save_restore_waiting(root, &still)?;
    if outcomes.is_empty() && !journal {
        return Ok(None);
    }
    let mut outcome = RestoreOutcome::of(outcomes);
    outcome.journal_refused = journal;
    report.restore = Some(RestoreRows { outcome });
    Ok(Some(report))
}

fn clear_pending_refresh(root: &Path) -> Result<(), String> {
    remove_record(root, "pending-refresh.json", "clear pending activation")
}

/// Remove the record `name` from `root`, then flush `root`, so the record stays gone after a
/// crash as a write of it stays written; one already gone needs neither. An error names `what`
/// was being done, or `root` when its flush failed.
fn remove_record(root: &Path, name: &str, what: &str) -> Result<(), String> {
    match std::fs::remove_file(root.join(name)) {
        Ok(()) => durable_directory(root).map_err(|e| format!("{}: {e}", root.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{what}: {e}")),
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

/// Of `put_back` (each key and the original value verified before relaunch), the keys
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

/// Relaunch the readers of `targets`, each once, so they read what was written: the
/// processes whose relaunch failed, which still run with what they read before (none: every
/// reader relaunched).
fn refresh(host: &mut dyn Host, targets: &[&Target], report: &mut Report) -> Vec<&'static str> {
    let targets: Vec<&Target> = targets
        .iter()
        .copied()
        .filter(|target| withheld(&*host, target).is_none())
        .collect();
    let mut unread = Vec::new();
    let mut refused = Vec::new();
    let mut clock = RowClock::start();
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
            unread.push(process);
            report.rows.push(Row {
                name: format!("Reload {process}"),
                status: "error".into(),
                detail,
                elapsed_ms: None,
            });
        }
        clock.lap(&mut report.rows);
    }
    if targets
        .iter()
        .any(|t| t.host || t.domain.contains("Trackpad") || t.domain.contains("trackpad"))
    {
        report.rows.push(Row { name: "Keyboard and trackpad activation".into(), status: "ok".into(), detail: "native preferences saved; log out and back in if an already connected device still uses its previous setting".into(), elapsed_ms: None });
    }
    if !refused.is_empty() {
        report.rows.push(journal_refused(&refused));
    }
    clock.lap(&mut report.rows);
    unread
}

#[cfg(test)]
mod tests;
