// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System State tab, the setup screen (`docs/DESIGN-init-on-by-default-2026-10-25.md` §4):
//! what this Mac reads now beside what aterm wants, in five lines, each with its state, its live
//! phrase and its switch, which takes effect at once; one pinned card for everything that needs
//! the person; the lockup as the run's progress. First launch admits its offer automatically at
//! the first unblocked tick, with no wait for a person. The row, headline and notes rules here
//! are the page's model; [`lines`] says what the screen says from them and [`page`] draws it.

mod kitty;
pub(crate) mod lines;
mod lockup;
pub(crate) mod marks;
mod page;
pub(crate) mod repositories;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use aterm_grapheme::GraphemeClusters;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{
    Access, BASE_PLAN, FailureKind, Group, Observation, PassOutcome, Plan, PutBack, RestoreOutcome,
    SYNC_OFF, Selection, Setting, StageRow, Standing, Subject, SyncKind, TRIED_TWICE, Unfinished,
};

use crate::native_app::{
    AppDescriptor, AppEvent, AppIcon, AppIndicators, AppKind, AppPresentation, CloseReadiness,
    CloseRequest, CloseScope, Command, CommonViewState, EventResult, NativeAppModel, SemanticInput,
    TextInputEvent, UpdateCx, ViewCx,
};
use crate::native_text_input::{TextInputProjection, TextInputState, TextSelection};
use crate::native_ui::{UiKey, UiTree};
use crate::system_state_host::PhraseOutcome;
use crate::workstation_setup::{Request, SetupAnswer, WorkstationSetupDecision};

pub(crate) use lockup::{LockupSpec, paint_lockup};
#[cfg(any(test, feature = "bench-support"))]
pub(crate) use marks::working_spinner;
pub(crate) use marks::{paint_mark, paint_surface};

/// Where a row stands, as the row rule says it (§3.3): its small mark on the page
/// ([`lines::Mark`]) and the words a screen reader hears for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Chip {
    /// Read as wanted.
    InPlace,
    /// The prior useful reading was in place; this is explicitly not current evidence.
    LastKnown,
    /// Managed and differs; `busy` while its step runs.
    WillChange { busy: bool },
    /// A step waits for aterm to try it again, or for Check again (a stage aterm parked): calm,
    /// nothing for the person to do here.
    Waiting,
    /// macOS waits for a person.
    NeedsYou,
    /// The last attempt failed.
    Failed,
    /// Its line is turned off: aterm leaves it as is.
    LeftAsIs,
}

impl Chip {
    /// What a screen reader hears for it, in the row's label.
    pub(crate) fn spoken(self) -> &'static str {
        match self {
            Self::InPlace => "in place",
            Self::LastKnown => "in place as last read",
            Self::WillChange { busy: false } => "will change",
            Self::WillChange { busy: true } => "changing now",
            Self::Waiting => "waiting",
            Self::NeedsYou => "needs you",
            Self::Failed => "failed",
            Self::LeftAsIs => "left as is",
        }
    }
}

/// A non-secret snapshot published by setup's worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupStatus {
    /// Acquisition observations sampled by the status worker, never the UI thread.
    pub repositories: Vec<aterm_repository_progress::Progress>,
    pub plan: Option<Plan>,
    pub check: Option<SetupCheck>,
    pub permissions: Option<InitialSetupPermissionPolicy>,
    /// The exact current setup run's durable admission, separately from its
    /// carried Permissions approval. Unknown reads supply no run policy.
    pub run: Option<InitialSetupRunPolicy>,
    pub busy: bool,
    pub finished: bool,
    /// The plan in force is one aterm chose by itself, which a look that finds the ALab
    /// signal widens (design §3.5). Filled by the worker. While it is, every group press is
    /// sent, even one that leaves the plan as it is, so the plan becomes the person's.
    pub provisional: bool,
    pub rows: Vec<SetupStatusRow>,
    /// The latest read-only reading of each row, keyed by subject (at most one
    /// per subject). Rows with no reading yet are absent.
    pub observed: Vec<aterm_types::macos_setup::Observation>,
    /// Presentation history only. Never used as current readiness or admission.
    pub last_known: Vec<aterm_types::macos_setup::Observation>,
    /// Subjects whose latest attempted reading supplied no current evidence.
    pub unconfirmed_observations: Vec<Subject>,
    /// The subjects being read or retained for an accepted read.
    /// Each entry carries its vault source, and whether it is queued rather than reading.
    pub observing: Vec<(Subject, Option<String>, bool)>,
    /// The vault source the Secrets and Repositories readings describe.
    /// Unknown sources never supply those rows to a review.
    pub observed_vault_url: Option<String>,
    /// Raised each time an observed row or live keyboard diagnostic changes, so a
    /// publication can be compared cheaply.
    pub observed_generation: u64,
    /// Current HID-service mappings, independently of the saved Caps setting's readiness.
    /// This diagnostic grants no setup completion or permission to change a setting.
    pub caps_live: Option<aterm_types::macos_setup::LiveKeyboardMapping>,
    /// The settings whose stage the running pass is executing now.
    pub running: Vec<Setting>,
    /// The access step whose macOS gesture is up or about to start, filled by
    /// the window (`App::system_state_access_step`), never by the worker.
    pub access_step: Option<aterm_types::macos_setup::Access>,
    /// What the busy worker is doing when it is not a setup run (Check again,
    /// Restore), filled by the window (`App::publish_system_state`), never by
    /// the worker. Busy with none is a run.
    pub operation: Option<SetupOperation>,
    /// The run's worker goes on by itself: it runs its passes again whatever one leaves, on a
    /// pause of its own kept on disk, and wakes for its steps' and parked stages' own pauses, so
    /// a failure in `rows`, or a row that failed, is the page's alone, calm, and the page starts
    /// no run of its own meanwhile (§3.5, §3.6). `false` once that worker has left: its run
    /// finished, a newer run replaced it, or no worker of this process runs it. Filled by the
    /// worker.
    pub retrying: bool,
    /// The run going now is the one aterm admitted by itself on hearing the ALab vault in this
    /// launch, which widened the plan it had chosen before (§3.5): the page says so in one calm
    /// note while it goes. Filled by the worker, from the look that widened the plan.
    pub widened: bool,
    /// The worker's read met a write of the saved setup under way (a pass saving its progress,
    /// a Stop saving its request): it says nothing of the plan or its Permissions group, which
    /// a read a moment later does. Filled by the worker.
    pub changing: bool,
    /// An Apply waits for another setup that holds this Mac's setup records (another aterm,
    /// `aterm doctor` in a terminal): someone's choice ([`Self::queued_held`]), the plan as it
    /// stands, which the page sent by itself, or Restore previous settings while `operation` says
    /// so, which aterm applies when that setup finishes (§3.3, §3.4). The page keeps the choice
    /// showing, marked waiting, and says why in one calm note: nothing failed, so nothing is
    /// signalled, and no run goes, so there is no meter. Only a choice has a Stop
    /// ([`Self::stoppable`]). Filled by the worker.
    pub queued: bool,
    /// What waits ([`Self::queued`]) is a plan someone chose and that is not yet durable, so the
    /// words call it their choice and a Stop lets it go. Filled by the window
    /// (`PendingApply::holds_unsaved_work`), never by the worker.
    pub queued_held: bool,
    /// A person stopped a choice that waited for another setup. That may have saved nothing (this
    /// process registered no run, or its last one ended), so the plan's run can still read allowed,
    /// and the page starts nothing by itself until a press ([`SystemStateApp::held`]). The window
    /// holds the automatic access steps with it, and a person's Turn On… ends only that. A run that
    /// starts without a press (an offer answered, a resume, the ALab watch's widening) was not
    /// stopped, so it ends this. Filled by the window, never by the worker; a relaunch forgets it.
    pub stopped_wait: bool,
    /// The plan in force is the one aterm set up afresh after it set aside a saved setup it
    /// could not read (§3.5): every Mac preference group left as is, aterm's own permissions
    /// set up. The page says so in one calm note until a person chooses. Filled by the worker.
    pub recovered: bool,
    /// The saved setup holds a Cancel an older aterm's first-launch review recorded (§3.3): the
    /// worker's plan has those groups turned off, so they read Leave as is and turn back on by
    /// their switch, as any group a person left as is. The page says nothing more of it. Filled
    /// by the worker.
    pub cancelled: bool,
    /// Where an install of Apple's Command Line Tools stands, which the GitHub repositories row
    /// waits on while they are missing (§3.3): its tab asks for the person's password or was
    /// restored, an install runs, Apple's own installer window waits for the person, or a press
    /// of Install… waits for a macOS step or opened nothing. `None`: none of these, so the row's
    /// Install… opens the tab. Filled by the window (`App::developer_tools_install_step`), never
    /// by the worker.
    pub developer_tools: Option<ToolsInstall>,
    /// The GitHub repositories row waits on Apple's Command Line Tools: the window's shim reads
    /// them missing on this Mac ([`crate::clt_shim::Shim::tools_missing`]), or they were found
    /// installed only since that row's last reading, which the window keeps calm until the next
    /// one lands (`App::developer_tools_owed`). The page's tools wait, its card item and the row's
    /// Install… read this typed fact ([`Self::repositories_tools_wait`]), never another row's
    /// note. Filled by the window, never by the worker.
    pub tools_owed: bool,
    /// What waits for another setup ([`Self::queued`]) is aterm's own: the automatic admission
    /// of the groups this Mac's ALab signal adds, which nobody pressed (§3.5). The page says it
    /// waits to add this Mac's ALab profile, never the person's choice. Filled by the worker.
    pub queued_widen: bool,
    /// The permission walk's page now and its place in the walk, which the Needs-you card says
    /// beside its grant (`Step 1 of 3`). Filled by the window (`App::system_state_walk_page`),
    /// never by the worker.
    pub walk: Option<WalkPage>,
    /// The automatic join has exceeded the initial observation interval. This is
    /// diagnostic state; it does not turn admission into a person's approval task.
    pub asking_long: bool,
    /// How long the last setup run took from its start to its end, which the finished page says
    /// (`ready in 1 min 12 s`). `None` before one finished in this process. Filled by the window,
    /// never by the worker.
    pub ready_in: Option<std::time::Duration>,
    /// This Mac is an ALab Mac, as its secrets records say (`aterm_cli::macos_doctor::alab_mac`):
    /// only an ALab Mac signs out of Messages and FaceTime, so elsewhere that line is out of
    /// scope and its switch stays off. Filled by the worker.
    pub alab: bool,
    /// aterm saved this Mac's preferences before it changed them, so Restore previous settings
    /// has something to put back: before that the footer offers no Restore. Filled by the
    /// worker.
    pub restorable: bool,
}

/// The permission walk's page now (`crate::permission_walk`), as the Needs-you card says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WalkPage {
    /// The grant its System Settings page is for.
    pub access: Access,
    /// The page's place, 1-based, among the pages the person sees.
    pub at: usize,
    /// How many pages the walk shows.
    pub of: usize,
}

pub(crate) use lines::JOIN_QUIET;

/// An install of Apple's Command Line Tools under way, or a press of Install… that is not (§3.3),
/// as the window sees it ([`SetupStatus::developer_tools`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolsInstall {
    /// `Install…` was pressed while a macOS permission gesture held the dialog slot: the tab
    /// opens once that step finishes and System State is in front.
    Queued,
    /// Its terminal tab waits for the person's password: they are needed now, and typing it is
    /// the one thing that changes the outcome.
    Password,
    /// A live installer tab was restored. Its exact prompt is not known: the person continues
    /// there, or closes it before retrying, and no second installer is opened.
    Recovered,
    /// An install runs (`softwareupdate` or `installer` holding the on-demand placeholder, a live
    /// run of aterm's root half that stamped it, the tab's program outliving it, or the tab having
    /// said they are installed while the row's next reading is still to come): the row waits,
    /// calmly, and moves on once the tools arrive.
    Installing,
    /// Apple's own installer window is open, at any age: waiting for the person's Install or
    /// Agree press, or running the install a press started there. They finish there.
    AppleWindow,
    /// The tab a person's Install… asked for did not open, for this reason: the row offers
    /// Install… again and says why.
    NotOpened(NotOpened),
    /// Low Data Mode holds the tools' download back: the row waits, calmly, and the tab opens by
    /// itself once the mode is off. Install… stays, for a person who turned it off.
    LowData,
    /// This account is none of the Mac's administrators, whom alone macOS lets install them: the
    /// row says so, with no Install… that would only say it again, and no signal line, since
    /// nothing here changes it.
    NotAdmin,
    /// aterm's own ask for setup's Mac password comes by itself (its tab is due, waits on a quiet
    /// keyboard or another macOS step, or tries again on the account's record's schedule): the
    /// row waits, calmly, with no signal line, and Install… stays for a person who wants it now.
    Coming,
}

/// Why the tab a person's Install… asked for did not open ([`ToolsInstall::NotOpened`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotOpened {
    /// System State was no longer the front view of its window when the tab was ready.
    LeftPage,
}

impl NotOpened {
    /// The GitHub repositories row's note for it, in place of the reason the tools are needed.
    pub(crate) const fn note(self) -> &'static str {
        match self {
            Self::LeftPage => TOOLS_NOT_OPENED_NOTE,
        }
    }
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

/// Whether this exact setup run remains admitted and not durably stopped.
/// This grants no permission gesture or access approval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitialSetupRunPolicy {
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

/// A step the last run, a Check or a Restore could not finish (§3.6): the page's row it
/// names, if one, whether it is a row Restore could not put back ([`restore_rows`]), whether it
/// is a Restore that failed as a whole, which names no row, whether a Check said it, its kind,
/// and whether aterm tries it again by itself ([`SetupStatus::retries_by_itself`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Failure {
    pub(crate) row: Option<Subject>,
    pub(crate) restore: bool,
    pub(crate) whole_restore: bool,
    /// A Check's result: a read, whose remedy is pressing Check again, so the page's note alone
    /// says it (§3.6), never the headline, the signal line or the tab's attention.
    pub(crate) check: bool,
    /// What kind of failure it is (§3.6): one naming a row whose reading needs a person is that
    /// person's, which the row and the headline say as needing you, never as a failure or a
    /// wait; any other is transient, which aterm tries again while the run's worker goes on.
    pub(crate) kind: FailureKind,
    /// aterm tries it again by itself (a transient failure whose words say so, or one the run's
    /// worker still retries): the page counts it as waiting (§3.2) and nothing signals it, since
    /// nobody needs to act.
    pub(crate) retried: bool,
}

impl Failure {
    /// Whether the page says it as a failure (§3.6): the headline names it failed, and the menu
    /// line and the tab's attention say it with it. Not a Check's result (a read, whose remedy
    /// is pressing Check again), not a step aterm tries again by itself, not a step whose row
    /// needs a person (the row and the headline say that), and not a Restore that failed as a
    /// whole, which the page alone says beside the button that tries it again (§3.4).
    pub(crate) fn failed(&self) -> bool {
        !self.check && !self.retried && !self.whole_restore && self.kind != FailureKind::NeedsPerson
    }
}

impl SetupStatus {
    /// Keep useful values while a new offer waits for its own status. History and
    /// its vault source are presentation only; no current reading or admission survives.
    pub(crate) fn for_new_offer(&self) -> Self {
        let mut last_known = self.last_known.clone();
        last_known.retain(|row| !matches!(row.standing, Standing::Unknown | Standing::Failed));
        for row in self.observed.iter().filter(|row| {
            !self.unconfirmed_observations.contains(&row.subject)
                && row.read_at().is_some_and(|at| at != 0)
                && !matches!(row.standing, Standing::Unknown | Standing::Failed)
        }) {
            match last_known.iter_mut().find(|old| old.subject == row.subject) {
                Some(old) => old.clone_from(row),
                None => last_known.push(row.clone()),
            }
        }
        Self {
            last_known,
            observed_vault_url: self.observed_vault_url.clone(),
            ..Self::default()
        }
    }

    /// A setup run is going: busy with neither a Check nor a Restore, and no press waiting for
    /// another setup ([`Self::queued`]).
    pub(crate) fn running(&self) -> bool {
        self.busy && self.operation.is_none() && !self.queued
    }

    /// Stop reaches this: a setup run going, or a choice waiting for another setup
    /// ([`Self::queued_held`]), which holds every update until that setup ends. Not a Check or a
    /// Restore, and not an Apply nobody chose (the page's own, or the ALab profile's widening,
    /// [`Self::queued_widen`]), which holds no update past its settle.
    pub(crate) fn stoppable(&self) -> bool {
        self.running()
            || (self.busy
                && self.queued
                && self.queued_held
                && !self.queued_widen
                && self.operation.is_none())
    }

    /// Default ALab admission stays automatic while pending, with no approval card,
    /// tab attention or menu request, including after its initial observation interval.
    pub(crate) fn joins_quietly(&self) -> bool {
        !self.newer()
            && self.observed.iter().any(|row| {
                row.subject == Subject::Setting(Setting::Secrets)
                    && row.now.as_deref() == Some(aterm_types::macos_setup::VAULT_ASKING)
            })
    }

    /// Whether a row the plan in force manages is owed a read at `now` (Unix seconds): an
    /// ordinary refresh withheld its read ([`Observation::is_withheld`]), so nothing in this
    /// process has read it, or it reads privately and its reading is past standing for it
    /// ([`Observation::stale_at`]). A person at the page in front lets the next ordinary refresh
    /// read it (§3.5).
    pub(crate) fn owes_a_read(&self, now: u64) -> bool {
        self.plan.as_ref().is_some_and(|plan| {
            self.observations_for(Some(&plan.vault_url)).any(|row| {
                manages(plan.selection, row.subject)
                    && (row.is_withheld() || (row.subject.reads_privately() && row.stale_at(now)))
            })
        })
    }

    /// Readings valid for this vault source; the rest of the Mac is shared
    /// across reviews. Compare the field's trimmed value without probing it.
    pub(crate) fn observations_for(
        &self,
        vault_url: Option<&str>,
    ) -> impl Iterator<Item = &Observation> {
        let source_matches = vault_url.map(str::trim).is_some_and(|source| {
            !source.is_empty() && self.observed_vault_url.as_deref().map(str::trim) == Some(source)
        });
        self.observed
            .iter()
            .filter(move |observation| !observation.subject.uses_vault_source() || source_matches)
    }

    fn last_known_for(&self, vault_url: Option<&str>, subject: Subject) -> Option<&Observation> {
        let source_matches = vault_url.map(str::trim).is_some_and(|source| {
            !source.is_empty() && self.observed_vault_url.as_deref().map(str::trim) == Some(source)
        });
        self.last_known
            .iter()
            .find(|row| row.subject == subject && (!subject.uses_vault_source() || source_matches))
    }

    fn observation_progress(&self, subject: Subject, vault_url: Option<&str>) -> Option<bool> {
        self.observing
            .iter()
            .filter(|(reading, source, _)| {
                *reading == subject
                    && (!subject.uses_vault_source()
                        || source
                            .as_deref()
                            .zip(vault_url.map(str::trim))
                            .is_some_and(|(source, shown)| !shown.is_empty() && source == shown))
            })
            .map(|(_, _, queued)| *queued)
            .reduce(|a, b| a && b)
    }

    /// Whether a reading that failed is a signal now (§3.6): never while the run still
    /// retries (§3.5, [`Self::retrying`]), when the page alone says it, as it waits for the next
    /// try. A reading carries no kind, so it is transient while the run retries; a step's own
    /// kind decides for it ([`Failure::retried`]).
    pub(crate) fn failures_signal(&self) -> bool {
        !self.retrying
    }

    /// A Check's result belongs to the exact plan it read. The page supplies
    /// its current review; a signal without a page supplies the saved plan.
    /// No known plan means no Check result, never a guessed default. Each
    /// comes with the name the page says it under: a Check's own result has
    /// the subject's name, without the `Check: ` its row's name opens with.
    fn results<'a>(&'a self, plan: Option<&'a Plan>) -> impl Iterator<Item = Said<'a>> + 'a {
        self.rows
            .iter()
            .map(|row| Said {
                row,
                name: row.name.as_str(),
                check: false,
                on_row: false,
            })
            .chain(
                self.check
                    .as_ref()
                    .filter(move |check| Some(&check.plan) == plan)
                    .into_iter()
                    .flat_map(|check| check.rows.iter())
                    .map(|row| Said {
                        row,
                        name: row.name.strip_prefix("Check: ").unwrap_or(&row.name),
                        check: true,
                        on_row: false,
                    }),
            )
    }

    /// Failed results applicable to this review, whether or not the worker
    /// still retries them. An unassigned result is about setup itself;
    /// a named row counts only while this plan manages it, but a row Restore
    /// could not put back counts whatever the plan: Restore leaves the groups
    /// it put back as is.
    fn result_failures<'a>(&'a self, plan: Option<&'a Plan>) -> impl Iterator<Item = Failure> + 'a {
        let selection = plan.map_or_else(Selection::none, |plan| plan.selection);
        self.results(plan)
            .filter_map(|result| {
                FailureKind::of_status(&result.row.status).map(|kind| (result, kind))
            })
            .map(move |(result, kind)| {
                let whole_restore = result.name == RESTORE_FINISHED;
                let (restore, name) = result
                    .name
                    .strip_prefix(RESTORE_ROW)
                    .map_or((whole_restore, result.name), |name| (true, name));
                let row = subject_named(name);
                let kind = if !restore && row.is_some_and(|row| self.needs_person(plan, row)) {
                    FailureKind::NeedsPerson
                } else {
                    kind
                };
                Failure {
                    row,
                    restore,
                    whole_restore,
                    check: result.check,
                    kind,
                    retried: !result.check
                        && !restore
                        && kind.retried()
                        && self.retries_by_itself(result.row),
                }
            })
            .filter(move |failure| {
                failure.restore
                    || failure
                        .row
                        .is_none_or(|subject| manages(selection, subject))
            })
    }

    /// The steps the last run could not finish (a stage error, a join the
    /// vault refused, a change that did not land), each with the row it
    /// names, if one: failures, which the signal line and the tab's attention
    /// say as they say a row that failed (§3.6), besides the page's notes,
    /// once nothing tries them again by itself (the run's retries, or a vault try on its own
    /// pause, [`Failure::retried`]) and no row they name needs a person
    /// ([`Failure::failed`]). A step naming a row `plan` no longer manages is
    /// left out. A Check's results are never among them: a Check only reads,
    /// and another Check is its remedy, which the page gives by itself. A Restore that failed
    /// as a whole is the page's alone (§3.4): its headline and note say it beside the button
    /// that tries it again.
    pub(crate) fn failed_steps<'a>(
        &'a self,
        plan: Option<&'a Plan>,
    ) -> impl Iterator<Item = Failure> + 'a {
        self.result_failures(plan)
            .filter(|failure| failure.failed())
    }

    /// Whether the GitHub repositories, which the plan in force manages, need a person (§3.6):
    /// a token the vault has not got, or one GitHub refuses. Their remedy is the vault's, which
    /// tells the page nothing, so a shown page looks for a renewed token by itself
    /// ([`crate::system_state_host::TOKEN_LOOK`]).
    pub(crate) fn github_needs_person(&self) -> bool {
        let repositories = Subject::Setting(Setting::Repositories);
        self.plan.as_ref().is_some_and(|plan| {
            manages(plan.selection, repositories) && self.needs_person(Some(plan), repositories)
        })
    }

    /// Whether `row`'s reading for `plan`'s vault needs a person (§3.6): a step that names it
    /// is theirs, whatever came of the step.
    fn needs_person(&self, plan: Option<&Plan>, row: Subject) -> bool {
        self.observations_for(plan.map(|plan| plan.vault_url.as_str()))
            .any(|observation| {
                observation.subject == row && observation.standing == Standing::NeedsYou
            })
    }

    /// The rows `plan` manages that a run, or a Check of `plan`, could not confirm (§3.2): a
    /// change the run could not confirm, or a row the Check could not read.
    fn unconfirmed<'a>(&'a self, plan: Option<&'a Plan>) -> impl Iterator<Item = Subject> + 'a {
        let selection = plan.map_or_else(Selection::none, |plan| plan.selection);
        self.results(plan)
            .filter(|result| {
                result.row.status == "unverified"
                    || (result.check && matches!(result.row.status.as_str(), "error" | "failed"))
            })
            .filter_map(|result| subject_named(result.name))
            .filter(move |subject| manages(selection, *subject))
    }

    /// Whether aterm tries `row`, a failed result, again by itself (§3.5): it is transient
    /// ([`FailureKind::of_status`]), and the words it carries say so (on its pause, or, a vault
    /// try that failed twice, less often, [`TRIED_TWICE`]), or the run's worker goes on.
    /// Meanwhile it counts as waiting, calm, and Set up now is no remedy the page offers for it
    /// ([`Said::words`]).
    fn retries_by_itself(&self, row: &SetupStatusRow) -> bool {
        FailureKind::of_status(&row.status).is_some_and(FailureKind::retried)
            && (row.detail == RETRIES_NOTE || row.detail == TRIED_TWICE || self.retrying)
    }

    /// The reason a step the last run could not finish gives for `row` (§3.6): its words, in the
    /// page's terms (`The change did not take.`, `GitHub can't be reached`), which the row keeps
    /// as its note where its reading gives none. Never a Check's (a read, whose note says to
    /// press it again), a Restore's, a whole sentence said alone ([`Said::whole`]), or the words
    /// a vault try carries that say only what comes next ([`RETRIES_NOTE`]). A vault try after two
    /// that failed says why and what comes next in one line ([`TRIED_TWICE`]), which the row keeps.
    fn step_reason<'a>(&'a self, plan: &'a Plan, row: Subject) -> Option<&'a str> {
        self.results(Some(plan))
            .find(|result| {
                !result.check
                    && subject_named(result.name) == Some(row)
                    && matches!(result.row.status.as_str(), "error" | "failed")
                    && !result.whole()
                    && !result.row.detail.trim().is_empty()
                    && result.row.detail != RETRIES_NOTE
            })
            .map(|result| result.row.detail.as_str())
    }

    /// What the stage that changes `subject`'s row says of it while that stage waits
    /// (init-on-by-default design §6.5): parked for its pause, or until Check again, after aterm
    /// stopped in it twice or it took over an hour (a `waiting` stage row in the ledger's
    /// words), still running past the hour in the aterm that runs it (a `running` one), or
    /// resuming after aterm stopped (a `running` or `pending` one). `None` when its stage says none of these.
    fn stage_says(&self, subject: Subject) -> Option<StageSays<'_>> {
        if matches!(subject, Subject::Sync(_)) {
            return None;
        }
        self.rows.iter().find_map(|row| {
            let stage = StageRow::named(&row.name)?;
            if !stage.settings.contains(&subject.setting()) {
                return None;
            }
            match row.status.as_str() {
                "waiting" if row.detail.ends_with(StageRow::PARKED_TAIL) => {
                    Some(StageSays::Parked(row.detail.as_str()))
                }
                "running" if row.detail.ends_with(StageRow::STILL_RUNNING_TAIL) => {
                    Some(StageSays::StillRunning(row.detail.as_str()))
                }
                "running" | "pending" if row.detail == StageRow::RESUMING => {
                    Some(StageSays::Resuming(row.detail.as_str()))
                }
                _ => None,
            }
        })
    }

    /// The notes the page shows under its headline, in the page's own words
    /// (§3.6): first, while a run aterm widened by itself goes, why its groups
    /// changed ([`WIDENED_NOTE`]); then the results an observation cannot say
    /// and someone should know (an action that did not start, a step that
    /// failed or could not be confirmed, a restore that finished), where the
    /// results said alike (a stage's rows, all failed) are one note naming
    /// each row; then one calm line naming every setting that is back as
    /// saved but loads only at the next login (`login`). That line is no
    /// failure: [`Self::failed_steps`], the signal line and the tab's attention
    /// leave it out.
    fn notes(&self, plan: &Plan) -> Vec<String> {
        let mut notes = Vec::new();
        if self.queued {
            notes.push(self.queued_note().to_owned());
        }
        if self.recovered {
            notes.push(self.recovered_note(plan));
        }
        if self.widened && self.running() {
            notes.push(widened_note(plan).to_owned());
        }
        // Each kind of result (whose, how it came out, the words it carries), with the
        // names it is said for, in the order first seen.
        let mut alike: Vec<(Said<'_>, Vec<&str>)> = Vec::new();
        let mut at_login: Vec<String> = Vec::new();
        for mut result in self.results(Some(plan)) {
            // A step naming a row that needs a person is said by that row and the headline,
            // which name the person's remedy (§3.6): a note that it failed, or that aterm tries
            // it again, would say otherwise.
            if !result.check
                && !result.name.starts_with(RESTORE_ROW)
                && subject_named(result.name).is_some_and(|row| self.needs_person(Some(plan), row))
            {
                continue;
            }
            // A calm line the headline or a row says already: no line says a thing twice.
            if result.row.status == NOTED && self.said_elsewhere(plan, &result.row.detail) {
                continue;
            }
            // A step's reason its row keeps as its note is said there: the note says what comes
            // next.
            result.on_row = !result.check
                && subject_named(result.name).is_some_and(|subject| {
                    manages(plan.selection, subject)
                        && row_view_for_source(
                            subject,
                            self,
                            plan.selection,
                            Some(plan.vault_url.as_str()),
                        )
                        .note
                        .as_deref()
                            == Some(result.row.detail.as_str())
                });
            // A vault try after two that failed says why and what comes next on its row: a note
            // would say it again.
            if result.on_row && result.row.detail == TRIED_TWICE {
                continue;
            }
            // A step that failed is the card's (§4.8): its item says what failed, the reason its
            // row keeps, and what aterm does next; and a row a run or a Check could not confirm
            // says so on the row, which the headline names (`Not confirmed: …`). A note would say
            // either again. Only words a failed step's row does not keep are a note's.
            let managed_row =
                subject_named(result.name).is_some_and(|subject| manages(plan.selection, subject));
            let carded = (result.on_row || result.row.detail.is_empty())
                && matches!(result.row.status.as_str(), "error" | "failed")
                && !self.retries_by_itself(result.row);
            if !result.name.starts_with(RESTORE_ROW)
                && managed_row
                && (carded || result.row.status == "unverified")
            {
                continue;
            }
            match result.row.status.as_str() {
                "not started" | "error" | "failed" | "unverified" | "restored" | NOTED => {
                    match alike.iter_mut().find(|(seen, _)| seen.alike(&result)) {
                        Some((_, names)) if !names.contains(&result.name) => {
                            names.push(result.name);
                        }
                        Some(_) => {}
                        None => alike.push((result, vec![result.name])),
                    }
                }
                "login" if !at_login.contains(&result.row.name) => {
                    at_login.push(result.row.name.clone());
                }
                _ => {}
            }
        }
        for (result, names) in alike {
            let managed = names
                .iter()
                .all(|name| subject_named(name).is_some_and(|row| manages(plan.selection, row)));
            notes.push(result.words(&names, self.retries_by_itself(result.row), managed));
        }
        // A reading that failed while the run retries waits for its next try (§3.5): its row
        // keeps its reason, and one line says what comes next, as a step's does.
        if self.retrying {
            let said: Vec<Subject> = self
                .results(Some(plan))
                .filter_map(|result| subject_named(result.name))
                .collect();
            let waiting: Vec<String> = self
                .observed
                .iter()
                .filter(|row| {
                    row.standing == Standing::Failed
                        && manages(plan.selection, row.subject)
                        && !said.contains(&row.subject)
                })
                .map(|row| row.subject.name().to_owned())
                .collect();
            if !waiting.is_empty() {
                notes.push(format!(
                    "{} did not finish this time. {RETRIES_NOTE}",
                    crate::toolchain_words::and_list(&waiting)
                ));
            }
        }
        if !at_login.is_empty() {
            let verb = if at_login.len() == 1 { "comes" } else { "come" };
            notes.push(format!(
                "{} {verb} back at your next login.",
                crate::toolchain_words::and_list(&at_login)
            ));
        }
        notes
    }
}

/// What a stage says of a row it changes while it waits ([`SetupStatus::stage_says`]), in its
/// own words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StageSays<'a> {
    /// Parked after aterm stopped twice in it, or after it took over an hour: the row waits,
    /// calm, for the park's pause, at whose end the run's worker begins the stage again by
    /// itself, or for a person's Check again, which lifts it at once. The page's own reading
    /// lifts nothing ([`crate::CheckBy::Page`]).
    Parked(&'a str),
    /// Past the hour and still running in the aterm that runs it: the row waits, calm, and
    /// nothing offers Check again, which could change nothing while it runs.
    StillRunning(&'a str),
    /// Going again after aterm stopped in it.
    Resuming(&'a str),
}

/// The one calm note of a plan aterm widened by itself (§3.5), from the moment the page shows the
/// widened plan to the end of its run: it heard this Mac's ALab signal (the vault, or vault or
/// fleet membership), so the groups it had left as is are set up now.
pub(crate) const WIDENED_NOTE: &str =
    "This Mac is an ALab Mac, so aterm is setting up its profile.";

/// The calm line while aterm widens a plan an older aterm saved with its permissions alone to the
/// base plan every Mac gets (`aterm_types::macos_setup::BASE_PLAN`), with no ALab signal: from
/// the moment the page shows it to the end of its run, in place of [`WIDENED_NOTE`], which names
/// the ALab profile.
pub(crate) const SYNC_ADDED_NOTE: &str =
    "Every Mac syncs now, so aterm added the Secrets vault and Rust in Spotlight.";

/// The calm note while a person's choice waits for another setup ([`SetupStatus::queued`]).
pub(crate) const QUEUED_CHOICE_NOTE: &str =
    "Another setup is running; aterm applies your choice when it finishes.";

/// The calm note while the page's own Apply of the plan as it stands waits for another setup: nobody
/// chose it ([`SetupStatus::queued_held`]).
pub(crate) const QUEUED_SETUP_NOTE: &str =
    "Another setup is running; aterm sets up this Mac when it finishes.";

/// The calm note while the automatic admission of the groups aterm widens its own plan by (the
/// ALab profile, or the base plan an older aterm's plan lacked) waits for another setup
/// ([`SetupStatus::queued_widen`]).
pub(crate) const QUEUED_WIDEN_NOTE: &str =
    "Another setup is running; aterm adds the rest of this Mac's setup when it finishes.";

/// The calm note while Restore previous settings waits for another setup
/// ([`SetupStatus::queued`]).
pub(crate) const QUEUED_RESTORE_NOTE: &str =
    "Another setup is running; aterm restores your settings when it finishes.";

/// The calm note while the plan in force is the one aterm set up afresh after setting aside a
/// saved setup it could not read ([`SetupStatus::recovered`]), up to what it says aterm kept
/// ([`SetupStatus::recovered_note`]).
pub(crate) const RECOVERED_NOTE: &str = "aterm could not read its saved setup, so it started \
                                         afresh: your Mac settings are left as they are";

impl SetupStatus {
    /// [`RECOVERED_NOTE`], saying what aterm kept set up of the plan it went on with (the plan
    /// in force, else the page's `plan`): the vault and the repositories where it sets them up,
    /// as it does on an ALab Mac, and nothing more where it does not.
    fn recovered_note(&self, plan: &Plan) -> String {
        let selection = self.plan.as_ref().unwrap_or(plan).selection;
        let kept = match (
            selection.contains(Setting::Secrets),
            selection.contains(Setting::Repositories),
        ) {
            (true, true) => ", and the vault and repositories stay set up",
            (true, false) => ", and the vault stays set up",
            (false, true) => ", and the repositories stay set up",
            (false, false) => "",
        };
        format!("{RECOVERED_NOTE}{kept}.")
    }

    /// The worker could not read the plan in force just now and says so in a calm line
    /// ([`STATE_UNREAD_NOTE`], [`calm_setup_row`]): it names no plan.
    fn plan_unread(&self) -> bool {
        self.plan.is_none()
            && self
                .rows
                .iter()
                .any(|row| row.name == SETUP_ROW && row.status == NOTED)
    }

    /// A newer aterm's saved setup is in force ([`NEWER_SETUP_NOTE`]): this aterm can read the
    /// Mac's rows but names no plan, so it chooses nothing and changes nothing (§3.2).
    pub(crate) fn newer(&self) -> bool {
        self.plan.is_none()
            && self.rows.iter().any(|row| {
                row.name == SETUP_ROW && row.status == NOTED && row.detail == NEWER_SETUP_NOTE
            })
    }

    /// Whether the calm line `words` says what the page says elsewhere (§3.6): a newer aterm's
    /// setup, which its headline says ([`NEWER_HEADLINE`]), and aterm's not reading this Mac's
    /// vault settings, a Join refused for them ([`VAULT_UNREAD_NOTE`]) or the launch's Join left
    /// undone by them ([`VAULT_NOT_JOINED_NOTE`]), while the Secrets vault row `plan` shows
    /// keeps that reason as its own note.
    fn said_elsewhere(&self, plan: &Plan, words: &str) -> bool {
        match words {
            NEWER_SETUP_NOTE => self.newer(),
            VAULT_UNREAD_NOTE | VAULT_NOT_JOINED_NOTE => row_view_for_source(
                Subject::Setting(Setting::Secrets),
                self,
                plan.selection,
                Some(&plan.vault_url),
            )
            .note
            .is_some_and(|note| {
                note.trim_end_matches('.') == VAULT_UNREAD_NOTE.trim_end_matches('.')
            }),
            _ => false,
        }
    }

    /// A setup run that did not finish as a whole (no row named), or a launch's setup that could
    /// not start ([`STARTUP_RETRY_NOTE`]), which aterm tries again by itself (§3.5): the page
    /// waits for that try, nothing a person presses changes what comes next, and nothing
    /// signals it.
    fn setup_retried(&self, plan: Option<&Plan>) -> bool {
        self.rows.iter().any(|row| {
            row.name == SETUP_ROW && row.status == NOTED && row.detail == STARTUP_RETRY_NOTE
        }) || self.result_failures(plan).any(|failure| {
            !failure.restore && !failure.check && failure.retried && failure.row.is_none()
        })
    }

    /// The note for the Apply waiting for another setup: a Restore's while `operation` says so, a
    /// choice's for someone's choice, else what aterm does by itself.
    fn queued_note(&self) -> &'static str {
        if self.operation == Some(SetupOperation::Restore) {
            QUEUED_RESTORE_NOTE
        } else if self.queued_widen {
            QUEUED_WIDEN_NOTE
        } else if self.queued_held {
            QUEUED_CHOICE_NOTE
        } else {
            QUEUED_SETUP_NOTE
        }
    }
}

/// Why a plan aterm widened by itself changed (§3.5), for the widened `plan`: [`WIDENED_NOTE`]
/// for the whole profile, which only the ALab signal adds; [`SYNC_ADDED_NOTE`] for the base plan
/// an older aterm's plan lacked.
fn widened_note(plan: &Plan) -> &'static str {
    if plan.selection == Selection::all() {
        WIDENED_NOTE
    } else {
        SYNC_ADDED_NOTE
    }
}

/// The row the page names `name`, if one.
fn subject_named(name: &str) -> Option<Subject> {
    Subject::all()
        .into_iter()
        .find(|subject| subject.name() == name)
}

/// One result the page can say ([`SetupStatus::results`]): its row, the name the page says it
/// under, whether a Check took it, and whether the row it names keeps its words as its note
/// ([`SetupStatus::step_reason`]), which the page's note then leaves out.
#[derive(Clone, Copy)]
struct Said<'a> {
    row: &'a SetupStatusRow,
    name: &'a str,
    check: bool,
    on_row: bool,
}

/// The page's rows for what a Restore did (design §3.4), in System State's own row names,
/// never a preference key, a keyboard or a process (`aterm doctor` lists those): one calm line
/// saying which rows are as they were, which come back at the next login (a relaunch that
/// failed after the write leaves its rows so), which stayed as they are because macOS hid
/// their earlier value, and which are put back once aterm has Full Disk Access; nobody can act
/// on any of those, so none is a failure. Each row Restore could not put back is a failure
/// (`Restore: <row>`), which stays in view whatever the plan and which the headline names; the
/// calm line then no longer says Restore finished.
pub fn restore_rows(outcome: &RestoreOutcome) -> Vec<SetupStatusRow> {
    let failed = outcome_rows(outcome, PutBack::Failed);
    let mut sentences = put_back_sentences(outcome, UNKNOWN_TO_RESTORE);
    if outcome.rows.is_empty() {
        sentences.push("There was nothing to put back.".to_owned());
    }
    if failed.is_empty() {
        sentences.insert(0, format!("{RESTORE_FINISHED} finished."));
    }
    let mut rows = Vec::new();
    if !sentences.is_empty() {
        rows.push(SetupStatusRow {
            name: RESTORE_FINISHED.to_owned(),
            status: "restored".to_owned(),
            detail: sentences.join(" "),
        });
    }
    for setting in failed {
        rows.push(SetupStatusRow {
            name: format!(
                "{RESTORE_ROW}{}",
                setting.map_or("another Mac setting", Setting::name)
            ),
            status: "failed".to_owned(),
            detail: about(
                &[setting],
                [
                    "could not be put back as it was.",
                    "could not be put back as they were.",
                    "could not be put back as they were.",
                ],
            ),
        });
    }
    if outcome.journal_refused {
        rows.push(journal_note());
    }
    rows
}

/// The page's rows for what a setup pass did that its rows' readings cannot say (design §3.4,
/// §3.6), in System State's own row names, never a preference key, a keyboard or a process
/// (`aterm doctor` lists those): each row whose change did not take, a failure the headline
/// names, or could not be confirmed, and one calm line for what the pass's rollback of those
/// changes put back, in a Restore's words.
pub fn pass_rows(outcome: &PassOutcome) -> Vec<SetupStatusRow> {
    let not_back = outcome
        .rolled_back
        .as_ref()
        .map(|rolled| outcome_rows(rolled, PutBack::Failed))
        .unwrap_or_default();
    let mut rows = Vec::new();
    for (setting, how) in &outcome.unfinished {
        rows.push(match how {
            Unfinished::Failed => SetupStatusRow {
                name: setting.name().to_owned(),
                status: "error".to_owned(),
                detail: if not_back.contains(&Some(*setting)) {
                    "The change did not take, and aterm could not put back its value from before."
                } else {
                    "The change did not take."
                }
                .to_owned(),
            },
            Unfinished::Unconfirmed => SetupStatusRow {
                name: setting.name().to_owned(),
                status: "unverified".to_owned(),
                detail: String::new(),
            },
        });
    }
    let Some(rolled) = &outcome.rolled_back else {
        return rows;
    };
    let sentences = put_back_sentences(rolled, UNKNOWN_TO_ROLLBACK);
    if !sentences.is_empty() {
        rows.push(SetupStatusRow {
            name: UNDONE.to_owned(),
            status: "restored".to_owned(),
            detail: format!(
                "aterm put back what it could not finish. {}",
                sentences.join(" ")
            ),
        });
    }
    for setting in not_back {
        let said = setting
            .is_some_and(|setting| outcome.unfinished.contains(&(setting, Unfinished::Failed)));
        if !said {
            rows.push(SetupStatusRow {
                name: setting
                    .map_or("Another Mac setting", Setting::name)
                    .to_owned(),
                status: "error".to_owned(),
                detail: "aterm could not put back its value from before.".to_owned(),
            });
        }
    }
    if rolled.journal_refused {
        rows.push(journal_note());
    }
    rows
}

/// The page's one calm line for a change aterm made and could not log (§3.6): the change went
/// ahead, and nobody can act on it now, so it is no failure. The fact for later is the exceptions
/// digest's ([`journal_refused_entry`]), and the log keeps which changes.
fn journal_note() -> SetupStatusRow {
    SetupStatusRow {
        name: JOURNAL_ROW.to_owned(),
        status: NOTED.to_owned(),
        detail: JOURNAL_NOTE.to_owned(),
    }
}

/// The name of the calm line for changes aterm could not log ([`journal_note`]).
const JOURNAL_ROW: &str = "Change log";

/// What the page says when aterm made a change it could not log.
pub(crate) const JOURNAL_NOTE: &str = "Every change went ahead; aterm could not log some of them.";

/// The status of a calm line the page says as it is, which is no failure.
const NOTED: &str = "noted";

/// The exceptions digest's entry for changes aterm made to this Mac and could not log (§3.6):
/// a fact for later, never an alert, the signal line or the tab's attention. `detail` is
/// `aterm doctor`'s line naming each change and why, which the log keeps.
#[must_use]
pub fn journal_refused_entry(detail: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "journal",
        "Setup changes went unlogged",
        Why::Spent {
            tried: Reason::new("aterm logs each change setup makes to this Mac"),
            reason: Reason::new("some changes went ahead without their log entry"),
        },
        Then::step("Read aterm.log for the changes it names"),
    )
    .log_line(format!("System State: {detail}"))
}

/// Raise [`journal_refused_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_journal_refused(detail: &str) {
    crate::message_inbox::queue_exception(journal_refused_entry(detail));
}

/// The exceptions digest's entry for a launch's setup that has not started for ten minutes,
/// trying a step again by itself (§3.5): a fact for later, never an alert, the signal line or the
/// tab's attention, which the page's calm line ([`STARTUP_RETRY_NOTE`]) leaves dark. `detail`
/// is why the step failed last, which the log keeps.
#[must_use]
pub fn setup_not_started_entry(detail: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "startup",
        "Setup has not started",
        Why::Spent {
            tried: Reason::new("tried to start this Mac's setup for ten minutes"),
            reason: Reason::new("a step keeps failing, and aterm tries it again by itself"),
        },
        Then::step("Read aterm.log for the step it names"),
    )
    .log_line(format!("System State: setup has not started: {detail}"))
}

/// Raise [`setup_not_started_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_setup_not_started(detail: &str) {
    crate::message_inbox::queue_exception(setup_not_started_entry(detail));
}

/// The launch's setup went on: [`setup_not_started_entry`] leaves the exceptions digest.
#[cfg(unix)]
pub fn clear_setup_not_started() {
    crate::message_inbox::queue_exception_clear(setup_not_started_entry("").key());
}

/// The exceptions digest's entry for a saved setup aterm could not read and set aside (§3.5): a
/// fact for later, never an alert; the page says it in one calm line ([`RECOVERED_NOTE`]), and
/// the entry goes with that line ([`clear_setup_set_aside`]). `kept` names the files as they are
/// kept now, which the log keeps.
#[must_use]
pub fn setup_set_aside_entry(kept: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "set-aside",
        "Saved setup set aside",
        Why::Spent {
            tried: Reason::new("read the setup it saved for this Mac"),
            reason: Reason::new(
                "aterm could not read its saved setup and set it aside, so it started afresh",
            ),
        },
        Then::step("Read aterm.log for the files it kept"),
    )
    .log_line(format!(
        "System State: unreadable setup records kept as {kept}"
    ))
}

/// Raise [`setup_set_aside_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_setup_set_aside(kept: &str) {
    crate::message_inbox::queue_exception(setup_set_aside_entry(kept));
}

/// A person chose, or Restore ran, since aterm set its saved setup aside: the page's note is
/// gone, and [`setup_set_aside_entry`] leaves the exceptions digest with it.
#[cfg(unix)]
pub fn clear_setup_set_aside() {
    crate::message_inbox::queue_exception_clear(setup_set_aside_entry("").key());
}

/// The exceptions digest's entry for settings that changed after this setup had them as wanted:
/// setup leaves them as they are (no automatic pass rewrites a row that read back ok), a fact for
/// later, never an alert. Nothing tells who changed them, and the change may be a person's, so
/// the entry asks nothing: it says only how to have aterm set them again, should they want it
/// (turn the group off and on, which forgets the kept keys of the groups it turns on, init design
/// §6.6), and its one navigation opens System State, where each such row reads "Changed since
/// aterm set it". `rows` names them, which the log keeps.
#[must_use]
pub fn setup_changed_since_entry(rows: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "changed-since",
        "Settings changed since setup",
        Why::OnlyYou(Reason::new(
            "settings this Mac's setup had set changed since, and setup leaves them as they are",
        )),
        Then::step(CHANGED_SINCE_STEP),
    )
    .show(aterm_messages::Intent::ReviewVaultSync)
    .log_line(format!(
        "System State: changed since setup set them, left as they are: {rows}"
    ))
}

/// What [`setup_changed_since_entry`] says to do, should the person want aterm to set the
/// changed settings again: never Set up now, which no row changed since aterm set it offers.
pub(crate) const CHANGED_SINCE_STEP: &str =
    "To have aterm set them again, turn their group off and on in System State";

/// Raise [`setup_changed_since_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_setup_changed_since(rows: &str) {
    crate::message_inbox::queue_exception(setup_changed_since_entry(rows));
}

/// A check that reads nothing out of place, or a Restore: [`setup_changed_since_entry`] leaves
/// the exceptions digest.
#[cfg(unix)]
pub fn clear_setup_changed_since() {
    crate::message_inbox::queue_exception_clear(setup_changed_since_entry("").key());
}

/// The key of the exceptions digest's entry an older aterm raised when its update successor
/// left setup unstarted ("Mac setup is unfinished"). No build raises it now: every launch, an
/// update successor's included, starts setup at once (init-on-by-default design §6.1). The
/// digest keeps an open entry across launches, so the key stays for [`clear_setup_unfinished`].
#[cfg(unix)]
fn setup_unfinished_key() -> String {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "unfinished",
        "Mac setup is unfinished",
        Why::OnlyYou(Reason::new(
            "an older aterm left this Mac's setup unstarted",
        )),
        Then::step("Open System State"),
    )
    .key()
}

/// A setup run was admitted, or offered: the entry an older aterm raised when its update
/// successor left setup unstarted leaves the exceptions digest ([`setup_unfinished_key`]).
#[cfg(unix)]
pub fn clear_setup_unfinished() {
    crate::message_inbox::queue_exception_clear(setup_unfinished_key());
}

/// The exceptions digest's entry for aCaffeine, which aterm sets up to keep this Mac awake, that
/// could not keep it awake for ten minutes (its mug cannot start, or macOS refuses its
/// activity), while launchd starts it again and it asks macOS again by itself: a fact for later,
/// never an alert. `why` is the trouble the mug recorded, which the log keeps.
#[must_use]
pub fn keep_awake_failing_entry(why: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "keep-awake",
        "aCaffeine cannot keep this Mac awake",
        Why::Spent {
            tried: Reason::new("kept this Mac awake with aCaffeine for ten minutes"),
            reason: Reason::new("its mug keeps failing, and aterm starts it again by itself"),
        },
        Then::step("Run aterm doctor and read its awake row"),
    )
    .log_line(format!("aCaffeine: not keeping this Mac awake: {why}"))
}

/// Raise [`keep_awake_failing_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_keep_awake_failing(why: &str) {
    crate::message_inbox::queue_exception(keep_awake_failing_entry(why));
}

/// The mug keeps this Mac awake again, or the person turned it off:
/// [`keep_awake_failing_entry`] leaves the exceptions digest.
#[cfg(unix)]
pub fn clear_keep_awake_failing() {
    crate::message_inbox::queue_exception_clear(keep_awake_failing_entry("").key());
}

/// The rows of `outcome` that came out `how`, in its order.
fn outcome_rows(outcome: &RestoreOutcome, how: PutBack) -> Vec<Option<Setting>> {
    outcome
        .rows
        .iter()
        .filter(|(_, put_back)| *put_back == how)
        .map(|(setting, _)| *setting)
        .collect()
}

/// What a Restore says of a row whose earlier value macOS hid: it is left as it is.
const UNKNOWN_TO_RESTORE: [&str; 3] = [
    "stayed as it is: macOS hid its earlier value.",
    "stayed as they are: macOS hid their earlier value.",
    "stayed as they are: macOS hid their earlier values.",
];

/// What a pass's rollback says of a row whose earlier value it could not read: it is left as
/// the pass set it.
const UNKNOWN_TO_ROLLBACK: [&str; 3] = [
    "stayed as aterm set it: its earlier value could not be read.",
    "stayed as aterm set them: their earlier value could not be read.",
    "stayed as aterm set them: their earlier values could not be read.",
];

/// The calm sentences for what was put back (`outcome`), one for each way a row came out
/// that nobody can act on: as it was, back at the next login, left as it is (`unknown`'s
/// words), put back once aterm has Full Disk Access. A key no row of this build writes (an
/// older aterm's backup) is unnamed where it is back, and is another Mac setting otherwise.
fn put_back_sentences(outcome: &RestoreOutcome, unknown: [&str; 3]) -> Vec<String> {
    let named = |how: PutBack| -> Vec<Option<Setting>> {
        outcome_rows(outcome, how)
            .into_iter()
            .filter(Option::is_some)
            .collect()
    };
    [
        (
            named(PutBack::Back),
            ["is as it was.", "are as they were.", "are as they were."],
        ),
        (
            named(PutBack::AtLogin),
            [
                "comes back at your next login.",
                "come back at your next login.",
                "come back at your next login.",
            ],
        ),
        (outcome_rows(outcome, PutBack::Unknown), unknown),
        (
            outcome_rows(outcome, PutBack::Waits),
            [
                "is put back once aterm has Full Disk Access.",
                "are put back once aterm has Full Disk Access.",
                "are put back once aterm has Full Disk Access.",
            ],
        ),
    ]
    .into_iter()
    .filter(|(rows, _)| !rows.is_empty())
    .map(|(rows, words)| about(&rows, words))
    .collect()
}

/// One sentence about `rows`, opening with their names: `words` is what follows for one row,
/// for one row that is a plural thing (Widgets), and for several rows. Each row is named as
/// the page names it, with no article, so every name in the line reads alike.
fn about(rows: &[Option<Setting>], words: [&str; 3]) -> String {
    let names: Vec<String> = rows
        .iter()
        .map(|row| row.map_or("another Mac setting", Setting::name).to_owned())
        .collect();
    let list = crate::toolchain_words::and_list(&names);
    let mut first = list.chars();
    let list: String = first
        .next()
        .map(|c| c.to_uppercase().chain(first).collect())
        .unwrap_or_default();
    let words = match rows {
        [Some(Setting::Widgets)] => words[1],
        [_] => words[0],
        _ => words[2],
    };
    format!("{list} {words}")
}

/// The footer's Restore button, and the name of a Restore's one calm line ([`restore_rows`]):
/// "Restore previous settings finished. …".
const RESTORE_FINISHED: &str = "Restore previous settings";

/// The name of a pass's calm line for what its rollback put back ([`pass_rows`]).
const UNDONE: &str = "Undo";

/// The footer's Restore button while its Restore waits for another setup
/// ([`SetupStatus::queued`]).
const RESTORE_WAITS: &str = "Waiting to restore…";

/// What opens the name of a row Restore could not put back ([`restore_rows`]): `Restore:
/// Widgets`.
const RESTORE_ROW: &str = "Restore: ";

impl Said<'_> {
    /// A calm line ([`restore_rows`], [`pass_rows`], [`calm_setup_row`]), a row Restore could
    /// not put back, a Restore that failed as a whole (its headline says it failed, its note
    /// why) or a setup error that carries its own words, whose words are whole sentences
    /// already, said alone.
    fn whole(&self) -> bool {
        !self.row.detail.is_empty()
            && match self.row.status.as_str() {
                "restored" | NOTED => true,
                "error" | "failed" => {
                    self.name.starts_with(RESTORE_ROW)
                        || self.name == SETUP_ROW
                        || self.name == RESTORE_FINISHED
                }
                _ => false,
            }
    }

    /// Whether `other` goes in the same note as this result, under its own name: they came out
    /// alike, from a run or both from a Check, with the same words left to say (the rows of a
    /// stage that failed as a whole, or rows that each keep their own reason). A whole sentence
    /// is said alone.
    fn alike(&self, other: &Self) -> bool {
        !self.whole()
            && !other.whole()
            && self.check == other.check
            && self.row.status == other.row.status
            && self.said_detail() == other.said_detail()
    }

    /// The words the result carries that the page's note says: none where its row keeps them.
    fn said_detail(&self) -> &str {
        if self.on_row { "" } else { &self.row.detail }
    }

    /// The note's words for `names`, the rows said alike ([`Self::alike`]), in the page's own
    /// terms (§3.6): what came out (`Dock failed.`, `Check again could not confirm GitHub
    /// repositories.`, `Check again did not start.`), then the words the result carries, which
    /// are the page's too: the worker gives the page no process, path or preference key, and
    /// `aterm doctor` and the log keep those. A failed step of the page's own rows that carries
    /// none says what comes next: Set up now tries it again, where `managed` (the page's plan
    /// still manages them), as it starts a setup run that could not go ([`SETUP_ROW`]) again.
    /// A step aterm tries again by itself (`retried`, [`SetupStatus::retries_by_itself`]) is
    /// no failure: it did not finish this time, for the reason it carries if one, and aterm tries
    /// again by itself (§3.5). A reason its rows keep as their notes ([`Self::said_detail`]) is
    /// said there, not here again.
    fn words(&self, names: &[&str], retried: bool, managed: bool) -> String {
        if self.whole() {
            return self.row.detail.clone();
        }
        let detail = self.said_detail();
        let list = crate::toolchain_words::and_list(
            &names
                .iter()
                .map(|name| {
                    if *name == "Check" {
                        "Check again"
                    } else {
                        name
                    }
                })
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        );
        let status = self.row.status.as_str();
        let step = managed || names == [SETUP_ROW];
        let failed_step = step && !self.check && matches!(status, "error" | "failed");
        let retried = failed_step && retried;
        let what = match status {
            _ if retried => format!("{list} did not finish this time."),
            "unverified" if self.check => format!("Check again could not confirm {list}."),
            "not started" => format!("{list} did not start."),
            "error" | "failed" => format!("{list} failed."),
            "unverified" => format!("{list} could not be confirmed."),
            "restored" => format!("{list} finished."),
            other => format!("{list}: {other}."),
        };
        let because = format!("{detail} {RETRIES_NOTE}");
        let then = if retried && !detail.is_empty() && detail != RETRIES_NOTE {
            Some(because.as_str())
        } else if retried {
            Some(RETRIES_NOTE)
        } else if !detail.is_empty() {
            Some(detail)
        } else if failed_step {
            Some(SET_UP_NOW_NOTE)
        } else {
            None
        };
        match then {
            Some(then) => format!("{what} {then}"),
            None => what,
        }
    }
}

/// The headline while rows are read: a Check going, or readings still to come.
const CHECKING: &str = "Checking this Mac…";

/// The headline while every other managed row is settled and the only rows left unread are
/// ones an ordinary refresh withheld (§3.5: another app's data, read only while System State
/// is in front with a person): calm, with no button, claiming neither a failure nor a set-up
/// Mac. The page reads them as soon as a person is at it ([`SetupStatus::owes_a_read`]).
pub(crate) const READ_WHEN_HERE: &str = "Some rows are read while you're at this page";

/// The headline while every group is left as is (§3.3): calm, with no button, since each group
/// turns back on by its own switch.
pub(crate) const EVERYTHING_LEFT_AS_IS: &str = "Everything is left as is";

/// The headline while the first launch's offer waits for the page's own answer, which it gives
/// on its first tick (design revision 4: no review, no countdown, no confirmation, and no wait for
/// a person), or once a busy page is free: the only signal it gives; no diamond, no signal line.
pub(crate) const STARTING_HEADLINE: &str = "Starting this Mac's setup";

/// The headline while a newer aterm's setup is in force ([`SetupStatus::newer`]): calm, and the
/// one line that says why the groups, Set up now and Restore are not this aterm's to press.
pub(crate) const NEWER_HEADLINE: &str = "A newer aterm manages this Mac's setup";

/// The headline while a setup run that did not finish as a whole waits for aterm to try it
/// again by itself (§3.5), and no row waits.
const SETUP_RETRIED: &str = "Waiting to try setup again…";

/// The name of the row for a setup run that could not go as a whole (its worker did not start,
/// a pass failed before its stages): no System State row, and the note's own words.
pub const SETUP_ROW: &str = "Setup";

/// What the page says after `Check again failed.`: the lines keep their last readings meanwhile,
/// and the page checks again by itself, said calmly. The log keeps why.
pub const CHECK_FAILED_NOTE: &str =
    "The lines show what aterm read before; aterm checks again by itself.";

/// What the page says under a Restore that failed as a whole (headlined `Restore previous
/// settings failed`) for a reason the page has no words of its own for. The log keeps why.
pub const RESTORE_FAILED_NOTE: &str = "Press it again to try once more.";

/// What the page says under a Restore that failed as a whole because the settings aterm saved
/// before setup changed them cannot be read: pressing it again reads the same record, so the
/// note names where to learn why instead.
pub const RESTORE_BACKUP_UNREAD_NOTE: &str =
    "aterm cannot read the settings it saved before setup changed them; aterm doctor says why.";

/// What the page says while aterm cannot read what it saved of this Mac's setup (its plan and
/// last run) just now: it reads it again by itself, and sets aside a record that stays
/// unreadable ([`SetupStatus::recovered`]). A calm line ([`calm_setup_row`]); the log keeps
/// why.
pub const STATE_UNREAD_NOTE: &str =
    "aterm could not read its saved setup just now; it reads it again by itself.";

/// What the worker says while a newer aterm's saved setup is in force (one built later, beside
/// this one, which set this Mac up since): this aterm reads none of it and changes nothing of
/// it, and a read again says the same until that aterm, or a person's choice in it, changes it.
/// A calm line ([`calm_setup_row`]) that the page says once, as its headline
/// ([`NEWER_HEADLINE`]); the log keeps why.
pub const NEWER_SETUP_NOTE: &str =
    "A newer aterm set up this Mac; this aterm leaves its setup as it is.";

/// What the page says when a Join was refused because aterm cannot read this Mac's vault settings
/// (a root-owned or newer-format vault configuration): the Join changed nothing, the typed vault
/// stays in its field for a Join once they read, and `aterm secrets status` says why. A calm line
/// ([`calm_setup_row`]), left out while the Secrets vault row says it as its own note; the log
/// keeps the file and the error.
pub const VAULT_UNREAD_NOTE: &str =
    "aterm cannot read this Mac's vault settings; aterm secrets status says why.";

/// What the page says while the launch's answer to its offer, a Join, was admitted without the
/// Join because aterm cannot read this Mac's vault settings: aterm's own permissions and every
/// other group went ahead, the vault was not joined, and the typed vault stays in its field for
/// a Join once they read. A calm line ([`calm_setup_row`]) for as long as the Secrets row reads
/// so, which the page leaves out while that row's own note says why.
pub const VAULT_NOT_JOINED_NOTE: &str = "aterm did not join the vault you typed: it cannot read \
                                         this Mac's vault settings; aterm secrets status says \
                                         why.";

/// What the page says while the launch's setup could not start and aterm tries again by itself
/// (§3.5): a calm line ([`calm_setup_row`]); the log keeps why.
pub const STARTUP_RETRY_NOTE: &str =
    "aterm could not start setup just now; it tries again by itself.";

/// A calm line about setup as a whole, in `words` that are whole sentences in the page's own
/// terms: no failure, so no failure headline, signal line or tab attention (§3.6).
#[must_use]
pub fn calm_setup_row(words: &str) -> SetupStatusRow {
    SetupStatusRow {
        name: SETUP_ROW.to_owned(),
        status: NOTED.to_owned(),
        detail: words.to_owned(),
    }
}

/// What a failed step's note says while the run still tries it again by itself (§3.5): the
/// words the worker's page gives a vault try on its own pause too
/// ([`aterm_types::macos_setup::TRIES_AGAIN_BY_ITSELF`]).
pub(crate) const RETRIES_NOTE: &str = aterm_types::macos_setup::TRIES_AGAIN_BY_ITSELF;

/// What a failed step's note says once the run no longer retries it: the page runs it again by
/// itself, a little later each time (§3.2, revision 4), so nobody has to press anything.
pub(crate) const SET_UP_NOW_NOTE: &str = "aterm tries it again by itself.";

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
    /// The first-run offer's automatic answer, which waits for nobody.
    Automatic,
    /// A person pressed the page.
    Person,
    /// A control client drove the page.
    Client,
}

impl Admission {
    /// Whether an Apply of this admission is what the records cannot give back: a person's press, or
    /// a control client's Choice or Join (`chosen`). Each is unsaved, and may carry a password.
    pub(crate) const fn holds_unsaved_work(self, chosen: bool) -> bool {
        match self {
            Self::Person => true,
            Self::Client => chosen,
            Self::Automatic => false,
        }
    }

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
    /// The launch's offer, answered by the page itself as the plan stands (design revision 4):
    /// a provisional plan stays provisional, and a vault address or password being typed stays
    /// a draft for Join, so no credential goes without a press.
    Offer,
}

/// What keeps the launch's offer from being made again after a handoff
/// ([`SystemStateApp::offer_hold`]).
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OfferHold {
    /// A person's draft in it (a group pressed, a vault address or password typed), which the
    /// successor could not carry: theirs, while it is there.
    Draft,
    /// The page's own answer, a queued page action or a Stop is under way, or the offer is not
    /// the page's to give again: the page settles it by itself.
    Answer,
}

#[derive(Debug)]
pub(crate) enum SystemStateAction {
    /// Submit this plan as the next run, with its vault password once. While
    /// a run is going, the worker stops it at its next step and runs this
    /// plan next (`InitialSetupHandlers::apply`).
    Apply(SetupAnswer, Admission),
    /// Read every row again for this plan, prompt-free, for a press or for the page itself.
    Check(Plan, crate::CheckBy),
    /// Put back the Mac preferences aterm saved.
    Restore,
    /// Ask the running pass to stop at its next step boundary.
    Stop,
    /// A Needs-you row's button (or the headline's) for this grant.
    OpenAccess { access: Access, by_person: bool },
    /// The Secrets vault row's Turn On Sync: this Mac's sync, which the
    /// person turned off, goes on again (`InitialSetupHandlers::sync_on`).
    TurnSyncOn { by_person: bool },
    /// The offer's one press: a person asks for every grant macOS needs them to answer
    /// (`InitialSetupHandlers::ask_access`), and the window's access steps take them in turn.
    AskForAccess { by_person: bool },
    /// The GitHub repositories row's Install…: the terminal tab that installs Apple's Command
    /// Line Tools opens now (`App::press_developer_tools_install`).
    InstallDeveloperTools { by_person: bool },
    /// The password headline's Go to tab: the Mac Setup tab comes to the front
    /// (`App::go_to_developer_tools_tab`).
    GoToToolsTab,
    /// The Apple-window card's Go to installer: Apple's own Command Line Tools installer window,
    /// already open, comes to the front (`App::go_to_developer_tools_installer`). It starts no
    /// install and asks for no permission.
    GoToToolsInstaller,
    /// A Copy Command: the line an admin Mac runs to approve this Mac goes on the clipboard, for
    /// a person's press alone.
    CopyCommand { command: String, by_person: bool },
    /// The Secrets vault row's Join under its phrase field: ALab lets this Mac in with the phrase
    /// a member shows once its person pressed Allow (`InitialSetupHandlers::join_phrase`), its
    /// words as typed, blanks between them made one, and the try they are ([`PhraseTry`]), which
    /// the join's answer names ([`SystemStateApp::phrase_finished`]).
    JoinPhrase(SecretBytes, PhraseTry),
}

/// One try of a phrase typed on the Secrets vault row ([`SystemStateAction::JoinPhrase`]): which
/// words a join's answer is about. Numbered across the process, so a page opened again while an
/// earlier page's try runs never takes that try's answer for one of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhraseTry(u64);

impl PhraseTry {
    /// A try no earlier one of this process was.
    fn next() -> Self {
        static TRIES: AtomicU64 = AtomicU64::new(0);
        Self(TRIES.fetch_add(1, Ordering::Relaxed))
    }
}

/// One text input to a plain field's words (`input`); Return and Escape change none.
fn edit_text(input: &mut TextInputState, event: TextInputEvent) {
    match event {
        TextInputEvent::Commit(text) => input.insert(&text),
        TextInputEvent::Preedit { text, selection } => input.set_preedit(text, selection),
        TextInputEvent::Backspace => input.delete_backward(),
        TextInputEvent::Delete => input.delete_forward(),
        TextInputEvent::Left { extend } => input.move_left(extend),
        TextInputEvent::Right { extend } => input.move_right(extend),
        TextInputEvent::WordLeft { extend } => input.move_word_left(extend),
        TextInputEvent::WordRight { extend } => input.move_word_right(extend),
        TextInputEvent::Home { extend } => input.move_to_start(extend),
        TextInputEvent::End { extend } => input.move_to_end(extend),
        TextInputEvent::KillToEnd => input.kill_to_end(),
        TextInputEvent::KillToStart => input.kill_to_start(),
        TextInputEvent::DeleteWordBackward => input.delete_word_backward(),
        TextInputEvent::DeleteWordForward => input.delete_word_forward(),
        TextInputEvent::SelectAll => input.select_all(),
        TextInputEvent::Undo => {
            input.undo();
        }
        TextInputEvent::Redo => {
            input.redo();
        }
        TextInputEvent::Submit | TextInputEvent::Cancel => {}
    }
}

/// The key of the footer's one button for the grants on offer ([`offered_access`]).
pub(crate) const ASK_FOR_ACCESS: &str = "system-state/access/ask";

/// The grants the footer's one press asks macOS for: those only a person can give while nobody
/// asked for them ([`aterm_types::macos_setup::ACCESS_OFFERED`]). They hold nothing back.
pub(crate) fn offered_access(status: &SetupStatus) -> Vec<Access> {
    status
        .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
        .filter(|seen| seen.note.as_deref() == Some(aterm_types::macos_setup::ACCESS_OFFERED))
        .filter_map(|seen| match seen.subject {
            Subject::Access(access) => Some(access),
            _ => None,
        })
        .collect()
}

/// Every grant on offer now ([`Observation::on_offer`]): those [`offered_access`] asks for, and
/// developer mode left to the person once setup's one Mac password does not turn it on by itself
/// ([`aterm_types::macos_setup::DEBUGGER_NOT_ASKED`], whose note names their routes). None is a
/// permission set up, and none holds anything back: the page reads each `Offered`, its headline
/// says aterm, not the Mac, is set up, and `setup=` names each `;offered:`
/// ([`crate::system_state_access::setup_field`]).
pub(crate) fn on_offer(status: &SetupStatus) -> Vec<Access> {
    status
        .observations_for(status.plan.as_ref().map(|plan| plan.vault_url.as_str()))
        .filter(|seen| seen.on_offer())
        .filter_map(|seen| match seen.subject {
            Subject::Access(access) => Some(access),
            _ => None,
        })
        .collect()
}

/// The key of the Secrets vault row's Turn On Sync button.
const TURN_ON_SYNC: &str = "system-state/row/secrets/sync-on";

/// The Secrets vault row's field for the phrase a Mac in ALab shows once its person presses Allow
/// (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.6 step 5), there while the row reads
/// [`aterm_types::macos_setup::VAULT_ASKING`]: the join runs in the window, so the person types
/// the phrase here and never in a terminal.
pub(crate) const PHRASE_FIELD: &str = "system-state/vault/phrase";

/// The phrase field's Join.
pub(crate) const PHRASE_JOIN: &str = "system-state/vault/phrase-join";

/// The line under the phrase field before a try.
pub(crate) const PHRASE_HELP: &str =
    "Type the three words a Mac in ALab shows once you press Allow on it.";

/// The line under the phrase field once a phrase did not let this Mac in: a phrase is good for
/// one try, so the member's person presses Allow again for a new one. Why it failed goes to the
/// log, never the page.
pub(crate) const PHRASE_REFUSED: &str = "That phrase did not let this Mac in. Press Allow again on a Mac in ALab, then type the new \
     phrase it shows.";

/// The line under the phrase field once a try found this Mac dropped from its vault
/// ([`PhraseOutcome::Dropped`]): no phrase lets its key in, so the line says how it comes back,
/// naming no path (`aterm secrets status` names the folder), and never sends the person to press
/// Allow again.
pub(crate) const PHRASE_DROPPED: &str = "No phrase lets this Mac in: a rotation left it out of \
    ALab. Set its vault folder aside to join again as a new Mac (aterm secrets status says \
    where).";

/// The line under the phrase field once a try reached no Mac in ALab: nothing was asked of the
/// phrase, so the words stay in the field, good for another Join, and the line says what helps.
pub(crate) const PHRASE_NOT_REACHED: &str = "No Mac in ALab answered on this network yet, so \
    the phrase is still good. Make sure the Mac that showed it is awake and on this network, \
    then press Join again.";

/// The line under the phrase field once a try stopped before it said what the phrase came to
/// (its worker ended): the words stay in the field, and Join tries again.
pub(crate) const PHRASE_STOPPED: &str = "The try with this phrase stopped before it finished. \
    The words are still here: press Join to try again.";

/// Whether `status` reads the Secrets row as waiting for input here: it needs the person and
/// carries no note (a note is a code an admin Mac approves, or a step elsewhere).
fn vault_waits_for_input(status: &SetupStatus) -> bool {
    status.observed.iter().any(|row| {
        row.subject == Subject::Setting(Setting::Secrets)
            && matches!(row.standing, Standing::NeedsYou | Standing::Failed)
            && row.note.is_none()
    })
}

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
        Subject::Sync(kind) => match kind {
            SyncKind::Peers => "sync-peers",
            SyncKind::Identity => "sync-identity",
            SyncKind::Fleet => "sync-fleet",
            SyncKind::Accounts => "sync-accounts",
            SyncKind::SharedData => "sync-shared-data",
            SyncKind::Name => "sync-name",
        },
    }
}

/// The muted value of an Access row whose grant is on offer
/// ([`aterm_types::macos_setup::ACCESS_OFFERED`]): nothing granted it yet.
pub(crate) const OFFERED_WORD: &str = "Offered";

/// A row's value, in the words a probe read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RowValue {
    /// `now → wanted`.
    Change { now: String, wanted: String },
    /// One value: read as wanted, or nothing to compare it with.
    One(String),
    /// A useful previous reading, displayed without supplying current authority.
    LastKnown(String),
    /// The read value of a row aterm leaves as is, in muted ink.
    Muted(String),
}

/// One row of the page, decided from its reading and the plan (§3.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RowView {
    pub(crate) subject: Subject,
    /// aterm manages the row: the selected settings need it.
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
    /// The GitHub repositories row's Install… button: Apple's Command Line Tools are missing
    /// and nothing installs them (Low Data Mode holding them back included), so a press opens
    /// their terminal tab (§3.3).
    pub(crate) install_tools: bool,
    /// Where the row stands (`None`: not read yet).
    pub(crate) standing: Option<Standing>,
    /// The row changed after aterm set it and read it back (design §6.6, by whoever changed it):
    /// it differs, and no automatic pass sets it again, so it is no change to make, and it is
    /// not set up either: it counts neither done nor among the changes to make.
    pub(crate) changed_since: bool,
    /// What a screen reader hears for the row.
    pub(crate) label: String,
}

impl RowView {
    /// A managed row that is not yet as wanted (it will change, needs a
    /// person, or failed), unless it changed since aterm set it ([`Self::changed_since`]), and
    /// not waiting ([`Chip::Waiting`]).
    fn to_change(&self) -> bool {
        self.managed
            && !self.changed_since
            && matches!(
                self.standing,
                Some(Standing::Differs | Standing::Waiting | Standing::NeedsYou | Standing::Failed)
            )
            // A row whose step waits for aterm's next try, or for Check again, is no change a
            // press of Set up now makes.
            && self.chip != Some(Chip::Waiting)
    }

    fn settled(&self) -> bool {
        self.managed && self.standing.is_some_and(Standing::settled)
    }

    fn attention(&self) -> bool {
        self.managed && self.standing.is_some_and(Standing::attention)
    }
}

/// Whether the selected settings manage this row. Accessibility belongs to
/// the account sign-outs; the other grants belong to Permissions.
pub(crate) fn manages(selection: Selection, subject: Subject) -> bool {
    match subject {
        Subject::Access(Access::Accessibility) => Access::Accessibility.needed_for(selection),
        Subject::Access(access) => {
            selection.contains(Setting::Permissions) && access.needed_for(selection)
        }
        Subject::Setting(setting) => selection.contains(setting),
        // Account provisioning rides Secrets and is supplied by its live base-layer reader.
        // Other sync categories remain absent until they have an implementation.
        Subject::Sync(SyncKind::Accounts) => selection.contains(Setting::Secrets),
        Subject::Sync(_) => false,
    }
}

/// THE ROW RULE (§3.3, pure): name, value and at most one chip from the
/// row's reading, the plan's selection and the step running now.
pub(crate) fn row_view(subject: Subject, status: &SetupStatus, selection: Selection) -> RowView {
    row_view_for_source(
        subject,
        status,
        selection,
        status.plan.as_ref().map(|plan| plan.vault_url.as_str()),
    )
}

fn row_view_for_source(
    subject: Subject,
    status: &SetupStatus,
    selection: Selection,
    vault_url: Option<&str>,
) -> RowView {
    row_view_as(subject, status, selection, vault_url, false)
}

/// [`row_view_for_source`] before admission when `review` (§3.3): each row
/// shows its current value and proposed change. A row that would need a
/// person once setup runs reads as one that will change, with its probe's note alone and no
/// button: nothing waits on the person until setup runs and asks them.
fn row_view_as(
    subject: Subject,
    status: &SetupStatus,
    selection: Selection,
    vault_url: Option<&str>,
    review: bool,
) -> RowView {
    let mut row = row_view_current(subject, status, selection, vault_url, review, None);
    // History is a display value only: Off/hold/not-needed rules above it stay
    // current, and no old reading supplies a button or settled standing below.
    if !row.managed || matches!(row.value, RowValue::Muted(_)) || status.newer() {
        return row;
    }
    let progress = status.observation_progress(subject, vault_url);
    let checking = progress.is_some();
    let unconfirmed = status.unconfirmed_observations.contains(&subject);
    let reading = status
        .observations_for(vault_url)
        .find(|seen| seen.subject == subject);
    let missing = unconfirmed || reading.is_none_or(|seen| seen.standing == Standing::Unknown);
    let previous = status.last_known_for(vault_url, subject);
    let last_read = !missing
        && reading.is_some_and(|seen| {
            seen.read_at().is_some_and(|at| at != 0)
                && !matches!(seen.standing, Standing::Unknown | Standing::Failed)
                && seen
                    .now
                    .as_deref()
                    .is_some_and(|value| !value.trim().is_empty())
        });
    let progress = if progress == Some(true) {
        "A new reading is queued."
    } else if previous.is_some() || last_read {
        "Checking again…"
    } else {
        "Checking…"
    };
    if missing && let Some(previous) = previous {
        let value = if previous.standing == Standing::AtLogin {
            previous.wanted.clone()
        } else {
            previous.now.clone().unwrap_or_else(|| "—".into())
        };
        row.value = RowValue::LastKnown(value.clone());
        let current_error = !unconfirmed
            && matches!(
                row.chip,
                Some(Chip::Failed | Chip::NeedsYou | Chip::Waiting)
            );
        if !current_error {
            row.chip = previous.standing.settled().then_some(Chip::LastKnown);
            row.standing = Some(Standing::Unknown);
        }
        // The value cell and reserved page caption carry history/checking;
        // adding a note to every open row would move the page during a Check.
        row.note = row.note.or_else(|| {
            row_view_current(
                subject,
                status,
                selection,
                vault_url,
                review,
                Some(previous),
            )
            .note
        });
        let current = if current_error {
            row.label
        } else {
            format!("{}: no current reading", subject.name())
        };
        let latest = if checking {
            progress
        } else {
            "No current reading is available."
        };
        row.label = format!("{current}. Last read: {value}. {latest}");
        if !current_error {
            row.open = None;
            row.turn_on_sync = false;
            row.install_tools = false;
        }
    } else if checking {
        if row.chip == Some(Chip::InPlace) {
            row.chip = Some(Chip::LastKnown);
            row.standing = Some(Standing::Unknown);
            if let RowValue::One(value) = &row.value {
                row.value = RowValue::LastKnown(value.clone());
            }
        }
        if last_read {
            row.label.push_str(", as last read");
        }
        row.label.push_str(&format!(". {progress}"));
    }
    if matches!(
        row.standing,
        Some(Standing::Failed | Standing::Waiting | Standing::NeedsYou | Standing::Differs)
    ) && let Some(previous) = previous
        && let Some(value) = previous.now.as_deref()
    {
        if reading.is_none_or(|seen| seen.now.is_none()) {
            row.value = RowValue::LastKnown(value.to_owned());
        }
        row.label.push_str(&format!(". Previously read: {value}."));
    }
    row
}

fn row_view_current(
    subject: Subject,
    status: &SetupStatus,
    selection: Selection,
    vault_url: Option<&str>,
    review: bool,
    previous: Option<&Observation>,
) -> RowView {
    let name = subject.name();
    let reading = previous.or_else(|| {
        status
            .observations_for(vault_url)
            .find(|observation| observation.subject == subject)
    });
    let wanted = reading.map_or_else(|| subject.wanted().to_owned(), |o| o.wanted.clone());
    let now = reading
        .and_then(|o| o.now.clone())
        .filter(|now| !now.trim().is_empty());
    let standing = reading.map(|o| o.standing);
    let group_set_up = selection.contains(subject.setting());
    let managed = manages(selection, subject);
    let read = now.clone().unwrap_or_else(|| "—".to_owned());
    // A value's middle dot (`Control · until login`) marks it for the eye; a screen reader
    // hears the words alone.
    let spoken_read = now
        .as_deref()
        .map_or_else(|| "not read".to_owned(), |now| now.replace(" · ", " "));
    // THE ROW'S NOTE: the short line a probe gives (an approval code, what a
    // waiting row waits on), else, where the row needs a person, is not
    // needed or was opted out of, its description: the macOS step a person
    // takes, why the row is not needed, the
    // person's opt-out. A row that differs or waits shows its probe's note
    // alone: its value names the part that differs, and the description a
    // planned or blocked engine row carries is the engine's own line
    // (`absent → true`, a token file's mode), which `aterm init` prints. A
    // waiting row is never without one ([`WAITING_NOTE`]).
    let explained = || reading.and_then(|o| o.note.clone().or_else(|| o.detail.clone()));
    let mut note = reading.and_then(|o| o.note.clone());
    if !group_set_up && !managed {
        return RowView {
            subject,
            managed,
            value: RowValue::Muted(read),
            chip: Some(Chip::LeftAsIs),
            note: None,
            open: None,
            turn_on_sync: false,
            install_tools: false,
            standing,
            changed_since: false,
            label: format!("{name}: {spoken_read}, left as is"),
        };
    }
    if !managed || standing == Some(Standing::NotNeeded) {
        // A reading taken while the row was still managed explains nothing
        // here; one that says the row is not needed says why. A grant on offer is neither
        // needed nor not: it is offered, its note saying how ([`Observation::on_offer`]).
        let note = (standing == Some(Standing::NotNeeded))
            .then(explained)
            .flatten();
        let offered = reading.is_some_and(Observation::on_offer);
        return RowView {
            subject,
            managed,
            value: RowValue::Muted(if offered { OFFERED_WORD } else { "Not needed" }.to_owned()),
            chip: None,
            note: note.filter(|note| !note.trim().is_empty()),
            open: None,
            turn_on_sync: false,
            install_tools: false,
            standing,
            changed_since: false,
            label: if offered {
                format!("{name}: offered")
            } else {
                format!("{name}: not needed")
            },
        };
    }
    let busy = status.running.contains(&subject.setting())
        && match subject {
            Subject::Access(access) => status.access_step.is_none_or(|step| step == access),
            Subject::Setting(_) => true,
            // A Sync row rides Secrets, so it is busy whenever that stage runs.
            Subject::Sync(_) => true,
        };
    // A row that will change changes what the Mac has after the next login: a value live only
    // until then is no from-side, and the value saved for that login is (`Caps Lock →
    // Control`, never `Control · until login → Control`).
    let from = match now
        .as_deref()
        .map(aterm_types::macos_setup::live_until_login)
    {
        Some(Some(_)) => reading
            .and_then(|o| o.after_login.clone())
            .filter(|value| !value.trim().is_empty()),
        _ => now.clone(),
    };
    let spoken_from = from.clone().unwrap_or_else(|| "not read".to_owned());
    let from = from.unwrap_or_else(|| "—".to_owned());
    let change = || RowValue::Change {
        now: from.clone(),
        wanted: wanted.clone(),
    };
    // The review reads a row that will need a person as one that will change (§3.3).
    let standing = match standing {
        Some(Standing::NeedsYou)
            if subject == Subject::Setting(Setting::Secrets)
                && reading.and_then(|row| row.now.as_deref())
                    == Some(aterm_types::macos_setup::VAULT_ASKING) =>
        {
            Some(Standing::Waiting)
        }
        Some(Standing::NeedsYou) if review => Some(Standing::Differs),
        standing => standing,
    };
    // Apple's Command Line Tools, which the repositories wait on (§3.3): an install running, or
    // aterm's own ask on its way, is a wait that ends by itself, its tab asking for the password
    // needs the person now, and missing tools nothing installs offer Install…. The row reads
    // that wait whatever its own reading says ([`ToolsWait::standing`]).
    let tools = (!review && subject == Subject::Setting(Setting::Repositories))
        .then(|| status.tools_wait())
        .flatten();
    match tools {
        Some(ToolsWait::Installing) => note = Some(INSTALLING_TOOLS_NOTE.to_owned()),
        Some(ToolsWait::Queued) => note = Some(TOOLS_QUEUED_NOTE.to_owned()),
        Some(ToolsWait::LowData) => note = Some(TOOLS_LOW_DATA_NOTE.to_owned()),
        Some(ToolsWait::Coming) => note = Some(TOOLS_COMING_NOTE.to_owned()),
        _ => {}
    }
    let standing = tools.map_or(standing, |wait| Some(wait.standing()));
    // A stage that waits (init-on-by-default design §6.5) says so on the rows it changes that are
    // not yet as wanted, in its own words: parked until Check again, the row waits, calm, which
    // the headline's Check again answers; resuming after aterm stopped, the row says so beside
    // what it changes to. A row that reads as wanted, needs a person or failed keeps its own, as
    // does one changed since aterm set it, which no stage sets again.
    let stage = (!review)
        .then(|| status.stage_says(subject))
        .flatten()
        .filter(|_| {
            matches!(
                standing,
                None | Some(Standing::Unknown | Standing::Differs | Standing::Waiting)
            ) && note.as_deref() != Some(aterm_types::macos_setup::CHANGED_SINCE)
        });
    let standing = match stage {
        Some(StageSays::Parked(words) | StageSays::StillRunning(words)) => {
            note = Some(words.to_owned());
            Some(Standing::Waiting)
        }
        Some(StageSays::Resuming(words)) => {
            if note.as_deref().is_none_or(|note| note.trim().is_empty()) {
                note = Some(words.to_owned());
            }
            standing
        }
        None => standing,
    };
    // Debugger access has no page of its own to open: setup's one early elevation turns
    // developer mode on (`aterm_machines::root_steps`), and Settings ▸ Security changes it.
    let open = match subject {
        Subject::Access(access)
            if standing == Some(Standing::NeedsYou) && access != Access::Debugger =>
        {
            Some(access)
        }
        _ => None,
    };
    // The vault read in place at Sync off is the person's opt-out: its row turns it back on,
    // quietly, unless it reads as what Apply will do, which acts on nothing yet. The base plan's
    // vault read in place not joined, where none answers, has no sync to turn on.
    let turn_on_sync = !review
        && subject == Subject::Setting(Setting::Secrets)
        && standing == Some(Standing::InPlace)
        && wanted == SYNC_OFF;
    // A step the last run said of this row (§3.6): one the page says failed takes the failed
    // chip, though the row reads in place, and one aterm tries again by itself the waiting one,
    // as the headline and the card say them. A step whose row needs a person says nothing of
    // its own: the row's reading says it.
    let plan = Plan {
        selection,
        vault_url: vault_url.unwrap_or_default().to_owned(),
    };
    let step = {
        let said: Vec<Failure> = status
            .result_failures(Some(&plan))
            .filter(|failure| !failure.restore && !failure.check && failure.row == Some(subject))
            .collect();
        if said.iter().any(Failure::failed) {
            Some(Chip::Failed)
        } else if said.iter().any(|failure| failure.retried) {
            Some(retry_chip(busy))
        } else {
            None
        }
    };
    // A row read in place that the last run, or a Check of this plan, could not confirm is no
    // settled row (§3.2): no check mark, and a short note the headline's Show lands on.
    let unconfirmed = step.is_none()
        && matches!(standing, Some(Standing::InPlace | Standing::AtLogin))
        && status
            .unconfirmed(
                vault_url
                    .map(|vault_url| Plan {
                        selection,
                        vault_url: vault_url.to_owned(),
                    })
                    .as_ref(),
            )
            .any(|row| row == subject);
    let (settled_chip, settled_words) = if unconfirmed {
        (None, "not confirmed")
    } else {
        (Some(Chip::InPlace), "in place")
    };
    // A row changed since aterm set it (design §6.6), read as it is now below, while a pass
    // runs its stage too, since that pass leaves it as it is.
    let changed_since = standing == Some(Standing::Differs)
        && note.as_deref() == Some(aterm_types::macos_setup::CHANGED_SINCE);
    let (value, chip, label) = match standing {
        None => (
            RowValue::One("—".to_owned()),
            None,
            format!("{name}: not read yet"),
        ),
        // A reading that ended without an answer says why in its note (`This reading did not
        // finish. Check again.`); one with none is still to come.
        Some(Standing::Unknown) => {
            let label = if note.is_some() {
                format!("{name}: not read")
            } else {
                format!("{name}: not read yet")
            };
            (RowValue::One("—".to_owned()), None, label)
        }
        // A row read as wanted shows the value read, never the wanted one in
        // its place: with nothing read it is `—` beside the check.
        Some(Standing::InPlace) => match now {
            // A grant the person opted out of reads in place at what they
            // chose, not at aterm's wanted value: its note says so.
            Some(value) => {
                if matches!(subject, Subject::Access(_)) && wanted != subject.wanted() {
                    note = explained();
                }
                let label = format!("{name}: {value}, {settled_words}");
                (RowValue::One(value), settled_chip, label)
            }
            None => (
                RowValue::One("—".to_owned()),
                settled_chip,
                format!("{name}: {settled_words}"),
            ),
        },
        // Set, and in use in full from the next login: the value with the check, and a quiet
        // note saying so (§3.3), never a mark beside the value, which `· until login` is.
        Some(Standing::AtLogin) => {
            note = Some(AT_LOGIN_NOTE.to_owned());
            (
                RowValue::One(wanted.clone()),
                settled_chip,
                format!("{name}: {wanted}, {settled_words}"),
            )
        }
        // A row changed since aterm set it (init design §6.6), by whoever changed it, which
        // nothing tells: no automatic pass sets it again, so it reads its value now, under its
        // note, with no chip saying it will change or that it is in place, while a pass runs its
        // stage too, since that pass leaves it as it is. Turning its group off and on, or Set up
        // now, has aterm set it again.
        Some(Standing::Differs) if changed_since => (
            RowValue::One(from.clone()),
            None,
            format!("{name}: {spoken_from}, changed since aterm set it"),
        ),
        // Asked, and nothing answered (the ALab vault off its LAN): aterm asks again by itself.
        // The row shows what it reads, with no check and no change it promises, calm.
        Some(Standing::Asking) => (
            RowValue::One(from.clone()),
            None,
            format!("{name}: {spoken_from}, {ASKS_AGAIN}"),
        ),
        // A row that waits on something its note names (a vault to answer, a member's Allow
        // recorded, the Command Line Tools installing, Check again for a stage aterm parked) is
        // calm: `Waiting`, never a change a press makes, until its step runs.
        Some(Standing::Waiting) if !busy => (
            change(),
            Some(Chip::Waiting),
            format!("{name}: {spoken_from}, waiting to change to {wanted}"),
        ),
        // The probe's own note only (set above): never the description.
        Some(Standing::Differs | Standing::Waiting) => {
            let chip = Chip::WillChange { busy };
            (
                change(),
                Some(chip),
                format!(
                    "{name}: {spoken_from}, {} {wanted}",
                    if busy {
                        "changing now to"
                    } else {
                        "will change to"
                    }
                ),
            )
        }
        Some(Standing::NeedsYou) => {
            note = match tools {
                Some(ToolsWait::Password) => Some(TOOLS_PASSWORD_NOTE.to_owned()),
                Some(ToolsWait::Recovered) => Some(TOOLS_RECOVERED_NOTE.to_owned()),
                Some(ToolsWait::AppleWindow) => Some(APPLE_WINDOW_NOTE.to_owned()),
                Some(ToolsWait::NotAdmin) => Some(TOOLS_NOT_ADMIN_NOTE.to_owned()),
                // Install… says what to do; the note says why, or why the last press opened
                // nothing.
                Some(ToolsWait::Missing { refused }) => Some(
                    refused
                        .map_or(TOOLS_NEEDED_NOTE, NotOpened::note)
                        .to_owned(),
                ),
                _ => explained(),
            };
            (
                change(),
                Some(Chip::NeedsYou),
                format!("{name}: {spoken_from}, needs you"),
            )
        }
        // A reading that failed keeps its reason in its note. While the run still retries it
        // waits for the next try (§3.5), as the headline and its line say: the waiting chip,
        // never Failed, unless a step nothing tries again says the row failed.
        Some(Standing::Failed) => {
            note = reading.and_then(|o| o.detail.clone().or_else(|| o.note.clone()));
            if status.failures_signal() || step == Some(Chip::Failed) {
                (
                    change(),
                    Some(Chip::Failed),
                    format!("{name}: {spoken_from}, failed"),
                )
            } else {
                let chip = retry_chip(busy);
                (
                    change(),
                    Some(chip),
                    format!("{name}: {spoken_from}, {}", retry_words(chip)),
                )
            }
        }
        Some(Standing::NotNeeded) => unreachable!("a row not needed returned above"),
    };
    // A step's chip stands for the row whose reading says less (§3.6): a row that needs a
    // person, or whose reading failed, has said its own.
    let (chip, label) = match step {
        Some(said) if !matches!(standing, Some(Standing::NeedsYou | Standing::Failed)) => {
            let words = if said == Chip::Failed {
                "failed"
            } else {
                retry_words(said)
            };
            (Some(said), format!("{name}: {spoken_read}, {words}"))
        }
        _ => (chip, label),
    };
    // A step said of this row keeps its reason on the row where the reading gives none (§3.6):
    // the page's note then says only what comes next, so no line says it twice.
    if step.is_some()
        && note.as_deref().is_none_or(|note| note.trim().is_empty())
        && !matches!(standing, Some(Standing::NeedsYou | Standing::Failed))
    {
        note = status.step_reason(&plan, subject).map(str::to_owned);
    }
    if unconfirmed {
        note = Some(UNCONFIRMED_NOTE.to_owned());
    }
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
        install_tools: matches!(
            tools,
            Some(ToolsWait::Missing { .. } | ToolsWait::LowData | ToolsWait::Coming)
        ),
        standing,
        changed_since,
        label,
    }
}

/// What a screen reader hears of a row whose step aterm tries again by itself (§3.5), after its
/// value, beside the calm `Waiting` chip, whose word it opens with: the next try is aterm's.
const RETRY_WORDS: &str = "waiting for aterm to try again";

/// The chip of a row whose step aterm tries again by itself (§3.5): `Waiting`, calm, until that
/// try runs (`busy`), when it changes now.
const fn retry_chip(busy: bool) -> Chip {
    if busy {
        Chip::WillChange { busy: true }
    } else {
        Chip::Waiting
    }
}

/// What a screen reader hears of a retried row beside `chip` ([`retry_chip`]): the words the chip
/// says, in full.
fn retry_words(chip: Chip) -> &'static str {
    match chip {
        Chip::Waiting => RETRY_WORDS,
        chip => chip.spoken(),
    }
}

/// The note of a waiting row whose probe gave no reason: it is blocked on
/// something, and nothing more can be said of it.
pub(crate) const WAITING_NOTE: &str = "Can't be set up yet";

/// The headline while the Mac Setup tab asks for the person's password
/// ([`ToolsInstall::Password`]): they are needed there now (§3.2), and its Go to tab button
/// ([`GO_TO_TOOLS_TAB`]) brings that tab to the front.
pub(crate) const TOOLS_PASSWORD_HEADLINE: &str = "Enter your password in the Mac Setup tab";

/// The button of a headline that names the Mac Setup tab (its password, or its restored
/// session): that tab comes to the front.
pub(crate) const GO_TO_TOOLS_TAB: &str = "system-state/headline/go-to-tab";

/// The GitHub repositories row's note while the Mac Setup tab asks for the password,
/// which takes the place of its Install… button.
pub(crate) const TOOLS_PASSWORD_NOTE: &str = "Waiting for your password in the Mac Setup tab";

/// The headline for a restored live installer tab: its command may not have started before
/// recovery, so no particular prompt is claimed. Its Go to tab ([`GO_TO_TOOLS_TAB`]) brings that
/// tab to the front.
pub(crate) const TOOLS_RECOVERED_HEADLINE: &str = "Continue in the Mac Setup tab";

/// The recovered tab remains the place to continue, or the person closes it before trying again.
/// It names the tab as its title and the headline do.
pub(crate) const TOOLS_RECOVERED_NOTE: &str =
    "Mac Setup tab restored. Continue there, or close it to try again.";

/// The GitHub repositories row's calm note while `Install…` is queued behind a macOS permission
/// gesture ([`ToolsInstall::Queued`]): nobody needs to press `Install…` again, and the tab opens
/// once that step finishes.
pub(crate) const TOOLS_QUEUED_NOTE: &str =
    "Queued until the current macOS permission step finishes";

/// The GitHub repositories row's calm note while Apple's Command Line Tools install
/// ([`ToolsInstall::Installing`]): nobody needs to act, and the row moves on once they arrive.
pub(crate) const INSTALLING_TOOLS_NOTE: &str = "Installing Apple's Command Line Tools…";

/// The GitHub repositories row's note while Install… shows: why the tools are needed, naming
/// them.
pub(crate) const TOOLS_NEEDED_NOTE: &str =
    "Git and your GitHub repositories need Apple's Command Line Tools.";

/// The label of the card's button that installs the tools ([`INSTALL_TOOLS`]), which it shows
/// and VoiceOver hears.
pub(crate) const INSTALL_TOOLS_LABEL: &str = "Install Command Line Tools…";

/// The GitHub repositories row's note while Apple's own installer window is open, waiting for the
/// person's press or running the install a press started ([`ToolsInstall::AppleWindow`]).
pub(crate) const APPLE_WINDOW_NOTE: &str = "Finish in Apple's installer window";

/// The button of the card's [`APPLE_WINDOW_NOTE`]: Apple's installer window, already open, comes
/// to the front ([`SystemStateAction::GoToToolsInstaller`]). It only goes there: it starts no
/// install and asks for no permission.
pub(crate) const GO_TO_TOOLS_INSTALLER: &str = "system-state/row/repositories/go-to-installer";

/// The GitHub repositories row's note when the tab a press of Install… asked for did not open
/// because System State was no longer in front ([`NotOpened::LeftPage`]); Install… is there again.
pub(crate) const TOOLS_NOT_OPENED_NOTE: &str =
    "The installer did not open: System State was no longer in front.";

/// The GitHub repositories row's calm note while aterm's own ask for setup's Mac password comes
/// by itself ([`ToolsInstall::Coming`]): nobody needs to act, and Install… stays for a person who
/// wants it now.
pub(crate) const TOOLS_COMING_NOTE: &str =
    "aterm asks for your Mac password to install Apple's Command Line Tools by itself";

/// The GitHub repositories row's calm note while Low Data Mode holds the tools' download back
/// ([`ToolsInstall::LowData`]): the tab opens by itself once the mode is off.
pub(crate) const TOOLS_LOW_DATA_NOTE: &str =
    "Waiting for Low Data Mode to be off (System Settings \u{203a} Network)";

/// The GitHub repositories row's note when this account may not install the tools
/// ([`ToolsInstall::NotAdmin`]), in place of Install….
pub(crate) const TOOLS_NOT_ADMIN_NOTE: &str =
    "Only an administrator of this Mac can install Apple's Command Line Tools";

/// The GitHub repositories row's button while Apple's Command Line Tools are missing and nothing
/// installs them: it opens their terminal tab, whose one password prompt installs them.
pub(crate) const INSTALL_TOOLS: &str = "system-state/row/repositories/install";

/// Where the GitHub repositories row stands on Apple's Command Line Tools, which every clone
/// runs (§3.3), once its reading needs a person to install them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolsWait {
    /// Nothing installs them: the row's Install… opens their tab. `refused` says why a person's
    /// last press opened nothing.
    Missing { refused: Option<NotOpened> },
    /// `Install…` is queued behind a macOS permission step holding the dialog slot.
    Queued,
    /// Their tab asks for the person's password.
    Password,
    /// Their live tab was restored; no particular prompt is known.
    Recovered,
    /// An install runs.
    Installing,
    /// Apple's own installer window waits for the person.
    AppleWindow,
    /// Low Data Mode holds the download back; the tab opens by itself once it is off.
    LowData,
    /// This account may not install them: only an administrator of the Mac can.
    NotAdmin,
    /// aterm's own ask comes by itself ([`ToolsInstall::Coming`]).
    Coming,
}

impl ToolsWait {
    /// Whether the wait needs nobody here: one that runs or ends by itself (an install, a queued
    /// press, Low Data Mode, aterm's own ask on its way), or one no press here changes (an
    /// account that may not install them). Calm: no signal line.
    pub(crate) const fn calm(self) -> bool {
        matches!(
            self,
            Self::Installing | Self::Queued | Self::LowData | Self::NotAdmin | Self::Coming
        )
    }

    /// The GitHub repositories row's standing while it waits on the tools, whatever its own
    /// reading says (a managed Git may read in place while Apple's tools are missing): a wait that
    /// ends by itself is [`Standing::Waiting`], one that needs a person (their password, Apple's
    /// window, an administrator, an Install… nothing else will do) is [`Standing::NeedsYou`].
    /// Either way the row is not set up, so neither the headline nor `setup=` says done.
    const fn standing(self) -> Standing {
        match self {
            Self::Installing | Self::Queued | Self::LowData | Self::Coming => Standing::Waiting,
            Self::Missing { .. }
            | Self::Password
            | Self::Recovered
            | Self::AppleWindow
            | Self::NotAdmin => Standing::NeedsYou,
        }
    }
}

impl SetupStatus {
    /// [`ToolsWait`] while the GitHub repositories row waits on Apple's Command Line Tools, which
    /// every clone runs ([`Self::tools_owed`], the window's typed reading), and the plan in force
    /// sets that row up: `None` once they are there and the row has read them since, and for a
    /// plan that leaves the repositories out, which installs no tools and waits on none.
    fn tools_wait(&self) -> Option<ToolsWait> {
        if !self.tools_owed
            || !self
                .plan
                .as_ref()
                .is_some_and(|plan| plan.selection.contains(Setting::Repositories))
        {
            return None;
        }
        Some(match self.developer_tools {
            None => ToolsWait::Missing { refused: None },
            Some(ToolsInstall::NotOpened(why)) => ToolsWait::Missing { refused: Some(why) },
            Some(ToolsInstall::Queued) => ToolsWait::Queued,
            Some(ToolsInstall::Password) => ToolsWait::Password,
            Some(ToolsInstall::Recovered) => ToolsWait::Recovered,
            Some(ToolsInstall::Installing) => ToolsWait::Installing,
            Some(ToolsInstall::AppleWindow) => ToolsWait::AppleWindow,
            Some(ToolsInstall::LowData) => ToolsWait::LowData,
            Some(ToolsInstall::NotAdmin) => ToolsWait::NotAdmin,
            Some(ToolsInstall::Coming) => ToolsWait::Coming,
        })
    }

    /// The GitHub repositories reading for the plan in force, if one.
    pub(crate) fn repositories_reading(&self) -> Option<&Observation> {
        let repositories = Subject::Setting(Setting::Repositories);
        self.observations_for(self.plan.as_ref().map(|plan| plan.vault_url.as_str()))
            .find(|observation| observation.subject == repositories)
    }

    /// [`Self::tools_wait`]: the GitHub repositories row's wait on Apple's Command Line Tools.
    pub(crate) fn repositories_tools_wait(&self) -> Option<ToolsWait> {
        self.tools_wait()
    }
}

/// The note of a row read in place that the last run, or a Check, could not confirm
/// ([`SetupStatus::unconfirmed`]): the headline counts it, and its Show lands here.
pub(crate) const UNCONFIRMED_NOTE: &str = "Not confirmed";

/// The note of a row a run set for macOS to take up in full at the next login
/// (Trackpad scrolling): it reads in place, and says when it is all in effect.
pub(crate) const AT_LOGIN_NOTE: &str = "Fully in effect after your next login";

/// Every row as it reads now and nothing more, in page order (§3.2): while a newer aterm's setup
/// is in force ([`SetupStatus::newer`]) this aterm knows no plan, so no row is managed, wanted
/// or due to change, and none has a chip or a button. A row keeps its reading's own note (what
/// it waits on, why it was not read), and a row not read yet reads `—`.
fn rows_as_they_stand(status: &SetupStatus) -> Vec<RowView> {
    Subject::all()
        .into_iter()
        .map(|subject| {
            let reading = status
                .observed
                .iter()
                .find(|observation| observation.subject == subject);
            let now = reading
                .and_then(|observation| observation.now.clone())
                .filter(|now| !now.trim().is_empty());
            let label = match now.as_deref() {
                Some(now) => format!("{}: {}", subject.name(), now.replace(" · ", " ")),
                None => format!("{}: not read", subject.name()),
            };
            RowView {
                subject,
                managed: false,
                value: RowValue::One(now.unwrap_or_else(|| "—".to_owned())),
                chip: None,
                note: reading
                    .and_then(|observation| observation.note.clone())
                    .filter(|note| !note.trim().is_empty()),
                open: None,
                turn_on_sync: false,
                install_tools: false,
                standing: reading.map(|observation| observation.standing),
                changed_since: false,
                label,
            }
        })
        .collect()
}

/// Every row of the page, in page order.
pub(crate) fn rows(status: &SetupStatus, selection: Selection) -> Vec<RowView> {
    visible_subjects(status)
        .into_iter()
        .map(|subject| row_view(subject, status, selection))
        .collect()
}

/// Live provisioning readers can add evidenced rows without inventing success for an
/// unimplemented sync category.
fn visible_subjects(status: &SetupStatus) -> Vec<Subject> {
    let mut subjects = Subject::all();
    if status
        .observed
        .iter()
        .any(|reading| reading.subject == Subject::Sync(SyncKind::Accounts))
    {
        subjects.push(Subject::Sync(SyncKind::Accounts));
    }
    subjects
}

/// A group's choice, read from the plan's bits (a test's read of the plan).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupChoice {
    SetUp,
    LeaveAsIs,
    /// Some bits set, some not: an older plan's mixed selection.
    Partly,
}

#[cfg(test)]
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
    /// Its Go to tab button brings the Mac Setup tab, which the headline names, to the
    /// front ([`GO_TO_TOOLS_TAB`]).
    pub(crate) go_to_tab: bool,
    /// A stage aterm parked waits for a Check (init-on-by-default design §6.5): the page's own,
    /// after a wait, or the person's Check again for one parked after aterm stopped in it.
    pub(crate) check: bool,
    /// It says the Mac is set up (§3.2: nothing differs in a managed group), or that setup
    /// finished where a row changed since aterm set it, which it names ([`SETUP_FINISHED`]);
    /// `setup=` reads either as done ([`crate::system_state_access::setup_field`]) and names those
    /// rows `;changed:`.
    pub(crate) set_up: bool,
    /// The readable plan manages no rows. This is the person's explicit opt-out, rather than
    /// evidence that the Mac's settings were applied.
    pub(crate) left_as_is: bool,
    /// The managed rows no usable reading stands behind, while every other managed row is
    /// settled, in page order: a read an ordinary refresh withheld ([`READ_WHEN_HERE`]), or one
    /// that ran and said nothing of its row ([`Observation::read_nothing`], `Could not read`).
    /// `setup=` says them `unread:`, neither done nor pending
    /// ([`crate::system_state_access::setup_field`]); a row read back as wanted before is no such
    /// row, whatever that reading's age.
    pub(crate) unread: Vec<Subject>,
    /// The managed rows asked and not answered ([`Standing::Asking`]: the ALab vault off its
    /// LAN), which aterm asks again by itself, while every other managed row is settled, in page
    /// order. The headline then says the rest is set up and that aterm asks again, never that
    /// the Mac is set up, and `setup=` says them `asking:`, neither done nor pending
    /// ([`crate::system_state_access::setup_field`]).
    pub(crate) asking: Vec<Subject>,
}

impl Headline {
    fn line(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            open: None,
            show: None,
            start: false,
            go_to_tab: false,
            check: false,
            set_up: false,
            left_as_is: false,
            unread: Vec::new(),
            asking: Vec::new(),
        }
    }

    /// [`Self::waiting`] for `waiting`, where a step aterm parked (init-on-by-default design
    /// §6.5) offers Check again alone: its rows say why in the stage's words, the worker begins
    /// it again by itself once its pause is over, and a press of Check again lifts the park at
    /// once. Nothing signals: no diamond, no signal line.
    fn waiting_on(status: &SetupStatus, waiting: &[Subject]) -> Self {
        let parked = waiting
            .iter()
            .any(|subject| matches!(status.stage_says(*subject), Some(StageSays::Parked(_))));
        if parked {
            Self {
                show: None,
                check: true,
                ..Self::waiting(waiting)
            }
        } else {
            Self::waiting(waiting)
        }
    }

    /// A headline that sends the person to the Mac Setup tab, with Go to tab.
    fn tools_tab(text: &str) -> Self {
        Self {
            go_to_tab: true,
            ..Self::line(text)
        }
    }

    /// What the worker is busy with, said before anything a reading says:
    /// `None` when it is idle.
    fn busy(status: &SetupStatus, rows: &[RowView]) -> Option<Self> {
        if !status.busy {
            return None;
        }
        if status.queued {
            return Some(Self::queued(status));
        }
        Some(Self::line(match status.operation {
            None => {
                let managed = rows.iter().filter(|row| row.managed).count();
                let done = rows.iter().filter(|row| row.settled()).count();
                format!("Setting up this Mac… {done} of {managed}")
            }
            Some(SetupOperation::Check) => CHECKING.to_owned(),
            Some(SetupOperation::Restore) => "Restoring previous settings…".to_owned(),
        }))
    }

    /// The headline while an Apply waits for another setup ([`SetupStatus::queued`]): a person's
    /// Restore, someone's choice, or what nobody chose: the ALab profile aterm adds by itself
    /// ([`SetupStatus::queued_widen`]) or the plan as it stands.
    fn queued(status: &SetupStatus) -> Self {
        Self::line(if status.operation == Some(SetupOperation::Restore) {
            "Waiting to restore previous settings…"
        } else if status.queued_widen {
            "Waiting to add this Mac's ALab profile…"
        } else if status.queued_held {
            "Waiting to apply your choice…"
        } else {
            "Waiting to set up this Mac…"
        })
    }

    /// The headline while the page does not know the worker's plan yet: it
    /// cannot say which rows aterm manages, so it says only what is going.
    pub(crate) fn before_plan(status: &SetupStatus) -> Self {
        if status.busy && status.queued {
            return Self::queued(status);
        }
        match (status.busy, status.operation) {
            (true, None) => Self::line("Setting up this Mac…"),
            (true, Some(SetupOperation::Restore)) => Self::line("Restoring previous settings…"),
            (false, _) => Self::restore_failed(status, None)
                .or_else(|| {
                    status
                        .result_failures(None)
                        .any(|failure| failure.row.is_none() && failure.failed())
                        .then(|| Self::line("Setup needs attention"))
                })
                .or_else(|| {
                    status
                        .setup_retried(None)
                        .then(|| Self::line(SETUP_RETRIED))
                })
                // A newer aterm's setup names no plan this aterm reads, now or later: its own
                // calm line, never a check that does not end.
                .or_else(|| status.newer().then(|| Self::line(NEWER_HEADLINE)))
                .unwrap_or_else(|| Self::line(CHECKING)),
            _ => Self::line(CHECKING),
        }
    }

    /// `1 step waiting: GitHub repositories`, with Show to the first of `waiting` (none: no
    /// headline of its own).
    fn waiting(waiting: &[Subject]) -> Self {
        let count = if waiting.len() == 1 { "step" } else { "steps" };
        let names: Vec<&str> = waiting.iter().map(|subject| subject.name()).collect();
        Self {
            show: waiting.first().copied(),
            ..Self::line(format!(
                "{} {count} waiting: {}",
                waiting.len(),
                names.join(", ")
            ))
        }
    }

    /// The headline of a Restore that could not put a row back (§3.4): `Restore previous
    /// settings failed: Widgets`, showing that row, whose reason is the page's note; or of a
    /// Restore that failed as a whole, `Restore previous settings failed`, whose reason is its
    /// note. `None` when every row Restore reached came out as nobody can change.
    fn restore_failed(status: &SetupStatus, plan: Option<&Plan>) -> Option<Self> {
        if status
            .result_failures(plan)
            .any(|failure| failure.whole_restore)
        {
            return Some(Self::line(format!("{RESTORE_FINISHED} failed")));
        }
        let rows: Vec<Option<Subject>> = status
            .result_failures(plan)
            .filter(|failure| failure.restore)
            .map(|failure| failure.row)
            .collect();
        if rows.is_empty() {
            return None;
        }
        let names: Vec<&str> = rows
            .iter()
            .map(|row| row.map_or("another Mac setting", Subject::name))
            .collect();
        Some(Self {
            show: rows.iter().flatten().next().copied(),
            ..Self::line(format!("{RESTORE_FINISHED} failed: {}", names.join(", ")))
        })
    }
}

/// THE HEADLINE RULE (§3.2, pure).
pub(crate) fn headline(status: &SetupStatus, rows: &[RowView], plan: Option<&Plan>) -> Headline {
    // The Mac Setup tab needs the person (§3.3): at its known password prompt, or
    // continuing its restored session without claiming which prompt it has reached.
    let repositories = Subject::Setting(Setting::Repositories);
    if rows.iter().any(|row| {
        row.subject == repositories && row.managed && row.standing == Some(Standing::NeedsYou)
    }) {
        match status.tools_wait() {
            Some(ToolsWait::Password) => return Headline::tools_tab(TOOLS_PASSWORD_HEADLINE),
            Some(ToolsWait::Recovered) => return Headline::tools_tab(TOOLS_RECOVERED_HEADLINE),
            _ => {}
        }
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
    if let Some(restore) = Headline::restore_failed(status, plan) {
        return restore;
    }
    let steps = Steps::of(status, rows, plan);
    if let Some(first) = steps.failed.first() {
        let failed = &steps.failed;
        let count = if failed.len() == 1 { "step" } else { "steps" };
        let names: Vec<&str> = failed.iter().map(|subject| subject.name()).collect();
        // Set up now is offered only where running the plan again can change the outcome: a
        // transient step the last run tried and could not finish (a change that did not take, a
        // stage that failed, a vault try after two that failed), whose note says Set up now
        // tries it again. A reading that failed by itself names its own remedy in its note, which Set up
        // now cannot give: Show alone takes the person there.
        return Headline {
            show: Some(*first),
            start: failed.iter().any(|subject| steps.rerun.contains(subject)),
            ..Headline::line(format!(
                "{} {count} failed: {}",
                failed.len(),
                names.join(", ")
            ))
        };
    }
    if status
        .result_failures(plan)
        .any(|failure| !failure.restore && failure.failed() && failure.row.is_none())
    {
        // A setup run that could not go (its worker did not start, a pass failed as a whole)
        // is one Set up now starts again, as its note says; a saved setup aterm cannot read
        // says so in its own words, and a Check that failed is pressed again where it is.
        return Headline {
            start: status.rows.iter().any(|row| {
                row.name == SETUP_ROW
                    && matches!(row.status.as_str(), "error" | "failed")
                    && row.detail.is_empty()
                    && !status.retries_by_itself(row)
            }),
            ..Headline::line("Setup needs attention")
        };
    }
    // A step, or a setup run as a whole, that aterm tries again by itself is no failure (§3.5):
    // it waits for that try, as a row that waits on something does, so the headline counts both
    // as waiting, before any change to make, since a run is coming by itself. A run with no row
    // waiting says it waits to try again.
    if !steps.retried.is_empty() || status.setup_retried(plan) {
        let waiting = steps.waiting(rows);
        if waiting.is_empty() {
            return Headline::line(SETUP_RETRIED);
        }
        return Headline::waiting_on(status, &waiting);
    }
    let progress = rows
        .iter()
        .filter_map(|row| {
            status.observation_progress(row.subject, plan.map(|plan| plan.vault_url.as_str()))
        })
        .reduce(|a, b| a && b);
    if let Some(queued) = progress {
        return Headline::line(if queued {
            "Waiting to read this Mac…"
        } else {
            CHECKING
        });
    }
    let past_unread = rows.iter().any(|row| {
        row.managed
            && row.standing == Some(Standing::Unknown)
            && matches!(row.value, RowValue::LastKnown(_))
    });
    // Readings still to come, or a saved setup aterm reads again by itself, whose calm line
    // says so: nothing is claimed of a plan the worker could not read just now.
    if (status.observed.is_empty()
        && !past_unread
        && !rows
            .iter()
            .any(|row| row.managed && status.unconfirmed_observations.contains(&row.subject)))
        || status.plan_unread()
    {
        return Headline::line(CHECKING);
    }
    // Past a person and a failure, a row not yet as wanted differs or waits.
    // Set up now runs the plan again, which changes the rows that differ: the
    // count is a change to make only while one does. Rows that only wait are
    // nothing a press changes: the headline names them, and each row's note
    // says what it waits on.
    // A row changed since aterm set it is left as it is (design §6.6): nothing automatic sets
    // it again, so it is no change to make and lights no Set up now of its own.
    let to_change = rows.iter().filter(|row| row.to_change()).count();
    let differs = rows
        .iter()
        .any(|row| row.managed && !row.changed_since && row.standing == Some(Standing::Differs));
    if differs {
        let changes = if to_change == 1 { "change" } else { "changes" };
        return Headline {
            start: true,
            ..Headline::line(format!("{to_change} {changes} to make"))
        };
    }
    let waiting = steps.waiting(rows);
    if !waiting.is_empty() {
        return Headline::waiting_on(status, &waiting);
    }
    let managed = rows.iter().filter(|row| row.managed).count();
    if managed == 0 {
        // Every group left as is, by the person or by a Cancel an older aterm recorded (§3.3):
        // each turns back on by its own switch, so nothing more is offered here.
        return Headline {
            left_as_is: true,
            ..Headline::line(EVERYTHING_LEFT_AS_IS)
        };
    }
    // A read that ran and said nothing of its row (a grant this process cannot read, a reader
    // that unwound and says so in its note) leaves no check under way for it, so the headline
    // names it and shows it, where its note says to Check again. A read an ordinary refresh
    // withheld never ran, so it failed nothing.
    let withheld = withheld(status, rows, plan);
    let unread: Vec<Subject> = rows
        .iter()
        .filter(|row| {
            row.managed
                && row.standing == Some(Standing::Unknown)
                && (status.unconfirmed_observations.contains(&row.subject)
                    // A read that ran and said nothing of the row, whatever the page shows of an
                    // earlier reading beside it.
                    || status
                        .observations_for(plan.map(|plan| plan.vault_url.as_str()))
                        .any(|seen| seen.subject == row.subject && seen.read_nothing())
                    // A note is a failure only where the row is not showing its last known value
                    // (whose note says so), unless the reading itself carries why.
                    || (row.note.is_some()
                        && (!matches!(row.value, RowValue::LastKnown(_))
                            || status
                                .observations_for(plan.map(|plan| plan.vault_url.as_str()))
                                .any(|seen| {
                                    seen.subject == row.subject
                                        && (seen.note.is_some() || seen.detail.is_some())
                                }))))
        })
        .map(|row| row.subject)
        .filter(|subject| !withheld.contains(subject))
        .collect();
    if let Some(first) = unread.first() {
        let names: Vec<&str> = unread.iter().map(|subject| subject.name()).collect();
        // Every other managed row is settled once no read of one is still to come and nothing is
        // left unconfirmed: these rows, and any withheld, are then unread, neither done nor
        // pending, and nothing waits on them.
        let to_come = rows.iter().any(|row| {
            row.managed
                && matches!(row.standing, None | Some(Standing::Unknown))
                && !withheld.contains(&row.subject)
                && !unread.contains(&row.subject)
        });
        let settled = !to_come && steps.unconfirmed.is_empty();
        return Headline {
            show: Some(*first),
            unread: rows
                .iter()
                .filter(|row| {
                    settled
                        && row.managed
                        && (unread.contains(&row.subject) || withheld.contains(&row.subject))
                })
                .map(|row| row.subject)
                .collect(),
            ..Headline::line(format!("Could not read: {}", names.join(", ")))
        };
    }
    if past_unread {
        return Headline::line("Current reading unavailable");
    }
    // A source change can leave just its managed rows unread. Known changes
    // and waits above stay useful, but unread rows cannot establish success. A row whose read an
    // ordinary refresh withheld waits for a person at the page, not for a read under way: it is
    // said below, once nothing else is left.
    if rows.iter().any(|row| {
        row.managed
            && matches!(row.standing, None | Some(Standing::Unknown))
            && !withheld.contains(&row.subject)
    }) {
        return Headline::line(CHECKING);
    }
    // The headline claims no more than was confirmed: a row whose change, or whose Check, could
    // not be confirmed is counted, and shown, where its note says so; a Check that failed as a
    // whole leaves every row as aterm read it before, which the headline says.
    if let Some(first) = steps.unconfirmed.first() {
        let names: Vec<&str> = steps
            .unconfirmed
            .iter()
            .map(|subject| subject.name())
            .collect();
        return Headline {
            show: Some(*first),
            ..Headline::line(format!("Not confirmed: {}", names.join(", ")))
        };
    }
    // Everything else is settled, and nothing in this process read these rows: no check is under
    // way for them, none failed, and nothing claims them set up (§3.5).
    if !withheld.is_empty() {
        return Headline {
            unread: withheld,
            ..Headline::line(READ_WHEN_HERE)
        };
    }
    // A plan that manages aterm's permissions and nothing beyond the base plan (a non-ALab Mac,
    // its profile left as is, §3.2) names the permissions: beside them it sets up only what every
    // Mac gets, the vault where one answers and Rust in Spotlight, and leaves the person's
    // preferences, Dock, repositories and accounts as is, which `This Mac is set up` would claim.
    let base_plan = rows
        .iter()
        .any(|row| row.managed && row.subject.group() == Group::Access)
        && rows
            .iter()
            .filter(|row| row.managed)
            .all(|row| BASE_PLAN.contains(row.subject.setting()));
    // A row changed since aterm set it is not set up (design §6.6): setup itself finished, and
    // the headline names the row rather than say the Mac is set up.
    let changed: Vec<&str> = rows
        .iter()
        .filter(|row| row.managed && row.changed_since)
        .map(|row| row.subject.name())
        .collect();
    // Grants still on offer are no permissions set up, and hold nothing back: setup itself ended.
    // Beside a row asked again by itself (below) the same choice says what of the rest is set up,
    // so neither headline claims more than the other.
    let (set_up, rest) = if !changed.is_empty() {
        (SETUP_FINISHED, REST_FINISHED)
    } else if !on_offer(status).is_empty() {
        (ATERM_SET_UP, REST_OF_ATERM_SET_UP)
    } else if base_plan {
        ("Permissions are set up", REST_PERMISSIONS_SET_UP)
    } else {
        ("This Mac is set up", REST_SET_UP)
    };
    let as_read = |said: &str| {
        if steps.check_failed {
            format!("{said}, {AS_LAST_READ}")
        } else {
            said.to_owned()
        }
    };
    let set_up = as_read(set_up);
    // A row set for macOS to take up in full at the next login is set, not yet all in use: the
    // headline says what that login brings, as the row's note does ([`AT_LOGIN_NOTE`]).
    let at_login = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::AtLogin))
        .count();
    let at_login = match at_login {
        0 => String::new(),
        1 => format!("; {AT_NEXT_LOGIN_ONE}"),
        n => format!("; {n} {AT_NEXT_LOGIN_MANY}"),
    };
    let changed_since = if changed.is_empty() {
        String::new()
    } else {
        format!("; {} {CHANGED_SINCE_WORDS}", changed.join(", "))
    };
    // A row asked and not answered (the ALab vault off its LAN, [`Standing::Asking`]) is neither
    // done nor a step anyone waits on: the headline names it, says aterm asks again by itself
    // and what of the rest is set up, as it would with no row asked, and never claims the Mac is
    // (H15). Nothing signals. A row changed since aterm set it is no row set up, so beside one
    // the headline says the rest of setup finished, and names it; a grant on offer is no
    // permission set up, so beside one it says the rest of aterm is; and the base plan names the
    // permissions, beside which it sets up only what every Mac gets.
    let asking: Vec<Subject> = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::Asking))
        .map(|row| row.subject)
        .collect();
    if !asking.is_empty() {
        let names: Vec<&str> = asking.iter().map(|subject| subject.name()).collect();
        let rest = as_read(rest);
        return Headline {
            asking,
            ..Headline::line(format!(
                "{}: {ASKS_AGAIN}; {rest}{at_login}{changed_since}",
                names.join(", ")
            ))
        };
    }
    Headline {
        set_up: true,
        ..Headline::line(format!("{set_up}{at_login}{changed_since}"))
    }
}

/// The set-up headline while a row changed since aterm set it: setup finished, which says nothing
/// of the changed rows, and the headline names them ([`CHANGED_SINCE_WORDS`]).
pub(crate) const SETUP_FINISHED: &str = "Setup finished";

/// What a set-up headline says after the names of the rows changed since aterm set them.
pub(crate) const CHANGED_SINCE_WORDS: &str = "changed since aterm set it";

/// What a row asked and not answered says beside its value, and what the headline says of it
/// ([`Standing::Asking`]): nothing is to do, and nothing is promised.
pub(crate) const ASKS_AGAIN: &str = "aterm asks again by itself";

/// What the headline says of every other managed row while one is asked and not answered
/// ([`Headline::asking`]), on a plan wider than the base one with nothing on offer: where the
/// headline with no row asked says This Mac is set up.
pub(crate) const REST_SET_UP: &str = "the rest is set up";

/// [`REST_SET_UP`] while macOS's grants are on offer ([`on_offer`]), as [`ATERM_SET_UP`] says it
/// with no row asked: a grant offered and never given is no permission set up.
pub(crate) const REST_OF_ATERM_SET_UP: &str = "the rest of aterm is set up";

/// [`REST_SET_UP`] on the base plan (§3.2), as its headline with no row asked names the
/// permissions: beside them that plan sets up only what every Mac gets, and leaves the person's
/// preferences, Dock, repositories and accounts as they are, which the rest being set up would
/// claim.
pub(crate) const REST_PERMISSIONS_SET_UP: &str = "permissions are set up";

/// [`REST_SET_UP`] where a row changed since aterm set it, which the headline names after it
/// ([`CHANGED_SINCE_WORDS`]): setup of the rest finished, which says nothing of that row.
pub(crate) const REST_FINISHED: &str = "the rest of setup finished";

/// What a set-up headline adds for one row set for macOS to take up in full at the next login
/// ([`AT_LOGIN_NOTE`] on its row).
pub(crate) const AT_NEXT_LOGIN_ONE: &str = "1 setting takes full effect at your next login";

/// What a set-up headline adds after the count of several such rows.
pub(crate) const AT_NEXT_LOGIN_MANY: &str = "settings take full effect at your next login";

/// The managed rows of `rows` that read nothing but a withheld read ([`Observation::is_withheld`]):
/// no stage's wait stands in for their reading, in page order.
fn withheld(status: &SetupStatus, rows: &[RowView], plan: Option<&Plan>) -> Vec<Subject> {
    rows.iter()
        .filter(|row| row.managed && row.standing == Some(Standing::Unknown))
        .filter(|row| {
            status
                .observations_for(plan.map(|plan| plan.vault_url.as_str()))
                .any(|observation| observation.subject == row.subject && observation.is_withheld())
        })
        .map(|row| row.subject)
        .collect()
}

/// The set-up headline while macOS's grants are on offer ([`offered_access`]).
pub(crate) const ATERM_SET_UP: &str = "aterm is set up";

/// What a set-up headline adds after a Check that failed as a whole: the rows show what aterm
/// read before ([`CHECK_FAILED_NOTE`]).
pub(crate) const AS_LAST_READ: &str = "as last read";

/// Where the steps the last run and Check said stand, against the page's rows (§3.2, §3.6):
/// what the headline names failed, counts as waiting or as not confirmed, and the card holds
/// ([`lines::card_items`]).
struct Steps {
    /// Managed rows that failed: a reading that failed once the run no longer retries, or a step
    /// the run could not finish that nothing tries again by itself ([`Failure::failed`]). A
    /// Check's result is never one: a Check only reads. Nor is a step whose row needs a person,
    /// which says so instead.
    failed: Vec<Subject>,
    /// The failed rows Set up now can change: a transient step the last run tried and could not
    /// finish, whose note says Set up now tries it again.
    rerun: Vec<Subject>,
    /// Managed rows, not failed, with a step aterm tries again by itself, or a reading that
    /// failed while the run still retries (§3.5): each waits for that try.
    retried: Vec<Subject>,
    /// Managed rows, not failed, whose change or Check could not be confirmed.
    unconfirmed: Vec<Subject>,
    /// A Check of this plan failed as a whole: the rows keep what aterm read before.
    check_failed: bool,
}

impl Steps {
    fn of(status: &SetupStatus, rows: &[RowView], plan: Option<&Plan>) -> Self {
        let failures: Vec<Failure> = status
            .result_failures(plan)
            .filter(|failure| !failure.restore)
            .collect();
        let named = |keep: &dyn Fn(&Failure) -> bool| -> Vec<Subject> {
            failures
                .iter()
                .filter(|failure| keep(failure))
                .filter_map(|failure| failure.row)
                .collect()
        };
        let stopped = named(&Failure::failed);
        let retried = named(&|failure| failure.retried);
        let unverified: Vec<Subject> = status.unconfirmed(plan).collect();
        let managed = |keep: &dyn Fn(&RowView) -> bool| -> Vec<Subject> {
            rows.iter()
                .filter(|row| row.managed && keep(row))
                .map(|row| row.subject)
                .collect()
        };
        // A reading that failed waits for the run's next try while the run retries, keeping its
        // reason on its row; once no worker retries the run it failed (§3.5, §3.6).
        let signal = status.failures_signal();
        let failed = managed(&|row| {
            (signal && row.standing == Some(Standing::Failed)) || stopped.contains(&row.subject)
        });
        let retried = managed(&|row| {
            !failed.contains(&row.subject)
                && (retried.contains(&row.subject) || row.standing == Some(Standing::Failed))
        });
        let unconfirmed = managed(&|row| {
            !failed.contains(&row.subject)
                && !retried.contains(&row.subject)
                && unverified.contains(&row.subject)
        });
        Self {
            check_failed: failures
                .iter()
                .any(|failure| failure.check && failure.row.is_none()),
            failed,
            rerun: stopped,
            retried,
            unconfirmed,
        }
    }

    /// The managed rows the headline counts as waiting, in page order: each that waits on
    /// something, and each with a step aterm tries again by itself.
    fn waiting(&self, rows: &[RowView]) -> Vec<Subject> {
        rows.iter()
            .filter(|row| {
                row.managed
                    && (row.standing == Some(Standing::Waiting)
                        || self.retried.contains(&row.subject))
                    && !self.failed.contains(&row.subject)
            })
            .map(|row| row.subject)
            .collect()
    }
}

/// §5.6 wake rule (pure): the setup page owes its own sub-second wake only while a
/// run goes (`running`) on a shown page, whose lockup moves with it, and the hands are
/// off the keys (`hands_off_keys`). Re-reads and offer answers ride the worker's
/// publications, a focus, file or network event, and, only while an open page shows
/// something moving, the status poll, each of which already wakes the loop
/// (`system_state_host`'s `Host::deadline`), so a settled, finished or hidden page owes
/// no animation tick and stands still, and a run whose motion is reduced still owes no
/// idle wake once it ends.
///
/// `hands_off_keys` is the app-wide typing hold ([`crate::hands_off_keys`], a
/// 2.5 s gap): while a key was typed that recently in a focused window, the walk's
/// wake is held, so a motion frame never preempts an input frame (§5.6). A running
/// page's status poll still wakes the loop, so the motion resumes within a poll once the
/// typing pauses.
///
/// The working spinner is not this wake's: it is a moving node ([`crate::native_ui::NodeMotion`]),
/// stepped by the window's own motion tick under the page's motion policy (§5.1, §5.5). The
/// lockup's glide is SCR-3's; until it lands the caret and kitty jump to where the run stands,
/// and this rule holds the idle wake at zero and motion off the typing path.
pub(crate) const fn page_owes_animation_wake(
    running: bool,
    shown: bool,
    hands_off_keys: bool,
) -> bool {
    running && shown && hands_off_keys
}

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
    /// The list's height (its room and the strip under it), its room and
    /// how far it can scroll, as last rendered.
    height: Cell<f32>,
    room: Cell<f32>,
    limit: Cell<f32>,
    /// The revealable keys the last render placed in the list.
    spans: RefCell<Vec<Span>>,
    /// The one line the person opened in this view (§4.1: one at a time; collapsed until they
    /// open it).
    open_line: Option<lines::Line>,
    repository_sort: u8,
    repository_reverse: bool,
    repository_open: std::collections::BTreeSet<String>,
    repositories_open: bool,
    /// A test's view with every line open at once, so a row-level test reads every row the
    /// page can show; the person opens one at a time.
    #[cfg(test)]
    every_line_open: Cell<bool>,
    /// The kitty pose this view's lockup drew last, which it keeps until the next one is baked
    /// ([`kitty::pose_to_draw`]).
    kitty_pose: Cell<Option<aterm_effects::pet_glyphs_gen::PetGlyphId>>,
}

impl SystemStateViewState {
    /// A view with the dev scenario's line open ([`fixture_line`]), so a lab capture sees that
    /// line's rows; `None` opens none.
    pub(crate) fn with_line_open(line: Option<lines::Line>) -> Self {
        Self {
            repositories_open: line == Some(lines::Line::Developer),
            open_line: line,
            ..Self::default()
        }
    }

    /// The line this view has open (a host test's read).
    #[cfg(test)]
    pub(crate) const fn open_line(&self) -> Option<&'static str> {
        match self.open_line {
            Some(line) => Some(line.slug()),
            None => None,
        }
    }

    /// Open every line of this view at once, so a host test reaches every row's controls
    /// without opening each line first.
    #[cfg(test)]
    pub(crate) fn open_every_line(&self) {
        self.every_line_open.set(true);
    }

    /// The list's scroll and its room, as last rendered (a host test's read).
    #[cfg(test)]
    pub(crate) fn list_scroll(&self) -> (f32, f32) {
        (self.scroll.get(), self.room.get())
    }

    /// Scroll the list so `key` shows whole: a note, a group, a row or one
    /// of their controls, the vault's fields too (the password field shows
    /// bullets, never what was typed). `true` when the list moved.
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
        let offset = whole_at(before, self.room.get(), top, bottom).clamp(0.0, self.limit.get());
        self.scroll.set(offset);
        (offset - before).abs() > 0.5
    }

    /// Move the list `steps` steps of `points`, down where `steps` is
    /// positive. A step never moves more than the room shows, so every line
    /// of the list passes through the room, even in a list one row tall
    /// (§3.4); and where a shorter step shows whole a span the room cut and
    /// the list holds whole, the step stops there, with the span at the
    /// list's top going down and, going up, at the room's foot (at the
    /// list's top where it stands taller than the room).
    fn scroll_steps(&self, steps: i32, points: f32) {
        // An edge cuts a span as the page's compile judges it.
        const EDGE: f32 = crate::native_ui::CLIP_EDGE_EPSILON;
        let (height, room) = (self.height.get(), self.room.get());
        let step = points.min(room);
        for _ in 0..steps.unsigned_abs() {
            let offset = self.scroll.get();
            let spans = self.spans.borrow();
            let whole = spans
                .iter()
                .filter(|span| span.bottom - span.top <= height + EDGE);
            let target = if steps > 0 {
                let target = offset + step;
                whole
                    .filter(|span| {
                        span.top > offset + EDGE
                            && span.top < target - EDGE
                            && span.bottom > offset + room + EDGE
                    })
                    .map(|span| span.top)
                    .fold(target, f32::min)
            } else {
                let target = offset - step;
                whole
                    .filter(|span| span.top < offset - EDGE && span.bottom > target + room + EDGE)
                    .map(|span| (span.bottom - room).min(span.top))
                    .fold(target, f32::max)
            }
            .clamp(0.0, self.limit.get());
            drop(spans);
            if (target - offset).abs() < EDGE {
                break;
            }
            self.scroll.set(target);
        }
    }
}

/// The scroll nearest `offset` at which a room `room` tall shows the content
/// from `top` to `bottom` whole, or from its top where it stands taller.
fn whole_at(offset: f32, room: f32, top: f32, bottom: f32) -> f32 {
    if top < offset {
        top
    } else if bottom > offset + room {
        (bottom - room).min(top)
    } else {
        offset
    }
}

/// Acceptance belongs to one submitted password value, independently of which
/// later plan owns the host's admission receipt. It carries no plan authority.
#[derive(Clone, Debug, Default)]
pub(crate) struct PasswordAcceptance(Arc<AtomicBool>);

impl PasswordAcceptance {
    pub(crate) fn accept(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn accepted(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Password text never enters a UI projection, undo history, restore data or a
/// diagnostic String. Its selection addresses graphemes in the masked value.
#[derive(Default)]
struct Password {
    value: Option<SecretBytes>,
    acceptance: Option<PasswordAcceptance>,
    anchor: usize,
    head: usize,
    /// Marked text is never kept, but its presence owns the vault draft.
    composing: bool,
}

impl Password {
    fn text(&self) -> &str {
        if self
            .acceptance
            .as_ref()
            .is_some_and(PasswordAcceptance::accepted)
        {
            return "";
        }
        self.value
            .as_ref()
            .and_then(SecretBytes::as_str)
            .unwrap_or_default()
    }

    fn boundaries(text: &str) -> Vec<usize> {
        text.grapheme_indices()
            .map(|(byte, _)| byte)
            .chain([text.len()])
            .collect()
    }

    fn count(&self) -> usize {
        self.text().graphemes().count()
    }

    fn replace(&mut self, text: SecretBytes) {
        self.composing = false;
        // A worker can accept the old value at any point. Derive the edit's
        // ranges and bytes from one snapshot, then detach its old marker.
        let old = self.text();
        let boundaries = Self::boundaries(old);
        let start = boundaries[self.anchor.min(self.head).min(boundaries.len() - 1)];
        let end = boundaries[self.anchor.max(self.head).min(boundaries.len() - 1)];
        let old = old.as_bytes();
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
        // A late receipt for the old value cannot consume this new draft.
        self.acceptance = None;
        // Insertion can join clusters on either side, as when a combining mark
        // follows a letter or a joiner connects two emoji. Resolve the caret in
        // the resulting value, at the first whole-cluster boundary after it.
        self.head =
            Self::boundaries(self.text()).partition_point(|boundary| *boundary < inserted_end);
        self.anchor = self.head;
    }

    fn input(&mut self, event: TextInputEvent) {
        match event {
            TextInputEvent::Commit(text) => self.replace(SecretBytes::from(text)),
            TextInputEvent::Preedit { text, .. } => {
                self.composing = !text.is_empty();
                drop(SecretBytes::from(text));
            }
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
            TextInputEvent::Delete | TextInputEvent::DeleteWordForward => {
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
            TextInputEvent::Undo | TextInputEvent::Redo | TextInputEvent::Submit => {}
            TextInputEvent::Cancel => self.composing = false,
        }
    }

    fn projection(&self) -> TextInputProjection {
        let count = self.count();
        TextInputProjection {
            text: "•".repeat(count),
            selection: TextSelection {
                anchor: self.anchor.min(count) * "•".len(),
                head: self.head.min(count) * "•".len(),
            },
            preedit: None,
        }
    }

    fn forget_accepted(&mut self) -> bool {
        if !self
            .acceptance
            .as_ref()
            .is_some_and(PasswordAcceptance::accepted)
        {
            return false;
        }
        drop(self.value.take());
        self.acceptance = None;
        self.anchor = 0;
        self.head = 0;
        true
    }

    fn copy_for_submission(&mut self) -> Option<SecretBytes> {
        self.forget_accepted();
        let password = self.value.clone().filter(|value| !value.is_empty());
        if password.is_some() {
            self.acceptance
                .get_or_insert_with(PasswordAcceptance::default);
        }
        password
    }

    /// The typed password, emptying the field: the tests' read of what the
    /// field still holds.
    #[cfg(test)]
    fn take(&mut self) -> Option<SecretBytes> {
        self.forget_accepted();
        self.acceptance = None;
        self.anchor = 0;
        self.head = 0;
        self.composing = false;
        self.value.take().filter(|value| !value.is_empty())
    }
}

/// A plan sent to the worker and not yet durably accepted.
struct Awaiting {
    plan: Plan,
    /// It carried the vault password (Join), which goes once accepted.
    password: Option<PasswordAcceptance>,
    /// The settings of the groups the person changed since the page last took the plan in
    /// force, which the worker applies onto it ([`SetupAnswer::changed`]); `None`: the plan
    /// as sent.
    changed: Option<Selection>,
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
    /// The Secrets vault row's phrase field ([`PHRASE_FIELD`]).
    phrase: TextInputState,
    /// The try that sent this field's phrase to the join, while its answer has not come back.
    phrase_sent: Option<PhraseTry>,
    /// What the last phrase came to ([`PHRASE_REFUSED`], [`PHRASE_NOT_REACHED`]), until the
    /// field is typed into again.
    phrase_came_to: Option<PhraseOutcome>,
    /// The person's own disclosure of the vault fields; `None` follows the row.
    vault_open: Option<bool>,
    status: SetupStatus,
    repositories: Vec<aterm_repository_progress::Progress>,
    request: Option<Request>,
    /// The offer in `request` waits for the page's own answer, which the page gives on its
    /// first tick ([`Self::tick`]): no review, no countdown, no confirmation and no wait for a
    /// person (design revision 4, init-on-by-default design §6.1).
    answer_waits: bool,
    actions: VecDeque<SystemStateAction>,
    /// Whether the gesture being handled is a person's (not a control client's).
    by_person: bool,
    /// What the page does by itself (design revision 4): no press for anything aterm can do.
    by_itself: ByItself,
}

/// The page's own runs and Checks (design revision 4, init-on-by-default design §4.3): changes to
/// make that nothing holds, or a step a run could not finish, start a run at once while no
/// worker goes on with the run by itself ([`SetupStatus::retrying`]); a Check that failed, or
/// left a row unconfirmed, reads again after a minute ([`crate::CheckBy::Page`], which lifts no
/// park). Each that leaves things as they were waits longer before the next
/// ([`by_itself_after`]). None of it is load-bearing: the worker's own pauses, kept on disk, try
/// every unfinished step again with no page open.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ByItself {
    /// Runs the page started since it last had nothing to start.
    runs: u32,
    /// When the next of them may start; `None`: at once.
    run_after: Option<aterm_time::Instant>,
    /// Checks the page started since no Check was owed.
    checks: u32,
    /// When the next of them goes; `None`: none is owed.
    check_after: Option<aterm_time::Instant>,
}

/// How long the page waits before its next try of the same thing by itself, after `tries`
/// (§4.3): 1, 2, 4, 8, then 15 minutes, so a step that keeps failing costs a run a quarter hour
/// at most, and none before the first try.
pub(crate) fn by_itself_after(tries: u32) -> aterm_time::Duration {
    const BY_ITSELF: aterm_types::retry_schedule::Backoff =
        aterm_types::retry_schedule::Backoff::new(
            aterm_time::Duration::from_secs(60),
            aterm_time::Duration::from_secs(15 * 60),
        );
    BY_ITSELF.after(tries)
}

impl SystemStateApp {
    pub(crate) fn text_input(&self, key: &str) -> Option<&TextInputState> {
        match key {
            "system-state/url" => Some(&self.url),
            PHRASE_FIELD => Some(&self.phrase),
            _ => None,
        }
    }

    /// Whether the field `key` shows marked text now, as its projection does
    /// (the password's never does), read without copying its words.
    pub(crate) fn field_composing(&self, key: &str) -> bool {
        match key {
            "system-state/url" => self.url.preedit().is_some(),
            PHRASE_FIELD => self.phrase.preedit().is_some(),
            _ => false,
        }
    }

    /// The field `key`'s projection, its words included: tests read what a field holds through
    /// it, and the window asks [`Self::field_composing`], which copies no words.
    #[cfg(test)]
    pub(crate) fn field_projection(&self, key: &str) -> Option<TextInputProjection> {
        match key {
            "system-state/url" => Some(self.url.projection()),
            "system-state/password" => Some(self.password.projection()),
            PHRASE_FIELD => Some(self.phrase.projection()),
            _ => None,
        }
    }

    pub(crate) fn cancel_preedit(&mut self, key: Option<&UiKey>) -> bool {
        if key.is_some_and(|key| key.as_str() == "system-state/password") {
            return std::mem::take(&mut self.password.composing);
        }
        let input = match key.map(UiKey::as_str) {
            Some("system-state/url") => &mut self.url,
            Some(PHRASE_FIELD) => &mut self.phrase,
            _ => return false,
        };
        if input.preedit().is_none() {
            return false;
        }
        input.cancel_preedit();
        true
    }

    /// Whether the Secrets vault row asks ALab's members to let this Mac in, so its phrase field
    /// shows ([`PHRASE_FIELD`]): a newer aterm's setup joins for this Mac itself.
    pub(crate) fn phrase_asked(&self) -> bool {
        !self.status.newer()
            && self.status.observed.iter().any(|row| {
                row.subject == Subject::Setting(Setting::Secrets)
                    && row.now.as_deref() == Some(aterm_types::macos_setup::VAULT_ASKING)
            })
    }

    /// Whether the phrase field takes words now: it shows, and no phrase is out for a join.
    fn phrase_editable(&self) -> bool {
        self.phrase_asked() && self.phrase_sent.is_none()
    }

    /// The try out for this field's phrase, while its join has not answered.
    #[cfg(test)]
    pub(crate) fn phrase_try(&self) -> Option<PhraseTry> {
        self.phrase_sent
    }

    /// Join with the phrase as typed, once: its words with one blank between each.
    fn join_phrase(&mut self) {
        let phrase = self
            .phrase
            .value()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !self.phrase_editable() || phrase.is_empty() {
            return;
        }
        let sent = PhraseTry::next();
        self.phrase_sent = Some(sent);
        self.phrase_came_to = None;
        self.actions.push_back(SystemStateAction::JoinPhrase(
            SecretBytes::from(phrase),
            sent,
        ));
    }

    /// The line under the phrase field: what to type, that the last phrase let nobody in, that
    /// it reached no Mac in ALab and is still good, or that no phrase lets a dropped Mac in.
    pub(crate) fn phrase_help(&self) -> &'static str {
        match self.phrase_came_to {
            Some(PhraseOutcome::Refused) => PHRASE_REFUSED,
            Some(PhraseOutcome::NotReached) => PHRASE_NOT_REACHED,
            Some(PhraseOutcome::Stopped) => PHRASE_STOPPED,
            Some(PhraseOutcome::Dropped) => PHRASE_DROPPED,
            Some(PhraseOutcome::Joined) | None => PHRASE_HELP,
        }
    }

    /// What the phrase try `sent` came to: once the join answered, a phrase that let this Mac in
    /// or was refused is spent (one try each), as is one a dropped Mac typed, which no Join lets
    /// in, and the field is emptied and wiped; one that
    /// reached no member was asked nothing, and a try that stopped before it said what the phrase
    /// came to may not have sent it, so their words stay for another Join; `None`, a join that
    /// never started, keeps the words too. Only the words that try sent are touched: the answer
    /// to another try (one a page closed since sent, while the person typed new words into this
    /// page, opened again) changes nothing here, so a phrase is wiped only once its own try is
    /// spent.
    pub(crate) fn phrase_finished(
        &mut self,
        sent: PhraseTry,
        outcome: Option<PhraseOutcome>,
    ) -> bool {
        if self.phrase_sent != Some(sent) {
            return false;
        }
        self.phrase_sent = None;
        if let Some(outcome) = outcome {
            self.phrase_came_to = Some(outcome);
            if matches!(
                outcome,
                PhraseOutcome::Joined | PhraseOutcome::Refused | PhraseOutcome::Dropped
            ) {
                self.phrase.wipe();
            }
        }
        true
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
            phrase: TextInputState::new(String::new()),
            phrase_sent: None,
            phrase_came_to: None,
            vault_open: None,
            status: SetupStatus::default(),
            repositories: Vec::new(),
            request: None,
            answer_waits: false,
            actions: VecDeque::new(),
            by_person: false,
            by_itself: ByItself::default(),
        }
    }

    /// Whether the page knows the plan in force (the offer's or the worker's).
    pub(crate) fn plan_known(&self) -> bool {
        self.plan_known
    }

    /// Whether the vault's address and password take words: while nothing runs, and this aterm
    /// could join with them. Before the plan in force is read they take a draft, which Join
    /// sends once it is ([`Self::plan_known`]); while a newer aterm's setup is in force this
    /// aterm joins nothing, so they read as they stand, as its Join does.
    pub(crate) fn vault_editable(&self) -> bool {
        !self.status.busy && !self.status.newer()
    }

    /// The vault address as this Mac's readings name it, for the page of a newer aterm's setup,
    /// which shows it in words: the source the rows were read for, else the address the page
    /// holds, else that none is set.
    pub(crate) fn vault_address_as_it_stands(&self) -> String {
        [
            self.status.observed_vault_url.as_deref(),
            Some(self.url.value()),
        ]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|address| !address.is_empty())
        .unwrap_or("None set")
        .to_owned()
    }

    pub(crate) fn can_offer(&self) -> bool {
        !self.status.busy
            && !self.request.as_ref().is_some_and(Request::pending)
            && self.awaiting.is_none()
            && !self.vault_draft()
            && self.phrase.value().is_empty()
            && self
                .phrase
                .preedit()
                .is_none_or(|preedit| preedit.text.is_empty())
            && self.phrase_sent.is_none()
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
        self.status = self.status.for_new_offer();
        self.request = Some(request);
        self.answer_waits = true;
        true
    }

    /// The calm lines over the groups ([`SetupStatus::notes`]): why a plan aterm widened shows
    /// more ([`widened_note`]) is said there while its run goes.
    fn notes(&self, plan: &Plan) -> Vec<String> {
        self.status.notes(plan)
    }

    /// Take the worker's latest status. Its plan becomes the page's while
    /// nothing the page sent waits for the worker (and no offer is up); a
    /// vault address being typed stays as typed.
    pub(crate) fn publish(&mut self, status: SetupStatus) -> bool {
        self.repositories.clone_from(&status.repositories);
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
        if !status.stoppable() {
            self.stopping = false;
        }
        // A vault that comes to wait for input here (a join password an admin Mac refused)
        // opens its fields again, even where the person closed them on an earlier reading.
        if vault_waits_for_input(&status) && !vault_waits_for_input(&self.status) {
            self.vault_open = None;
        }
        self.status = status;
        // The phrase field hid (the row no longer asks ALab's members): a phrase typed in it is
        // wiped, never kept for a field that is gone.
        if !self.phrase_asked() && !self.phrase.value().is_empty() {
            self.phrase.wipe();
            changed = true;
        }
        changed
    }

    pub(crate) fn take_action(&mut self) -> Option<SystemStateAction> {
        self.actions.pop_front()
    }

    /// Note whose gesture the next event is: the host stamps it before the
    /// reducer runs, from the App's gesture source.
    pub(crate) const fn gesture(&mut self, by_person: bool) {
        self.by_person = by_person;
    }

    /// Whether the offer up still waits for the page's own answer ([`Self::answer_waits`]).
    fn reviewing(&self) -> bool {
        self.pending() && self.answer_waits
    }

    /// A withdrawn offer is let go. An offer that waits for its answer is answered by the page
    /// itself, as the plan stands, at its first tick, whoever is at this Mac, focused, shown or
    /// not, whatever was clicked or typed meanwhile: no review, no countdown, no confirmation and
    /// no wait for a person (init-on-by-default design §6.1),
    /// since only the person stages wait for one (`App::system_state_front_with_a_person`); a
    /// status that says the page is busy holds it until it is not. `true` when the page changed.
    pub(crate) fn tick(&mut self) -> bool {
        self.tick_at(aterm_time::Instant::now())
    }

    /// [`Self::tick`] at `now`, which also starts what the page does by itself ([`ByItself`]).
    pub(crate) fn tick_at(&mut self, now: aterm_time::Instant) -> bool {
        let repository_changed = self.repositories.iter().any(|row| !row.phase.is_finished());
        let before = (self.request.is_some(), self.status.busy, self.reviewing());
        if self
            .request
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            self.request = None;
            self.answer_waits = false;
        }
        if self.reviewing() && !self.status.busy {
            self.apply(Admission::Automatic, Submission::Offer, None);
        } else {
            self.go_on_by_itself(now);
        }
        repository_changed || before != (self.request.is_some(), self.status.busy, self.reviewing())
    }

    /// Start what the page does by itself and is due at `now` ([`ByItself`]): a run of the plan
    /// in force as it stands, as Set up now would, or a Check.
    fn go_on_by_itself(&mut self, now: aterm_time::Instant) {
        if self.wants_start() {
            if self.by_itself.run_after.is_none() && self.last_run_failed() {
                // The run just tried a step it could not finish, its own tries spent: the next
                // waits its turn, so the failure is said before it goes again.
                self.by_itself.runs = 1;
                self.by_itself.run_after = Some(now + by_itself_after(1));
            }
            if self.by_itself.run_after.is_none_or(|after| after <= now) {
                self.by_itself.runs = self.by_itself.runs.saturating_add(1);
                self.by_itself.run_after = Some(now + by_itself_after(self.by_itself.runs));
                // The window's own admission, as its answer to the launch's offer is: the plan
                // as it stands, a provisional plan staying provisional.
                self.apply(
                    Admission::Automatic,
                    Submission::AsItStands,
                    Some(Selection::none()),
                );
                return;
            }
        } else if !self.status.busy && !self.headline().start {
            self.by_itself.runs = 0;
            self.by_itself.run_after = None;
        }
        if self.check_owed() {
            match self.by_itself.check_after {
                None => {
                    self.by_itself.check_after =
                        Some(now + by_itself_after(self.by_itself.checks.saturating_add(1)));
                }
                Some(after) if after <= now && self.can_check() && !self.status.busy => {
                    self.by_itself.checks = self.by_itself.checks.saturating_add(1);
                    self.by_itself.check_after = None;
                    self.check_again(crate::CheckBy::Page);
                }
                Some(_) => {}
            }
        } else if !self.status.busy {
            self.by_itself.checks = 0;
            self.by_itself.check_after = None;
        }
    }

    /// The keys of the card's buttons, in the card's order: where keyboard focus goes when the
    /// item it was on leaves the card (the walk moved on, a step resolved), so it never stays on
    /// a control that is gone.
    pub(crate) fn card_buttons(&self) -> Vec<String> {
        let rows = page::page_rows(self);
        lines::card_items(self, &rows)
            .into_iter()
            .filter_map(|item| item.button.map(|button| button.key))
            .collect()
    }

    /// When the page next does something by itself, for the window's wake: `None` while nothing
    /// is due.
    pub(crate) fn next_by_itself(&self) -> Option<aterm_time::Instant> {
        let run = self.by_itself.run_after.filter(|_| self.wants_start());
        let check = self.by_itself.check_after.filter(|_| self.check_owed());
        run.into_iter().chain(check).min()
    }

    /// A hold stands on the plan's run: a person's Stop, of a run or of a choice that waited
    /// ([`SetupStatus::stopped_wait`]), or an older aterm's Cancel. Only then do changes to make
    /// wait for a press: the footer's quiet Set up now (revision 4).
    pub(crate) fn held(&self) -> bool {
        self.stopping
            || self.status.stopped_wait
            || self.status.cancelled
            || self.status.run.is_some_and(|run| !run.allowed)
    }

    /// Whether the plan in force has something a run changes, nothing holds it, and no worker
    /// goes on with the run by itself ([`SetupStatus::retrying`]): the page starts that run
    /// itself ([`Self::go_on_by_itself`]). While a worker goes on, a run the page started would
    /// admit the plan again and lose every step's pause; the worker's next try is the page's.
    fn wants_start(&self) -> bool {
        self.plan_known
            && !self.status.busy
            && !self.status.retrying
            && !self.status.queued
            && !self.status.newer()
            && self.request.is_none()
            && self.awaiting.is_none()
            && !self.held()
            && self.headline().start
    }

    /// Whether the page starts a run by itself at its next tick: the hero says it sets up. A run
    /// that just failed a step waits its turn first ([`Self::last_run_failed`]).
    pub(crate) fn starts_by_itself(&self) -> bool {
        self.wants_start() && self.by_itself.run_after.is_none() && !self.last_run_failed()
    }

    /// Whether the last run tried a step it could not finish, which the card says.
    fn last_run_failed(&self) -> bool {
        let plan = self.reviewed_plan();
        let rows = self.reviewed_rows();
        !Steps::of(&self.status, &rows, Some(&plan))
            .failed
            .is_empty()
    }

    /// Whether a run left things as they were, and the next waits its turn.
    pub(crate) fn retry_waits(&self) -> bool {
        self.wants_start() && (self.by_itself.run_after.is_some() || self.last_run_failed())
    }

    /// Whether a Check is owed: the last one failed as a whole, or a row a run or a Check could
    /// not confirm waits for one. A stage aterm parked (init-on-by-default design §6.5) owes
    /// none: the worker begins it again at the end of its pause, and only a press lifts it
    /// sooner ([`crate::CheckBy`]).
    fn check_owed(&self) -> bool {
        if !self.plan_known || self.status.newer() {
            return false;
        }
        let plan = self.reviewed_plan();
        let rows = self.reviewed_rows();
        let steps = Steps::of(&self.status, &rows, Some(&plan));
        steps.check_failed || !steps.unconfirmed.is_empty()
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
    }

    /// What keeps `request` from being recreated after a handoff: the page's own answer, a queued
    /// page action or a Stop under way, or an offer that is not the page's to give again
    /// ([`OfferHold::Answer`], which the page settles by itself); else a person's draft in it
    /// ([`OfferHold::Draft`], theirs while it is there). `None` for an unanswered offer with
    /// neither, which can be recreated: this answers nothing, and the successor reads the durable
    /// plan and offers it again.
    ///
    /// What the setup worker is doing is not the offer's: a busy status (a run going, a Check)
    /// holds the page's own answer ([`Self::tick_at`]) and never the offer, since a run goes on
    /// through a handoff (cut off at the Commit by no crash, resumed by the successor), and the
    /// work a handoff must not drop holds it by itself (`App::system_state_handoff_pending`: an
    /// Apply the worker has not taken, a Restore, a queued choice). A busy status once made an
    /// untouched offer hold a staged update for as long as the worker ran (from 0.148.0
    /// until UPD1).
    #[cfg(any(unix, test))]
    pub(crate) fn offer_hold(&self, request: &Request) -> Option<OfferHold> {
        let Some(offered) = self.request.as_ref() else {
            return Some(OfferHold::Answer);
        };
        let plan = request.initial();
        if !(offered.same_offer(request)
            && offered.pending()
            && self.answer_waits
            && self.awaiting.is_none()
            && self.actions.is_empty()
            && !self.stopping)
        {
            return Some(OfferHold::Answer);
        }
        self.drafted(plan).then_some(OfferHold::Draft)
    }

    /// Whether a person's draft is in this page's copy of `request`: a group pressed, a vault
    /// address or password typed, whatever the page's own answer is doing. An update past the
    /// page's own answer still holds for it (`App::workstation_setup_handoff_hold`).
    #[cfg(any(unix, test))]
    pub(crate) fn offer_draft(&self, request: &Request) -> bool {
        self.request
            .as_ref()
            .is_some_and(|offered| offered.same_offer(request))
            && self.drafted(request.initial())
    }

    #[cfg(any(unix, test))]
    fn drafted(&self, plan: &Plan) -> bool {
        self.plan != *plan || self.url.value() != plan.vault_url || self.vault_draft()
    }

    /// A vault address or password typed and not joined yet.
    fn vault_draft(&self) -> bool {
        self.password.composing
            || !self.password.text().is_empty()
            || self.url.value().trim() != self.submitted.vault_url
            || self
                .url
                .preedit()
                .is_some_and(|preedit| !preedit.text.is_empty())
    }

    /// The page's plan with the vault address as typed (what Check again
    /// reads).
    pub(crate) fn reviewed_plan(&self) -> Plan {
        let mut plan = self.plan.clone();
        plan.vault_url = self.url.value().trim().to_owned();
        plan
    }

    /// An ordinary reading follows current group choices and the accepted
    /// vault source. A typed address is read only by an explicit Check or Join.
    pub(crate) fn observation_plan(&self) -> Plan {
        Plan {
            vault_url: self.submitted.vault_url.clone(),
            ..self.plan.clone()
        }
    }

    /// A row as the page shows it: for the plan as reviewed, and as what Apply, or Set up now,
    /// will do ([`row_view_as`]) where nothing the person approved runs it yet
    /// ([`Self::reads_as_review`]).
    fn reviewed_row(&self, subject: Subject) -> RowView {
        row_view_as(
            subject,
            &self.status,
            self.plan.selection,
            Some(self.url.value().trim()),
            self.reads_as_review(),
        )
    }

    /// Whether `subject`'s row reads as what setup will do (§3.3): every row while the launch's
    /// offer waits for the page's answer at its first tick (nothing was admitted yet, so nothing
    /// waits on anyone).
    fn reads_as_review(&self) -> bool {
        self.reviewing()
    }

    /// Whether the page's own signals are still (§3.3, §3.6): while the launch's offer waits for
    /// the page's answer at its first tick, its headline is the one signal.
    pub(crate) fn signals_still(&self) -> bool {
        self.reviewing()
    }

    fn reviewed_rows(&self) -> Vec<RowView> {
        visible_subjects(&self.status)
            .into_iter()
            .map(|subject| self.reviewed_row(subject))
            .collect()
    }

    /// The plan in force with the vault address its readings were taken for, while a draft no
    /// reading has read is typed ([`Self::vault_draft`]); else the page's plan as reviewed.
    fn judged_plan(&self) -> Plan {
        let mut plan = self.reviewed_plan();
        if let Some(observed) = self.status.observed_vault_url.as_deref().map(str::trim)
            && self.vault_draft()
            && observed != plan.vault_url
        {
            observed.clone_into(&mut plan.vault_url);
        }
        plan
    }

    /// The rows `plan` reads ([`Self::judged_plan`]).
    fn judged_rows(&self, plan: &Plan) -> Vec<RowView> {
        visible_subjects(&self.status)
            .into_iter()
            .map(|subject| {
                row_view_for_source(subject, &self.status, plan.selection, Some(&plan.vault_url))
            })
            .collect()
    }

    /// The page's headline (§3.2): before the plan is known, only what is going. Nothing reads
    /// a typed vault address until Join or Check again, so while one leaves rows unread the
    /// headline says what the readings of the vault in force say, never that a check is going
    /// when none is; a Check's result still belongs to its exact plan alone.
    pub(crate) fn headline(&self) -> Headline {
        if self.reviewing() {
            if self.status.busy {
                return Headline::before_plan(&self.status);
            }
            return Headline::line(STARTING_HEADLINE);
        }
        if !self.plan_known {
            return Headline::before_plan(&self.status);
        }
        let plan = self.reviewed_plan();
        let line = headline(&self.status, &self.reviewed_rows(), Some(&plan));
        let in_force = self.judged_plan();
        if in_force == plan || self.status.busy || line.text != CHECKING {
            return line;
        }
        let status = SetupStatus {
            check: None,
            ..self.status.clone()
        };
        headline(&status, &self.judged_rows(&in_force), Some(&in_force))
    }

    /// An unadmitted row offers no Open ([`Self::reads_as_review`]).
    pub(crate) fn can_open_access(&self, access: Access) -> bool {
        self.plan_known && self.reviewed_row(Subject::Access(access)).open == Some(access)
    }

    /// The tab's attention diamond (§3.6): a managed row needs a person, or
    /// failed and the run no longer retries it (§3.5). Never while the page's signals are still
    /// ([`Self::signals_still`]): the launch's admission still waits (§3.3).
    pub(crate) fn needs_attention(&self) -> bool {
        if self.signals_still() {
            return false;
        }
        let signal = self.status.failures_signal();
        let reviewed = self.reviewed_plan();
        let quiet = self.status.joins_quietly();
        (self.plan_known
            && self.reviewed_rows().iter().any(|row| {
                row.attention()
                    && (signal || row.standing != Some(Standing::Failed))
                    && !(quiet && row.subject == Subject::Setting(Setting::Secrets))
            }))
            || self
                .status
                .failed_steps(self.plan_known.then_some(&reviewed))
                .next()
                .is_some()
    }

    /// Send the page's groups to the worker now (§3.3: a choice takes effect
    /// at once). A run that is going stops at its next step and this plan
    /// runs next: the host and the worker see to that, so nothing waits here
    /// on a view that may close. Join sends the vault address as typed and
    /// its password; any other submission sends the address the worker holds
    /// and leaves a draft where it is. `changed` is what the worker takes of the plan onto the
    /// plan in force ([`SetupAnswer::changed`]).
    fn apply(&mut self, admission: Admission, submission: Submission, changed: Option<Selection>) {
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
            self.password.copy_for_submission()
        } else {
            None
        };
        self.status.busy = true;
        self.status.operation = None;
        self.stopping = false;
        if admission != Admission::Automatic {
            self.status.stopped_wait = false;
        }
        let chosen = !matches!(submission, Submission::AsItStands | Submission::Offer);
        self.status.queued_held = admission.holds_unsaved_work(chosen);
        self.answer_waits = false;
        self.awaiting = Some(Awaiting {
            plan: plan.clone(),
            password: password.as_ref().and(self.password.acceptance.clone()),
            changed,
        });
        self.actions.push_back(SystemStateAction::Apply(
            SetupAnswer {
                decision: WorkstationSetupDecision::Begin(plan),
                password,
                receipt: None,
                chosen,
                vault_typed: with_vault,
                changed,
            },
            admission,
        ));
    }

    pub(crate) fn pending_password_acceptance(&self, plan: &Plan) -> Option<PasswordAcceptance> {
        self.awaiting
            .as_ref()
            .filter(|awaiting| &awaiting.plan == plan)
            .and_then(|awaiting| awaiting.password.clone())
    }

    pub(crate) fn forget_accepted_password(&mut self) -> bool {
        self.password.forget_accepted()
    }

    /// The worker durably accepted `plan`.
    pub(crate) fn apply_accepted(&mut self, plan: Plan) -> bool {
        self.submitted = plan.clone();
        self.plan_known = true;
        self.request = None;
        self.answer_waits = false;
        if let Some(password) = self
            .awaiting
            .take_if(|awaiting| awaiting.plan == plan)
            .and_then(|awaiting| awaiting.password)
        {
            // Joined: the address is the plan's now, and the password went once.
            password.accept();
            self.password.forget_accepted();
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
    /// Closing never answers (§3.3): an offer still waiting for the page's answer is let go
    /// ([`Request::let_go`]), so the launch's is made again where a page can answer it, and
    /// nothing is recorded. What the page sent is the worker's already.
    pub(crate) fn closed(&mut self, _scope: CloseScope) {
        if let Some(request) = self.request.take() {
            request.let_go();
        }
        self.answer_waits = false;
    }

    /// The plan the worker will run next: the one sent last, else the one it
    /// holds.
    fn next_plan(&self) -> &Plan {
        self.awaiting
            .as_ref()
            .map_or(&self.submitted, |awaiting| &awaiting.plan)
    }

    /// Whether the worker is putting back the Mac preferences aterm saved
    /// (Restore previous settings): the groups take no choice meanwhile, since
    /// the plan the page shows is the one the Restore is changing (§3.4). A
    /// Restore waiting for another setup ([`SetupStatus::queued`]) changes
    /// nothing yet: a group press replaces it, as a later press replaces a
    /// choice waiting, since the latest intent wins.
    fn restoring(&self) -> bool {
        self.status.operation == Some(SetupOperation::Restore) && !self.status.queued
    }

    /// Whether Check again can start: nothing is busy, or what is busy is a
    /// press waiting for another setup ([`SetupStatus::queued`]), which runs
    /// nothing meanwhile, so a reading of the rows can go (§3.4). A newer aterm's setup names
    /// no plan, yet reading writes nothing: Check again reads its rows as they stand (§3.2).
    fn can_check(&self) -> bool {
        (self.plan_known || self.status.newer()) && (!self.status.busy || self.status.queued)
    }

    /// Check again, from the footer or a parked step's headline (`by` a press), or the page's
    /// own reading ([`crate::CheckBy::Page`]): the rows are read again for the page's plan, and
    /// for a press the worker lifts every stage aterm parked (init-on-by-default design §6.5).
    fn check_again(&mut self, by: crate::CheckBy) {
        let plan = self.reviewed_plan();
        // A press waiting for another setup keeps its words: the reading goes beside it.
        if !self.status.queued {
            self.status.busy = true;
            self.status.operation = Some(SetupOperation::Check);
        }
        self.actions.push_back(SystemStateAction::Check(plan, by));
    }

    /// Whether Restore previous settings can start: nothing is busy, no offer waits, and the
    /// setup records are this aterm's to put back, not a newer aterm's (§3.4).
    fn can_restore(&self) -> bool {
        !self.status.busy && !self.reviewing() && !self.status.newer()
    }

    /// The settings of the groups the person changed since the page last took
    /// the plan in force, with `settings` (§3.3): a group pressed and pressed
    /// back counts, so the worker takes the page's value of each onto the plan
    /// in force, whichever earlier choice it admitted. A plan sent whole and not
    /// yet accepted keeps the next one whole.
    fn changed_with(&self, settings: &[Setting]) -> Option<Selection> {
        let mut changed = match &self.awaiting {
            Some(awaiting) => awaiting.changed?,
            None => Selection::none(),
        };
        for setting in settings {
            changed.set(*setting, true);
        }
        Some(changed)
    }

    /// What a press takes onto the plan in force ([`SetupAnswer::changed`]): while the offer
    /// waits for its answer, the press answers it with the plan the page shows, whole (`None`),
    /// so the groups an offer of the ALab signal's widening adds go with the press; otherwise
    /// the groups changed since the page last took the plan in force, `settings` among them.
    fn press_changed(&self, settings: &[Setting]) -> Option<Selection> {
        if self.reviewing() {
            None
        } else {
            self.changed_with(settings)
        }
    }

    /// Set every bit of `group` to `on`, as [`Self::choose_settings`] does: a test's press on
    /// one plan group, where the page's lines each turn one or two groups.
    #[cfg(test)]
    fn choose(&mut self, group: Group, on: bool) {
        self.choose_settings(group.settings(), on);
    }

    /// Set every bit of `settings` (a group's, a line's) to `on`, and submit the plan when that
    /// changes what the worker will run, or while the plan aterm chose by itself may still
    /// widen ([`Self::may_widen`]): a choice makes the plan the person's, even one that returns
    /// to aterm's plan (§3.5). Nothing while a Restore runs.
    fn choose_settings(&mut self, settings: &[Setting], on: bool) {
        if !self.plan_known || self.restoring() {
            return;
        }
        for setting in settings {
            self.plan.selection.set(*setting, on);
        }
        // A press while the offer waits for its answer answers it, as the presser's choice: an
        // offer still pending is one the plan may widen.
        if self.plan.selection != self.next_plan().selection || self.may_widen() {
            let changed = self.press_changed(settings);
            self.apply(
                Admission::of_gesture(self.by_person),
                Submission::Choice,
                changed,
            );
        }
    }

    /// Whether any bit of `line` is set: its switch reads On.
    fn line_on(&self, line: lines::Line) -> bool {
        line.settings()
            .iter()
            .any(|setting| self.plan.selection.contains(*setting))
    }

    /// Whether the plan in force is aterm's own, which a look that finds the ALab signal
    /// widens: the worker says so ([`SetupStatus::provisional`]), and the startup's offer
    /// still waiting for its answer may be. A press then is sent even where it leaves the
    /// groups as they are, or the widen would set a group the person chose to leave as is.
    fn may_widen(&self) -> bool {
        self.status.provisional || self.pending()
    }

    /// Whether the vault's fields show: as the person last set the
    /// disclosure ([`vault_waits_for_input`] clears it on a reading that
    /// comes to wait), else open while the vault waits for input here.
    fn vault_shown(&self) -> bool {
        self.vault_open.unwrap_or_else(|| {
            // As the vault reads, whatever the review shows of it: a vault that waits for a
            // password typed here opens its fields during the review too.
            let row = row_view_for_source(
                Subject::Setting(Setting::Secrets),
                &self.status,
                self.plan.selection,
                Some(self.url.value().trim()),
            );
            row.managed
                && matches!(row.standing, Some(Standing::NeedsYou | Standing::Failed))
                && row.note.is_none()
        })
    }

    /// Whether the field with focus takes words now: the phrase field while it shows and no
    /// phrase is out, whether or not a run goes (the join is no part of one); the vault's own
    /// fields while they are editable ([`Self::vault_editable`]).
    fn takes_words(&self, view: &SystemStateViewState) -> bool {
        if view
            .common
            .last_focus
            .as_ref()
            .is_some_and(|key| key.as_str() == PHRASE_FIELD)
        {
            return self.phrase_editable();
        }
        self.vault_editable()
    }

    /// One text input to the vault field with focus, which scrolls into
    /// view as it does when it takes focus: typing never goes on out of
    /// sight.
    fn type_into(&mut self, view: &mut SystemStateViewState, event: TextInputEvent) {
        // What can change a field's words; a caret move, a selection or a
        // submit leaves the vault's disclosure as it stood.
        let edits = match &event {
            TextInputEvent::Preedit { text, .. } | TextInputEvent::Commit(text) => !text.is_empty(),
            TextInputEvent::Backspace
            | TextInputEvent::Delete
            | TextInputEvent::KillToEnd
            | TextInputEvent::KillToStart
            | TextInputEvent::DeleteWordBackward
            | TextInputEvent::DeleteWordForward
            | TextInputEvent::Undo
            | TextInputEvent::Redo => true,
            TextInputEvent::Left { .. }
            | TextInputEvent::Right { .. }
            | TextInputEvent::Home { .. }
            | TextInputEvent::End { .. }
            | TextInputEvent::WordLeft { .. }
            | TextInputEvent::WordRight { .. }
            | TextInputEvent::SelectAll
            | TextInputEvent::Submit
            | TextInputEvent::Cancel => false,
        };
        if let Some(key) = view.common.last_focus.clone()
            && self.field_event(key.as_str(), event)
        {
            // A field someone types into stays shown, as one filled through
            // its action does: a draft is never hidden. The phrase field sits
            // above the disclosure and opens nothing.
            if edits && key.as_str() != PHRASE_FIELD {
                self.vault_open = Some(true);
            }
            view.reveal_listed_control(&key);
        }
    }

    /// One text input to the vault field `key`: `false` when `key` names
    /// no vault field. Return in the phrase field joins with it, as its Join does.
    fn field_event(&mut self, key: &str, event: TextInputEvent) -> bool {
        // Return in the vault's address or password joins, as its Join does.
        if matches!(key, "system-state/url" | "system-state/password")
            && event == TextInputEvent::Submit
        {
            self.join_vault();
            return true;
        }
        if key == "system-state/password" {
            self.password.input(event);
            return true;
        }
        if key == PHRASE_FIELD {
            if !self.phrase_editable() {
                return false;
            }
            if event == TextInputEvent::Submit {
                self.join_phrase();
            } else {
                // A new phrase is on its way in: what the last one came to is old news.
                self.phrase_came_to = None;
                edit_text(&mut self.phrase, event);
            }
            return true;
        }
        if key != "system-state/url" {
            return false;
        }
        edit_text(&mut self.url, event);
        true
    }

    /// Join the vault at the address typed, with the password typed: Join, or Return in either
    /// field.
    fn join_vault(&mut self) {
        if !self.vault_editable() || !self.plan_known {
            return;
        }
        self.vault_open = Some(true);
        self.plan.selection.set(Setting::Secrets, true);
        let changed = self.press_changed(&[Setting::Secrets]);
        self.apply(
            Admission::of_gesture(self.by_person),
            Submission::Join,
            changed,
        );
    }

    /// One pressed control of the page.
    fn act(&mut self, view: &mut SystemStateViewState, key: &str, value: Option<SemanticInput>) {
        if repositories::act(view, key) {
            return;
        }
        // A test's press on one plan group (`system-state/group/<index>/<set-up|leave>`): the
        // page draws no such control, its lines each turning one or two groups at once.
        #[cfg(test)]
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
        if let Some(rest) = key.strip_prefix("system-state/line/") {
            let mut parts = rest.split('/');
            if let (Some(line), Some(part)) =
                (parts.next().and_then(lines::Line::from_slug), parts.next())
            {
                match part {
                    "switch" => {
                        let on = match value {
                            Some(SemanticInput::Bool(on)) => on,
                            _ => !self.line_on(line),
                        };
                        // A line out of this Mac's scope (the sign-outs outside ALab) stays off.
                        if on && lines::out_of_scope(self, line, self.line_on(line)) {
                            return;
                        }
                        self.choose_settings(line.settings(), on);
                    }
                    // One line open at a time (§4.1): opening one closes the other.
                    "disclose" => {
                        view.open_line = (view.open_line != Some(line)).then_some(line);
                    }
                    _ => {}
                }
            }
            return;
        }
        match key {
            "system-state/stop" if self.status.stoppable() && !self.stopping => {
                // Asked once: the worker keeps it, so a run that has not
                // shown up yet stops once it does.
                self.stopping = true;
                self.actions.push_back(SystemStateAction::Stop);
            }
            "system-state/check" if self.can_check() => self.check_again(crate::CheckBy::Press),
            "system-state/restore" if self.can_restore() => {
                self.status.busy = true;
                self.status.operation = Some(SetupOperation::Restore);
                self.actions.push_back(SystemStateAction::Restore);
            }
            "system-state/headline/start"
                if !self.status.busy && self.plan_known && self.headline().start =>
            {
                // The plan in force as it stands: nothing of the page's own.
                self.apply(
                    Admission::of_gesture(self.by_person),
                    Submission::AsItStands,
                    Some(Selection::none()),
                );
            }
            "system-state/row/secrets/disclose" => {
                self.vault_open = Some(!self.vault_shown());
            }
            ASK_FOR_ACCESS if !offered_access(&self.status).is_empty() => {
                self.actions.push_back(SystemStateAction::AskForAccess {
                    by_person: self.by_person,
                });
            }
            TURN_ON_SYNC => {
                if self.reviewed_rows().iter().any(|row| row.turn_on_sync) {
                    self.actions.push_back(SystemStateAction::TurnSyncOn {
                        by_person: self.by_person,
                    });
                }
            }
            GO_TO_TOOLS_TAB if self.headline().go_to_tab => {
                self.actions.push_back(SystemStateAction::GoToToolsTab);
            }
            // Only while the card offers it: a press left from a page that read Apple's window
            // goes nowhere once the row reads otherwise.
            GO_TO_TOOLS_INSTALLER
                if self.status.repositories_tools_wait() == Some(ToolsWait::AppleWindow) =>
            {
                self.actions
                    .push_back(SystemStateAction::GoToToolsInstaller);
            }
            lines::COPY_APPROVE_COMMAND => {
                let command = self
                    .reviewed_rows()
                    .iter()
                    .find(|row| row.subject == Subject::Setting(Setting::Secrets))
                    .and_then(|row| row.note.as_deref().and_then(lines::approve_command))
                    .map(str::to_owned);
                if let Some(command) = command {
                    self.actions.push_back(SystemStateAction::CopyCommand {
                        command,
                        by_person: self.by_person,
                    });
                }
            }
            INSTALL_TOOLS => {
                if self.plan_known && self.reviewed_rows().iter().any(|row| row.install_tools) {
                    self.actions
                        .push_back(SystemStateAction::InstallDeveloperTools {
                            by_person: self.by_person,
                        });
                }
            }
            PHRASE_JOIN => self.join_phrase(),
            PHRASE_FIELD if self.phrase_editable() => {
                view.common.last_focus = Some(UiKey::new(key));
                match value {
                    Some(SemanticInput::Text(text)) => {
                        self.phrase_came_to = None;
                        self.phrase.select_all();
                        self.phrase.insert(&text);
                    }
                    Some(SemanticInput::TextPosition { byte, extend }) => {
                        let anchor = if extend {
                            self.phrase.selection().anchor
                        } else {
                            byte
                        };
                        self.phrase.set_selection(anchor, byte);
                    }
                    _ => {}
                }
            }
            "system-state/vault/join" => self.join_vault(),
            "system-state/url" | "system-state/password" if self.vault_editable() => {
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
                    && self.can_open_access(access)
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
        // The page closed: a phrase typed on the vault row goes with it, wiped.
        self.phrase.wipe();
    }
}

impl NativeAppModel for SystemStateApp {
    type ViewState = SystemStateViewState;

    fn descriptor(&self) -> AppDescriptor {
        AppDescriptor {
            kind: AppKind::SystemState,
            name: "System State",
            icon: AppIcon::SystemState,
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
                view.scroll_steps(lines, LINE_PX);
                true
            }
            AppEvent::ScrollPage(direction) => {
                let (room, line) = (view.room.get(), LINE_PX.min(view.room.get()));
                view.scroll_steps(direction, (room - line).max(line));
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
                if self.takes_words(view) && event != TextInputEvent::Cancel {
                    self.type_into(view, event);
                }
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                if self.takes_words(view) {
                    self.type_into(view, TextInputEvent::Commit(text));
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
        page::Page::new(self, view, cx).tree()
    }

    fn commands(&self, _view: &Self::ViewState, _out: &mut Vec<Command>) {}

    fn presentation(&self, _view: &Self::ViewState) -> AppPresentation {
        AppPresentation {
            title: "System State".to_owned(),
            icon: AppIcon::SystemState,
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

/// Why the fixtures' repositories failed for a while: GitHub stopped answering, which passes.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
const GITHUB_STOPPED: &str = "GitHub stopped answering partway through.";

/// Why a step of the repositories failed, in `init_observe`'s words (`Wait::Unreached`).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) const GITHUB_UNREACHED: &str = "GitHub can't be reached";

/// The repositories' note where init.toml's GitHub account is not the token's, in
/// `init_observe`'s words (`Wait::OtherAccount`).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) const GITHUB_OTHER_ACCOUNT: &str = "GitHub account setting differs from the token's";

/// The page's scenarios for looking at it without a Mac to set up
/// (`ATERM_DEV_SYSTEM_STATE_FIXTURE`), in the order [`fixture_status`] draws them: the names its
/// switch answers, and no others, so a scenario is drawn only once it is listed here, and every
/// test that sweeps the scenarios sweeps it. `older-cancel` is a Cancel an older aterm recorded.
/// Debug and dev-seam builds only.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) const FIXTURES: &[&str] = &[
    "repository-progress@developer",
    "repository-waiting@developer",
    "repository-outcomes@developer",
    "checking",
    "check-last-known",
    "check-last-known@mac",
    "check-last-known-idle@mac",
    "check-last-known-failed@mac",
    "offer-last-known@mac",
    "account-provisioning",
    "plan-unread",
    "vault-source-mismatch",
    "check-failed",
    "check-unconfirmed",
    "running",
    "needs-you",
    "vault",
    "vault-unreached",
    "vault-not-syncing",
    "vault-not-set-up",
    "sync-off",
    "vault-failed",
    "set-up",
    "non-alab",
    "non-alab-absent",
    "widened",
    "widen-stopped",
    "failed",
    "github-account",
    "required-dependencies@developer",
    "named-result-failed",
    "named-results-failed",
    "restore-at-login",
    "mixed",
    "restore-blocked",
    "restore-unlogged",
    "fresh",
    "queued-choice",
    "queued-widen",
    "queued-restore",
    "set-aside",
    "set-aside-non-alab",
    "startup-retry",
    "restore-failed",
    "setup-retried",
    "failed-retrying",
    "vault-unreadable",
    "join-refused",
    "vault-not-joined",
    "vault-held",
    "newer",
    "restoring",
    "older-cancel",
    "clt-queued",
    "clt-password",
    "clt-recovered",
    "clt-installing",
    "clt-missing",
    "clt-apple-window",
    "clt-not-opened",
    "changed-since",
    "parked",
    "still-running",
    "resumed",
    "vault-asking",
    "vault-allowed",
    "clt-low-data",
    "clt-not-admin",
    "vault-asking-long",
    "walk",
    "vault-approve",
    "mixed@mac",
    "set-up@secrets",
    "named-result-failed@developer",
    "vault-approve@secrets",
];

/// The line a scenario named `<scenario>@<line>` ([`FIXTURES`]) shows open, so the gallery
/// judges that line's rows and phrases (`mixed@mac`); `None` for a scenario shown collapsed.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn fixture_line(name: &str) -> Option<lines::Line> {
    let (_, line) = name.split_once('@')?;
    lines::Line::from_slug(line)
}

/// The scenario `name` ([`FIXTURES`]), `None` for a name not listed there. Each draws only what
/// the worker gives the page: the readings `init_observe` and the worker's probes make, and the
/// rows a run, a Check or a Restore reports.
/// Debug and dev-seam builds only: a release build carries none of
/// their words.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn fixture_status(name: &str) -> Option<SetupStatus> {
    if !FIXTURES.contains(&name) {
        return None;
    }
    // `<scenario>@<line>` is that scenario with the line open ([`fixture_line`]).
    let name = name.split_once('@').map_or(name, |(scenario, _)| scenario);
    use Standing::{AtLogin, Differs, Failed, InPlace, NeedsYou, NotNeeded, Unknown, Waiting};
    use aterm_types::macos_setup::until_login;
    let read = |subject: Subject, standing: Standing, now: Option<&str>| Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: (subject == Subject::Setting(Setting::Secrets)
            && standing == Standing::InPlace
            && now == Some(Setting::Secrets.wanted()))
        .then(|| aterm_types::macos_setup::VAULT_LOCAL_READY_NOTE.to_owned()),
        detail: None,
        read: aterm_types::macos_setup::Read::Unstamped,
    };
    // A value live until the next login, as `init_observe` marks it right after a Restore: the
    // Mac reads `after` from then.
    let until = |setting: Setting, live: &str, after: &str| Observation {
        now: Some(until_login(live)),
        after_login: Some(after.to_owned()),
        ..read(Subject::Setting(setting), Differs, None)
    };
    // The plan a Restore leaves: the groups it put back are left as is.
    let restored = |mut selection: Selection| {
        for group in [Group::Input, Group::Desktop] {
            for setting in group.settings() {
                selection.set(*setting, false);
            }
        }
        Plan::from(selection)
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
    // The repositories as the worker reads them before the vault has synced
    // this Mac, and so its GitHub token: no count, and what they wait on in
    // their note.
    let token_waiting = || Observation {
        note: Some("Waits for the vault to finish syncing this Mac".to_owned()),
        ..setting(Setting::Repositories, Waiting, None)
    };
    let all = Plan::default();
    // An ALab Mac aterm has set up before, so Restore has its saved preferences to put back,
    // unless the scenario says otherwise.
    let mut status = SetupStatus {
        observed_vault_url: Some(all.vault_url.clone()),
        plan: Some(all.clone()),
        alab: true,
        restorable: true,
        ..SetupStatus::default()
    };
    // Nothing changed on this Mac yet: Restore has nothing to put back.
    if matches!(name, "checking" | "plan-unread" | "fresh" | "older-cancel") {
        status.restorable = false;
    }
    // No ALab membership on this Mac (§3.5): the sign-outs are out of its scope.
    if matches!(
        name,
        "non-alab" | "non-alab-absent" | "set-aside-non-alab" | "vault-approve"
    ) {
        status.alab = false;
    }
    match name {
        "repository-waiting" => {
            use aterm_repository_progress::{Phase, Progress, Transition};
            status.busy = true;
            status.running = vec![Setting::Repositories];
            status.observed = with(
                in_place(),
                vec![setting(
                    Setting::Repositories,
                    Waiting,
                    Some("Waiting for disk space"),
                )],
            );
            let mut row = Progress::new(
                "project",
                "/home/person/workspaces/research/very-long-project-name/checkout".into(),
            );
            row.staging = Some("/home/person/workspaces/research/very-long-project-name/.checkout-clone-in-progress".into());
            row.enter(Phase::Queued);
            row.enter(Phase::WaitingForSpace);
            row.previous = vec![Transition {
                at_ms: row.started_ms.saturating_sub(1000),
                phase: Phase::Interrupted,
            }];
            status.repositories.push(row);
        }
        "repository-outcomes" => {
            use aterm_repository_progress::{FailureKind, Phase, PreservationReason, Progress};
            status.observed = with(
                in_place(),
                vec![setting(
                    Setting::Repositories,
                    Differs,
                    Some("1 of 3 ready"),
                )],
            );
            let mut failed = Progress::new("project", "/home/person/project".into());
            failed.git_line("Receiving objects: 75% (3/4), 1.50 GiB | 8.20 MiB/s");
            failed.fail(FailureKind::Network);
            let mut deferred = Progress::new("tools", "/home/person/tools".into());
            deferred.usable = true;
            deferred.placed = true;
            deferred.enter(Phase::Checking);
            deferred.defer(PreservationReason::LocalChanges);
            let mut interrupted = Progress::new("compiler", "/home/person/compiler".into());
            interrupted.enter(Phase::Verifying);
            interrupted.interrupt();
            status.repositories = vec![failed, deferred, interrupted];
        }
        "repository-progress" => {
            use aterm_repository_progress::{Phase, Progress};
            status.busy = true;
            status.running = vec![Setting::Repositories];
            status.observed = with(
                in_place(),
                vec![setting(
                    Setting::Repositories,
                    Differs,
                    Some("2 of 4 ready"),
                )],
            );
            for (name, phase, percent, usable) in [
                ("project", Phase::Downloading, Some(75), false),
                ("compiler", Phase::Verifying, None, false),
                ("tools", Phase::Fetching, Some(40), true),
                ("library", Phase::Ready, None, true),
            ] {
                let mut row = Progress::new(
                    name,
                    std::path::PathBuf::from(format!("/home/person/{name}")),
                );
                row.enter(phase);
                row.percent = percent;
                row.usable = usable;
                row.placed = usable;
                if percent.is_some() {
                    row.received = Some("1.50 GiB".into());
                    row.speed = Some("8.20 MiB/s".into());
                }
                status.repositories.push(row);
            }
        }
        "checking" => {}
        "offer-last-known" => {
            // Capture the actual accepted-offer transition before its next status arrives.
            status.observed = in_place()
                .into_iter()
                .map(|row| row.stamped(1_700_000_000))
                .collect();
            status.finished = true;
            let mut page = SystemStateApp::opened();
            page.publish(status);
            let (send, _receive) = std::sync::mpsc::channel();
            assert!(page.offer(Request::new(all.clone(), send)));
            status = page.status.clone();
            // The real controller knows this plan from the accepted offer. The gallery
            // projects its status without keeping a live request or starting any worker.
            status.plan = Some(all.clone());
        }
        "check-last-known" | "check-last-known-idle" | "check-last-known-failed" => {
            // A Check retires current readings while retaining useful presentation history.
            status.last_known = in_place();
            if name == "check-last-known" {
                status.observing = status
                    .last_known
                    .iter()
                    .map(|row| {
                        (
                            row.subject,
                            row.subject
                                .uses_vault_source()
                                .then(|| all.vault_url.clone()),
                            false,
                        )
                    })
                    .collect();
            } else if name == "check-last-known-failed" {
                status.observed = vec![Observation {
                    note: Some("The latest keyboard settings could not be read".into()),
                    ..setting(Setting::CapsLock, Failed, None)
                }];
            }
        }
        "plan-unread" => {
            status.plan = None;
            status.observed_vault_url = None;
            status.rows.push(calm_setup_row(STATE_UNREAD_NOTE));
        }
        "vault-source-mismatch" => {
            status.plan.as_mut()?.vault_url = "http://reviewed-vault.local:7484/v1/team".into();
            let mut secrets = setting(Setting::Secrets, NeedsYou, Some("Awaiting code"));
            secrets.note = Some("Approve the old vault: OLD-VAULT-42".into());
            let mut repos = setting(Setting::Repositories, Failed, None);
            repos.detail = Some("Old vault credentials were refused.".into());
            status.observed = with(
                in_place(),
                vec![
                    secrets,
                    repos,
                    setting(Setting::CapsLock, Differs, Some("Caps Lock")),
                ],
            );
        }
        "check-failed" => {
            // A Check that failed as a whole: the rows keep their last readings.
            status.observed = in_place();
            status.check = Some(SetupCheck {
                plan: all,
                rows: vec![SetupStatusRow {
                    name: "Check".into(),
                    status: "failed".into(),
                    detail: CHECK_FAILED_NOTE.into(),
                }],
            });
        }
        "check-unconfirmed" => {
            // A Check whose repositories stage gave no answer: the page says it under its row.
            status.observed = in_place();
            status.check = Some(SetupCheck {
                plan: all,
                rows: vec![SetupStatusRow {
                    name: "Check: GitHub repositories".into(),
                    status: "unverified".into(),
                    detail: String::new(),
                }],
            });
        }
        "running" => {
            status.busy = true;
            status.running = vec![Setting::Dock];
            // As `macos_doctor` reads a vault the plan names but this network does not answer
            // for, on a Join the person typed a password for that the run still holds for a try
            // (aterm's own join reads Not joined, asked again by itself, there, as `non-alab`
            // does), and the repositories that wait on it.
            let mut secrets_waiting = setting(Setting::Secrets, Waiting, Some("Not joined"));
            secrets_waiting.note = Some("The ALab vault did not answer on this network".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Dock, Differs, Some("9 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    secrets_waiting,
                    token_waiting(),
                ],
            );
        }
        "walk" => {
            // The permission walk at its first page of three, while the run carries on: the card
            // says the grant, where the walk stands, and why.
            status.busy = true;
            status.running = vec![Setting::Permissions];
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Off"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            status.walk = Some(WalkPage {
                access: Access::FullDiskAccess,
                at: 1,
                of: 3,
            });
            status.observed = with(
                in_place(),
                vec![
                    fda,
                    access(Access::DeveloperTools, NeedsYou, Some("Off")),
                    access(Access::Accessibility, NeedsYou, Some("Off")),
                ],
            );
        }
        "needs-you" => {
            // Each reason where its probe puts it: a grant's in the description, Developer
            // Tools' in the note; and the ALab join, asking the Macs in ALab past its quiet, so
            // automatic admission stays quiet while Full Disk Access still needs the person.
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Not confirmed"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            let mut tools = access(Access::DeveloperTools, Waiting, None);
            tools.note = Some("Readable once Full Disk Access is on".to_owned());
            status.asking_long = true;
            let mut vault = setting(
                Setting::Secrets,
                NeedsYou,
                Some(aterm_types::macos_setup::VAULT_ASKING),
            );
            vault.note = Some(aterm_types::macos_setup::VAULT_ASKING_NOTE.to_owned());
            status.observed = with(in_place(), vec![fda, tools, vault, token_waiting()]);
        }
        "vault-approve" => {
            // A Mac outside ALab that asked to join a vault of its own: an admin Mac of that
            // vault approves it with the command its row's note names.
            let mut plan = Plan::from(BASE_PLAN);
            plan.vault_url = "https://vault.home.example/v1/family".to_owned();
            status.observed_vault_url = Some(plan.vault_url.clone());
            status.plan = Some(plan);
            let mut vault = setting(Setting::Secrets, NeedsYou, Some("Awaiting code"));
            vault.note =
                Some("On an admin Mac: aterm secrets approve 7QK2-M9WD-4HTX-PL3B".to_owned());
            status.observed = with(
                in_place(),
                vec![access(Access::Accessibility, NotNeeded, None), vault],
            );
        }
        "vault" => {
            // A Join the person typed a password for found no vault at the ALab address: while
            // the run holds that password for a try, the vault waits for that vault or another
            // address, which its disclosure's fields take, and the repositories for its token.
            // aterm's own join reads Not joined in place here (§3.2).
            let mut vault = setting(Setting::Secrets, Waiting, Some("Not joined"));
            vault.note = Some("No vault on this network".to_owned());
            status.observed = with(in_place(), vec![vault, token_waiting()]);
        }
        "vault-asking" | "vault-asking-long" => {
            // ALab admission and its retries stay automatic throughout the wait.
            status.asking_long = name == "vault-asking-long";
            let mut vault = setting(
                Setting::Secrets,
                NeedsYou,
                Some(aterm_types::macos_setup::VAULT_ASKING),
            );
            vault.note = Some(aterm_types::macos_setup::VAULT_ASKING_NOTE.to_owned());
            status.observed = with(in_place(), vec![vault, token_waiting()]);
        }
        "vault-allowed" => {
            // A member let this Mac in: the vault records it by itself, so nothing waits on the
            // person and the row is calm.
            let mut vault = setting(
                Setting::Secrets,
                Waiting,
                Some(aterm_types::macos_setup::VAULT_ALLOWED),
            );
            vault.note = Some(aterm_types::macos_setup::VAULT_ALLOWED_NOTE.to_owned());
            status.observed = with(in_place(), vec![vault, token_waiting()]);
        }
        "vault-unreached" => {
            // Idle after a pass whose Join, typed with a password the run still holds, found the
            // vault not answering: the worker tries again by itself when this Mac's network
            // changes, so nothing is a failure meanwhile.
            status.retrying = true;
            let mut vault = setting(Setting::Secrets, Waiting, Some("Not joined"));
            vault.note = Some("The ALab vault did not answer on this network".to_owned());
            status.observed = with(in_place(), vec![vault, token_waiting()]);
        }
        "vault-not-syncing" => {
            let mut vault = setting(Setting::Secrets, Differs, Some("Not syncing"));
            vault.note = Some("The sync is not running; Set up now starts it".to_owned());
            status.observed = with(in_place(), vec![vault]);
        }
        "vault-not-set-up" => {
            let mut vault = setting(Setting::Secrets, Differs, Some("Not set up"));
            vault.note = Some("Not set up to sync this Mac; Set up now installs it".to_owned());
            status.observed = with(in_place(), vec![vault]);
        }
        "sync-off" => {
            // The person turned sync off: their choice is the row's wanted value.
            let mut vault = setting(Setting::Secrets, InPlace, Some("Sync off"));
            "Sync off".clone_into(&mut vault.wanted);
            vault.note = Some("You turned sync off on this Mac".to_owned());
            status.observed = with(in_place(), vec![vault]);
        }
        "vault-failed" => {
            // A join the vault refused, as the worker's page says it while it tries again by
            // itself: the vault's row, in the page's words.
            status.retrying = true;
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    token_waiting(),
                ],
            );
            status.rows = vec![SetupStatusRow {
                name: Setting::Secrets.name().into(),
                status: "error".into(),
                detail: RETRIES_NOTE.into(),
            }];
        }
        "set-up" => {
            // Trackpad scrolling, set in this login session, is in full use from the next. The
            // run took 72 seconds.
            status.ready_in = Some(std::time::Duration::from_secs(72));
            status.observed = with(
                in_place(),
                vec![setting(Setting::TrackpadScrolling, AtLogin, Some("All on"))],
            );
        }
        "account-provisioning" => {
            status.finished = true;
            status.observed = in_place();
            status.observed.push(Observation {
                subject: Subject::Sync(SyncKind::Accounts),
                standing: Standing::Waiting,
                now: Some("Not provisioned".to_owned()),
                after_login: None,
                wanted: "Provisioned".to_owned(),
                note: Some("No active Claude account is assigned to this Mac. Automatic provisioning resumes when the synced account roster changes.".to_owned()),
                detail: None,
                read: aterm_types::macos_setup::Read::Unstamped,
            });
        }
        "non-alab" | "non-alab-absent" => {
            // The base plan aterm admits by itself on a Mac with no ALab signal (§3.5): aterm's
            // permissions and Spotlight in place, and the vault not joined. Off the ALab LAN
            // nothing answers at the ALab vault's address (`alab-secrets.local` resolves to
            // nothing), so `macos_doctor` reads the vault asked again by itself, neither in place
            // nor waited on (`non-alab`, what such a Mac really reads); where a server there
            // answered with none, it reads in place (`non-alab-absent`). Either way no join the
            // person typed waits on it. The profile is left as is.
            status.plan = Some(Plan::from(BASE_PLAN));
            status.provisional = true;
            let vault = if name == "non-alab-absent" {
                let mut vault = setting(Setting::Secrets, InPlace, Some("Not joined"));
                "Not joined".clone_into(&mut vault.wanted);
                vault.note = Some("No vault on this network".to_owned());
                vault
            } else {
                let mut vault = setting(Setting::Secrets, Standing::Asking, Some("Not joined"));
                vault.note = Some("The ALab vault did not answer on this network".to_owned());
                vault
            };
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
                    vault,
                    setting(Setting::Repositories, InPlace, Some("All")),
                ],
            );
        }
        "widened" => {
            // A run aterm widened by itself on hearing the ALab vault on this network: its
            // note says why the groups changed while it goes.
            status.busy = true;
            status.widened = true;
            status.running = vec![Setting::CapsLock];
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::CapsLock, Differs, Some("Caps Lock")),
                    setting(Setting::Dock, Differs, Some("14 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    token_waiting(),
                ],
            );
        }
        "widen-stopped" => {
            // A Stop pressed as aterm widened the plan: the plan stays as it was, Permissions
            // alone, and the rest is left as is though the ALab vault answers here.
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            status.plan = Some(Plan::from(selection));
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::CapsLock, Differs, Some("Caps Lock")),
                    setting(Setting::Dock, Differs, Some("14 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    setting(Setting::Repositories, Unknown, None),
                ],
            );
        }
        "failed" => {
            // The run's GitHub repositories step, which could not reach GitHub on its last try,
            // once no worker goes on with the run: aterm no longer tries it by itself, and Set up now
            // does, so the headline, the row, the signal line and the diamond say it failed. The
            // worker's page says it under its row in the reason the step decided
            // (`init_observe::failure_note`; the log keeps GitHub's words), which the row keeps
            // as its note, so the page's note names Set up now alone; the row reads the clones
            // the run made.
            status.observed = with(
                in_place(),
                vec![setting(Setting::Repositories, Differs, Some("3 of 12"))],
            );
            status.rows = vec![SetupStatusRow {
                name: Setting::Repositories.name().into(),
                status: "error".into(),
                detail: GITHUB_UNREACHED.into(),
            }];
        }
        "required-dependencies" => {
            // The parent checkout exists and credentials are ready, but selected dependency
            // work is preserved at another pin. Neither the count nor the line is complete.
            let mut repositories = setting(Setting::Repositories, NeedsYou, Some("0 of 1"));
            repositories.wanted = "All 1".to_owned();
            repositories.detail = Some(
                "~/project/vendor/library is on its own branch at another commit; its work \
                 stays untouched. The required dependency keeps this repository unfinished."
                    .to_owned(),
            );
            status.observed = with(in_place(), vec![repositories]);
        }
        "github-account" => {
            // init.toml's GitHub account is not the one the vault's token signs in as, as
            // `init_observe` reads the repositories then: a person fixes the setting, so the
            // row needs them, with the remedy in its note, and no run retries it (§3.6).
            let mut repositories = setting(Setting::Repositories, NeedsYou, Some("Needs sign-in"));
            repositories.note = Some(GITHUB_OTHER_ACCOUNT.to_owned());
            status.observed = with(in_place(), vec![repositories]);
        }
        "named-result-failed" => {
            status.observed = in_place();
            // The repositories stage failed as a whole: the worker's page says it under its row,
            // with no words of the stage's own.
            status.rows = vec![SetupStatusRow {
                name: "GitHub repositories".into(),
                status: "error".into(),
                detail: String::new(),
            }];
        }
        "named-results-failed" => {
            // A whole Workstation-stage error is published under each selected setting, as
            // initial::a_stage_that_fails_as_a_whole_is_paged_under_its_rows exercises.
            status.observed = in_place();
            status.rows = [Setting::CapsLock, Setting::Dock]
                .map(|setting| SetupStatusRow {
                    name: setting.name().into(),
                    status: "error".into(),
                    detail: String::new(),
                })
                .into();
        }
        "restore-at-login" => {
            // Right after a Restore of the whole profile (§3.4): the groups it put back are
            // left as is; the Caps Lock key and the scroll direction are back as saved and load
            // at the next login, so each reads its live value until then, as `init_observe`
            // marks it, and Trackpad scrolling reads its saved value; and the page's one calm
            // line comes from the outcome such a Restore reports (`macos_doctor`'s tests hold it
            // to that), in the page's own words.
            status.plan = Some(restored(Selection::all()));
            status.finished = true;
            status.observed = with(
                in_place(),
                vec![
                    until(Setting::CapsLock, "Control", "Caps Lock"),
                    until(Setting::NaturalScrolling, "Off", "On"),
                    setting(Setting::TrackpadScrolling, Differs, Some("Some off")),
                    setting(Setting::Desktop, Differs, Some("Icons hidden")),
                    setting(Setting::Widgets, Differs, Some("Shown")),
                    setting(Setting::Dock, Differs, Some("9 apps")),
                ],
            );
            status.rows = restore_rows(&RestoreOutcome::of(
                [
                    (Setting::CapsLock, PutBack::AtLogin),
                    (Setting::NaturalScrolling, PutBack::AtLogin),
                    (Setting::TrackpadScrolling, PutBack::AtLogin),
                    (Setting::Desktop, PutBack::Back),
                    (Setting::Widgets, PutBack::Back),
                    (Setting::Dock, PutBack::Back),
                ]
                .map(|(setting, put_back)| (Some(setting), put_back)),
            ));
        }
        "mixed" => {
            // Some groups set up and some left as is, after a Restore put back the Mac's own
            // scrolling, Widgets and Dock: their rows read the values the Restore line names,
            // left as is. Permissions are set up, as they are by default: two of them, and the
            // Rust documentation in Spotlight, are still to change.
            let mut selection = Selection::all();
            selection.set(Setting::Messages, false);
            selection.set(Setting::FaceTime, false);
            status.plan = Some(restored(selection));
            let mut accessibility = access(Access::Accessibility, NotNeeded, None);
            accessibility.detail =
                Some("Only the Messages and FaceTime sign-outs need it.".to_owned());
            // Debugger access reads off as the doctor reads a managed one on an admin account:
            // waiting on setup's one early Mac password, never offered and never the person's.
            let mut debugger = access(Access::Debugger, Waiting, Some("Off"));
            debugger.note = Some(aterm_types::macos_setup::DEBUGGER_BY_ELEVATION.to_owned());
            debugger.detail =
                Some("Turning it on lets debuggers attach without a password.".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    debugger,
                    accessibility,
                    setting(Setting::NaturalScrolling, Differs, Some("On")),
                    setting(Setting::TrackpadScrolling, Differs, Some("Some off")),
                    setting(Setting::Widgets, Differs, Some("Shown")),
                    setting(Setting::Dock, Differs, Some("9 apps")),
                    setting(Setting::Messages, Differs, Some("Signed in")),
                    setting(Setting::FaceTime, Differs, Some("Signed in")),
                    setting(Setting::Spotlight, Differs, Some("Docs indexed")),
                ],
            );
            status.rows = restore_rows(&RestoreOutcome::of(
                [
                    (Setting::NaturalScrolling, PutBack::Back),
                    (Setting::TrackpadScrolling, PutBack::AtLogin),
                    (Setting::Widgets, PutBack::Back),
                    (Setting::Dock, PutBack::Back),
                ]
                .map(|(setting, put_back)| (Some(setting), put_back)),
            ));
        }
        "restore-blocked" => {
            // A Restore that put most rows back and could not put Widgets back: a failure that
            // names Restore and the row, beside the calm line for the rest, which says a value
            // macOS hid stayed as it is (nobody can change that, so it is no failure).
            status.plan = Some(restored(Selection::all()));
            status.finished = true;
            status.observed = with(
                in_place(),
                vec![
                    until(Setting::CapsLock, "Control", "Caps Lock"),
                    setting(Setting::NaturalScrolling, Differs, Some("On")),
                    setting(Setting::TrackpadScrolling, Differs, Some("Some off")),
                    setting(Setting::Dock, Differs, Some("9 apps")),
                ],
            );
            status.rows = restore_rows(&RestoreOutcome::of(
                [
                    (Setting::CapsLock, PutBack::AtLogin),
                    (Setting::NaturalScrolling, PutBack::Back),
                    (Setting::TrackpadScrolling, PutBack::AtLogin),
                    (Setting::Desktop, PutBack::Unknown),
                    (Setting::Widgets, PutBack::Failed),
                    (Setting::Dock, PutBack::Back),
                ]
                .map(|(setting, put_back)| (Some(setting), put_back)),
            ));
        }
        "restore-unlogged" => {
            // A Restore that put everything back and whose change log refused a record: one
            // calm line, no failure.
            status.plan = Some(restored(Selection::all()));
            status.finished = true;
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Desktop, Differs, Some("Icons hidden")),
                    setting(Setting::Dock, Differs, Some("9 apps")),
                ],
            );
            let mut outcome = RestoreOutcome::of(
                [
                    (Setting::Desktop, PutBack::Back),
                    (Setting::Dock, PutBack::Back),
                ]
                .map(|(setting, put_back)| (Some(setting), put_back)),
            );
            outcome.journal_refused = true;
            status.rows = restore_rows(&outcome);
        }
        "fresh" => {
            // A fresh Mac as the Plan pass reads it, every group set up: most
            // rows will change, and the repositories wait for the token the
            // vault this Mac has not joined yet holds. Each carries the
            // engine's own line in its description (`init_observe` gives a
            // row that differs none, and a blocked part's reason is that
            // the token file is not there yet): the page shows none, only
            // the waiting row's note.
            let engine = |setting: Setting, standing: Standing, now: Option<&str>, line: &str| {
                let mut row = read(Subject::Setting(setting), standing, now);
                row.detail = Some(line.to_owned());
                row
            };
            status.observed = with(
                in_place(),
                vec![
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
                            "the GitHub token file (/Users//alice/.secrets/gh_access_token) \
                             is not on this Mac yet"
                                .to_owned(),
                        );
                        repositories
                    },
                ],
            );
        }
        "queued-choice" => {
            // The person left Desktop & Dock as is while another setup (another aterm, `aterm
            // doctor` in a terminal) holds this Mac's setup records: their choice shows, and
            // waits for it.
            status.busy = true;
            status.queued = true;
            status.queued_held = true;
            let mut selection = Selection::all();
            for setting in Group::Desktop.settings() {
                selection.set(*setting, false);
            }
            status.plan = Some(Plan::from(selection));
            status.observed = with(
                in_place(),
                vec![setting(Setting::Dock, Differs, Some("9 apps"))],
            );
        }
        "queued-widen" => {
            // The automatic admission of the groups this Mac's ALab signal adds, while another
            // setup holds the records (§3.5): it waits to add the profile, which nobody chose,
            // and the plan in force sets up Permissions alone.
            status.busy = true;
            status.queued = true;
            status.queued_widen = true;
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            status.plan = Some(Plan::from(selection));
            status.observed = in_place();
        }
        "queued-restore" => {
            // Restore previous settings waits for another setup that holds the records.
            status.busy = true;
            status.queued = true;
            status.operation = Some(SetupOperation::Restore);
            status.observed = in_place();
        }
        "set-aside" | "set-aside-non-alab" => {
            // aterm could not read its saved setup and set it aside: the plan it went on with
            // leaves every Mac preference group as is and sets up aterm's permissions, and, on
            // an ALab Mac, the vault and the repositories, as a fresh install does.
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            if name == "set-aside" {
                selection.set(Setting::Secrets, true);
                selection.set(Setting::Repositories, true);
            }
            status.plan = Some(Plan::from(selection));
            status.recovered = true;
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Dock, Differs, Some("9 apps")),
                    setting(Setting::Spotlight, Differs, Some("Docs indexed")),
                ],
            );
        }
        "startup-retry" => {
            // The launch's setup could not start just now, and tries again by itself.
            status.retrying = true;
            status.rows.push(calm_setup_row(STARTUP_RETRY_NOTE));
            status.observed = with(
                in_place(),
                vec![setting(Setting::Dock, Differs, Some("9 apps"))],
            );
        }
        "restore-failed" => {
            // A Restore that failed as a whole: aterm cannot read what it saved before setup
            // changed this Mac.
            status.observed = in_place();
            status.rows = vec![SetupStatusRow {
                name: RESTORE_FINISHED.into(),
                status: "failed".into(),
                detail: RESTORE_BACKUP_UNREAD_NOTE.into(),
            }];
        }
        "setup-retried" => {
            // A setup run that did not finish as a whole, which the run tries again by itself.
            status.retrying = true;
            status.observed = in_place();
            status.rows = vec![SetupStatusRow {
                name: SETUP_ROW.into(),
                status: "failed".into(),
                detail: String::new(),
            }];
        }
        "failed-retrying" => {
            // A reading that failed for a reason that passes (GitHub stopped answering) while
            // the run still tries again by itself: it waits for that try, calmly, and its row
            // keeps its reason under the waiting chip.
            status.retrying = true;
            let mut repositories = setting(Setting::Repositories, Failed, Some("3 of 12"));
            repositories.detail = Some(GITHUB_STOPPED.to_owned());
            status.observed = with(in_place(), vec![repositories]);
        }
        "vault-unreadable" => {
            // aterm cannot read this Mac's vault settings, as `init_observe` reads them: one calm
            // note in the page's words, and the error (a path) for `aterm doctor`'s eyes alone.
            let mut vault = read(Subject::Setting(Setting::Secrets), Unknown, None);
            vault.note = Some(
                "aterm cannot read this Mac's vault settings; aterm secrets status says why"
                    .to_owned(),
            );
            vault.detail = Some(
                "~/.config/aterm/secrets/source.toml: Permission denied (os error 13)".to_owned(),
            );
            status.observed = with(in_place(), vec![vault]);
        }
        "join-refused" | "vault-not-joined" => {
            // A Join refused because aterm cannot read this Mac's vault settings (main's
            // `setup_error_row`), or the launch's Join admitted without the Join for them (main's
            // `add_vault_not_joined`): the Secrets vault row says why in its own note, which the
            // page's line would only repeat.
            status = fixture_status("vault-unreadable")?;
            status.rows = vec![calm_setup_row(if name == "join-refused" {
                VAULT_UNREAD_NOTE
            } else {
                VAULT_NOT_JOINED_NOTE
            })];
        }
        "changed-since" => {
            // The Dock was rearranged after aterm set it (init-on-by-default design §6.6), by
            // whoever did it: it reads as it is now, with no chip saying it will change or that it
            // is in place, and no automatic pass sets it again.
            let mut dock = setting(Setting::Dock, Differs, Some("12 apps"));
            dock.note = Some(aterm_types::macos_setup::CHANGED_SINCE.to_owned());
            status.observed = with(in_place(), vec![dock]);
        }
        "parked" => {
            // aterm stopped twice while cloning the repositories, so the run's ledger parked that
            // stage for its pause, or until Check again (init-on-by-default design §6.5), and the
            // others went on.
            status.observed = with(
                in_place(),
                vec![setting(Setting::Repositories, Differs, Some("3 of 12"))],
            );
            status.rows = vec![SetupStatusRow {
                name: aterm_types::macos_setup::REPOSITORIES_STAGE.name.into(),
                status: "waiting".into(),
                detail: format!(
                    "Stopped twice while cloning GitHub repositories; {}",
                    StageRow::PARKED_TAIL
                ),
            }];
        }
        "still-running" => {
            // The repositories stage passed the hour while its owner still runs it: the row
            // waits calmly and offers no Check again, which could change nothing yet.
            status = fixture_status("parked")?;
            let row = status.rows.first_mut()?;
            row.status = "running".into();
            row.detail = format!(
                "Still cloning GitHub repositories after more than an hour; {}",
                StageRow::STILL_RUNNING_TAIL
            );
        }
        "resumed" => {
            // aterm stopped while it set the Mac settings: this launch's pass goes on with them.
            status.busy = true;
            status.running = aterm_types::macos_setup::WORKSTATION_STAGE
                .settings
                .to_vec();
            status.observed = with(
                in_place(),
                vec![setting(Setting::Dock, Differs, Some("14 apps"))],
            );
            status.rows = vec![SetupStatusRow {
                name: aterm_types::macos_setup::WORKSTATION_STAGE.name.into(),
                status: "running".into(),
                detail: StageRow::RESUMING.into(),
            }];
        }
        "vault-held" => {
            // Two vault tries in a row did not take: aterm tries again by itself, less often, and
            // at once when this Mac's network or vault changes, so the row waits, calm, saying so.
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Secrets, Differs, Some("Not joined")),
                    token_waiting(),
                ],
            );
            status.rows = vec![SetupStatusRow {
                name: Setting::Secrets.name().into(),
                status: "error".into(),
                detail: TRIED_TWICE.into(),
            }];
        }
        "newer" => {
            // A newer aterm set this Mac up: this aterm names no plan, reads the rows as they
            // stand with nothing set up (no account and no network read but on Check again),
            // and changes nothing.
            status.plan = None;
            status.rows = vec![calm_setup_row(NEWER_SETUP_NOTE)];
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::Dock, Differs, Some("9 apps")),
                    read(Subject::Setting(Setting::Messages), Unknown, None),
                    read(Subject::Setting(Setting::FaceTime), Unknown, None),
                    read(Subject::Setting(Setting::Repositories), Unknown, None),
                ],
            );
        }
        "restoring" => {
            // Restore previous settings, which waited for another setup, now holds the records
            // and puts them back.
            status.busy = true;
            status.operation = Some(SetupOperation::Restore);
            status.observed = in_place();
        }
        "older-cancel" => {
            // A fresh Mac whose saved setup holds a Cancel an older aterm's first-launch review
            // recorded (§3.3): the worker's plan has every group turned off, so each reads Leave
            // as is and turns back on by its switch, with no word of a review. A grant that would
            // need a person once Permissions run asks nothing of anyone while they are left as is.
            status = fixture_status("fresh")?;
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Not confirmed"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            status.observed = with(status.observed, vec![fda]);
            status.plan = Some(Plan::from(Selection::none()));
            status.cancelled = true;
        }
        "clt-queued" | "clt-password" | "clt-recovered" | "clt-installing" | "clt-missing"
        | "clt-apple-window" | "clt-not-opened" | "clt-low-data" | "clt-not-admin" => {
            // The GitHub repositories wait on Apple's Command Line Tools (§3.3), which the
            // window installs in a terminal tab of its own: the tab asking for the password
            // (`clt-password`), its restored live tab (`clt-recovered`), an install running
            // (`clt-installing`), the tools missing with no tab open and nothing installing them
            // (`clt-missing`), where the row offers Install…, Apple's own installer window
            // waiting for the person's press (`clt-apple-window`), a press of Install… queued
            // behind a macOS permission step (`clt-queued`), a press whose tab did not open
            // because the page was left (`clt-not-opened`), Low Data Mode holding the download
            // back (`clt-low-data`), and an account that may not install them (`clt-not-admin`).
            let repositories = setting(Setting::Repositories, NeedsYou, None);
            status.observed = with(in_place(), vec![repositories]);
            status.tools_owed = true;
            status.developer_tools = match name {
                "clt-queued" => Some(ToolsInstall::Queued),
                "clt-password" => Some(ToolsInstall::Password),
                "clt-recovered" => Some(ToolsInstall::Recovered),
                "clt-installing" => Some(ToolsInstall::Installing),
                "clt-apple-window" => Some(ToolsInstall::AppleWindow),
                "clt-not-opened" => Some(ToolsInstall::NotOpened(NotOpened::LeftPage)),
                "clt-low-data" => Some(ToolsInstall::LowData),
                "clt-not-admin" => Some(ToolsInstall::NotAdmin),
                "clt-coming" => Some(ToolsInstall::Coming),
                _ => None,
            };
        }
        _ => return None,
    }
    status.observed_generation = 1;
    Some(status)
}

#[cfg(test)]
mod page_tests;
#[cfg(test)]
mod tests;
