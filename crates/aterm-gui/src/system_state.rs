// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System State tab (`docs/DESIGN-system-state-2026-10-21.md` §3): what
//! this Mac reads now beside what aterm wants, in five groups. A group's
//! `Set up │ Leave as is` choice takes effect at once; rows only report.

mod paint;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;

use aterm_grapheme::GraphemeClusters;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{
    Access, Group, Observation, Plan, Selection, Setting, Standing, Subject,
};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, CloseScope, Command, CommonViewState, EventResult, NativeAppModel, SemanticInput,
    TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::{TextInputProjection, TextInputState, TextSelection};
use crate::native_ui::{
    ActionId, AuditedCustomNode, ButtonSpec, Control, ControlState, GroupSpec, Insets, Layout,
    Length, SemanticRole, SemanticValue, StyleRef, TextFieldSpec, TextSpec, UiContent, UiKey,
    UiNode, UiTree,
};
use crate::workstation_setup::{Request, SetupAnswer, WorkstationSetupDecision};

pub(crate) use paint::{BrandSpec, Chip, paint_brand, paint_chip, paint_meter};

/// A non-secret snapshot published by setup's worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupStatus {
    pub plan: Option<Plan>,
    pub check: Option<SetupCheck>,
    pub permissions: Option<InitialSetupPermissionPolicy>,
    pub busy: bool,
    pub finished: bool,
    pub rows: Vec<SetupStatusRow>,
    /// The latest read-only reading of each row, keyed by subject (at most one
    /// per subject). Rows with no reading yet are absent.
    pub observed: Vec<aterm_types::macos_setup::Observation>,
    /// Raised each time `observed` changes, so a publication can be compared cheaply.
    pub observed_generation: u64,
    /// The settings whose stage the running pass is executing now.
    pub running: Vec<Setting>,
    /// The access step whose macOS gesture is up or about to start, filled by
    /// the window (`App::system_state_access_step`), never by the worker.
    pub access_step: Option<aterm_types::macos_setup::Access>,
    /// What the busy worker is doing when it is not a setup run (Check again,
    /// Restore), filled by the window (`App::publish_system_state`), never by
    /// the worker. Busy with none is a run.
    pub operation: Option<SetupOperation>,
    /// The run's worker still runs its passes again by itself (a minute
    /// apart, up to ten in a row) when one fails: a failure in `rows`, or a
    /// row that failed, is the page's alone meanwhile, and reaches the menu
    /// line and the tab's attention only once the run stops retrying it
    /// (§3.5, §3.6). Filled by the worker.
    pub retrying: bool,
}

/// A read-only Check again or a Restore the window started: the worker is
/// busy, but no run is going, so the page shows no meter and no Stop (design
/// §3.2, §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SetupOperation {
    Check,
    Restore,
}

/// A read-only observation of one exact reviewed checklist and vault source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupCheck {
    pub plan: Plan,
    pub rows: Vec<SetupStatusRow>,
}

/// Native permission guidance follows this durably admitted setup run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialSetupPermissionPolicy {
    pub revision: u64,
    pub allowed: bool,
}

/// One result an observation cannot express (a refused action, a stage
/// error), with no credential values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupStatusRow {
    pub name: String,
    pub status: String,
    pub detail: String,
}

impl SetupStatus {
    /// A setup run is going: busy with neither a Check nor a Restore.
    pub(crate) fn running(&self) -> bool {
        self.busy && self.operation.is_none()
    }

    /// The latest reading of `subject`, if one was taken.
    pub(crate) fn observation(&self, subject: Subject) -> Option<&Observation> {
        self.observed
            .iter()
            .find(|observation| observation.subject == subject)
    }

    /// Whether a failure is a signal now (§3.6): never while the run still
    /// retries it by itself (§3.5, [`Self::retrying`]), when the page alone
    /// says it.
    pub(crate) fn failures_signal(&self) -> bool {
        !self.retrying
    }

    /// A Check's result belongs to the exact plan it read. The page supplies
    /// its current review; a signal without a page supplies the saved plan.
    /// No known plan means no Check result, never a guessed default. Keep its
    /// display words, beside the original subject name which Check prefixes.
    fn results<'a>(
        &'a self,
        plan: Option<&'a Plan>,
    ) -> impl Iterator<Item = (&'a SetupStatusRow, &'a str)> + 'a {
        self.rows.iter().map(|row| (row, row.name.as_str())).chain(
            self.check
                .as_ref()
                .filter(move |check| Some(&check.plan) == plan)
                .into_iter()
                .flat_map(|check| check.rows.iter())
                .map(|row| (row, row.name.strip_prefix("Check: ").unwrap_or(&row.name))),
        )
    }

    /// The steps the last run could not finish (a stage error, a join the
    /// vault refused, a change that did not land), each with the row it
    /// names, if one: failures, which the menu line and the tab's attention
    /// say as they say a row that failed (§3.6), besides the page's notes,
    /// once the run no longer retries them ([`Self::failures_signal`]). A
    /// step naming a row `plan` no longer manages is left out. A Check's
    /// failures count only for that exact plan.
    pub(crate) fn failed_steps<'a>(
        &'a self,
        plan: Option<&'a Plan>,
    ) -> impl Iterator<Item = Option<Subject>> + 'a {
        let signal = self.failures_signal();
        let selection = plan.map_or_else(Selection::none, |plan| plan.selection);
        self.results(plan)
            .filter(move |(row, _)| signal && matches!(row.status.as_str(), "error" | "failed"))
            .map(|(_, subject_name)| {
                Subject::all()
                    .into_iter()
                    .find(|subject| subject.name() == subject_name)
            })
            .filter(move |row| row.is_none_or(|subject| manages(selection, subject)))
    }

    /// The results the page shows under its headline: what an observation
    /// cannot say and someone should know (an action that did not start, a
    /// stage that failed, a restore that finished).
    fn notes<'a>(&'a self, plan: &'a Plan) -> impl Iterator<Item = &'a SetupStatusRow> + 'a {
        self.results(Some(plan)).map(|(row, _)| row).filter(|row| {
            matches!(
                row.status.as_str(),
                "not started" | "error" | "failed" | "unverified" | "restored"
            )
        })
    }
}

impl SetupStatusRow {
    /// The note's words: `Check again did not start. Try again.`
    fn words(&self) -> String {
        let what = match self.status.as_str() {
            "not started" => format!("{} did not start.", self.name),
            "error" | "failed" => format!("{} failed.", self.name),
            "unverified" => format!("{} could not be confirmed.", self.name),
            "restored" => format!("{} finished.", self.name),
            other => format!("{}: {other}.", self.name),
        };
        if self.detail.is_empty() {
            what
        } else {
            format!("{what} {}", self.detail)
        }
    }
}

/// Empty preedit clears platform composition state without any typed input.
pub(crate) const fn text_input_interacts(event: &TextInputEvent) -> bool {
    !matches!(event, TextInputEvent::Preedit { text, .. } if text.is_empty())
}

/// Whose choice a submitted plan is (design §6, "The standing yes"): the
/// page's own answer to the first-run offer, a person's press, or a control
/// client's `act`/`key`, which never counts as a person's. It rides the plan's
/// admission receipt to the worker (`InitialSetupApplyReceipt::admission`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The first-run offer, applied by the page once it was presented in an
    /// active window.
    Automatic,
    /// A person pressed the page.
    Person,
    /// A control client drove the page.
    Client,
}

impl Admission {
    fn of_gesture(by_person: bool) -> Self {
        if by_person {
            Self::Person
        } else {
            Self::Client
        }
    }
}

/// What a submission asks of the plan (design §3.3, §3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Submission {
    /// The plan as it stands: the first-run offer applied, or Set up now. A
    /// provisional plan stays provisional.
    AsItStands,
    /// A group choice: the plan is the chooser's from now on, even where it
    /// returns to the provisional plan aterm chose by itself.
    Choice,
    /// Join: a group choice that also sends the vault address as typed and
    /// its password.
    Join,
}

#[derive(Debug)]
pub(crate) enum SystemStateAction {
    /// Submit this plan as the next run, with its vault password once. While
    /// a run is going, the worker stops it at its next step and runs this
    /// plan next (`InitialSetupHandlers::apply`).
    Apply(SetupAnswer, Admission),
    /// Read every row again for this plan, prompt-free.
    Check(Plan),
    /// Put back the Mac preferences aterm saved.
    Restore,
    /// Ask the running pass to stop at its next step boundary.
    Stop,
    /// A Needs-you row's button (or the headline's) for this grant.
    OpenAccess { access: Access, by_person: bool },
    /// The Secrets vault row's Turn On Sync: this Mac's sync, which the
    /// person turned off, goes on again (`InitialSetupHandlers::sync_on`).
    TurnSyncOn { by_person: bool },
}

/// The key of the Secrets vault row's Turn On Sync button.
const TURN_ON_SYNC: &str = "system-state/row/secrets/sync-on";

/// The slug a row's keys carry (`system-state/row/<slug>`).
pub(crate) fn row_slug(subject: Subject) -> &'static str {
    match subject {
        Subject::Access(access) => access.word(),
        Subject::Setting(setting) => match setting {
            Setting::CapsLock => "caps-lock",
            Setting::NaturalScrolling => "natural-scrolling",
            Setting::TrackpadScrolling => "trackpad-scrolling",
            Setting::Desktop => "desktop",
            Setting::Widgets => "widgets",
            Setting::Dock => "dock",
            Setting::Messages => "messages",
            Setting::FaceTime => "facetime",
            Setting::Spotlight => "rust-docs",
            Setting::Permissions => "permissions",
            Setting::Secrets => "secrets",
            Setting::Repositories => "repositories",
        },
    }
}

/// The words a Needs-you row's button says (§3.4), and the shorter ones the
/// headline's button paints.
fn open_words(access: Access) -> (String, &'static str) {
    match access {
        Access::Debugger => (format!("Turn On {}…", access.name()), "Turn On…"),
        _ => (format!("Open {}", access.name()), "Open"),
    }
}

/// A row's value, in the words a probe read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowValue {
    /// `now → wanted`.
    Change { now: String, wanted: String },
    /// One value: read as wanted, or nothing to compare it with.
    One(String),
    /// The read value of a row aterm leaves as is, in muted ink.
    Muted(String),
}

/// One row of the page, decided from its reading and the plan (§3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowView {
    pub(crate) subject: Subject,
    /// aterm manages the row: its group is set up and the plan needs it.
    pub(crate) managed: bool,
    pub(crate) value: RowValue,
    pub(crate) chip: Option<Chip>,
    /// A short line under the row, which a screen reader reads after it: a
    /// probe's own note (an approval code, what a waiting row waits on), or
    /// the row's description where it explains something a person needs (a
    /// failure's reason, the macOS step a person takes, why it is not needed,
    /// their opt-out). A row that will change shows no description, and a
    /// row that waits always has a note ([`WAITING_NOTE`] where its probe
    /// gave none).
    pub(crate) note: Option<String>,
    /// The grant a Needs-you row's button opens.
    pub(crate) open: Option<Access>,
    /// The Secrets vault row's quiet Turn On Sync button: the person turned
    /// this Mac's sync off, which reads in place as their choice (§3.3).
    pub(crate) turn_on_sync: bool,
    /// Where the row stands (`None`: not read yet).
    pub(crate) standing: Option<Standing>,
    /// What a screen reader hears for the row.
    pub(crate) label: String,
}

impl RowView {
    /// A managed row that is not yet as wanted (it will change, needs a
    /// person, or failed).
    fn to_change(&self) -> bool {
        self.managed
            && matches!(
                self.standing,
                Some(Standing::Differs | Standing::Waiting | Standing::NeedsYou | Standing::Failed)
            )
    }

    fn settled(&self) -> bool {
        self.managed && self.standing.is_some_and(Standing::settled)
    }

    fn attention(&self) -> bool {
        self.managed && self.standing.is_some_and(Standing::attention)
    }
}

/// Whether `selection` manages `subject`: its group's bit is set and, for a
/// grant, the selection needs it.
fn manages(selection: Selection, subject: Subject) -> bool {
    selection.contains(subject.setting())
        && match subject {
            Subject::Access(access) => access.needed_for(selection),
            Subject::Setting(_) => true,
        }
}

/// THE ROW RULE (§3.3, pure): name, value and at most one chip from the
/// row's reading, the plan's selection and the step running now.
pub(crate) fn row_view(subject: Subject, status: &SetupStatus, selection: Selection) -> RowView {
    let name = subject.name();
    let reading = status.observation(subject);
    let wanted = reading.map_or_else(|| subject.wanted().to_owned(), |o| o.wanted.clone());
    let now = reading
        .and_then(|o| o.now.clone())
        .filter(|now| !now.trim().is_empty());
    let standing = reading.map(|o| o.standing);
    let group_set_up = selection.contains(subject.setting());
    let managed = manages(selection, subject);
    let read = now.clone().unwrap_or_else(|| "—".to_owned());
    let spoken_read = now.clone().unwrap_or_else(|| "not read".to_owned());
    // THE ROW'S NOTE: the short line a probe gives (an approval code, what a
    // waiting row waits on), else, where the row needs a person, is not
    // needed or was opted out of, its description: the macOS step a person
    // takes (the SamplingTools dialog), why the row is not needed, the
    // person's opt-out. A row that differs or waits shows its probe's note
    // alone: its value names the part that differs, and the description a
    // planned or blocked engine row carries is the engine's own line
    // (`absent → true`, a token file's mode), which `aterm init` prints. A
    // waiting row is never without one ([`WAITING_NOTE`]).
    let explained = || reading.and_then(|o| o.note.clone().or_else(|| o.detail.clone()));
    let mut note = reading.and_then(|o| o.note.clone());
    if !group_set_up {
        return RowView {
            subject,
            managed,
            value: RowValue::Muted(read),
            chip: Some(Chip::LeftAsIs),
            note: None,
            open: None,
            turn_on_sync: false,
            standing,
            label: format!("{name}: {spoken_read}, left as is"),
        };
    }
    if !managed || standing == Some(Standing::NotNeeded) {
        // A reading taken while the row was still managed explains nothing
        // here; one that says the row is not needed says why.
        let note = (standing == Some(Standing::NotNeeded))
            .then(explained)
            .flatten();
        return RowView {
            subject,
            managed,
            value: RowValue::Muted("Not needed".to_owned()),
            chip: None,
            note: note.filter(|note| !note.trim().is_empty()),
            open: None,
            turn_on_sync: false,
            standing,
            label: format!("{name}: not needed"),
        };
    }
    let busy = status.running.contains(&subject.setting())
        && match subject {
            Subject::Access(access) => status.access_step.is_none_or(|step| step == access),
            Subject::Setting(_) => true,
        };
    let change = || RowValue::Change {
        now: read.clone(),
        wanted: wanted.clone(),
    };
    let open = match subject {
        Subject::Access(access) if standing == Some(Standing::NeedsYou) => Some(access),
        _ => None,
    };
    // A setting read in place at a value other than aterm's is the person's
    // opt-out (the vault's Sync off): its row turns it back on, quietly.
    let turn_on_sync = subject == Subject::Setting(Setting::Secrets)
        && standing == Some(Standing::InPlace)
        && wanted != subject.wanted();
    let (value, chip, label) = match standing {
        None | Some(Standing::Unknown) => (
            RowValue::One("—".to_owned()),
            None,
            format!("{name}: not read yet"),
        ),
        // A row read as wanted shows the value read, never the wanted one in
        // its place: with nothing read it is `—` beside the check.
        Some(Standing::InPlace) => match now {
            // A grant the person opted out of reads in place at what they
            // chose, not at aterm's wanted value: its note says so.
            Some(value) => {
                if matches!(subject, Subject::Access(_)) && wanted != subject.wanted() {
                    note = explained();
                }
                let label = format!("{name}: {value}, in place");
                (RowValue::One(value), Some(Chip::InPlace), label)
            }
            None => (
                RowValue::One("—".to_owned()),
                Some(Chip::InPlace),
                format!("{name}: in place"),
            ),
        },
        Some(Standing::AtLogin) => (
            RowValue::One(format!("{wanted} · at login")),
            Some(Chip::InPlace),
            format!("{name}: {wanted} at login, in place"),
        ),
        // The probe's own note only (set above): never the description.
        Some(Standing::Differs | Standing::Waiting) => {
            let chip = Chip::WillChange { busy };
            (
                change(),
                Some(chip),
                format!(
                    "{name}: {spoken_read}, {} {wanted}",
                    if busy {
                        "changing now to"
                    } else {
                        "will change to"
                    }
                ),
            )
        }
        Some(Standing::NeedsYou) => {
            note = explained();
            (
                change(),
                Some(Chip::NeedsYou),
                format!("{name}: {spoken_read}, needs you"),
            )
        }
        Some(Standing::Failed) => {
            note = reading.and_then(|o| o.detail.clone().or_else(|| o.note.clone()));
            (
                change(),
                Some(Chip::Failed),
                format!("{name}: {spoken_read}, failed"),
            )
        }
        Some(Standing::NotNeeded) => unreachable!("a row not needed returned above"),
    };
    let note = note.filter(|note| !note.trim().is_empty());
    // A row that waits always says why (§3.3): its probe's reason, or, where the
    // probe gave none, the one thing true of every waiting row.
    let note = if standing == Some(Standing::Waiting) {
        note.or_else(|| Some(WAITING_NOTE.to_owned()))
    } else {
        note
    };
    RowView {
        subject,
        managed,
        value,
        chip,
        note,
        open,
        turn_on_sync,
        standing,
        label,
    }
}

/// The note of a waiting row whose probe gave no reason: it is blocked on
/// something, and nothing more can be said of it.
pub(crate) const WAITING_NOTE: &str = "Can't be set up yet";

/// Every row of the page, in page order.
pub(crate) fn rows(status: &SetupStatus, selection: Selection) -> Vec<RowView> {
    Subject::all()
        .into_iter()
        .map(|subject| row_view(subject, status, selection))
        .collect()
}

/// A group's choice, read from the plan's bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupChoice {
    SetUp,
    LeaveAsIs,
    /// Some bits set, some not: an older plan's mixed selection.
    Partly,
}

pub(crate) fn group_choice(group: Group, selection: Selection) -> GroupChoice {
    let set = group
        .settings()
        .iter()
        .filter(|setting| selection.contains(**setting))
        .count();
    if set == 0 {
        GroupChoice::LeaveAsIs
    } else if set == group.settings().len() {
        GroupChoice::SetUp
    } else {
        GroupChoice::Partly
    }
}

/// A group header's count (`1 of 3 to change`, `Partly set up`), shown only
/// when the group is not all in place, and the group's spoken label.
pub(crate) fn group_words(
    group: Group,
    selection: Selection,
    rows: &[RowView],
) -> (Option<String>, String) {
    let title = group.title();
    let members: Vec<&RowView> = rows
        .iter()
        .filter(|row| row.subject.group() == group)
        .collect();
    let managed = members.iter().filter(|row| row.managed).count();
    let to_change = members.iter().filter(|row| row.to_change()).count();
    match group_choice(group, selection) {
        GroupChoice::SetUp if to_change > 0 => {
            let count = format!("{to_change} of {managed} to change");
            (Some(count.clone()), format!("{title}, Set up, {count}"))
        }
        GroupChoice::SetUp => (None, format!("{title}, Set up")),
        GroupChoice::LeaveAsIs => (None, format!("{title}, Left as is")),
        GroupChoice::Partly => (
            Some("Partly set up".to_owned()),
            format!("{title}, Partly set up"),
        ),
    }
}

/// The line under the page heading (§3.2) and the button beside it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Headline {
    pub(crate) text: String,
    /// The grant its Open button opens.
    pub(crate) open: Option<Access>,
    /// The row its Show button scrolls to: the first that failed, the vault
    /// when it needs you, or the first that waits.
    pub(crate) show: Option<Subject>,
    /// Its Set up now button runs the plan again: managed rows differ and
    /// nothing is going (a stopped run, a step that could not finish).
    pub(crate) start: bool,
}

impl Headline {
    fn line(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            open: None,
            show: None,
            start: false,
        }
    }

    /// What the worker is busy with, said before anything a reading says:
    /// `None` when it is idle.
    fn busy(status: &SetupStatus, rows: &[RowView]) -> Option<Self> {
        if !status.busy {
            return None;
        }
        Some(Self::line(match status.operation {
            None => {
                let managed = rows.iter().filter(|row| row.managed).count();
                let done = rows.iter().filter(|row| row.settled()).count();
                format!("Setting up this Mac… {done} of {managed}")
            }
            Some(SetupOperation::Check) => "Checking this Mac…".to_owned(),
            Some(SetupOperation::Restore) => "Restoring previous settings…".to_owned(),
        }))
    }

    /// The headline while the page does not know the worker's plan yet: it
    /// cannot say which rows aterm manages, so it says only what is going.
    pub(crate) fn before_plan(status: &SetupStatus) -> Self {
        match (status.busy, status.operation) {
            (true, None) => Self::line("Setting up this Mac…"),
            (true, Some(SetupOperation::Restore)) => Self::line("Restoring previous settings…"),
            _ => Self::line("Checking this Mac…"),
        }
    }
}

/// THE HEADLINE RULE (§3.2, pure).
pub(crate) fn headline(status: &SetupStatus, rows: &[RowView]) -> Headline {
    if status.access_step == Some(Access::Debugger) {
        return Headline::line(format!(
            "Enter your password for {}",
            Access::Debugger.name()
        ));
    }
    let needs: Vec<&RowView> = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::NeedsYou))
        .collect();
    if let Some(first) = needs.first() {
        let more = match needs.len() - 1 {
            0 => String::new(),
            n => format!(" and {n} more"),
        };
        // macOS's pane opens from here; a row with none (the vault) is
        // shown instead, where its own controls are.
        return Headline {
            open: first.open,
            show: first.open.is_none().then_some(first.subject),
            ..Headline::line(format!("Needs you: {}{more}", first.subject.name()))
        };
    }
    if let Some(busy) = Headline::busy(status, rows) {
        return busy;
    }
    let failed: Vec<Subject> = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::Failed))
        .map(|row| row.subject)
        .collect();
    if let Some(first) = failed.first() {
        let steps = if failed.len() == 1 { "step" } else { "steps" };
        let names: Vec<&str> = failed.iter().map(|subject| subject.name()).collect();
        return Headline {
            show: Some(*first),
            ..Headline::line(format!(
                "{} {steps} failed: {}",
                failed.len(),
                names.join(", ")
            ))
        };
    }
    if status.observed.is_empty() {
        return Headline::line("Checking this Mac…");
    }
    // Past a person and a failure, a row not yet as wanted differs or waits.
    // Set up now runs the plan again, which changes the rows that differ: the
    // count is a change to make only while one does. Rows that only wait are
    // nothing a press changes: the headline names them, and each row's note
    // says what it waits on.
    let to_change = rows.iter().filter(|row| row.to_change()).count();
    let differs = rows
        .iter()
        .any(|row| row.managed && row.standing == Some(Standing::Differs));
    if differs {
        let changes = if to_change == 1 { "change" } else { "changes" };
        return Headline {
            start: true,
            ..Headline::line(format!("{to_change} {changes} to make"))
        };
    }
    let waiting: Vec<Subject> = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::Waiting))
        .map(|row| row.subject)
        .collect();
    if let Some(first) = waiting.first() {
        let steps = if waiting.len() == 1 { "step" } else { "steps" };
        let names: Vec<&str> = waiting.iter().map(|subject| subject.name()).collect();
        return Headline {
            show: Some(*first),
            ..Headline::line(format!(
                "{} {steps} waiting: {}",
                waiting.len(),
                names.join(", ")
            ))
        };
    }
    let managed = rows.iter().filter(|row| row.managed).count();
    if managed == 0 {
        return Headline::line("Everything is left as is");
    }
    let only_permissions = rows
        .iter()
        .filter(|row| row.managed)
        .all(|row| row.subject.group() == Group::Access);
    Headline::line(if only_permissions {
        "Permissions are set up"
    } else {
        "This Mac is set up"
    })
}

/// THE VILLAGE RULE (pure): the scene
/// [`crate::settings::paint_rainbow_kitty_village`] draws beside the lockup,
/// one station per [`Setting`] in its order, in the painter's letters: `_`
/// left as is, `E` a managed row needs a person or failed, `W` running now,
/// `G` every managed row in place, `D` otherwise (it differs, waits or is not
/// read yet). `done` and `total` are the meter's count of managed rows.
pub(crate) fn village_spec(
    status: &SetupStatus,
    rows: &[RowView],
    selection: Selection,
    phase_ms: u64,
) -> String {
    let states: String = Setting::ALL
        .iter()
        .map(|&setting| {
            let mut managed = rows
                .iter()
                .filter(|row| row.managed && row.subject.setting() == setting);
            if !selection.contains(setting) {
                '_'
            } else if managed.clone().any(RowView::attention) {
                'E'
            } else if status.running.contains(&setting) {
                'W'
            } else if managed.all(RowView::settled) {
                'G'
            } else {
                'D'
            }
        })
        .collect();
    let total = rows.iter().filter(|row| row.managed).count();
    let done = rows.iter().filter(|row| row.settled()).count();
    let busy = u8::from(status.busy);
    format!("phase={phase_ms};busy={busy};done={done};total={total};states={states}")
}

/// The least room the village takes beside the lockup: a narrower page shows
/// the lockup alone.
const VILLAGE_MIN_W: f32 = 200.0;

/// How far the village's kitties walk in one page tick.
const VILLAGE_TICK_MS: u64 = 120;

/// How far one wheel line scrolls the page.
const LINE_PX: f32 = 40.0;

/// The strip under a scrolling list's room that its rows fade out in.
const FADE_STRIP: f32 = 24.0;

/// Where the last render placed one revealable key in the list's content.
#[derive(Clone, Debug)]
struct Span {
    key: Arc<str>,
    top: f32,
    bottom: f32,
}

#[derive(Default)]
pub(crate) struct SystemStateViewState {
    pub(crate) common: CommonViewState,
    /// The list's scroll, in points.
    scroll: Cell<f32>,
    /// The list's room and how far it can scroll, as last rendered.
    room: Cell<f32>,
    limit: Cell<f32>,
    /// The revealable keys the last render placed in the list.
    spans: RefCell<Vec<Span>>,
}

impl SystemStateViewState {
    /// Scroll the list so `key` shows whole: a group, a row or one of their
    /// controls, the vault's fields too (the password field shows bullets,
    /// never what was typed). `true` when the list moved.
    pub(crate) fn reveal_listed_control(&mut self, key: &UiKey) -> bool {
        let Some((top, bottom)) = self
            .spans
            .borrow()
            .iter()
            .find(|span| *span.key == *key.as_str())
            .map(|span| (span.top, span.bottom))
        else {
            return false;
        };
        let before = self.scroll.get();
        let room = self.room.get();
        let mut offset = before;
        if top < offset {
            offset = top;
        } else if bottom > offset + room {
            offset = (bottom - room).min(top);
        }
        let offset = offset.clamp(0.0, self.limit.get());
        self.scroll.set(offset);
        (offset - before).abs() > 0.5
    }

    fn scroll_by(&self, points: f32) -> bool {
        let before = self.scroll.get();
        let offset = (before + points).clamp(0.0, self.limit.get());
        self.scroll.set(offset);
        (offset - before).abs() > 0.5
    }
}

/// Password text never enters a UI projection, undo history, restore data or a
/// diagnostic String. Its selection addresses graphemes in the masked value.
#[derive(Default)]
struct Password {
    value: Option<SecretBytes>,
    anchor: usize,
    head: usize,
}

impl Password {
    fn text(&self) -> &str {
        self.value
            .as_ref()
            .and_then(SecretBytes::as_str)
            .unwrap_or_default()
    }

    fn boundaries(&self) -> Vec<usize> {
        self.text()
            .grapheme_indices()
            .map(|(byte, _)| byte)
            .chain([self.text().len()])
            .collect()
    }

    fn count(&self) -> usize {
        self.text().graphemes().count()
    }

    fn replace(&mut self, text: SecretBytes) {
        let boundaries = self.boundaries();
        let start = boundaries[self.anchor.min(self.head).min(boundaries.len() - 1)];
        let end = boundaries[self.anchor.max(self.head).min(boundaries.len() - 1)];
        let old = self.text().as_bytes();
        if old.len() - (end - start) + text.len() > 1023
            || text
                .as_slice()
                .iter()
                .any(|byte| matches!(byte, b'\n' | b'\r' | 0))
        {
            return;
        }
        let inserted_end = start + text.len();
        let mut bytes = Vec::with_capacity(old.len() - (end - start) + text.len());
        bytes.extend_from_slice(&old[..start]);
        bytes.extend_from_slice(text.as_slice());
        bytes.extend_from_slice(&old[end..]);
        self.value = Some(SecretBytes::new(bytes));
        // Insertion can join clusters on either side, as when a combining mark
        // follows a letter or a joiner connects two emoji. Resolve the caret in
        // the resulting value, at the first whole-cluster boundary after it.
        self.head = self
            .boundaries()
            .partition_point(|boundary| *boundary < inserted_end);
        self.anchor = self.head;
    }

    fn input(&mut self, event: TextInputEvent) {
        match event {
            TextInputEvent::Commit(text) => self.replace(SecretBytes::from(text)),
            TextInputEvent::Preedit { text, .. } => drop(SecretBytes::from(text)),
            TextInputEvent::SelectAll => {
                self.anchor = 0;
                self.head = self.count();
            }
            TextInputEvent::Left { extend } | TextInputEvent::WordLeft { extend } => {
                self.head = if !extend && self.anchor != self.head {
                    self.anchor.min(self.head)
                } else {
                    self.head.saturating_sub(1)
                };
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Right { extend } | TextInputEvent::WordRight { extend } => {
                self.head = if !extend && self.anchor != self.head {
                    self.anchor.max(self.head)
                } else {
                    (self.head + 1).min(self.count())
                };
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Home { extend } => {
                self.head = 0;
                if !extend {
                    self.anchor = 0;
                }
            }
            TextInputEvent::End { extend } => {
                self.head = self.count();
                if !extend {
                    self.anchor = self.head;
                }
            }
            TextInputEvent::Backspace | TextInputEvent::DeleteWordBackward => {
                if self.anchor == self.head {
                    self.anchor = self.head.saturating_sub(1);
                }
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::Delete => {
                if self.anchor == self.head {
                    self.anchor = (self.head + 1).min(self.count());
                }
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::KillToStart => {
                self.anchor = 0;
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::KillToEnd => {
                self.anchor = self.count();
                self.replace(SecretBytes::new(Vec::new()));
            }
            TextInputEvent::Undo
            | TextInputEvent::Redo
            | TextInputEvent::Submit
            | TextInputEvent::Cancel => {}
        }
    }

    fn projection(&self) -> TextInputProjection {
        TextInputProjection {
            text: "•".repeat(self.count()),
            selection: TextSelection {
                anchor: self.anchor * "•".len(),
                head: self.head * "•".len(),
            },
            preedit: None,
        }
    }

    fn take(&mut self) -> Option<SecretBytes> {
        self.anchor = 0;
        self.head = 0;
        self.value.take().filter(|value| !value.is_empty())
    }
}

/// A plan sent to the worker and not yet durably accepted.
struct Awaiting {
    plan: Plan,
    /// It carried the vault password (Join), which goes once accepted.
    password: bool,
}

pub(crate) struct SystemStateApp {
    /// The plan the page shows: the groups as the person (or the offer) set
    /// them.
    plan: Plan,
    /// Whether `plan` is the worker's (or the offer's) yet. A page opened by
    /// hand knows none until the first status names one: until then its
    /// groups choose nothing and it asks for no reading (§3.5).
    plan_known: bool,
    /// The plan the worker last accepted.
    submitted: Plan,
    /// The plan sent last, until the worker accepts or refuses it. A choice
    /// takes effect at once (§3.3), so a later one replaces it: the worker
    /// runs the newest.
    awaiting: Option<Awaiting>,
    /// Stop was pressed for the run going now.
    stopping: bool,
    url: TextInputState,
    password: Password,
    /// The person's own disclosure of the vault fields; `None` follows the row.
    vault_open: Option<bool>,
    status: SetupStatus,
    request: Option<Request>,
    actions: VecDeque<SystemStateAction>,
    /// Whether the gesture being handled is a person's (not a control client's).
    by_person: bool,
    /// Page ticks seen while a run goes: the village's walk.
    anim_ticks: u64,
    /// The last page drawn shows a village whose kitties walk, so a busy tick
    /// repaints it.
    village_walks: Cell<bool>,
}

impl SystemStateApp {
    pub(crate) fn text_input(&self, key: &str) -> Option<&TextInputState> {
        (key == "system-state/url").then_some(&self.url)
    }

    pub(crate) fn field_projection(&self, key: &str) -> Option<TextInputProjection> {
        match key {
            "system-state/url" => Some(self.url.projection()),
            "system-state/password" => Some(self.password.projection()),
            _ => None,
        }
    }

    /// A page that knows `plan` from the start (a test's stand-in for the
    /// worker's status or the offer).
    #[cfg(test)]
    pub(crate) fn new(plan: Plan) -> Self {
        let mut page = Self::awaiting_plan(plan);
        page.plan_known = true;
        page
    }

    /// A page opened by hand: it waits for the worker's status to name the
    /// plan in force before its groups choose anything.
    pub(crate) fn opened() -> Self {
        Self::awaiting_plan(Plan::default())
    }

    fn awaiting_plan(plan: Plan) -> Self {
        Self {
            url: TextInputState::new(plan.vault_url.clone()),
            submitted: plan.clone(),
            plan,
            plan_known: false,
            awaiting: None,
            stopping: false,
            password: Password::default(),
            vault_open: None,
            status: SetupStatus::default(),
            request: None,
            actions: VecDeque::new(),
            by_person: false,
            anim_ticks: 0,
            village_walks: Cell::new(false),
        }
    }

    /// Whether the page knows the plan in force (the offer's or the worker's).
    pub(crate) fn plan_known(&self) -> bool {
        self.plan_known
    }

    pub(crate) fn can_offer(&self) -> bool {
        !self.status.busy
            && !self.request.as_ref().is_some_and(Request::pending)
            && self.awaiting.is_none()
            && !self.vault_draft()
    }

    pub(crate) fn offer(&mut self, request: Request) -> bool {
        if !request.pending() || !self.can_offer() {
            request.finish(WorkstationSetupDecision::Unavailable);
            return false;
        }
        self.plan = request.initial().clone();
        self.plan_known = true;
        self.submitted = self.plan.clone();
        self.url = TextInputState::new(self.plan.vault_url.clone());
        self.password = Password::default();
        self.status = SetupStatus::default();
        self.request = Some(request);
        true
    }

    /// Take the worker's latest status. Its plan becomes the page's while
    /// nothing the page sent waits for the worker (and no offer is up); a
    /// vault address being typed stays as typed.
    pub(crate) fn publish(&mut self, status: SetupStatus) -> bool {
        let mut changed = self.status != status;
        if self.request.is_none()
            && self.awaiting.is_none()
            && let Some(plan) = &status.plan
        {
            if !self.plan_known || self.plan.selection != plan.selection {
                self.plan.selection = plan.selection;
                changed = true;
            }
            self.plan_known = true;
            if !self.vault_draft() && self.url.value() != plan.vault_url {
                self.url = TextInputState::new(plan.vault_url.clone());
                changed = true;
            }
            self.plan.vault_url = plan.vault_url.clone();
            self.submitted = plan.clone();
        }
        if !status.running() {
            self.stopping = false;
        }
        self.status = status;
        changed
    }

    pub(crate) fn take_action(&mut self) -> Option<SystemStateAction> {
        self.actions.pop_front()
    }

    /// Note whose gesture the next event is: the host stamps it before the
    /// reducer runs, from the App's gesture source.
    pub(crate) fn gesture(&mut self, by_person: bool) {
        self.by_person = by_person;
    }

    /// The page's clock-free step: a withdrawn offer is let go, and, in an
    /// active window, the first-run offer is applied; while a run goes, the
    /// village's kitties walk a step. `true` when the page changed.
    pub(crate) fn tick(&mut self, active: bool) -> bool {
        let before = (self.request.is_some(), self.status.busy);
        if self
            .request
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            self.request = None;
        }
        if !self.status.busy && active && self.pending() {
            self.apply(Admission::Automatic, Submission::AsItStands);
        }
        let walked = self.status.busy && {
            self.anim_ticks = self.anim_ticks.wrapping_add(1);
            self.village_walks.get()
        };
        walked || before != (self.request.is_some(), self.status.busy)
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
    }

    /// A vault address or password typed and not joined yet.
    fn vault_draft(&self) -> bool {
        !self.password.text().is_empty() || self.url.value().trim() != self.submitted.vault_url
    }

    /// The page's plan with the vault address as typed (what Check again
    /// reads).
    pub(crate) fn reviewed_plan(&self) -> Plan {
        let mut plan = self.plan.clone();
        plan.vault_url = self.url.value().trim().to_owned();
        plan
    }

    /// The tab's attention diamond (§3.6): a managed row needs a person, or
    /// failed and the run no longer retries it (§3.5).
    pub(crate) fn needs_attention(&self) -> bool {
        let signal = self.status.failures_signal();
        let reviewed = self.reviewed_plan();
        self.plan_known
            && (rows(&self.status, self.plan.selection)
                .iter()
                .any(|row| row.attention() && (signal || row.standing != Some(Standing::Failed)))
                || self.status.failed_steps(Some(&reviewed)).next().is_some())
    }

    /// Send the page's groups to the worker now (§3.3: a choice takes effect
    /// at once). A run that is going stops at its next step and this plan
    /// runs next: the host and the worker see to that, so nothing waits here
    /// on a view that may close. Join sends the vault address as typed and
    /// its password; any other submission sends the address the worker holds
    /// and leaves a draft where it is.
    fn apply(&mut self, admission: Admission, submission: Submission) {
        let with_vault = submission == Submission::Join;
        let mut plan = self.plan.clone();
        plan.vault_url = if with_vault {
            self.url.value().trim().to_owned()
        } else {
            self.submitted.vault_url.clone()
        };
        // The callback may refuse to start. Keep the scrubbable password until
        // the host confirms this answer was accepted.
        let password = if with_vault {
            self.password
                .value
                .clone()
                .filter(|value| !value.is_empty())
        } else {
            None
        };
        self.status.busy = true;
        self.status.operation = None;
        self.stopping = false;
        self.awaiting = Some(Awaiting {
            plan: plan.clone(),
            password: password.is_some(),
        });
        self.actions.push_back(SystemStateAction::Apply(
            SetupAnswer {
                decision: WorkstationSetupDecision::Begin(plan),
                password,
                receipt: None,
                chosen: submission != Submission::AsItStands,
                vault_typed: with_vault,
            },
            admission,
        ));
    }

    /// The worker durably accepted `plan`.
    pub(crate) fn apply_accepted(&mut self, plan: Plan) -> bool {
        self.submitted = plan.clone();
        self.plan_known = true;
        self.request = None;
        if self
            .awaiting
            .take_if(|awaiting| awaiting.plan == plan)
            .is_some_and(|awaiting| awaiting.password)
        {
            // Joined: the address is the plan's now, and the password went once.
            drop(self.password.take());
            self.url = TextInputState::new(plan.vault_url.clone());
        }
        true
    }

    /// The plan sent last did not start (the worker refused it or could not
    /// start): the groups go back to the plan the worker holds, so the page
    /// never shows a choice that is not taking effect. A vault draft and its
    /// password stay for another try.
    pub(crate) fn apply_refused(&mut self) -> bool {
        if self.awaiting.take().is_none() {
            return false;
        }
        self.plan.selection = self.submitted.selection;
        true
    }

    /// The host calls this only after the final view has actually detached.
    /// Closing never cancels (§3.5): an unanswered offer is let go, and setup
    /// resumes at the next launch. What the page sent is the worker's already.
    pub(crate) fn closed(&mut self, _scope: CloseScope) {
        if let Some(request) = self.request.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
    }

    /// The plan the worker will run next: the one sent last, else the one it
    /// holds.
    fn next_plan(&self) -> &Plan {
        self.awaiting
            .as_ref()
            .map_or(&self.submitted, |awaiting| &awaiting.plan)
    }

    /// Set every bit of `group` to `on`, and submit the plan when that changes
    /// what the worker will run.
    fn choose(&mut self, group: Group, on: bool) {
        if !self.plan_known {
            return;
        }
        for setting in group.settings() {
            self.plan.selection.set(*setting, on);
        }
        if self.plan.selection != self.next_plan().selection {
            self.apply(Admission::of_gesture(self.by_person), Submission::Choice);
        }
    }

    /// Whether the vault's fields show: as the person last set the
    /// disclosure, else open while the vault waits for input here.
    fn vault_shown(&self) -> bool {
        self.vault_open.unwrap_or_else(|| {
            let row = row_view(
                Subject::Setting(Setting::Secrets),
                &self.status,
                self.plan.selection,
            );
            row.managed
                && matches!(row.standing, Some(Standing::NeedsYou | Standing::Failed))
                && row.note.is_none()
        })
    }

    fn field_event(&mut self, key: &str, event: TextInputEvent) {
        if key == "system-state/password" {
            self.password.input(event);
            return;
        }
        if key != "system-state/url" {
            return;
        }
        match event {
            TextInputEvent::Commit(text) => self.url.insert(&text),
            TextInputEvent::Preedit { text, selection } => self.url.set_preedit(text, selection),
            TextInputEvent::Backspace => self.url.delete_backward(),
            TextInputEvent::Delete => self.url.delete_forward(),
            TextInputEvent::Left { extend } => self.url.move_left(extend),
            TextInputEvent::Right { extend } => self.url.move_right(extend),
            TextInputEvent::WordLeft { extend } => self.url.move_word_left(extend),
            TextInputEvent::WordRight { extend } => self.url.move_word_right(extend),
            TextInputEvent::Home { extend } => self.url.move_to_start(extend),
            TextInputEvent::End { extend } => self.url.move_to_end(extend),
            TextInputEvent::KillToEnd => self.url.kill_to_end(),
            TextInputEvent::KillToStart => self.url.kill_to_start(),
            TextInputEvent::DeleteWordBackward => self.url.delete_word_backward(),
            TextInputEvent::SelectAll => self.url.select_all(),
            TextInputEvent::Undo => {
                self.url.undo();
            }
            TextInputEvent::Redo => {
                self.url.redo();
            }
            TextInputEvent::Submit | TextInputEvent::Cancel => {}
        }
    }

    /// One pressed control of the page.
    fn act(&mut self, view: &mut SystemStateViewState, key: &str, value: Option<SemanticInput>) {
        if let Some(rest) = key.strip_prefix("system-state/group/") {
            let mut parts = rest.split('/');
            if let (Some(group), Some(choice)) = (
                parts
                    .next()
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| Group::ALL.get(index)),
                parts.next(),
            ) && matches!(choice, "set-up" | "leave")
            {
                self.choose(*group, choice == "set-up");
            }
            return;
        }
        match key {
            "system-state/stop" if self.status.running() && !self.stopping => {
                // Asked once: the worker keeps it, so a run that has not
                // shown up yet stops once it does.
                self.stopping = true;
                self.actions.push_back(SystemStateAction::Stop);
            }
            "system-state/check" if !self.status.busy => {
                let plan = self.reviewed_plan();
                self.status.busy = true;
                self.status.operation = Some(SetupOperation::Check);
                self.actions.push_back(SystemStateAction::Check(plan));
            }
            "system-state/restore" if !self.status.busy => {
                self.status.busy = true;
                self.status.operation = Some(SetupOperation::Restore);
                self.actions.push_back(SystemStateAction::Restore);
            }
            "system-state/headline/start" if !self.status.busy && self.plan_known => {
                self.apply(
                    Admission::of_gesture(self.by_person),
                    Submission::AsItStands,
                );
            }
            "system-state/headline/open" => {
                if let Some(access) =
                    headline(&self.status, &rows(&self.status, self.plan.selection)).open
                {
                    self.actions.push_back(SystemStateAction::OpenAccess {
                        access,
                        by_person: self.by_person,
                    });
                }
            }
            "system-state/headline/show" => {
                if let Some(subject) =
                    headline(&self.status, &rows(&self.status, self.plan.selection)).show
                {
                    // The vault's fields sit under its row: Join, their last
                    // control, brings all of them into view.
                    let key = if subject == Subject::Setting(Setting::Secrets) && self.vault_shown()
                    {
                        "system-state/vault/join".to_owned()
                    } else {
                        format!("system-state/row/{}", row_slug(subject))
                    };
                    view.reveal_listed_control(&UiKey::new(key));
                }
            }
            "system-state/row/secrets/disclose" => {
                self.vault_open = Some(!self.vault_shown());
            }
            TURN_ON_SYNC => {
                if rows(&self.status, self.plan.selection)
                    .iter()
                    .any(|row| row.turn_on_sync)
                {
                    self.actions.push_back(SystemStateAction::TurnSyncOn {
                        by_person: self.by_person,
                    });
                }
            }
            "system-state/vault/join" if self.plan_known => {
                self.vault_open = Some(true);
                self.plan.selection.set(Setting::Secrets, true);
                self.apply(Admission::of_gesture(self.by_person), Submission::Join);
            }
            "system-state/url" | "system-state/password" => {
                // A field someone fills shows: a draft is never hidden.
                self.vault_open = Some(true);
                view.common.last_focus = Some(UiKey::new(key));
                match value {
                    Some(SemanticInput::Text(text)) if key.ends_with("password") => {
                        self.password.anchor = 0;
                        self.password.head = self.password.count();
                        self.password.replace(SecretBytes::from(text));
                    }
                    Some(SemanticInput::Text(text)) => {
                        self.url.select_all();
                        self.url.insert(&text);
                    }
                    Some(SemanticInput::TextPosition { byte, extend })
                        if key.ends_with("password") =>
                    {
                        self.password.head = (byte / "•".len()).min(self.password.count());
                        if !extend {
                            self.password.anchor = self.password.head;
                        }
                    }
                    Some(SemanticInput::TextPosition { byte, extend }) => {
                        let anchor = if extend {
                            self.url.selection().anchor
                        } else {
                            byte
                        };
                        self.url.set_selection(anchor, byte);
                    }
                    _ => {}
                }
            }
            _ => {
                if let Some(access) = key
                    .strip_prefix("system-state/row/")
                    .and_then(|rest| rest.strip_suffix("/open"))
                    .and_then(|slug| Access::ALL.into_iter().find(|a| a.word() == slug))
                    && rows(&self.status, self.plan.selection)
                        .iter()
                        .any(|row| row.open == Some(access))
                {
                    self.actions.push_back(SystemStateAction::OpenAccess {
                        access,
                        by_person: self.by_person,
                    });
                }
            }
        }
    }
}

impl Drop for SystemStateApp {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.finish(WorkstationSetupDecision::Unavailable);
        }
    }
}

impl NativeAppModel for SystemStateApp {
    type ViewState = SystemStateViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::SystemState,
            name: "System State",
            icon: AppIcon::Settings,
            singleton: true,
        }
    }

    fn update(
        &mut self,
        view: &mut Self::ViewState,
        event: AppEvent,
        cx: &mut UpdateCx<'_>,
    ) -> EventResult {
        // Scrolling and focus are this view's own; every other change is the
        // shared page's.
        let shared = !matches!(
            event,
            AppEvent::FocusChanged(_)
                | AppEvent::ScrollLines(_)
                | AppEvent::ScrollAt { .. }
                | AppEvent::ScrollPage(_)
                | AppEvent::PressListPicture(_)
        );
        #[cfg(any(a11y_tree, test))]
        let shared = shared && !matches!(event, AppEvent::RevealControl(_));
        let handled = match event {
            AppEvent::FocusChanged(focus) => {
                if let Some(key) = &focus {
                    view.reveal_listed_control(key);
                }
                view.common.last_focus = focus;
                true
            }
            AppEvent::ScrollLines(lines) | AppEvent::ScrollAt { lines, .. } => {
                view.scroll_by(lines as f32 * LINE_PX);
                true
            }
            AppEvent::ScrollPage(direction) => {
                view.scroll_by(direction as f32 * (view.room.get() - LINE_PX).max(LINE_PX));
                true
            }
            AppEvent::PressListPicture(key) => {
                view.reveal_listed_control(&key);
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::RevealControl(key) => {
                view.reveal_listed_control(&key);
                true
            }
            AppEvent::TextInput(event) => {
                if !self.status.busy && event != TextInputEvent::Cancel {
                    self.field_event(
                        view.common.last_focus.as_ref().map_or("", UiKey::as_str),
                        event,
                    );
                }
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                if !self.status.busy {
                    self.field_event(
                        view.common.last_focus.as_ref().map_or("", UiKey::as_str),
                        TextInputEvent::Commit(text),
                    );
                }
                true
            }
            AppEvent::Action(action) => {
                let before = self.actions.len();
                self.act(view, action.id.as_str(), action.value);
                if self.actions.len() != before {
                    // A queued intent retires any review a held key could repeat.
                    cx.retire_review();
                }
                true
            }
            _ => false,
        };
        if handled {
            view.common.presentation_revision = view.common.presentation_revision.wrapping_add(1);
            if shared {
                cx.repaint_instance(crate::native_app::DamageRegion::All);
            } else {
                cx.repaint(crate::native_app::DamageRegion::All);
            }
            EventResult::Handled
        } else {
            EventResult::Bubble
        }
    }

    fn view(&self, view: &Self::ViewState, cx: &ViewCx<'_>) -> UiTree {
        Page::new(self, view, cx).tree()
    }

    fn commands(&self, _view: &Self::ViewState, _out: &mut Vec<Command>) {}

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: "System State".to_owned(),
            icon: AppIcon::Settings,
            indicators: AppIndicators {
                // A choice takes effect at once: nothing here is ever unsaved.
                dirty: false,
                busy: self.status.busy,
                attention: self.needs_attention(),
            },
            closable: true,
            tooltip: Some("System State".to_owned()),
        }
    }

    fn prepare_close(&mut self, _close: CloseRequest, _cx: &mut UpdateCx<'_>) -> CloseReadiness {
        CloseReadiness::Ready
    }
}

/// The page's measures at one viewport and text scale.
struct Metrics {
    compact: bool,
    pad_x: f32,
    pad_y: f32,
    gap: f32,
    /// The list's width.
    inner: f32,
    /// A card's side padding.
    card_pad_x: f32,
    /// A card's content width.
    card_inner: f32,
    /// Rows put the value under the name: the card is too narrow for both at this text size.
    two_line: bool,
    body_px: f32,
    row_h: f32,
    line_h: f32,
    control_h: f32,
    header_h: f32,
    name_w: f32,
    chip_w: f32,
}

impl Metrics {
    const CARD_PAD_Y: f32 = 6.0;

    fn new(width: f32, height: f32) -> Self {
        let compact = width < 720.0 || height < 700.0;
        let narrow = width < 260.0;
        let scale = crate::native_settings::settings_text_scale();
        let pad_x = if narrow {
            8.0
        } else if compact {
            12.0
        } else {
            24.0
        };
        let card_pad_x = if narrow { 8.0 } else { 14.0 };
        let inner = (width - pad_x * 2.0).max(1.0);
        let card_inner = (inner - card_pad_x * 2.0).max(1.0);
        let body_px = crate::native_ui::native_type_px(crate::type_scale::TypeStep::Body).get();
        let name_w = Subject::all()
            .into_iter()
            .map(|subject| crate::tray_raster::ui_text_width(subject.name(), body_px))
            .fold(0.0_f32, f32::max)
            .ceil()
            + 20.0;
        let chip_w = [
            Chip::WillChange { busy: true },
            Chip::NeedsYou,
            Chip::Failed,
            Chip::LeftAsIs,
            Chip::InPlace,
        ]
        .into_iter()
        .map(Chip::width)
        .fold(0.0_f32, f32::max);
        Self {
            compact,
            pad_x,
            pad_y: if compact { 10.0 } else { 16.0 },
            gap: if compact { 10.0 } else { 12.0 },
            inner,
            card_pad_x,
            card_inner,
            two_line: card_inner < 520.0 * scale.max(1.0),
            body_px,
            row_h: (30.0 * scale).max(30.0),
            line_h: (20.0 * scale).max(20.0),
            control_h: (30.0 * scale).max(30.0),
            header_h: (38.0 * scale).max(38.0),
            name_w: name_w.min(card_inner * 0.45),
            chip_w,
        }
    }

    fn text_w(&self, words: &str) -> f32 {
        crate::tray_raster::ui_text_width(words, self.body_px).ceil() + 1.0
    }

    /// The width `words` paint at in the face and size `role` and `style` give them.
    fn words_w(role: SemanticRole, style: StyleRef, words: &str) -> f32 {
        let (px, face) = crate::native_ui::text_paint_metrics(role, style);
        crate::tray_raster::ui_text_width_for(face, words, px).ceil() + 1.0
    }

    /// A button's width for `words` (padding either side).
    fn button_w(&self, words: &str) -> f32 {
        self.text_w(words) + 28.0
    }
}

/// One rendering of the page.
struct Page<'a, 'b> {
    app: &'a SystemStateApp,
    view: &'a SystemStateViewState,
    cx: &'a ViewCx<'b>,
    m: Metrics,
    rows: Vec<RowView>,
    /// Revealable keys and where they sit in the list's content.
    spans: Vec<Span>,
}

impl<'a, 'b> Page<'a, 'b> {
    fn new(app: &'a SystemStateApp, view: &'a SystemStateViewState, cx: &'a ViewCx<'b>) -> Self {
        let m = Metrics::new(cx.viewport.width.max(64.0), cx.viewport.height.max(120.0));
        // Until the page knows the plan in force it cannot say which rows
        // aterm manages: every row reads `—`, with no chip.
        let rows = if app.plan_known {
            rows(&app.status, app.plan.selection)
        } else {
            rows(&SetupStatus::default(), Selection::all())
        };
        Self {
            rows,
            app,
            view,
            cx,
            m,
            spans: Vec::new(),
        }
    }

    fn busy(&self) -> bool {
        self.app.status.busy
    }

    fn tree(mut self) -> UiTree {
        let (pad_x, pad_y, gap, row_h) = (self.m.pad_x, self.m.pad_y, self.m.gap, self.m.row_h);
        let headline = if self.app.plan_known {
            headline(&self.app.status, &self.rows)
        } else {
            Headline::before_plan(&self.app.status)
        };
        let brand = self.brand();
        let mut heading = crate::native_settings::page_heading("System State", &headline.text);
        let subtitle = heading.pop();
        let mut title = heading.pop();
        let fixed = |node: Option<&UiNode>| match node.map(|node| node.layout.height) {
            Some(Length::Fixed(height)) => height,
            _ => 0.0,
        };
        let subtitle_h = fixed(subtitle.as_ref());
        let mut title_h = fixed(title.as_ref());
        // A title wider than the page takes a line per word that fits.
        if let Some(node) = title.as_ref()
            && let UiContent::Text(spec) = &node.content
            && Metrics::words_w(spec.role, spec.style, &spec.text) > self.m.inner
        {
            let (px, _) = crate::native_ui::text_paint_metrics(spec.role, spec.style);
            let line_h = (px * 1.2).ceil().min(title_h);
            let (wrapped, height) = words_node(
                node.key.as_str(),
                &spec.text.clone(),
                spec.role,
                spec.style,
                self.m.inner,
                line_h,
            );
            title_h = height + (title_h - line_h).max(0.0);
            title = Some(
                wrapped.layout(
                    Layout::column()
                        .gap(0.0)
                        .width(Length::Fill)
                        .height(Length::Fixed(title_h)),
                ),
            );
        }
        let stop = self.app.status.running().then_some(!self.app.stopping);
        let (headline_row, headline_h) = self.headline_row(subtitle, subtitle_h, &headline, stop);
        // The meter shows only while a run is going (§3.2).
        let meter = (self.app.plan_known && self.app.status.running()).then(|| self.meter());
        let (footer, footer_h) = self.footer();
        let meter_h = if meter.is_some() { 6.0 + 4.0 } else { 0.0 };
        // The list's lead gap: a spacer between two of the column's own gaps,
        // so the list sits `gap` under the headline.
        let lead = (gap - 8.0).max(0.0);
        // The column's own gaps (4pt between the headline, the lead gap, the
        // list and the footer) and the lead gap itself.
        let rest = self.cx.viewport.height
            - pad_y * 2.0
            - headline_h
            - meter_h
            - footer_h
            - lead
            - 4.0 * 3.0;
        // The room under the tallest headline and the meter any state shows,
        // which the brand and the title are judged by, so a run that starts,
        // needs you or fails one step neither takes them away nor puts them
        // back; a headline naming several rows that stands taller still
        // counts its own.
        let steady =
            rest - (self.tallest_headline().max(headline_h) - headline_h) - (6.0 + 4.0 - meter_h);
        // A window too short for the headline, the footer and two rows' height
        // of the list leaves out the brand, then the title: each shows only
        // where the list keeps that height with it. The list then shrinks to
        // one row's height before the footer leaves the window, since the list
        // scrolls and the footer does not.
        let roomy = row_h * 2.0;
        let title_cost = title.as_ref().map_or(0.0, |_| title_h + 4.0);
        let show_title = steady - title_cost >= roomy;
        let show_brand = show_title && steady - title_cost - (brand.1 + 4.0) >= roomy;
        let list_h = (rest
            - if show_title { title_cost } else { 0.0 }
            - if show_brand { brand.1 + 4.0 } else { 0.0 })
        .max(row_h);
        let list = self.list(list_h);
        let mut children = Vec::new();
        if show_brand {
            children.push(brand.0);
        } else {
            self.app.village_walks.set(false);
        }
        if show_title {
            children.extend(title);
        }
        children.push(headline_row);
        if let Some(meter) = meter {
            children.push(meter);
        }
        children.push(
            UiNode::new(
                "system-state/list-gap",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().height(Length::Fixed(lead)))
            .paint_only(),
        );
        children.push(list);
        children.push(footer);
        *self.view.spans.borrow_mut() = std::mem::take(&mut self.spans);
        UiTree::new(
            UiNode::new(
                "system-state/app",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Application)),
            )
            .layout(
                Layout::column()
                    .padding(Insets::symmetric(pad_x, pad_y))
                    .gap(4.0)
                    .clipped(),
            )
            .children(children),
        )
    }

    /// The brand lockup (§3.1), with its height.
    fn brand(&self) -> (UiNode, f32) {
        let mut spec = BrandSpec {
            kitty: !self.cx.motion.serious
                && !crate::native_appearance::current_preferences().high_contrast,
            px: if self.m.compact { 17.0 } else { 22.0 },
            scale: self.cx.terminal_scale.max(1.0),
        };
        // A narrow page sets the whole lockup smaller rather than cut it.
        while spec.width() > self.m.inner && spec.px > 8.0 {
            spec.px -= 1.0;
        }
        let height =
            (if self.m.compact { 44.0 } else { 56.0_f32 }).max(spec.needed_height().ceil());
        let width = spec.width().min(self.m.inner);
        let village = self.village(spec.kitty, self.m.inner - width - 12.0);
        let motion = &self.cx.motion;
        self.app
            .village_walks
            .set(village.is_some() && !motion.system_reduced && !motion.performance_reduced);
        let mut children = vec![
            UiNode::new(
                "system-state/brand/art",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::SYSTEM_STATE_BRAND_AUDIT,
                    role: SemanticRole::Text,
                    label: paint::BRAND_WORDS.to_owned(),
                    value: Some(spec.value()),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(width))
                    .height(Length::Fill),
            )
            .paint_only(),
        ];
        children.extend(village);
        (
            UiNode::new(
                "system-state/brand",
                UiContent::Group(GroupSpec {
                    label: Some(paint::BRAND_WORDS.to_owned()),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::row().gap(12.0).height(Length::Fixed(height)))
            .children(children),
            height,
        )
    }

    /// The village beside the lockup ([`village_spec`]): the run's progress
    /// as a scene, never a second mark. It shows where the brand row has
    /// `room` for it, the plan is known and the kitty may show (not in
    /// serious mode or high contrast); its kitties stand still where motion
    /// is reduced. Paint only: the headline and the meter say the same in
    /// words.
    fn village(&self, kitty: bool, room: f32) -> Option<UiNode> {
        if !kitty || !self.app.plan_known || room < VILLAGE_MIN_W {
            return None;
        }
        let motion = &self.cx.motion;
        let phase_ms = if motion.system_reduced || motion.performance_reduced {
            0
        } else {
            self.cx
                .animation_phase_ms
                .wrapping_add(self.app.anim_ticks.wrapping_mul(VILLAGE_TICK_MS))
        };
        Some(
            UiNode::new(
                "system-state/village",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::RAINBOW_BANNER_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    value: Some(village_spec(
                        &self.app.status,
                        &self.rows,
                        self.app.plan.selection,
                        phase_ms,
                    )),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(Layout::default().width(Length::Fill).height(Length::Fill))
            .paint_only(),
        )
    }

    fn button(
        &self,
        key: &str,
        label: String,
        visual: Option<&str>,
        style: StyleRef,
        enabled: bool,
    ) -> UiNode {
        let width = self.m.button_w(visual.unwrap_or(&label));
        let mut spec = ButtonSpec::new(label);
        spec.visual_label = visual.map(str::to_owned);
        UiNode::new(
            key.to_owned(),
            UiContent::Button(
                Control::new(spec, ActionId::new(key.to_owned()))
                    .state(ControlState {
                        enabled,
                        ..ControlState::default()
                    })
                    .style(style),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fixed(self.m.control_h)),
        )
    }

    /// A button at most `room` wide, with its width and height: its face on one line where it
    /// fits, else its words on as many lines as they need in a button `room` wide, else (a
    /// word wider than the room) its `short` face where that fits.
    #[allow(
        clippy::too_many_arguments,
        reason = "a button's words, look and room, as `button` takes them"
    )]
    fn fitted_button(
        &self,
        key: &str,
        label: String,
        visual: Option<&str>,
        short: Option<&str>,
        style: StyleRef,
        enabled: bool,
        room: f32,
    ) -> (UiNode, f32, f32) {
        let face = visual.unwrap_or(&label).to_owned();
        let width = self.m.button_w(&face);
        if width <= room {
            return (
                self.button(key, label, visual, style, enabled),
                width,
                self.m.control_h,
            );
        }
        let mut node = self.button(key, label, Some(&face), style, enabled);
        let mut height = self.m.control_h;
        if let UiContent::Button(control) = &mut node.content {
            let words_room = crate::native_ui::text_button_label_room(&control.spec, room);
            match crate::native_ui::native_button_label_lines(&face, words_room) {
                Some(lines) if lines.len() > 1 => {
                    height = crate::native_ui::native_button_height_for_lines(lines.len(), height);
                    control.spec.visual_label = Some(lines.join("\n"));
                }
                None if short.is_some_and(|short| self.m.button_w(short) <= room) => {
                    control.spec.visual_label = short.map(str::to_owned);
                }
                _ => {}
            }
        }
        (
            node.layout(
                Layout::default()
                    .width(Length::Fixed(room))
                    .height(Length::Fixed(height)),
            ),
            room,
            height,
        )
    }

    /// `buttons` (each `(node, width, height)`) side by side where they fit `room`, else one to
    /// a row, as one node keyed `key`, with its height.
    fn button_rows(key: &str, buttons: Vec<(UiNode, f32, f32)>, room: f32) -> (UiNode, f32) {
        let gaps = 8.0 * buttons.len().saturating_sub(1) as f32;
        // Side by side, the trailing space takes a gap too.
        let side_by_side = buttons.iter().map(|b| b.1).sum::<f32>() + gaps + 8.0 <= room;
        let height = if side_by_side {
            buttons.iter().map(|b| b.2).fold(0.0_f32, f32::max)
        } else {
            buttons.iter().map(|b| b.2).sum::<f32>() + gaps
        };
        let layout = if side_by_side {
            Layout::row()
        } else {
            Layout::column()
        };
        let mut children: Vec<UiNode> = buttons.into_iter().map(|b| b.0).collect();
        if side_by_side {
            children.push(
                UiNode::new(
                    format!("{key}/space"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::default().width(Length::Fill))
                .paint_only(),
            );
        }
        (
            UiNode::new(
                key.to_owned(),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(layout.gap(8.0).height(Length::Fixed(height)))
            .children(children),
            height,
        )
    }

    /// Centre `node` (of `height`) down a row `row_h` tall.
    fn centred(key: String, node: UiNode, width: Length, height: f32, row_h: f32) -> UiNode {
        let top = ((row_h - height) / 2.0).max(0.0);
        UiNode::new(
            key,
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::column()
                .width(width)
                .height(Length::Fixed(row_h))
                .padding(Insets {
                    top,
                    right: 0.0,
                    bottom: 0.0,
                    left: 0.0,
                }),
        )
        .children(vec![node])
    }

    /// The height of the tallest headline the page's states put at this
    /// width, each with Stop beside it: waiting on the debugger password, one
    /// that needs you with its Turn On or its Show, one failed or waiting step
    /// with its Show, changes to make with Set up now, and the run's own words.
    /// The forms that name a row name the longest, and those that count rows
    /// count every row. A longer list of names can stand taller still; the
    /// page then counts its own.
    fn tallest_headline(&self) -> f32 {
        let rows = Subject::all();
        let longest = rows
            .iter()
            .copied()
            .max_by_key(|subject| subject.name().len())
            .unwrap_or(Subject::Access(Access::Debugger))
            .name();
        let (all, more) = (rows.len(), rows.len().saturating_sub(1));
        let needs = format!("Needs you: {longest} and {more} more");
        let forms = [
            Headline::line(format!(
                "Enter your password for {}",
                Access::Debugger.name()
            )),
            Headline {
                open: Some(Access::Debugger),
                ..Headline::line(needs.clone())
            },
            Headline {
                show: Some(Subject::Access(Access::Debugger)),
                ..Headline::line(needs)
            },
            Headline {
                show: Some(Subject::Access(Access::Debugger)),
                ..Headline::line(format!("1 step failed: {longest}"))
            },
            Headline {
                show: Some(Subject::Access(Access::Debugger)),
                ..Headline::line(format!("1 step waiting: {longest}"))
            },
            Headline {
                start: true,
                ..Headline::line(format!("{all} changes to make"))
            },
            Headline::line(format!("Setting up this Mac… {all} of {all}")),
            Headline::line("Restoring previous settings…"),
        ];
        forms
            .iter()
            .map(|form| {
                let subtitle =
                    crate::native_settings::page_heading("System State", &form.text).pop();
                let subtitle_h = match subtitle.as_ref().map(|node| node.layout.height) {
                    Some(Length::Fixed(height)) => height,
                    _ => 0.0,
                };
                self.headline_row(subtitle, subtitle_h, form, Some(true)).1
            })
            .fold(0.0, f32::max)
    }

    /// The headline (§3.2) beside its button: Open, Show, Set up now, or Stop,
    /// which `stop` asks for while a run is going and says whether it is enabled.
    /// Where they do not fit one line, the words take the page's width and the
    /// buttons go under them. Returns the node and its height.
    fn headline_row(
        &self,
        subtitle: Option<UiNode>,
        subtitle_h: f32,
        headline: &Headline,
        stop: Option<bool>,
    ) -> (UiNode, f32) {
        let m = &self.m;
        let row_h = subtitle_h.max(m.control_h);
        let mut faces: Vec<(&str, String, Option<&str>, StyleRef, bool)> = Vec::new();
        if let Some(access) = headline.open {
            let (label, visual) = open_words(access);
            faces.push((
                "system-state/headline/open",
                label,
                Some(visual),
                StyleRef::Accent,
                true,
            ));
        }
        if let Some(subject) = headline.show {
            faces.push((
                "system-state/headline/show",
                format!("Show {}", subject.name()),
                Some("Show"),
                StyleRef::Secondary,
                true,
            ));
        }
        if headline.start {
            faces.push((
                "system-state/headline/start",
                format!("Set up now: {}", headline.text),
                Some("Set up now"),
                StyleRef::Accent,
                true,
            ));
        }
        if let Some(enabled) = stop {
            faces.push((
                "system-state/stop",
                "Stop".to_owned(),
                None,
                StyleRef::Secondary,
                enabled,
            ));
        }
        // The buttons sit beside the words they act on (§3.2), not across
        // the page from them.
        let buttons_w: f32 = faces
            .iter()
            .map(|(_, label, visual, ..)| m.button_w(visual.unwrap_or(label)) + 8.0)
            .sum();
        let words_w = Metrics::words_w(SemanticRole::Text, StyleRef::Quiet, &headline.text) + 12.0;
        if words_w + buttons_w <= m.inner {
            let mut children = Vec::new();
            if let Some(subtitle) = subtitle {
                children.push(Self::centred(
                    "system-state/headline".to_owned(),
                    subtitle,
                    Length::Fixed(words_w),
                    subtitle_h,
                    row_h,
                ));
            }
            children.extend(
                faces
                    .into_iter()
                    .map(|(key, label, visual, style, enabled)| {
                        self.button(key, label, visual, style, enabled)
                    }),
            );
            children.push(
                UiNode::new(
                    "system-state/headline/space",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::default().width(Length::Fill))
                .paint_only(),
            );
            return (
                UiNode::new(
                    "system-state/headline-row",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::row().gap(8.0).height(Length::Fixed(row_h)))
                .children(children),
                row_h,
            );
        }
        let mut children = Vec::new();
        let mut height = 0.0;
        if subtitle.is_some() {
            let (words, words_h) = words_node(
                "settings/page-subtitle/system-state",
                &headline.text,
                SemanticRole::Text,
                StyleRef::Quiet,
                m.inner,
                subtitle_h,
            );
            children.push(
                UiNode::new(
                    "system-state/headline",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::column().height(Length::Fixed(words_h)))
                .children(vec![words]),
            );
            height += words_h;
        }
        if !faces.is_empty() {
            let buttons = faces
                .into_iter()
                .map(|(key, label, visual, style, enabled)| {
                    self.fitted_button(key, label, visual, None, style, enabled, m.inner)
                })
                .collect();
            let (buttons, buttons_h) =
                Self::button_rows("system-state/headline/buttons", buttons, m.inner);
            if height > 0.0 {
                height += 4.0;
            }
            children.push(buttons);
            height += buttons_h;
        }
        (
            UiNode::new(
                "system-state/headline-row",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::column().gap(4.0).height(Length::Fixed(height)))
            .children(children),
            height,
        )
    }

    fn meter(&self) -> UiNode {
        let managed = self.rows.iter().filter(|row| row.managed).count();
        let done = self.rows.iter().filter(|row| row.settled()).count();
        UiNode::new(
            "system-state/meter",
            UiContent::Custom(AuditedCustomNode {
                audit_id: crate::native_ui::SYSTEM_STATE_METER_AUDIT,
                role: SemanticRole::Group,
                label: String::new(),
                value: Some(format!("{done},{managed}")),
                action: None,
                focusable: false,
                focused: false,
                focus_visible: false,
            }),
        )
        .layout(
            Layout::default()
                .width(Length::Fixed(self.m.inner.min(320.0)))
                .height(Length::Fixed(6.0)),
        )
        .paint_only()
    }

    /// Check again, and, quiet and right-aligned, Restore previous settings
    /// (§3.4); one to a row where both do not fit. Returns the node and its height.
    fn footer(&self) -> (UiNode, f32) {
        let room = self.m.inner;
        let check = self.fitted_button(
            "system-state/check",
            "Check again".to_owned(),
            None,
            Some("Check"),
            StyleRef::Secondary,
            !self.busy(),
            room,
        );
        let mut restore = self.fitted_button(
            "system-state/restore",
            "Restore previous settings".to_owned(),
            None,
            Some("Restore"),
            StyleRef::Quiet,
            !self.busy(),
            room,
        );
        if let UiContent::Button(control) = &mut restore.0.content {
            control.spec.description = Some(
                "Puts back the Mac preferences aterm saved before changing them. Sign-outs, \
                 Rust docs, repositories, credentials and permissions stay."
                    .to_owned(),
            );
        }
        let side_by_side = check.1 + 16.0 + restore.1 <= room;
        if !side_by_side {
            return Self::button_rows("system-state/footer", vec![check, restore], room);
        }
        let height = check.2.max(restore.2);
        (
            UiNode::new(
                "system-state/footer",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().gap(8.0).height(Length::Fixed(height)))
            .children(vec![
                check.0,
                UiNode::new(
                    "system-state/footer/space",
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(Layout::default().width(Length::Fill))
                .paint_only(),
                restore.0,
            ]),
            height,
        )
    }

    /// The scrolling list: the notes an observation cannot carry, then the
    /// five groups, `list_h` tall. A list that scrolls keeps its last
    /// [`FADE_STRIP`] as a strip its rows fade out in, so a card its room
    /// cuts reads as going on, never as a control cut off with no words; and
    /// it reaches into the page's right gutter, where its thumb stands clear
    /// of the cards, as a Settings page's does
    /// (`settings_scroll::seat_page_scroller`): cards, headline and footer
    /// end at one edge.
    fn list(&mut self, list_h: f32) -> UiNode {
        let gap = self.m.gap;
        let mut items: Vec<(UiNode, f32)> = Vec::new();
        let mut top = 0.0;
        let reviewed = self.app.reviewed_plan();
        let notes: Vec<String> = self
            .app
            .status
            .notes(&reviewed)
            .map(SetupStatusRow::words)
            .collect();
        for (index, note) in notes.iter().enumerate() {
            let (node, height) = crate::native_settings::wrapped_copy_node(
                &format!("system-state/note/{index}"),
                note,
                SemanticRole::Text,
                StyleRef::Secondary,
                self.m.inner,
                self.m.line_h,
                Some(3),
            );
            items.push((node, height));
            top += height + gap;
        }
        for (index, group) in Group::ALL.into_iter().enumerate() {
            let (node, height) = self.card(index, group, top);
            items.push((node, height));
            top += height + gap;
        }
        let content = (top - gap).max(0.0);
        let scrolls = content > list_h + 0.5;
        // The live room, above the strip: the last row scrolls fully into it.
        let room = if scrolls {
            list_h - FADE_STRIP.min(list_h / 4.0)
        } else {
            list_h
        };
        let limit = (content - room).max(0.0);
        let offset = self.view.scroll.get().clamp(0.0, limit);
        self.view.scroll.set(offset);
        self.view.room.set(room);
        self.view.limit.set(limit);
        let mut y = 0.0;
        let children: Vec<UiNode> = items
            .into_iter()
            .map(|(node, height)| {
                let (item_top, item_bottom) = (y - offset, y - offset + height);
                y += height + gap;
                if item_bottom <= 0.01 || item_top >= list_h - 0.01 {
                    node.offscreen()
                } else {
                    node
                }
            })
            .collect();
        let mut list = UiNode::new(
            "system-state/groups",
            UiContent::Group(GroupSpec {
                label: Some("Mac setup".to_owned()),
                role: if scrolls {
                    SemanticRole::List
                } else {
                    SemanticRole::Group
                },
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::column()
                .width(Length::Fill)
                .height(Length::Fixed(list_h))
                .gap(gap)
                .padding(Insets {
                    top: 0.0,
                    right: if scrolls { self.m.pad_x } else { 0.0 },
                    bottom: 0.0,
                    left: 0.0,
                })
                .scrolled(offset)
                .clipped(),
        )
        .room(room, scrolls)
        .children(children);
        if let Some(room) = list.room.as_mut() {
            room.lists_all = true;
        }
        if !scrolls {
            return list;
        }
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "points of a page a few thousand tall, rounded"
        )]
        let share = format!(
            "{},{},{}",
            offset.round() as u32,
            room.round() as u32,
            content.round() as u32
        );
        UiNode::new(
            "system-state/scroll",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(list_h))
                .outdent(Insets {
                    right: self.m.pad_x,
                    ..Insets::default()
                }),
        )
        .children(vec![
            list,
            UiNode::new(
                "system-state/scroll/thumb",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::PAGE_SCROLL_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    value: Some(share),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            )
            .layout(
                Layout::default()
                    .width(Length::Fill)
                    .height(Length::Fixed(room)),
            )
            .paint_only(),
        ])
    }

    fn span(&mut self, key: impl Into<Arc<str>>, top: f32, height: f32) {
        self.spans.push(Span {
            key: key.into(),
            top,
            bottom: top + height,
        });
    }

    /// One group's card (§3.3): its name, count and segmented choice over its
    /// rows. `top` is where it starts in the list.
    fn card(&mut self, index: usize, group: Group, top: f32) -> (UiNode, f32) {
        let selection = self.app.plan.selection;
        let (count, label) = if self.app.plan_known {
            group_words(group, selection, &self.rows)
        } else {
            (None, group.title().to_owned())
        };
        let key = format!("system-state/group/{index}");
        let mut y = Metrics::CARD_PAD_Y;
        let mut children = Vec::new();
        let (header, header_h) = self.card_header(index, group, count);
        self.span(format!("{key}/set-up"), top + y, header_h);
        self.span(format!("{key}/leave"), top + y, header_h);
        children.push(header);
        y += header_h;
        let members: Vec<RowView> = self
            .rows
            .iter()
            .filter(|row| row.subject.group() == group)
            .cloned()
            .collect();
        for row in &members {
            children.push(rule(format!(
                "system-state/row/{}/rule",
                row_slug(row.subject)
            )));
            y += 1.0;
            let (node, height) = self.row(row);
            let slug = row_slug(row.subject);
            self.span(format!("system-state/row/{slug}"), top + y, height);
            if row.open.is_some() {
                self.span(format!("system-state/row/{slug}/open"), top + y, height);
            }
            if row.turn_on_sync {
                self.span(TURN_ON_SYNC.to_owned(), top + y, height);
            }
            children.push(node);
            y += height;
            if row.subject == Subject::Setting(Setting::Secrets) {
                for (node, height, keys) in self.vault(top + y) {
                    for (key, at, h) in keys {
                        self.span(key, at, h);
                    }
                    children.push(node);
                    y += height;
                }
            }
        }
        y += Metrics::CARD_PAD_Y;
        self.span(key.clone(), top, y);
        (
            UiNode::new(
                key,
                UiContent::Group(GroupSpec {
                    label: Some(label),
                    role: SemanticRole::Group,
                    style: StyleRef::Secondary,
                }),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(y))
                    .padding(Insets {
                        top: Metrics::CARD_PAD_Y,
                        right: self.m.card_pad_x,
                        bottom: Metrics::CARD_PAD_Y,
                        left: self.m.card_pad_x,
                    }),
            )
            .children(children),
            y,
        )
    }

    /// A group's name, its count, and `Set up │ Leave as is`: one line where they fit,
    /// else the name (and the count beside it where both fit) over the choice, whose two
    /// buttons go one to a row, the card's width, where side by side they do not fit.
    fn card_header(&self, index: usize, group: Group, count: Option<String>) -> (UiNode, f32) {
        let m = &self.m;
        let key = format!("system-state/group/{index}");
        // Until the plan is known, neither segment is chosen or pressable.
        let known = self.app.plan_known;
        let choice = if known {
            group_choice(group, self.app.plan.selection)
        } else {
            GroupChoice::Partly
        };
        let seg_h = m.control_h;
        let seg_w = m.button_w("Set up") + m.button_w("Leave as is") + 2.0;
        let stacked_choice = seg_w > m.card_inner;
        let segments: Vec<(UiNode, f32)> = [("set-up", "Set up"), ("leave", "Leave as is")]
            .into_iter()
            .map(|(word, words)| {
                let chosen = match choice {
                    GroupChoice::SetUp => word == "set-up",
                    GroupChoice::LeaveAsIs => word == "leave",
                    GroupChoice::Partly => false,
                };
                let mut spec = ButtonSpec {
                    visual_label: Some(words.to_owned()),
                    ..ButtonSpec::new(format!("{}: {words}", group.title()))
                };
                let (width, height) = if stacked_choice {
                    let room = crate::native_ui::text_button_label_room(&spec, m.card_inner);
                    let lines = crate::native_ui::native_button_label_lines(words, room)
                        .filter(|lines| lines.len() > 1);
                    let height = lines.as_ref().map_or(seg_h, |lines| {
                        crate::native_ui::native_button_height_for_lines(lines.len(), seg_h)
                    });
                    if let Some(lines) = lines {
                        spec.visual_label = Some(lines.join("\n"));
                    }
                    (Length::Fixed(m.card_inner), height)
                } else {
                    (Length::Fixed(m.button_w(words)), seg_h)
                };
                let mut control = Control::new(spec, ActionId::new(format!("{key}/{word}")))
                    .state(ControlState {
                        selected: chosen,
                        enabled: known,
                        ..ControlState::default()
                    })
                    .style(StyleRef::Navigation);
                if chosen {
                    control = control.value(SemanticValue::Text(words.to_owned()));
                }
                let height_len = if stacked_choice {
                    Length::Fixed(height)
                } else {
                    Length::Fill
                };
                (
                    UiNode::new(format!("{key}/{word}"), UiContent::Button(control))
                        .layout(Layout::default().width(width).height(height_len)),
                    height,
                )
            })
            .collect();
        let choice_h = if stacked_choice {
            segments.iter().map(|s| s.1).sum::<f32>() + 4.0
        } else {
            seg_h
        };
        let choice_group = UiNode::new(
            format!("{key}/choice"),
            UiContent::Group(GroupSpec {
                label: Some(group.title().to_owned()),
                role: SemanticRole::Group,
                style: StyleRef::Secondary,
            }),
        );
        let segments: Vec<UiNode> = segments.into_iter().map(|s| s.0).collect();
        let segmented = if stacked_choice {
            choice_group
                .layout(
                    Layout::column()
                        .gap(4.0)
                        .width(Length::Fixed(m.card_inner))
                        .height(Length::Fixed(choice_h)),
                )
                .children(segments)
        } else {
            choice_group
                .layout(
                    Layout::row()
                        .gap(2.0)
                        .width(Length::Fixed(seg_w))
                        .height(Length::Fixed(seg_h)),
                )
                .children(segments)
                .segmented()
        };
        let title = UiNode::new(
            format!("{key}/title"),
            UiContent::Text(TextSpec {
                text: group.title().to_owned(),
                role: SemanticRole::Heading,
                style: StyleRef::Plain,
            }),
        );
        let title_w = Metrics::words_w(SemanticRole::Heading, StyleRef::Plain, group.title()) + 4.0;
        let count_w = count
            .as_deref()
            .map(|count| Metrics::words_w(SemanticRole::Text, StyleRef::Quiet, count) + 4.0);
        let count_node = |count: String| {
            UiNode::new(
                format!("{key}/count"),
                UiContent::Text(TextSpec {
                    text: count,
                    role: SemanticRole::Text,
                    style: StyleRef::Quiet,
                }),
            )
            .paint_only()
        };
        let one_line = title_w + count_w.unwrap_or(0.0) + 24.0 + seg_w <= m.card_inner;
        if !one_line {
            let line = m.header_h * 0.8;
            let count_beside = count_w.is_some_and(|w| title_w + 12.0 + w <= m.card_inner);
            let mut column = Vec::new();
            let mut height = 0.0;
            if title_w <= m.card_inner {
                let mut top_line = vec![
                    title.layout(
                        Layout::default()
                            .width(Length::Fixed(title_w))
                            .height(Length::Fill),
                    ),
                ];
                if count_beside && let Some(count) = count.clone() {
                    top_line.push(
                        count_node(count)
                            .layout(Layout::default().width(Length::Fill).height(Length::Fill)),
                    );
                }
                column.push(
                    UiNode::new(
                        format!("{key}/header/line"),
                        UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                    )
                    .layout(Layout::row().gap(12.0).height(Length::Fixed(line)))
                    .children(top_line),
                );
                height += line;
            } else {
                let (title, title_h) = words_node(
                    &format!("{key}/title"),
                    group.title(),
                    SemanticRole::Heading,
                    StyleRef::Plain,
                    m.card_inner,
                    m.line_h,
                );
                column.push(title);
                height += title_h;
            }
            if let Some(count) = count.filter(|_| !count_beside) {
                let (count, count_h) = words_node(
                    &format!("{key}/count"),
                    &count,
                    SemanticRole::Text,
                    StyleRef::Quiet,
                    m.card_inner,
                    m.line_h,
                );
                column.push(count.paint_only());
                height += count_h;
            }
            column.push(segmented);
            height += choice_h + 6.0;
            return (
                UiNode::new(
                    format!("{key}/header"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::column()
                        .gap(0.0)
                        .height(Length::Fixed(height))
                        .padding(Insets {
                            top: 0.0,
                            right: 0.0,
                            bottom: 6.0,
                            left: 0.0,
                        }),
                )
                .children(column),
                height,
            );
        }
        let row_h = m.header_h;
        let mut children = vec![
            title.layout(
                Layout::default()
                    .width(Length::Fixed(title_w))
                    .height(Length::Fill),
            ),
        ];
        children.push(match count {
            Some(count) => {
                count_node(count).layout(Layout::default().width(Length::Fill).height(Length::Fill))
            }
            None => UiNode::new(
                format!("{key}/header/space"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().width(Length::Fill))
            .paint_only(),
        });
        children.push(Self::centred(
            format!("{key}/choice-seat"),
            segmented,
            Length::Fixed(seg_w),
            seg_h,
            row_h,
        ));
        (
            UiNode::new(
                format!("{key}/header"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().gap(12.0).height(Length::Fixed(row_h)))
            .children(children),
            row_h,
        )
    }

    /// The value's words as text pieces: `now → wanted`, one value, or the
    /// muted reading, each `(words, style)`.
    fn value_pieces(row: &RowView) -> Vec<(String, StyleRef)> {
        match &row.value {
            RowValue::Change { now, wanted } => vec![
                (now.clone(), StyleRef::Secondary),
                ("→".to_owned(), StyleRef::Quiet),
                (wanted.clone(), StyleRef::Primary),
            ],
            RowValue::One(value) => vec![(value.clone(), StyleRef::Primary)],
            RowValue::Muted(value) => vec![(value.clone(), StyleRef::Secondary)],
        }
    }

    /// Paint-only text nodes for `pieces`, keyed from `first` on, each as wide as its
    /// words, within `room` (the last one takes what is left).
    fn value_nodes(
        &self,
        key: &str,
        pieces: Vec<(String, StyleRef)>,
        first: usize,
        room: f32,
    ) -> Vec<UiNode> {
        let mut left = room.max(0.0);
        pieces
            .into_iter()
            .enumerate()
            .map(|(index, (words, style))| {
                let index = first + index;
                // The last piece takes what is left, elided where it must be.
                let width = self.m.text_w(&words).min(left);
                left = (left - width - 8.0).max(0.0);
                UiNode::new(
                    format!("{key}/value/{index}"),
                    UiContent::Text(TextSpec {
                        text: words,
                        role: SemanticRole::Text,
                        style,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(width))
                        .height(Length::Fill),
                )
                .paint_only()
            })
            .collect()
    }

    /// The chip's node, as wide as the widest chip (a column), or `own` its own width.
    fn chip_node(&self, key: &str, chip: Option<Chip>) -> UiNode {
        self.chip_node_as(key, chip, false)
    }

    fn chip_node_as(&self, key: &str, chip: Option<Chip>, own: bool) -> UiNode {
        let width = match chip {
            Some(chip) if own => chip.width().ceil(),
            _ => self.m.chip_w,
        };
        match chip {
            Some(chip) => UiNode::new(
                format!("{key}/chip"),
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::SYSTEM_STATE_CHIP_AUDIT,
                    role: SemanticRole::Text,
                    label: chip.spoken().to_owned(),
                    value: Some(chip.value().to_owned()),
                    action: None,
                    focusable: false,
                    focused: false,
                    focus_visible: false,
                }),
            ),
            None => UiNode::new(
                format!("{key}/chip"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            ),
        }
        .layout(
            Layout::default()
                .width(Length::Fixed(width))
                .height(Length::Fill),
        )
        .paint_only()
    }

    /// One row (§3.3): its name, value and chip, read as one line by a
    /// screen reader, and its quiet button (a Needs-you row's, or the vault's
    /// Turn On Sync). Where one line cannot hold them, the value goes under the
    /// name, `now` and `→ wanted` each on a line of its own where together they do
    /// not fit, the chip under them where it does not fit beside the name, and the
    /// button on its own line where it does not fit beside the value. Its note
    /// follows it.
    fn row(&self, row: &RowView) -> (UiNode, f32) {
        let m = &self.m;
        let slug = row_slug(row.subject);
        let key = format!("system-state/row/{slug}");
        let name_style = if row.managed || row.chip != Some(Chip::LeftAsIs) {
            StyleRef::Primary
        } else {
            StyleRef::Secondary
        };
        let name_text = row.subject.name();
        // The button's key, label, face and shortest face.
        let face = row
            .open
            .map(|access| {
                let (label, short) = open_words(access);
                let visual = (access == Access::Debugger).then_some(short);
                (format!("{key}/open"), label, visual, Some(short))
            })
            .or_else(|| {
                row.turn_on_sync.then(|| {
                    (
                        TURN_ON_SYNC.to_owned(),
                        "Turn On Sync".to_owned(),
                        None,
                        None,
                    )
                })
            });
        let face_w = face
            .as_ref()
            .map(|(_, label, visual, _)| m.button_w(visual.unwrap_or(label)));
        let spacer = |name: &str| {
            UiNode::new(
                format!("{key}/{name}"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::default().width(Length::Fill))
            .paint_only()
        };
        let line_of = |name: String, children: Vec<UiNode>, height: f32| {
            UiNode::new(
                name,
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::row().gap(8.0).height(Length::Fixed(height)))
            .children(children)
            .paint_only()
        };
        let pieces = Self::value_pieces(row);
        let pieces_w = |pieces: &[(String, StyleRef)]| {
            pieces.iter().map(|(words, _)| m.text_w(words)).sum::<f32>()
                + 8.0 * pieces.len().saturating_sub(1) as f32
        };
        let value_w = pieces_w(&pieces);
        let beside = face_w.map_or(0.0, |width| width + 8.0);
        // The one line's gaps: after the name, the value and the space.
        let one_line = !m.two_line
            && m.text_w(name_text) <= m.name_w
            && value_w <= m.card_inner - m.name_w - m.chip_w - beside - 24.0;
        // Each text line's height, and where the button's line starts (`None`: beside the chip).
        let (info, row_h, button_at) = if one_line {
            let room = m.card_inner - m.name_w - m.chip_w - beside - 24.0;
            let mut children = vec![
                UiNode::new(
                    format!("{key}/name"),
                    UiContent::Text(TextSpec {
                        text: name_text.to_owned(),
                        role: SemanticRole::Text,
                        style: name_style,
                    }),
                )
                .layout(
                    Layout::default()
                        .width(Length::Fixed(m.name_w))
                        .height(Length::Fill),
                )
                .paint_only(),
            ];
            children.extend(self.value_nodes(&key, pieces, 0, room));
            children.push(spacer("space"));
            children.push(self.chip_node(&key, row.chip));
            let info = UiNode::new(
                format!("{key}/info"),
                UiContent::Group(GroupSpec {
                    label: Some(row.label.clone()),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::row()
                    .gap(8.0)
                    .width(Length::Fill)
                    .height(Length::Fill),
            )
            .children(children);
            (info, m.row_h, None)
        } else {
            let line_h = m.row_h * 0.8;
            let mut lines = Vec::new();
            let mut height = 0.0;
            // The name, and the chip beside it where both fit.
            let chip_beside = m.text_w(name_text) + 8.0 + m.chip_w <= m.card_inner;
            let name_room = if chip_beside {
                m.card_inner - m.chip_w - 8.0
            } else {
                m.card_inner
            };
            let (name, name_h) = words_node(
                &format!("{key}/name"),
                name_text,
                SemanticRole::Text,
                name_style,
                name_room,
                line_h,
            );
            let mut first = vec![name.paint_only()];
            if chip_beside {
                first.push(self.chip_node(&key, row.chip));
            }
            lines.push(line_of(format!("{key}/line-0"), first, name_h));
            height += name_h;
            // The value, beside the button where both fit.
            let button_beside = face_w.is_some_and(|width| value_w + 16.0 + width <= m.card_inner);
            let value_room = m.card_inner - if button_beside { beside } else { 0.0 };
            let value_top = height;
            // A value line ends in a space, which takes a gap.
            if value_w + 8.0 <= value_room {
                let mut second = self.value_nodes(&key, pieces, 0, value_room);
                second.push(spacer("space"));
                lines.push(line_of(format!("{key}/line-1"), second, line_h));
                height += line_h;
            } else {
                // `now`, then `→ wanted`, each on its line, its words wrapped where one
                // line cannot hold them.
                let mut at = 0;
                for (index, part) in pieces
                    .chunks(1)
                    .take(1)
                    .chain(pieces[1..].chunks(2))
                    .enumerate()
                {
                    let part = part.to_vec();
                    let part_w = pieces_w(&part);
                    let first = at;
                    at += part.len();
                    let (children, part_h) = if part_w + 8.0 <= m.card_inner {
                        let mut children = self.value_nodes(&key, part, first, m.card_inner);
                        children.push(spacer(&format!("space-{index}")));
                        (children, line_h)
                    } else {
                        // The last piece wraps beside any before it (the arrow).
                        let (last_words, last_style) = part[part.len() - 1].clone();
                        let lead: Vec<(String, StyleRef)> = part[..part.len() - 1].to_vec();
                        let lead_w = if lead.is_empty() {
                            0.0
                        } else {
                            pieces_w(&lead) + 8.0
                        };
                        let mut children = self.value_nodes(&key, lead, first, lead_w);
                        let (words, words_h) = words_node(
                            &format!("{key}/value/{}", first + part.len() - 1),
                            &last_words,
                            SemanticRole::Text,
                            last_style,
                            m.card_inner - lead_w,
                            line_h,
                        );
                        children.push(words.paint_only());
                        (children, words_h)
                    };
                    lines.push(line_of(
                        format!("{key}/line-{}", index + 1),
                        children,
                        part_h,
                    ));
                    height += part_h;
                }
            }
            if !chip_beside {
                lines.push(line_of(
                    format!("{key}/line-chip"),
                    vec![
                        self.chip_node_as(&key, row.chip, true),
                        spacer("chip-space"),
                    ],
                    line_h,
                ));
                height += line_h;
            }
            let info = UiNode::new(
                format!("{key}/info"),
                UiContent::Group(GroupSpec {
                    label: Some(row.label.clone()),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(
                Layout::column()
                    .width(Length::Fill)
                    .height(Length::Fixed(height)),
            )
            .children(lines);
            let button_at = if button_beside {
                Some((value_top, line_h))
            } else {
                Some((height, 0.0))
            };
            (info, height, button_at)
        };
        let mut row_h = row_h;
        let mut layers = vec![info];
        if let Some((button_key, label, visual, short)) = face {
            let (button, top, right, room) = match button_at {
                // Beside the chip's column, on the one line.
                None => {
                    let button = self.button(&button_key, label, visual, StyleRef::Quiet, true);
                    let height = m.control_h.min(row_h);
                    (
                        button.layout(
                            Layout::default()
                                .width(Length::Fixed(face_w.unwrap_or(0.0)))
                                .height(Length::Fixed(height)),
                        ),
                        ((row_h - height) / 2.0).max(0.0),
                        m.chip_w + 8.0,
                        height,
                    )
                }
                // Beside the value, on its first line.
                Some((top, line_h)) if line_h > 0.0 => {
                    let button = self.button(&button_key, label, visual, StyleRef::Quiet, true);
                    let height = m.control_h.min(line_h);
                    (
                        button.layout(
                            Layout::default()
                                .width(Length::Fixed(face_w.unwrap_or(0.0)))
                                .height(Length::Fixed(height)),
                        ),
                        top + ((line_h - height) / 2.0).max(0.0),
                        0.0,
                        height,
                    )
                }
                // On a line of its own, under the rest.
                Some((top, _)) => {
                    let (button, _, height) = self.fitted_button(
                        &button_key,
                        label,
                        visual,
                        short,
                        StyleRef::Quiet,
                        true,
                        m.card_inner,
                    );
                    row_h = top + height + 4.0;
                    (button, top + 2.0, 0.0, height)
                }
            };
            let own_line = matches!(button_at, Some((_, line_h)) if line_h <= 0.0);
            let children = if own_line {
                vec![button, spacer("actions/space")]
            } else {
                vec![spacer("actions/space"), button]
            };
            layers.push(
                UiNode::new(
                    format!("{key}/actions"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::row()
                        .width(Length::Fill)
                        .height(Length::Fixed(top + room))
                        .padding(Insets {
                            top,
                            right,
                            bottom: 0.0,
                            left: 0.0,
                        }),
                )
                .children(children),
            );
        }
        let line = UiNode::new(
            key.clone(),
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(row_h)),
        )
        .children(layers);
        let Some(note) = &row.note else {
            return (line, row_h);
        };
        let indent = if one_line { m.name_w + 8.0 } else { 0.0 };
        let (note, note_h) = crate::native_settings::wrapped_copy_node(
            &format!("{key}/note"),
            note,
            SemanticRole::Text,
            StyleRef::Secondary,
            (m.card_inner - indent).max(40.0),
            m.line_h,
            None,
        );
        let height = row_h + note_h + 6.0;
        (
            UiNode::new(
                format!("{key}/with-note"),
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(Layout::column().height(Length::Fixed(height)))
            .children(vec![
                line,
                UiNode::new(
                    format!("{key}/note-seat"),
                    UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
                )
                .layout(
                    Layout::column()
                        .height(Length::Fixed(note_h + 6.0))
                        .padding(Insets {
                            top: 0.0,
                            right: 0.0,
                            bottom: 6.0,
                            left: indent,
                        }),
                )
                .children(vec![note]),
            ]),
            height,
        )
    }

    /// The Secrets vault's disclosure and, open, its fields and Join (§3.3),
    /// each `(node, height, revealable keys at their tops)`.
    #[allow(
        clippy::type_complexity,
        reason = "one caller zips these into the card it builds"
    )]
    fn vault(&self, top: f32) -> Vec<(UiNode, f32, Vec<(String, f32, f32)>)> {
        let m = &self.m;
        let open = self.app.vault_shown();
        let mut disclose_h = m.row_h * 0.9;
        // The disclosure and the fields sit under the row's value column.
        let indent = if m.two_line { 0.0 } else { m.name_w + 8.0 };
        let mut out = Vec::new();
        let mut spec = ButtonSpec::new("Vault address and password");
        // Its words wrap where the card cannot hold them on one line.
        let room = crate::native_ui::disclosure_label_room(m.card_inner - (indent - 10.0).max(0.0));
        match crate::native_ui::native_button_label_lines(&spec.label, room) {
            Some(lines) if lines.len() > 1 => {
                disclose_h =
                    crate::native_ui::native_button_height_for_lines(lines.len(), disclose_h);
                spec.visual_label = Some(lines.join("\n"));
            }
            None => spec.visual_label = Some("Vault".to_owned()),
            Some(_) => {}
        }
        let disclose = UiNode::new(
            "system-state/row/secrets/disclose",
            UiContent::Button(
                Control::new(spec, ActionId::new("system-state/row/secrets/disclose"))
                    .state(ControlState {
                        expanded: Some(open),
                        ..ControlState::default()
                    })
                    .style(StyleRef::Plain),
            ),
        )
        .layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(disclose_h)),
        );
        // The button's own inset puts its words on the value column.
        let disclose = UiNode::new(
            "system-state/row/secrets/disclose-seat",
            UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
        )
        .layout(
            Layout::row()
                .width(Length::Fill)
                .height(Length::Fixed(disclose_h))
                .padding(Insets {
                    top: 0.0,
                    right: 0.0,
                    bottom: 0.0,
                    left: (indent - 10.0).max(0.0),
                }),
        )
        .children(vec![disclose]);
        out.push((
            disclose,
            disclose_h,
            vec![(
                "system-state/row/secrets/disclose".to_owned(),
                top,
                disclose_h,
            )],
        ));
        if !open {
            return out;
        }
        let field_w = (m.card_inner - indent).clamp(40.0, 420.0);
        let enabled = !self.app.status.busy;
        let caption = |key: &str, words: &str| {
            let (node, height) = words_node(
                key,
                words,
                SemanticRole::Text,
                StyleRef::Secondary,
                (m.card_inner - indent).max(40.0),
                m.line_h,
            );
            (node.paint_only(), height)
        };
        let (address_caption, address_h) =
            caption("system-state/vault/address-label", "Vault address");
        let (password_caption, password_h) =
            caption("system-state/vault/password-label", "Password (optional)");
        let field = |key: &str,
                     label: &str,
                     input: TextInputProjection,
                     secret: bool,
                     description: Option<&str>| {
            UiNode::new(
                key.to_owned(),
                UiContent::TextField(
                    Control::new(
                        TextFieldSpec {
                            label: label.to_owned(),
                            placeholder: (!secret)
                                .then(|| aterm_types::macos_setup::DEFAULT_VAULT_URL.to_owned()),
                            secret,
                            visual_value: None,
                            input: Some(input.clone()),
                            swatch: None,
                            swatch_line: false,
                            trailing_hint: None,
                            description: description.map(str::to_owned),
                            end_button: false,
                        },
                        ActionId::new(key.to_owned()),
                    )
                    .value(SemanticValue::Text(input.text))
                    .state(ControlState {
                        enabled,
                        focused: self
                            .view
                            .common
                            .last_focus
                            .as_ref()
                            .is_some_and(|focus| focus.as_str() == key),
                        ..ControlState::default()
                    })
                    .style(StyleRef::Secondary),
                ),
            )
            .layout(
                Layout::default()
                    .width(Length::Fixed(field_w))
                    .height(Length::Fixed(m.control_h)),
            )
        };
        let blank = "Leave the password blank to join an open vault at once, or to wait for \
                     approval from another Mac.";
        // The hint wraps under the field rather than lose its words on a narrow page.
        let (help, help_h) = crate::native_settings::wrapped_copy_node(
            "system-state/vault/password-help",
            blank,
            SemanticRole::Text,
            StyleRef::Secondary,
            (m.card_inner - indent).max(40.0),
            m.line_h,
            None,
        );
        let column = vec![
            address_caption,
            field(
                "system-state/url",
                "Vault address",
                self.app.url.projection(),
                false,
                None,
            ),
            password_caption,
            field(
                "system-state/password",
                "Password (optional)",
                self.app.password.projection(),
                true,
                Some(blank),
            ),
            help,
            self.button(
                "system-state/vault/join",
                "Join".to_owned(),
                None,
                StyleRef::Secondary,
                enabled,
            ),
        ];
        let height = address_h + password_h + help_h + m.control_h * 3.0 + 4.0 * 5.0 + 10.0;
        // Each field under its caption, 4pt apart (the column's gap).
        let url_top = top + disclose_h + address_h + 4.0;
        let password_top = url_top + m.control_h + 4.0 + password_h + 4.0;
        let join_top =
            top + disclose_h + address_h + password_h + help_h + m.control_h * 2.0 + 4.0 * 5.0;
        out.push((
            UiNode::new(
                "system-state/vault",
                UiContent::Group(GroupSpec::unlabeled(SemanticRole::Group)),
            )
            .layout(
                Layout::column()
                    .gap(4.0)
                    .height(Length::Fixed(height))
                    .padding(Insets {
                        top: 0.0,
                        right: 0.0,
                        bottom: 10.0,
                        left: indent,
                    }),
            )
            .children(column),
            height,
            vec![
                ("system-state/url".to_owned(), url_top, m.control_h),
                (
                    "system-state/password".to_owned(),
                    password_top,
                    m.control_h,
                ),
                ("system-state/vault/join".to_owned(), join_top, m.control_h),
            ],
        ));
        out
    }
}

/// `text` set in a box `width` wide in the face and size `role` and `style` paint: whole
/// words, a word broken only where it cannot fit a line ([`crate::native_settings::wrap_to_box`]).
fn lines_in(text: &str, role: SemanticRole, style: StyleRef, width: f32) -> Vec<String> {
    let (px, face) = crate::native_ui::text_paint_metrics(role, style);
    let measure = |line: &str| crate::tray_raster::ui_text_width_for(face, line, px);
    crate::native_settings::wrap_to_box(text, width.max(1.0), &measure)
}

/// `text` as a node `width` wide on its [`lines_in`], `line_h` apart, with its height: one
/// line is the text node `key`, more are `key/line-N` under a group `key` that names it whole.
fn words_node(
    key: &str,
    text: &str,
    role: SemanticRole,
    style: StyleRef,
    width: f32,
    line_h: f32,
) -> (UiNode, f32) {
    let line = |key: String, text: String| {
        UiNode::new(key, UiContent::Text(TextSpec { text, role, style })).layout(
            Layout::default()
                .width(Length::Fill)
                .height(Length::Fixed(line_h)),
        )
    };
    let lines = lines_in(text, role, style, width);
    if lines.len() <= 1 {
        return (line(key.to_owned(), text.to_owned()), line_h);
    }
    let height = line_h * lines.len() as f32;
    let children = lines
        .into_iter()
        .enumerate()
        .map(|(index, words)| line(format!("{key}/line-{index}"), words).paint_only())
        .collect();
    (
        UiNode::new(
            key.to_owned(),
            UiContent::Group(GroupSpec {
                label: Some(text.to_owned()),
                role,
                style: StyleRef::Plain,
            }),
        )
        .layout(
            Layout::column()
                .gap(0.0)
                .width(Length::Fill)
                .height(Length::Fixed(height)),
        )
        .children(children),
        height,
    )
}

/// A hairline between a card's rows.
fn rule(key: String) -> UiNode {
    UiNode::new(
        key,
        UiContent::Custom(AuditedCustomNode {
            audit_id: crate::native_ui::MESSAGES_RULE_AUDIT,
            role: SemanticRole::Group,
            label: String::new(),
            value: None,
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(1.0)))
    .paint_only()
}

/// The page's scenarios for looking at it without a Mac to set up
/// (`ATERM_DEV_SYSTEM_STATE_FIXTURE`): `checking`, `running`, `debugger`,
/// `needs-you`, `vault`, `set-up`, `non-alab`, `failed`, `check-failed`, `mixed` and
/// `fresh`. Debug and dev-seam builds only: a release build carries none of
/// their words.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn fixture_status(name: &str) -> Option<SetupStatus> {
    use Standing::{AtLogin, Differs, Failed, InPlace, NeedsYou, NotNeeded, Unknown, Waiting};
    let read = |subject: Subject, standing: Standing, now: Option<&str>| Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        wanted: subject.wanted().to_owned(),
        note: None,
        detail: None,
    };
    let access = |access: Access, standing: Standing, now: Option<&str>| {
        read(Subject::Access(access), standing, now)
    };
    let setting = |setting: Setting, standing: Standing, now: Option<&str>| {
        read(Subject::Setting(setting), standing, now)
    };
    let in_place = || -> Vec<Observation> {
        Subject::all()
            .into_iter()
            .map(|subject| read(subject, InPlace, Some(subject.wanted())))
            .collect()
    };
    let with = |mut all: Vec<Observation>, changes: Vec<Observation>| {
        for change in changes {
            if let Some(slot) = all.iter_mut().find(|o| o.subject == change.subject) {
                *slot = change;
            }
        }
        all
    };
    // The repositories as `init_observe` reads them before the vault has
    // put a GitHub token on this Mac: no count, and the reason in its note.
    let token_waiting = || Observation {
        note: Some("Waits for your GitHub token from the Secrets vault".to_owned()),
        ..setting(Setting::Repositories, Waiting, None)
    };
    let all = Plan::default();
    let mut status = SetupStatus {
        plan: Some(all.clone()),
        ..SetupStatus::default()
    };
    match name {
        "checking" => {}
        "check-failed" => {
            status.observed = in_place();
            status.check = Some(SetupCheck {
                plan: all,
                rows: vec![
                    SetupStatusRow {
                        name: "Check".into(),
                        status: "failed".into(),
                        detail: "The saved setup state could not be read. Check again.".into(),
                    },
                    SetupStatusRow {
                        name: "Check: GitHub repositories".into(),
                        status: "unverified".into(),
                        detail: "GitHub did not answer.".into(),
                    },
                ],
            });
        }
        "running" => {
            status.busy = true;
            status.running = vec![Setting::Dock];
            // As `macos_doctor` reads a vault the plan names but this network
            // does not answer for, and the repositories that wait on it.
            let mut secrets_waiting = setting(Setting::Secrets, Waiting, Some("Not joined"));
            secrets_waiting.note = Some("The ALab vault did not answer on this network".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    access(Access::LocalNetwork, Differs, None),
                    setting(Setting::Dock, Differs, Some("9 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    secrets_waiting,
                    token_waiting(),
                ],
            );
        }
        "debugger" => {
            status.busy = true;
            status.running = vec![Setting::Permissions];
            status.access_step = Some(Access::Debugger);
            // As `macos_doctor` reads it: the reason is the description.
            let mut debugger = access(Access::Debugger, NeedsYou, Some("Off"));
            debugger.detail = Some(
                "Turning it on lets debuggers attach without a password. macOS asks for an \
                 administrator password in a dialog named SamplingTools."
                    .to_owned(),
            );
            status.observed = with(
                in_place(),
                vec![debugger, access(Access::LocalNetwork, Differs, None)],
            );
        }
        "needs-you" => {
            // Each reason where its probe puts it: a grant's in the
            // description, Developer Tools' and the vault's code in the note.
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Off"));
            fda.detail =
                Some("Switch on aterm in Privacy & Security ▸ Full Disk Access.".to_owned());
            let mut tools = access(Access::DeveloperTools, Waiting, None);
            tools.note = Some("Readable once Full Disk Access is on".to_owned());
            let mut vault = setting(Setting::Secrets, NeedsYou, Some("Awaiting code"));
            vault.note = Some("Approve on an admin Mac: 7QK2-M9WD-4HTX-PL3B".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    fda,
                    tools,
                    access(Access::LocalNetwork, Differs, None),
                    vault,
                ],
            );
        }
        "vault" => {
            // The vault waits for an address or a password: its fields show.
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Secrets, NeedsYou, Some("Not joined")),
                    token_waiting(),
                ],
            );
        }
        "set-up" => {
            status.observed = with(
                in_place(),
                vec![setting(Setting::CapsLock, AtLogin, Some("Caps Lock"))],
            );
        }
        "non-alab" => {
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            status.plan = Some(Plan::from(selection));
            status.observed = with(
                in_place(),
                vec![
                    access(Access::Accessibility, NotNeeded, None),
                    setting(Setting::CapsLock, Differs, Some("Caps Lock")),
                    setting(Setting::NaturalScrolling, Differs, Some("On")),
                    setting(Setting::Desktop, Differs, Some("Icons hidden")),
                    setting(Setting::Widgets, Differs, Some("Shown")),
                    setting(Setting::Dock, Differs, Some("14 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    setting(Setting::Spotlight, Differs, Some("Docs indexed")),
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    setting(Setting::Repositories, Unknown, None),
                ],
            );
        }
        "failed" => {
            let mut repositories = setting(Setting::Repositories, Failed, Some("3 of 12"));
            repositories.detail = Some(
                "GitHub refused the saved sign-in: the token expired. Sign in with gh auth \
                 login, then Check again."
                    .to_owned(),
            );
            status.observed = with(in_place(), vec![repositories]);
        }
        "mixed" => {
            let mut selection = Selection::all();
            selection.set(Setting::NaturalScrolling, false);
            selection.set(Setting::TrackpadScrolling, false);
            selection.set(Setting::Messages, false);
            selection.set(Setting::FaceTime, false);
            status.plan = Some(Plan::from(selection));
            let mut accessibility = access(Access::Accessibility, NotNeeded, None);
            accessibility.detail =
                Some("Only the Messages and FaceTime sign-outs need it.".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    access(Access::Debugger, Differs, Some("Off")),
                    access(Access::LocalNetwork, Differs, None),
                    accessibility,
                    setting(Setting::CapsLock, AtLogin, Some("Caps Lock")),
                    setting(Setting::NaturalScrolling, Differs, Some("On")),
                    setting(Setting::Widgets, Differs, Some("Shown")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::Repositories, Unknown, None),
                ],
            );
            status.rows = vec![SetupStatusRow {
                name: "Restore previous settings".to_owned(),
                status: "restored".to_owned(),
                detail: "Natural scrolling and Trackpad scrolling are as they were.".to_owned(),
            }];
        }
        "fresh" => {
            // A fresh Mac as the Plan pass reads it, every group set up: most
            // rows will change, and the repositories wait for the token the
            // vault this Mac has not joined yet holds. Each carries the
            // engine's own line in its description (`init_observe` gives a
            // row that differs none, and a blocked part's reason is the
            // token file's rule): the page shows none, only the waiting
            // row's note.
            let engine = |setting: Setting, standing: Standing, now: Option<&str>, line: &str| {
                let mut row = read(Subject::Setting(setting), standing, now);
                row.detail = Some(line.to_owned());
                row
            };
            status.observed = with(
                in_place(),
                vec![
                    access(Access::LocalNetwork, Unknown, None),
                    engine(
                        Setting::CapsLock,
                        Differs,
                        Some("Caps Lock"),
                        "Caps Lock is not Control on every attached service → Caps Lock acts \
                         as Control immediately",
                    ),
                    engine(
                        Setting::NaturalScrolling,
                        Differs,
                        Some("On"),
                        "Natural scrolling: on in System Settings; preferred value not \
                         persisted → off",
                    ),
                    engine(
                        Setting::TrackpadScrolling,
                        Differs,
                        Some("Some off"),
                        "absent → true; false → true; absent → true",
                    ),
                    engine(
                        Setting::Desktop,
                        Differs,
                        Some("Icons hidden"),
                        "com.apple.finder CreateDesktop → true",
                    ),
                    engine(
                        Setting::Widgets,
                        Differs,
                        Some("Shown"),
                        "com.apple.WindowManager StandardHideWidgets → true",
                    ),
                    engine(
                        Setting::Dock,
                        Differs,
                        Some("7 apps"),
                        "com.apple.dock persistent-apps, com.apple.dock show-recents → Dock \
                         pins: aterm, Safari, System Settings",
                    ),
                    engine(
                        Setting::Messages,
                        Differs,
                        Some("Signed in"),
                        "account remains registered (including disabled accounts) → signed out",
                    ),
                    engine(
                        Setting::FaceTime,
                        Differs,
                        Some("Signed in"),
                        "account remains registered (including disabled accounts) → signed out",
                    ),
                    engine(
                        Setting::Spotlight,
                        Differs,
                        Some("Docs indexed"),
                        "rustup's profile is default, which installs rust-docs → minimal",
                    ),
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    {
                        let mut repositories = token_waiting();
                        repositories.detail = Some(
                            "GitHub token source must be an owned private regular file \
                             (0600), without links, at most 64 KiB"
                                .to_owned(),
                        );
                        repositories
                    },
                ],
            );
        }
        _ => return None,
    }
    status.observed_generation = 1;
    Some(status)
}

#[cfg(test)]
mod tests;
