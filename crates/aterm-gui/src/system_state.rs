// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The System State tab (`docs/DESIGN-system-state-2026-10-21.md` §3): what
//! this Mac reads now beside what aterm wants, in five groups. A group's
//! `Set up │ Leave as is` choice takes effect at once. First launch admits
//! its offer automatically at the first unblocked tick with a person present.

mod paint;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use aterm_grapheme::GraphemeClusters;
use aterm_secrets::SecretBytes;
use aterm_types::macos_setup::{
    Access, FailureKind, Group, Observation, PassOutcome, Plan, PutBack, RestoreOutcome, Selection,
    Setting, Standing, Subject, TRIED_TWICE, Unfinished,
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
use crate::workstation_setup::{OfferKind, Request, SetupAnswer, WorkstationSetupDecision};

pub(crate) use paint::{BrandSpec, Chip, paint_brand, paint_chip, paint_meter};

/// A non-secret snapshot published by setup's worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SetupStatus {
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
    /// The vault source the Secrets and Repositories readings describe.
    /// Unknown sources never supply those rows to a review.
    pub observed_vault_url: Option<String>,
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
    /// The run going now is the one aterm admitted by itself on hearing the ALab vault in this
    /// launch, which widened the plan it had chosen before (§3.5): the page says so in one calm
    /// note while it goes. Filled by the worker, from the look that widened the plan.
    pub widened: bool,
    /// The worker's read met a write of the saved setup under way (a pass saving its progress,
    /// a Stop saving its request): it says nothing of the plan or its Permissions group, which
    /// a read a moment later does. Filled by the worker.
    pub changing: bool,
    /// A person's press waits for another setup that holds this Mac's setup records (another
    /// aterm, `aterm doctor` in a terminal): their choice, or Restore previous settings while
    /// `operation` says so, which aterm applies when that setup finishes (§3.3, §3.4). The page
    /// keeps the choice showing, marked waiting, and says why in one calm note: nothing failed,
    /// so nothing is signalled, and no run goes, so there is no meter and no Stop. Filled by
    /// the worker.
    pub queued: bool,
    /// The plan in force is the one aterm set up afresh after it set aside a saved setup it
    /// could not read (§3.5): every Mac preference group left as is, aterm's own permissions
    /// set up. The page says so in one calm note until a person chooses. Filled by the worker.
    pub recovered: bool,
    /// A person's Cancel of the first-launch review stands (§3.3): every group reads Leave as
    /// is, nothing runs, and the page says so in one calm line beside Set up now, which applies
    /// the plan the review showed ([`CANCELLED_NOTE`]). Filled by the worker.
    pub cancelled: bool,
    /// The person closed this launch's review unanswered and no run is admitted (§3.3): the
    /// page shows the plan the review showed with Set up now, calm, and lights nothing for the
    /// permissions nobody approved: no Needs you, no diamond, no menu line. Filled by the worker.
    pub unanswered: bool,
    /// Where an install of Apple's Command Line Tools stands, which the GitHub repositories row
    /// waits on while they are missing (§3.3): its tab asks for the person's password, was
    /// restored, or the installer runs. `None`: none, so the row's Install… opens the tab. Filled by
    /// the window (`App::developer_tools_install_step`), never by the worker.
    pub developer_tools: Option<ToolsInstall>,
}

/// An install of Apple's Command Line Tools under way (§3.3), as the window sees it
/// ([`SetupStatus::developer_tools`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolsInstall {
    /// Its terminal tab waits for the person's password: they are needed now, and typing it is
    /// the one thing that changes the outcome.
    Password,
    /// A live installer tab was restored. Its exact prompt is not known: the person continues
    /// there, or closes it before retrying, and no second installer is opened.
    Recovered,
    /// The installer runs (its on-demand placeholder, or Apple's own installer, is there): the
    /// row waits, calmly, and moves on once the tools arrive.
    Installing,
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
    /// says it (§3.6), never the headline, the menu line or the tab's attention.
    pub(crate) check: bool,
    /// What kind of failure it is (§3.6): one naming a row whose reading needs a person is that
    /// person's, which the row and the headline say as needing you, never as a failure or a
    /// wait; any other is transient, which aterm tries again while its tries last.
    pub(crate) kind: FailureKind,
    /// aterm tries it again by itself (a transient failure whose words say so, or one the run
    /// still retries and whose tries are not spent): the page counts it as waiting (§3.2) and
    /// nothing signals it, since nobody needs to act.
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
    /// A setup run is going: busy with neither a Check nor a Restore, and no press waiting for
    /// another setup ([`Self::queued`]).
    pub(crate) fn running(&self) -> bool {
        self.busy && self.operation.is_none() && !self.queued
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

    /// Whether a reading that failed is a signal now (§3.6): never while the run still
    /// retries (§3.5, [`Self::retrying`]), when the page alone says it, as it waits for the next
    /// try. A reading carries no kind, so it is transient while the tries last; a step's own
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
    /// names, if one: failures, which the menu line and the tab's attention
    /// say as they say a row that failed (§3.6), besides the page's notes,
    /// once nothing tries them again by itself (the run's retries, or a vault try on its own
    /// pause, [`Failure::retried`]) and no row they name needs a person
    /// ([`Failure::failed`]). A step naming a row `plan` no longer manages is
    /// left out. A Check's results are never among them: a Check only reads,
    /// and pressing it again is its remedy, which the page's note says. A Restore that failed
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
    /// ([`FailureKind::of_status`]), and the words it carries say so, or the run still retries
    /// and its words do not say its own tries are spent (a vault try held after two,
    /// [`TRIED_TWICE`]). Meanwhile it counts as waiting, and Set up now is no remedy the page
    /// offers for it ([`Said::words`]).
    fn retries_by_itself(&self, row: &SetupStatusRow) -> bool {
        FailureKind::of_status(&row.status).is_some_and(FailureKind::retried)
            && (row.detail == RETRIES_NOTE || (self.retrying && row.detail != TRIED_TWICE))
    }

    /// The reason a step the last run could not finish gives for `row` (§3.6): its words, in the
    /// page's terms (`The change did not take.`, `GitHub can't be reached`), which the row keeps
    /// as its note where its reading gives none. Never a Check's (a read, whose note says to
    /// press it again), a Restore's, a whole sentence said alone ([`Said::whole`]), or the words
    /// of what comes next a vault try carries ([`RETRIES_NOTE`], [`TRIED_TWICE`]).
    fn step_reason<'a>(&'a self, plan: &'a Plan, row: Subject) -> Option<&'a str> {
        self.results(Some(plan))
            .find(|result| {
                !result.check
                    && subject_named(result.name) == Some(row)
                    && matches!(result.row.status.as_str(), "error" | "failed")
                    && !result.whole()
                    && !result.row.detail.trim().is_empty()
                    && result.row.detail != RETRIES_NOTE
                    && result.row.detail != TRIED_TWICE
            })
            .map(|result| result.row.detail.as_str())
    }

    /// The notes the page shows under its headline, in the page's own words
    /// (§3.6): first, while a run aterm widened by itself goes, why its groups
    /// changed ([`WIDENED_NOTE`]); then the results an observation cannot say
    /// and someone should know (an action that did not start, a step that
    /// failed or could not be confirmed, a restore that finished), where the
    /// results said alike (a stage's rows, all failed) are one note naming
    /// each row; then one calm line naming every setting that is back as
    /// saved but loads only at the next login (`login`). That line is no
    /// failure: [`Self::failed_steps`], the menu line and the tab's attention
    /// leave it out.
    fn notes(&self, plan: &Plan) -> Vec<String> {
        let mut notes = Vec::new();
        if self.cancelled {
            notes.push(CANCELLED_NOTE.to_owned());
        }
        if self.queued {
            notes.push(self.queued_note().to_owned());
        }
        if self.recovered {
            notes.push(self.recovered_note(plan));
        }
        if self.widened && self.running() {
            notes.push(WIDENED_NOTE.to_owned());
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

/// The calm note a run aterm widened by itself carries while it goes (§3.5): it heard this
/// Mac's ALab signal (the vault, or vault or fleet membership), so the groups it had left as
/// is are set up now. It names that fact as [`ALAB_ADDED_NOTE`] does.
pub(crate) const WIDENED_NOTE: &str =
    "This Mac is an ALab Mac, so aterm is setting up its profile.";

/// The calm line while a person's Cancel of the first-launch review stands
/// ([`SetupStatus::cancelled`]), beside Set up now.
pub(crate) const CANCELLED_NOTE: &str =
    "Setup was cancelled. Set up now applies the setup the review showed.";

/// The headline while the review a person closed this launch waits for an answer
/// ([`SetupStatus::unanswered`]), beside Set up now.
pub(crate) const NOT_STARTED: &str = "Setup has not started";

/// The calm line while a review shows the profile this Mac's ALab signal added (§3.5).
pub(crate) const ALAB_ADDED_NOTE: &str = "This Mac is an ALab Mac, so its profile was added.";

/// The calm note while a person's choice waits for another setup ([`SetupStatus::queued`]).
pub(crate) const QUEUED_CHOICE_NOTE: &str =
    "Another setup is running; aterm applies your choice when it finishes.";

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

    /// The note for the press waiting for another setup: a Restore's while `operation` says so,
    /// else a choice's.
    fn queued_note(&self) -> &'static str {
        if self.operation == Some(SetupOperation::Restore) {
            QUEUED_RESTORE_NOTE
        } else {
            QUEUED_CHOICE_NOTE
        }
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
/// a fact for later, never an alert, the menu line or the tab's attention. `detail` is
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
/// trying a step again by itself (§3.5): a fact for later, never an alert, the menu line or the
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

/// The exceptions digest's entry for settings a person changed after this setup had them as
/// wanted: setup leaves them as they are (no automatic pass rewrites a row that read back ok), a
/// fact for later, never an alert, whose one navigation opens System State to set them again.
/// `rows` names them, which the log keeps.
#[must_use]
pub fn setup_changed_since_entry(rows: &str) -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "changed-since",
        "Settings changed since setup",
        Why::OnlyYou(Reason::new(
            "settings this Mac's setup had set were changed since, and setup leaves them as they are",
        )),
        Then::step("Open System State and Set up now to set them again"),
    )
    .show(aterm_messages::Intent::ReviewVaultSync)
    .log_line(format!(
        "System State: changed since setup set them, left as they are: {rows}"
    ))
}

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

/// The exceptions digest's entry for setup an update successor did not offer (§3.3): an update
/// may relaunch aterm with nobody at this Mac, so a setup nobody reviewed waits for the person,
/// a fact for later whose one navigation opens System State, where they review and apply it.
#[must_use]
pub fn setup_unfinished_entry() -> aterm_messages::exceptions::Entry {
    use aterm_messages::need::{Reason, Then, Why};
    aterm_messages::exceptions::Entry::new(
        aterm_messages::exceptions::Family::Setup,
        "",
        "unfinished",
        "Mac setup is unfinished",
        Why::OnlyYou(Reason::new(
            "this Mac's setup needs your review, and an update relaunched aterm without it",
        )),
        Then::step("Open System State to review and apply it"),
    )
    .show(aterm_messages::Intent::ReviewVaultSync)
    .log_line("System State: an update relaunched aterm; setup waits for its review")
}

/// Raise [`setup_unfinished_entry`] in the exceptions digest.
#[cfg(unix)]
pub fn note_setup_unfinished() {
    crate::message_inbox::queue_exception(setup_unfinished_entry());
}

/// A setup run was admitted, or the review is on the page: [`setup_unfinished_entry`] leaves the
/// exceptions digest.
#[cfg(unix)]
pub fn clear_setup_unfinished() {
    crate::message_inbox::queue_exception_clear(setup_unfinished_entry().key());
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

/// The headline while the first launch's offer waits for the page's own answer, which it gives
/// on its first tick with a person at this Mac (design revision 4: no review, no countdown, no
/// confirmation): the only signal it gives; no diamond, no menu line.
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

/// What the page says after `Check again failed.`: the rows keep their last readings meanwhile,
/// and the button is its own remedy, said calmly. The log keeps why.
pub const CHECK_FAILED_NOTE: &str =
    "The rows show what aterm read before; pressing it again reads them anew.";

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
/// terms: no failure, so no failure headline, menu line or tab attention (§3.6).
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

/// What a failed step's note says once the run no longer retries it: the headline's Set up now
/// runs it again (§3.2).
pub(crate) const SET_UP_NOW_NOTE: &str = "Set up now tries again.";

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
    /// The first-run offer's automatic answer while a person is at this Mac.
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
    /// The launch's offer, answered by the page itself as the plan stands (design revision 4):
    /// a provisional plan stays provisional, and a vault address or password being typed stays
    /// a draft for Join, so no credential goes without a press.
    Offer,
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
    /// The GitHub repositories row's Install…: the terminal tab that installs Apple's Command
    /// Line Tools opens now (`App::press_developer_tools_install`).
    InstallDeveloperTools { by_person: bool },
}

/// The key of the Secrets vault row's Turn On Sync button.
const TURN_ON_SYNC: &str = "system-state/row/secrets/sync-on";

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
    /// and nothing installs them, so a press opens their terminal tab (§3.3).
    pub(crate) install_tools: bool,
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

/// Whether the selected settings manage this row. Accessibility belongs to
/// the account sign-outs; the other grants belong to Permissions.
pub(crate) fn manages(selection: Selection, subject: Subject) -> bool {
    match subject {
        Subject::Access(Access::Accessibility) => Access::Accessibility.needed_for(selection),
        Subject::Access(access) => {
            selection.contains(Setting::Permissions) && access.needed_for(selection)
        }
        Subject::Setting(setting) => selection.contains(setting),
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
    let name = subject.name();
    let reading = status
        .observations_for(vault_url)
        .find(|observation| observation.subject == subject);
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
    // takes (the SamplingTools dialog), why the row is not needed, the
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
            install_tools: false,
            standing,
            label: format!("{name}: not needed"),
        };
    }
    let busy = status.running.contains(&subject.setting())
        && match subject {
            Subject::Access(access) => status.access_step.is_none_or(|step| step == access),
            Subject::Setting(_) => true,
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
    // While macOS's administrator dialog for debugger access is up, or about to be (the
    // window's debugger step), a press of Turn On… can start nothing: the row says what it
    // waits for in place of the button (§3.4), and still needs the person, whose dialog may
    // be behind other windows.
    let asking = subject == Subject::Access(Access::Debugger)
        && status.access_step == Some(Access::Debugger);
    // The review reads a row that will need a person as one that will change (§3.3).
    let standing = match standing {
        Some(Standing::NeedsYou) if review => Some(Standing::Differs),
        standing => standing,
    };
    // Apple's Command Line Tools, which the repositories wait on (§3.3): an install running is a
    // wait that ends by itself, its tab asking for the password needs the person now, and
    // missing tools nothing installs offer Install….
    let tools = (!review && subject == Subject::Setting(Setting::Repositories))
        .then(|| status.tools_wait(reading))
        .flatten();
    let standing = if tools == Some(ToolsWait::Installing) {
        note = Some(INSTALLING_TOOLS_NOTE.to_owned());
        Some(Standing::Waiting)
    } else {
        standing
    };
    let open = match subject {
        Subject::Access(access) if standing == Some(Standing::NeedsYou) && !asking => Some(access),
        _ => None,
    };
    // A setting read in place at a value other than aterm's is the person's
    // opt-out (the vault's Sync off): its row turns it back on, quietly, unless it reads as what
    // Apply will do, which acts on nothing yet.
    let turn_on_sync = !review
        && subject == Subject::Setting(Setting::Secrets)
        && standing == Some(Standing::InPlace)
        && wanted != subject.wanted();
    // A step the last run said of this row (§3.6): one the page says failed takes the failed
    // chip, though the row reads in place, and one aterm tries again by itself the waiting one,
    // as the headline and the village say them. A step whose row needs a person says nothing of
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
            Some(Chip::WillChange { busy })
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
        // macOS tells aterm nothing of it, and nothing in this launch can read it (Local
        // Network): `—`, its probe's one calm note, and no check claimed, so it never holds the
        // headline at a check nothing is making (§3.2).
        Some(Standing::Untold) => (
            RowValue::One("—".to_owned()),
            None,
            format!("{name}: unknown"),
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
            note = if asking {
                Some(PASSWORD_NOTE.to_owned())
            } else if tools == Some(ToolsWait::Password) {
                Some(TOOLS_PASSWORD_NOTE.to_owned())
            } else if tools == Some(ToolsWait::Recovered) {
                Some(TOOLS_RECOVERED_NOTE.to_owned())
            } else {
                explained()
            };
            (
                change(),
                Some(Chip::NeedsYou),
                format!("{name}: {spoken_from}, needs you"),
            )
        }
        // A reading that failed keeps its reason in its note. While the run still retries it
        // waits for the next try (§3.5), as the headline and the village say: the waiting chip,
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
                (
                    change(),
                    Some(Chip::WillChange { busy }),
                    format!("{name}: {spoken_from}, {RETRY_WORDS}"),
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
                RETRY_WORDS
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
        install_tools: tools == Some(ToolsWait::Missing),
        standing,
        label,
    }
}

/// What a screen reader hears of a row whose step aterm tries again by itself (§3.5), after its
/// value: the waiting chip's words would say it will change, which is the next try's to say.
const RETRY_WORDS: &str = "waiting for aterm to try again";

/// The note of a waiting row whose probe gave no reason: it is blocked on
/// something, and nothing more can be said of it.
pub(crate) const WAITING_NOTE: &str = "Can't be set up yet";

/// The note of the Debugger access row while macOS's administrator dialog for it is up, which
/// takes the place of its Turn On… button: nothing on the page moves it, and the dialog may be
/// behind other windows.
pub(crate) const PASSWORD_NOTE: &str =
    "Waiting for your administrator password in the SamplingTools dialog";

/// The headline while the Command Line Tools tab asks for the person's password
/// ([`ToolsInstall::Password`]): they are needed there now (§3.2).
pub(crate) const TOOLS_PASSWORD_HEADLINE: &str =
    "Enter your password in the Command Line Tools tab";

/// The GitHub repositories row's note while the Command Line Tools tab asks for the password,
/// which takes the place of its Install… button.
pub(crate) const TOOLS_PASSWORD_NOTE: &str =
    "Waiting for your password in the Command Line Tools tab";

/// The headline for a restored live installer tab: its command may not have started before
/// recovery, so no particular prompt is claimed.
pub(crate) const TOOLS_RECOVERED_HEADLINE: &str = "Continue in the Command Line Tools tab";

/// The recovered tab remains the place to continue, or the person closes it before retrying.
pub(crate) const TOOLS_RECOVERED_NOTE: &str =
    "Installer tab restored. Continue there, or close it to retry.";

/// The GitHub repositories row's calm note while Apple's Command Line Tools install
/// ([`ToolsInstall::Installing`]): nobody needs to act, and the row moves on once they arrive.
pub(crate) const INSTALLING_TOOLS_NOTE: &str = "Installing Apple's Command Line Tools…";

/// The GitHub repositories row's button while Apple's Command Line Tools are missing and nothing
/// installs them: it opens their terminal tab, whose one password prompt installs them.
pub(crate) const INSTALL_TOOLS: &str = "system-state/row/repositories/install";

/// Where the GitHub repositories row stands on Apple's Command Line Tools, which every clone
/// runs (§3.3), once its reading needs a person to install them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolsWait {
    /// Nothing installs them: the row's Install… opens their tab.
    Missing,
    /// Their tab asks for the person's password.
    Password,
    /// Their live tab was restored; no particular prompt is known.
    Recovered,
    /// The installer runs.
    Installing,
}

impl SetupStatus {
    /// [`ToolsWait`] for `reading`, the GitHub repositories row's: `None` unless it needs a person
    /// for want of Apple's Command Line Tools ([`aterm_types::macos_setup::COMMAND_LINE_TOOLS_MISSING`],
    /// typed where the wait is decided).
    fn tools_wait(&self, reading: Option<&Observation>) -> Option<ToolsWait> {
        let reading = reading?;
        (reading.standing == Standing::NeedsYou
            && reading.note.as_deref()
                == Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_MISSING))
        .then_some(match self.developer_tools {
            None => ToolsWait::Missing,
            Some(ToolsInstall::Password) => ToolsWait::Password,
            Some(ToolsInstall::Recovered) => ToolsWait::Recovered,
            Some(ToolsInstall::Installing) => ToolsWait::Installing,
        })
    }

    /// [`Self::tools_wait`] of the GitHub repositories reading for the plan in force.
    pub(crate) fn repositories_tools_wait(&self) -> Option<ToolsWait> {
        let repositories = Subject::Setting(Setting::Repositories);
        self.tools_wait(
            self.observations_for(self.plan.as_ref().map(|plan| plan.vault_url.as_str()))
                .find(|observation| observation.subject == repositories),
        )
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
            // A reading macOS answers nothing for (Local Network) is unknown, as on the managed
            // page: no read is still to come.
            let label = match (
                now.as_deref(),
                reading.map(|observation| observation.standing),
            ) {
                (Some(now), _) => format!("{}: {}", subject.name(), now.replace(" · ", " ")),
                (None, Some(Standing::Untold)) => format!("{}: unknown", subject.name()),
                (None, _) => format!("{}: not read", subject.name()),
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
                label,
            }
        })
        .collect()
}

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

    /// The headline while a person's press waits for another setup ([`SetupStatus::queued`]).
    fn queued(status: &SetupStatus) -> Self {
        Self::line(if status.operation == Some(SetupOperation::Restore) {
            "Waiting to restore previous settings…"
        } else {
            "Waiting to apply your choice…"
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
    if status.access_step == Some(Access::Debugger) {
        return Headline::line(format!(
            "Enter your password for {}",
            Access::Debugger.name()
        ));
    }
    // A review closed unanswered (§3.3): nothing was approved, so no row needs anyone yet.
    if status.unanswered && !status.busy {
        return Headline {
            start: true,
            ..Headline::line(NOT_STARTED)
        };
    }
    // The Command Line Tools tab needs the person (§3.3): at its known password prompt, or
    // continuing its restored session without claiming which prompt it has reached.
    let repositories = Subject::Setting(Setting::Repositories);
    if rows.iter().any(|row| {
        row.subject == repositories && row.managed && row.standing == Some(Standing::NeedsYou)
    }) {
        match status.tools_wait(
            status
                .observations_for(plan.map(|plan| plan.vault_url.as_str()))
                .find(|observation| observation.subject == repositories),
        ) {
            Some(ToolsWait::Password) => return Headline::line(TOOLS_PASSWORD_HEADLINE),
            Some(ToolsWait::Recovered) => return Headline::line(TOOLS_RECOVERED_HEADLINE),
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
        // stage that failed, a vault try held after two), whose note says Set up now tries it
        // again. A reading that failed by itself names its own remedy in its note, which Set up
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
        return Headline::waiting(&waiting);
    }
    // Readings still to come, or a saved setup aterm reads again by itself, whose calm line
    // says so: nothing is claimed of a plan the worker could not read just now.
    if status.observed.is_empty() || status.plan_unread() {
        return Headline::line(CHECKING);
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
    let waiting = steps.waiting(rows);
    if !waiting.is_empty() {
        return Headline::waiting(&waiting);
    }
    let managed = rows.iter().filter(|row| row.managed).count();
    if managed == 0 {
        // After a person's Cancel, Set up now applies the plan the review showed (§3.3).
        return Headline {
            start: status.cancelled,
            ..Headline::line("Everything is left as is")
        };
    }
    // A reading that ended without an answer (its reader unwound) says so in
    // its note: no check is under way for it, so the headline names it and
    // shows it, where its note says to Check again.
    let unread: Vec<Subject> = rows
        .iter()
        .filter(|row| row.managed && row.standing == Some(Standing::Unknown) && row.note.is_some())
        .map(|row| row.subject)
        .collect();
    if let Some(first) = unread.first() {
        let rows = if unread.len() == 1 { "row" } else { "rows" };
        let names: Vec<&str> = unread.iter().map(|subject| subject.name()).collect();
        return Headline {
            show: Some(*first),
            ..Headline::line(format!(
                "{} {rows} could not be read: {}",
                unread.len(),
                names.join(", ")
            ))
        };
    }
    // A source change can leave just its managed rows unread. Known changes
    // and waits above stay useful, but unread rows cannot establish success.
    // A row macOS tells nothing of (`Untold`) was read: nothing in this launch
    // reads more of it, so it never holds the page at a check.
    if rows
        .iter()
        .any(|row| row.managed && matches!(row.standing, None | Some(Standing::Unknown)))
    {
        return Headline::line(CHECKING);
    }
    // The headline claims no more than was confirmed: a row whose change, or whose Check, could
    // not be confirmed is counted, and shown, where its note says so; a Check that failed as a
    // whole leaves every row as aterm read it before, which the headline says.
    if let Some(first) = steps.unconfirmed.first() {
        let count = if steps.unconfirmed.len() == 1 {
            "row"
        } else {
            "rows"
        };
        let names: Vec<&str> = steps
            .unconfirmed
            .iter()
            .map(|subject| subject.name())
            .collect();
        return Headline {
            show: Some(*first),
            ..Headline::line(format!(
                "{} {count} not confirmed: {}",
                steps.unconfirmed.len(),
                names.join(", ")
            ))
        };
    }
    let only_permissions = rows
        .iter()
        .filter(|row| row.managed)
        .all(|row| row.subject.group() == Group::Access);
    let set_up = if only_permissions {
        "Permissions are set up"
    } else {
        "This Mac is set up"
    };
    Headline::line(if steps.check_failed {
        format!("{set_up}, {AS_LAST_READ}")
    } else {
        set_up.to_owned()
    })
}

/// What a set-up headline adds after a Check that failed as a whole: the rows show what aterm
/// read before ([`CHECK_FAILED_NOTE`]).
const AS_LAST_READ: &str = "as last read";

/// Where the steps the last run and Check said stand, against the page's rows (§3.2, §3.6):
/// what the headline names failed, counts as waiting or as not confirmed, and the village
/// draws ([`village_spec`]).
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
        // reason on its row; once the tries stop it failed (§3.5, §3.6).
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

/// THE VILLAGE RULE (pure): the scene
/// [`crate::settings::paint_rainbow_kitty_village`] draws beside the lockup,
/// one station per [`Setting`] in its order, in the painter's letters: `_`
/// left as is, `E` a managed row needs a person or failed (a reading, or a
/// step, the headline names failed), `W` running now, `D` a step aterm tries
/// again by itself, which waits for that try, or a row a run or a Check could
/// not confirm, `G` every managed row in place, `D` otherwise (it differs,
/// waits or is not read yet). `done` and `total`
/// are the meter's count of managed rows. `plan` is the page's review, whose
/// selection says what is left as is.
pub(crate) fn village_spec(
    status: &SetupStatus,
    rows: &[RowView],
    plan: &Plan,
    phase_ms: u64,
) -> String {
    let steps = Steps::of(status, rows, Some(plan));
    let of = |subjects: &[Subject], setting: Setting| {
        subjects.iter().any(|subject| subject.setting() == setting)
    };
    let states: String = Setting::ALL
        .iter()
        .map(|&setting| {
            let mut managed = rows
                .iter()
                .filter(|row| row.managed && row.subject.setting() == setting);
            if !plan.selection.contains(setting) {
                '_'
            } else if managed
                .clone()
                .any(|row| row.standing == Some(Standing::NeedsYou))
                || of(&steps.failed, setting)
            {
                'E'
            } else if status.running.contains(&setting) {
                'W'
            } else if of(&steps.retried, setting) || of(&steps.unconfirmed, setting) {
                'D'
            } else if managed.all(RowView::settled) {
                'G'
            } else {
                'D'
            }
        })
        .collect();
    let total = rows.iter().filter(|row| row.managed).count();
    let done = rows.iter().filter(|row| row.settled()).count();
    // Only a setup run walks: Check again and Restore are busy, and no run.
    let busy = u8::from(status.running());
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
    /// The list's height (its room and the strip under it), its room and
    /// how far it can scroll, as last rendered.
    height: Cell<f32>,
    room: Cell<f32>,
    limit: Cell<f32>,
    /// The revealable keys the last render placed in the list.
    spans: RefCell<Vec<Span>>,
    /// The last render of this view showed a village whose kitties walk.
    village_walks: Cell<bool>,
}

impl SystemStateViewState {
    /// Whether this view's last render showed a village whose kitties walk.
    /// Each view renders at its own window's size, so one view's village can
    /// show while another's is left out: a busy tick repaints the page only
    /// where a view someone sees draws its village walking
    /// ([`SystemStateApp::tick`]).
    pub(crate) fn village_walks(&self) -> bool {
        self.village_walks.get()
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
    /// The person's own disclosure of the vault fields; `None` follows the row.
    vault_open: Option<bool>,
    status: SetupStatus,
    request: Option<Request>,
    /// The offer in `request` waits for the page's own answer, which the page gives on its
    /// first tick with a person at this Mac ([`Self::tick`]): no review, no countdown and
    /// no confirmation (design revision 4). Only a person at the Mac changes it (SAFE-1).
    answer_waits: bool,
    /// The launch's offer shows the plan this Mac's ALab signal widened it to while it waited
    /// (§3.5).
    review_widened: bool,
    actions: VecDeque<SystemStateAction>,
    /// Whether the gesture being handled is a person's (not a control client's).
    by_person: bool,
    /// Page ticks seen while a run goes and the page is shown: the village's
    /// walk, and its only clock, so idle kitties stand still.
    anim_ticks: u64,
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

    pub(crate) fn cancel_preedit(&mut self, key: Option<&UiKey>) -> bool {
        if key.is_some_and(|key| key.as_str() == "system-state/password") {
            return std::mem::take(&mut self.password.composing);
        }
        if key.is_some_and(|key| key.as_str() == "system-state/url") && self.url.preedit().is_some()
        {
            self.url.cancel_preedit();
            return true;
        }
        false
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
            answer_waits: false,
            review_widened: false,
            actions: VecDeque::new(),
            by_person: false,
            anim_ticks: 0,
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
        // The review of the groups the ALab signal adds keeps the plan in force as the one the
        // worker holds: a Cancel leaves its groups as they are (§3.5). The vault it offers is the
        // worker's, never a draft of the person's.
        self.submitted = match request.kind() {
            OfferKind::Launch => self.plan.clone(),
            OfferKind::Widen { in_force } => Plan {
                selection: in_force.selection,
                vault_url: self.plan.vault_url.clone(),
            },
        };
        self.url = TextInputState::new(self.plan.vault_url.clone());
        self.password = Password::default();
        self.status = SetupStatus::default();
        self.request = Some(request);
        self.answer_waits = true;
        self.review_widened = false;
        true
    }

    /// Whether the review up is the one of the groups the ALab signal adds (§3.5).
    fn reviewing_widen(&self) -> bool {
        self.reviewing()
            && self
                .request
                .as_ref()
                .is_some_and(|request| matches!(request.kind(), OfferKind::Widen { .. }))
    }

    /// The worker heard this Mac's ALab signal while the launch's offer waits for its answer
    /// (§3.5): the offer widens to `widened` in place, and the page says why
    /// ([`ALAB_ADDED_NOTE`]); a vault address or password being typed stays as it is, unwidened.
    /// Whether the page changed.
    fn widen_review(&mut self, widened: Plan) -> bool {
        if !self.reviewing()
            || self.reviewing_widen()
            || self.plan != self.submitted
            || self.vault_draft()
        {
            return false;
        }
        self.url = TextInputState::new(widened.vault_url.clone());
        self.plan = widened.clone();
        self.submitted = widened;
        self.review_widened = true;
        true
    }

    /// The calm lines over the groups: why a review shows the ALab profile, then the
    /// status's own ([`SetupStatus::notes`]).
    fn notes(&self, plan: &Plan) -> Vec<String> {
        let mut notes = Vec::new();
        if self.review_widened || self.reviewing_widen() {
            notes.push(ALAB_ADDED_NOTE.to_owned());
        }
        notes.extend(self.status.notes(plan));
        notes
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
        // A vault that comes to wait for input here (a join password an admin Mac refused)
        // opens its fields again, even where the person closed them on an earlier reading.
        if vault_waits_for_input(&status) && !vault_waits_for_input(&self.status) {
            self.vault_open = None;
        }
        self.status = status;
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
    /// itself, as the plan stands, on the first tick a person is at this Mac (`present`: a
    /// keyboard, mouse or trackpad used within the last minute, and the screen neither locked
    /// nor switched away), focused, shown or not, whatever was clicked or typed meanwhile:
    /// no review, no countdown, no confirmation (design revision 4); a status that says the page
    /// is busy holds it until it is not. While a setup run goes and the page is `shown` (the
    /// visible view of a window), the village's kitties walk a step. `walks` says whether a shown
    /// view's last render showed a walking village ([`SystemStateViewState::village_walks`]).
    /// `true` when the page changed: a page nobody sees, one Check again or Restore keeps busy,
    /// or one whose shown views draw no walking village owes no repaint for the walk.
    pub(crate) fn tick(&mut self, present: bool, shown: bool, walks: bool) -> bool {
        let before = (self.request.is_some(), self.status.busy, self.reviewing());
        if self
            .request
            .as_ref()
            .is_some_and(|request| !request.pending())
        {
            self.request = None;
            self.answer_waits = false;
            self.review_widened = false;
        }
        let widened = self
            .request
            .as_ref()
            .and_then(Request::take_widened)
            .is_some_and(|widened| self.widen_review(widened));
        if self.reviewing() && present && !self.status.busy {
            self.apply(Admission::Automatic, Submission::Offer, None);
        }
        let walked = self.status.running() && shown && {
            self.anim_ticks = self.anim_ticks.wrapping_add(1);
            walks
        };
        walked || widened || before != (self.request.is_some(), self.status.busy, self.reviewing())
    }

    pub(crate) fn pending(&self) -> bool {
        self.request.as_ref().is_some_and(Request::pending)
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
            self.reads_as_review(subject),
        )
    }

    /// Whether `subject`'s row reads as what setup, or Set up now, will do (§3.3): every row
    /// while the first launch's offer waits for its answer, or after it was closed unanswered
    /// (nothing was approved, so nothing waits on anyone yet), and, in the offer of the groups
    /// the ALab signal adds (§3.5), the rows those groups add. A row the plan in force manages
    /// keeps its real reading there, with its buttons and signals: that offer changes none of it.
    fn reads_as_review(&self, subject: Subject) -> bool {
        if self.status.unanswered {
            return true;
        }
        if !self.reviewing() {
            return false;
        }
        match self.request.as_ref().map(Request::kind) {
            Some(OfferKind::Widen { in_force }) => !manages(in_force.selection, subject),
            _ => true,
        }
    }

    /// Whether the page's own signals are still (§3.3, §3.6): while the first launch's offer
    /// waits for its answer its headline is the one signal, and an offer closed unanswered
    /// approved nothing. The offer of the groups the ALab signal adds stills nothing: its tab
    /// never takes focus, and the plan in force goes on under it.
    pub(crate) fn signals_still(&self) -> bool {
        self.status.unanswered || (self.reviewing() && !self.reviewing_widen())
    }

    fn reviewed_rows(&self) -> Vec<RowView> {
        Subject::all()
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
        Subject::all()
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
            // The offer of the groups the ALab signal adds says what the plan in force needs of
            // a person, as its rows, the diamond and the menu line do; the offer applies by
            // itself, so the headline offers no Set up now beside it.
            if self.reviewing_widen() && self.needs_attention() {
                let plan = self.reviewed_plan();
                return Headline {
                    start: false,
                    ..headline(&self.status, &self.reviewed_rows(), Some(&plan))
                };
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
    /// ([`Self::signals_still`]): first-launch admission still waits or its page closed
    /// unanswered (§3.3). While wider scope awaits admission, the plan already in force
    /// keeps its existing signals.
    pub(crate) fn needs_attention(&self) -> bool {
        if self.signals_still() {
            return false;
        }
        let signal = self.status.failures_signal();
        let reviewed = self.reviewed_plan();
        (self.plan_known
            && self
                .reviewed_rows()
                .iter()
                .any(|row| row.attention() && (signal || row.standing != Some(Standing::Failed))))
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
        let offer = submission == Submission::Offer;
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
        self.answer_waits = false;
        let widened = offer && std::mem::take(&mut self.review_widened);
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
                chosen: !matches!(submission, Submission::AsItStands | Submission::Offer),
                vault_typed: with_vault,
                changed,
                widened,
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
    /// Closing never cancels (§3.3): an offer still waiting for its answer is
    /// closed for this launch, nothing is recorded, and the next launch offers
    /// it again. What the page sent is the worker's already.
    pub(crate) fn closed(&mut self, _scope: CloseScope) {
        if let Some(request) = self.request.take() {
            request.finish(WorkstationSetupDecision::Closed);
        }
        self.answer_waits = false;
        self.review_widened = false;
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

    /// Set every bit of `group` to `on`, and submit the plan when that changes
    /// what the worker will run, or while the plan aterm chose by itself may still
    /// widen ([`Self::may_widen`]): a group choice makes the plan the person's, even
    /// one that returns to aterm's plan (§3.5). Nothing while a Restore runs.
    fn choose(&mut self, group: Group, on: bool) {
        if !self.plan_known || self.restoring() {
            return;
        }
        for setting in group.settings() {
            self.plan.selection.set(*setting, on);
        }
        // A press while the offer waits for its answer answers it, as the presser's choice: an
        // offer still pending is one the plan may widen.
        if self.plan.selection != self.next_plan().selection || self.may_widen() {
            let changed = self.press_changed(group.settings());
            self.apply(
                Admission::of_gesture(self.by_person),
                Submission::Choice,
                changed,
            );
        }
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
            // its action does: a draft is never hidden.
            if edits {
                self.vault_open = Some(true);
            }
            view.reveal_listed_control(&key);
        }
    }

    /// One text input to the vault field `key`: `false` when `key` names
    /// neither vault field.
    fn field_event(&mut self, key: &str, event: TextInputEvent) -> bool {
        if key == "system-state/password" {
            self.password.input(event);
            return true;
        }
        if key != "system-state/url" {
            return false;
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
            TextInputEvent::DeleteWordForward => self.url.delete_word_forward(),
            TextInputEvent::SelectAll => self.url.select_all(),
            TextInputEvent::Undo => {
                self.url.undo();
            }
            TextInputEvent::Redo => {
                self.url.redo();
            }
            TextInputEvent::Submit | TextInputEvent::Cancel => {}
        }
        true
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
        if let Some(slug) = key.strip_prefix("system-state/headline/open/") {
            if self.plan_known
                && let Some(access) = Access::ALL.into_iter().find(|access| access.word() == slug)
                && self.headline().open == Some(access)
            {
                self.actions.push_back(SystemStateAction::OpenAccess {
                    access,
                    by_person: self.by_person,
                });
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
            "system-state/check" if self.can_check() => {
                let plan = self.reviewed_plan();
                // A press waiting for another setup keeps its words: the reading goes beside it.
                if !self.status.queued {
                    self.status.busy = true;
                    self.status.operation = Some(SetupOperation::Check);
                }
                self.actions.push_back(SystemStateAction::Check(plan));
            }
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
            "system-state/headline/show" => {
                if let Some(subject) = self.headline().show {
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
                if self.reviewed_rows().iter().any(|row| row.turn_on_sync) {
                    self.actions.push_back(SystemStateAction::TurnSyncOn {
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
            "system-state/vault/join" if self.vault_editable() && self.plan_known => {
                self.vault_open = Some(true);
                self.plan.selection.set(Setting::Secrets, true);
                let changed = self.press_changed(&[Setting::Secrets]);
                self.apply(
                    Admission::of_gesture(self.by_person),
                    Submission::Join,
                    changed,
                );
            }
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
                if self.vault_editable() && event != TextInputEvent::Cancel {
                    self.type_into(view, event);
                }
                true
            }
            #[cfg(any(a11y_tree, test))]
            AppEvent::InsertText(text) => {
                if self.vault_editable() {
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
        // aterm manages: every row reads `—`, with no chip. A newer aterm's
        // setup names none it can know, so its rows read as they stand.
        let rows = if app.plan_known {
            app.reviewed_rows()
        } else if app.status.newer() {
            rows_as_they_stand(&app.status)
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

    fn tree(mut self) -> UiTree {
        let (pad_x, pad_y, gap, row_h) = (self.m.pad_x, self.m.pad_y, self.m.gap, self.m.row_h);
        let headline = self.app.headline();
        let (brand, brand_h, brand_walks) = self.brand();
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
        let show_brand = show_title && steady - title_cost - (brand_h + 4.0) >= roomy;
        let list_h = (rest
            - if show_title { title_cost } else { 0.0 }
            - if show_brand { brand_h + 4.0 } else { 0.0 })
        .max(row_h);
        let list = self.list(list_h);
        // A village left out with the brand walks for nobody.
        self.view.village_walks.set(show_brand && brand_walks);
        let mut children = Vec::new();
        if show_brand {
            children.push(brand);
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

    /// The brand lockup (§3.1), with its height and whether the village beside
    /// it walks (it shows, and motion is not reduced).
    fn brand(&self) -> (UiNode, f32, bool) {
        // The words' middle dot marks the lockup for the eye; a screen reader hears the words
        // alone, as it does a row's value.
        let spoken = paint::BRAND_WORDS.replace(" · ", " ");
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
        let walks = village.is_some() && !motion.system_reduced && !motion.performance_reduced;
        let mut children = vec![
            UiNode::new(
                "system-state/brand/art",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::SYSTEM_STATE_BRAND_AUDIT,
                    role: SemanticRole::Text,
                    label: spoken.clone(),
                    paint: Some(spec.value()),
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
                    label: Some(spoken),
                    role: SemanticRole::Text,
                    style: StyleRef::Plain,
                }),
            )
            .layout(Layout::row().gap(12.0).height(Length::Fixed(height)))
            .children(children),
            height,
            walks,
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
        // The page's own ticks are the walk's only clock: kitties stand still
        // between runs, whatever else repaints the page.
        let phase_ms = if motion.system_reduced || motion.performance_reduced {
            0
        } else {
            self.app.anim_ticks.wrapping_mul(VILLAGE_TICK_MS)
        };
        Some(
            UiNode::new(
                "system-state/village",
                UiContent::Custom(AuditedCustomNode {
                    audit_id: crate::native_ui::RAINBOW_BANNER_AUDIT,
                    role: SemanticRole::Group,
                    label: String::new(),
                    paint: Some(village_spec(
                        &self.app.status,
                        &self.rows,
                        &self.app.reviewed_plan(),
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
    /// that needs you with its Turn On or its Show, one failed step with its Show
    /// and Set up now, one waiting step with its Show, changes to make with Set
    /// up now, and the run's own words.
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
            Headline::line(TOOLS_PASSWORD_HEADLINE),
            Headline::line(TOOLS_RECOVERED_HEADLINE),
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
                start: true,
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
        let open_key = headline
            .open
            .map(|access| format!("system-state/headline/open/{}", access.word()));
        if let Some((access, key)) = headline.open.zip(open_key.as_deref()) {
            let (label, visual) = open_words(access);
            faces.push((key, label, Some(visual), StyleRef::Accent, true));
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
            // Its label is the action alone: the headline beside it already says what it is
            // about, and VoiceOver reads them together.
            faces.push((
                "system-state/headline/start",
                "Set up now".to_owned(),
                None,
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
                paint: Some(format!("{done},{managed}")),
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
            self.app.can_check(),
            room,
        );
        // A Restore waiting for another setup says so on its own button, which waits with it.
        let status = &self.app.status;
        let restore_waits = status.queued && status.operation == Some(SetupOperation::Restore);
        let mut restore = if restore_waits {
            self.fitted_button(
                "system-state/restore",
                RESTORE_WAITS.to_owned(),
                None,
                Some("Waiting…"),
                StyleRef::Quiet,
                false,
                room,
            )
        } else {
            self.fitted_button(
                "system-state/restore",
                RESTORE_FINISHED.to_owned(),
                None,
                Some("Restore"),
                StyleRef::Quiet,
                self.app.can_restore(),
                room,
            )
        };
        if let UiContent::Button(control) = &mut restore.0.content {
            control.spec.description = Some(if restore_waits {
                QUEUED_RESTORE_NOTE.to_owned()
            } else {
                "Puts back the Mac preferences aterm saved before changing them. Sign-outs, \
                 Rust docs, repositories, credentials and permissions stay."
                    .to_owned()
            });
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
        let notes = self.app.notes(&reviewed);
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
            self.span(format!("system-state/note/{index}"), top, height);
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
        let mut offset = self.view.scroll.get().clamp(0.0, limit);
        // A room that shrank (as when the brand or the title comes back, or
        // the window gets shorter), or a list whose rows above it changed
        // height (a vault row whose note leaves once a draft address is typed),
        // keeps in view the focused control it showed: whole, or more of it
        // than a sliver the list leaves unpainted.
        let (was, before) = (self.view.room.get(), self.view.scroll.get());
        const EDGE: f32 = crate::native_ui::CLIP_EDGE_EPSILON;
        if let Some(focus) = self.view.common.last_focus.as_ref()
            && let Some((old_top, old_bottom)) = self
                .view
                .spans
                .borrow()
                .iter()
                .find(|span| *span.key == *focus.as_str())
                .map(|span| (span.top, span.bottom))
            && let Some(span) = self.spans.iter().find(|span| *span.key == *focus.as_str())
            && (room + EDGE < was || (span.top - old_top).abs() > EDGE)
            && old_bottom.min(before + was) - old_top.max(before) + EDGE
                >= crate::native_ui::LIST_PEEK_MIN.min(old_bottom - old_top)
        {
            offset = whole_at(offset, room, span.top, span.bottom).clamp(0.0, limit);
        }
        self.view.scroll.set(offset);
        self.view.height.set(list_h);
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
        let list = UiNode::new(
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
        .lists_every_row()
        .children(children);
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
                    paint: Some(share),
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
            if row.install_tools {
                self.span(INSTALL_TOOLS.to_owned(), top + y, height);
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
        // Until the plan is known, neither segment is chosen; neither is
        // pressable then, nor while a Restore changes the plan (§3.4).
        let known = self.app.plan_known;
        let pressable = known && !self.app.restoring();
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
                        enabled: pressable,
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
                    paint: Some(chip.value().to_owned()),
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
        // A row read as what Apply will do has none (`row_view_as`).
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
            })
            .or_else(|| {
                row.install_tools.then(|| {
                    (
                        INSTALL_TOOLS.to_owned(),
                        "Install Command Line Tools…".to_owned(),
                        Some("Install…"),
                        Some("Install…"),
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
        let enabled = self.app.vault_editable();
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
                     an admin Mac to approve this one.";
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
        let mut column = vec![
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
        ];
        column.push(self.button(
            "system-state/vault/join",
            "Join".to_owned(),
            None,
            StyleRef::Secondary,
            enabled && self.app.plan_known,
        ));
        let height = address_h + password_h + help_h + m.control_h * 3.0 + 4.0 * 5.0 + 10.0;
        // Each field under its caption, 4pt apart (the column's gap).
        let url_top = top + disclose_h + address_h + 4.0;
        let password_top = url_top + m.control_h + 4.0 + password_h + 4.0;
        let join_top =
            top + disclose_h + address_h + password_h + help_h + m.control_h * 2.0 + 4.0 * 5.0;
        let keys = vec![
            ("system-state/url".to_owned(), url_top, m.control_h),
            (
                "system-state/password".to_owned(),
                password_top,
                m.control_h,
            ),
            ("system-state/vault/join".to_owned(), join_top, m.control_h),
        ];
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
            keys,
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
            paint: None,
            action: None,
            focusable: false,
            focused: false,
            focus_visible: false,
        }),
    )
    .layout(Layout::default().height(Length::Fixed(1.0)))
    .paint_only()
}

/// Why the fixtures' repositories failed for a while: GitHub stopped answering, which passes.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
const GITHUB_STOPPED: &str = "GitHub stopped answering partway through.";

/// Why a step of the repositories failed, in `init_observe`'s words (`Wait::Unreached`).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) const GITHUB_UNREACHED: &str = "GitHub can't be reached";

/// The repositories' note where GitHub refuses the token the vault syncs, in
/// `init_observe`'s words (`Wait::TokenRefused`).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) const GITHUB_TOKEN_REFUSED: &str = "GitHub refuses your token: renew it in the vault";

/// The page's scenarios for looking at it without a Mac to set up
/// (`ATERM_DEV_SYSTEM_STATE_FIXTURE`): `checking`, `running`, `widened`, `debugger`,
/// `needs-you`, `vault`, `vault-unreached`, `vault-not-syncing`, `vault-not-set-up`,
/// `sync-off`, `vault-failed`, `set-up`, `non-alab`, `widen-stopped`, `failed`,
/// `check-failed`, `check-unconfirmed`, `mixed`, `named-result-failed`, `restore-at-login`,
/// `restore-blocked`, `restore-unlogged`, `vault-source-mismatch`, `plan-unread`, `fresh`,
/// `queued-choice`, `queued-restore`, `set-aside`, `set-aside-non-alab`, `startup-retry`,
/// `restore-failed`, `setup-retried`, `failed-retrying`, `github-sign-in`, `vault-unreadable`,
/// `join-refused`, `vault-not-joined`, `vault-held`, `newer`, `restoring`, the first launch's
/// offer waiting for a person at this Mac, `review` and `review-widened` ([`fixture_review`]), a
/// `cancelled` an earlier aterm recorded and `review-closed`, the offer of the groups the ALab
/// signal adds, `widen-review`,
/// and Apple's Command Line Tools: `clt-password`, `clt-recovered`, `clt-installing` and `clt-missing`.
/// Each draws only what the worker gives the page: the readings `init_observe` and the
/// worker's probes make, and the rows a run, a Check or a Restore reports.
/// Debug and dev-seam builds only: a release build carries none of
/// their words.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn fixture_status(name: &str) -> Option<SetupStatus> {
    use Standing::{
        AtLogin, Differs, Failed, InPlace, NeedsYou, NotNeeded, Unknown, Untold, Waiting,
    };
    use aterm_types::macos_setup::until_login;
    let read = |subject: Subject, standing: Standing, now: Option<&str>| Observation {
        subject,
        standing,
        now: now.map(str::to_owned),
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: None,
        detail: None,
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
    // Local Network as `access::observation` reads it where no LAN look has answered aterm
    // (no ALab vault on this network, or none asked in this launch): macOS tells apps nothing.
    let untold_lan = || Observation {
        note: Some(aterm_types::macos_setup::LOCAL_NETWORK_UNTOLD.to_owned()),
        ..access(Access::LocalNetwork, Untold, None)
    };
    // The repositories as the worker reads them before the vault has synced
    // this Mac, and so its GitHub token: no count, and what they wait on in
    // their note.
    let token_waiting = || Observation {
        note: Some("Waits for the vault to finish syncing this Mac".to_owned()),
        ..setting(Setting::Repositories, Waiting, None)
    };
    let all = Plan::default();
    let mut status = SetupStatus {
        observed_vault_url: Some(all.vault_url.clone()),
        plan: Some(all.clone()),
        ..SetupStatus::default()
    };
    match name {
        "checking" => {}
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
            // As `macos_doctor` reads a vault the plan names but this network
            // does not answer for, and the repositories that wait on it.
            let mut secrets_waiting = setting(Setting::Secrets, Waiting, Some("Not joined"));
            secrets_waiting.note = Some("The ALab vault did not answer on this network".to_owned());
            status.observed = with(
                in_place(),
                vec![
                    // Local Network as `access::observation` reads it where no LAN look has
                    // answered aterm: macOS tells apps nothing of it.
                    untold_lan(),
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
            status.observed = with(in_place(), vec![debugger]);
        }
        "needs-you" => {
            // Each reason where its probe puts it: a grant's in the
            // description, Developer Tools' and the vault's code in the note.
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Not confirmed"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            let mut tools = access(Access::DeveloperTools, Waiting, None);
            tools.note = Some("Readable once Full Disk Access is on".to_owned());
            let mut vault = setting(Setting::Secrets, NeedsYou, Some("Awaiting code"));
            vault.note =
                Some("On an admin Mac: aterm secrets approve 7QK2-M9WD-4HTX-PL3B".to_owned());
            status.observed = with(in_place(), vec![fda, tools, vault]);
        }
        "vault" => {
            // No vault answers on this network: the vault waits for an address, which its
            // disclosure's fields take, and the repositories for its token.
            let mut vault = setting(Setting::Secrets, Waiting, Some("Not joined"));
            vault.note = Some("No vault on this network".to_owned());
            status.observed = with(in_place(), vec![vault, token_waiting()]);
        }
        "vault-unreached" => {
            // Idle after a run whose vault did not answer: the worker tries again by itself
            // when this Mac's network changes, so nothing is a failure meanwhile.
            status.retrying = true;
            let mut vault = setting(Setting::Secrets, Waiting, Some("Not joined"));
            vault.note = Some("The ALab vault did not answer on this network".to_owned());
            status.observed = with(in_place(), vec![untold_lan(), vault, token_waiting()]);
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
            // Trackpad scrolling, set in this login session, is in full use from the next.
            // Local Network reads as `access::observation` reads it where no LAN look has
            // answered aterm: macOS tells apps nothing of it.
            status.observed = with(
                in_place(),
                vec![
                    setting(Setting::TrackpadScrolling, AtLogin, Some("All on")),
                    untold_lan(),
                ],
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
                    untold_lan(),
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
            // once the run's tries are spent: aterm no longer tries it by itself, and Set up now
            // does, so the headline, the row, the menu line and the diamond say it failed. The
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
        "github-sign-in" => {
            // GitHub refuses the token the vault syncs, as `init_observe` reads the
            // repositories then: a person renews it, so the row needs them, with the remedy in
            // its note, and no run retries it (§3.6).
            let mut repositories = setting(Setting::Repositories, NeedsYou, Some("Needs sign-in"));
            repositories.note = Some(GITHUB_TOKEN_REFUSED.to_owned());
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
            // Permissions are managed, so a grant that is off needs the person.
            let mut debugger = access(Access::Debugger, NeedsYou, Some("Off"));
            debugger.detail = Some(
                "Turning it on lets debuggers attach without a password. macOS asks for an \
                 administrator password in a dialog named SamplingTools."
                    .to_owned(),
            );
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
                    untold_lan(),
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
        "queued-choice" => {
            // The person left Desktop & Dock as is while another setup (another aterm, `aterm
            // doctor` in a terminal) holds this Mac's setup records: their choice shows, and
            // waits for it.
            status.busy = true;
            status.queued = true;
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
                    untold_lan(),
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
        "vault-held" => {
            // A vault try held after two tries did not take: aterm tries it again only when
            // this Mac's network or vault changes, so it failed, and Set up now tries it now.
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
                    untold_lan(),
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
        "cancelled" => {
            // The first launch's review a person cancelled (§3.3): the worker keeps every group
            // at Leave as is, and the page says so in one calm line, Set up now beside it applying
            // the setup the review showed. A grant that would need a person once Permissions run
            // asks nothing of anyone while Permissions are left as is.
            status = fixture_status("review")?;
            status.plan = Some(Plan::from(Selection::none()));
            status.cancelled = true;
        }
        "review-closed" => {
            // The first launch's review a person closed unanswered, and the page opened again in
            // that launch (§3.3): nothing was approved, so it reads as the review did, calm, with
            // Set up now.
            status = fixture_status("review")?;
            status.unanswered = true;
        }
        "review-widened" => {
            // The first launch's review on a Mac whose ALab signal the worker heard while it was
            // up (§3.3): offered Permissions alone, it widens in place to the whole profile
            // ([`fixture_review`]), and the page says why.
            status = fixture_status("review")?;
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            status.plan = Some(Plan::from(selection));
        }
        "widen-review" => {
            // The review of the groups the ALab signal adds to a plan that set up Permissions
            // alone (§3.5), while that plan waits on Full Disk Access: the grant still needs the
            // person, with its Open and its signals, and only the added groups read as what
            // Apply will do.
            let mut selection = Selection::none();
            selection.set(Setting::Permissions, true);
            status = fixture_status("non-alab")?;
            status.plan = Some(Plan::from(selection));
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Not confirmed"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            status.observed = with(status.observed, vec![fda]);
        }
        "clt-password" | "clt-recovered" | "clt-installing" | "clt-missing" => {
            // The GitHub repositories wait on Apple's Command Line Tools (§3.3), which the
            // window installs in a terminal tab of its own: the tab asking for the password
            // (`clt-password`), its restored live tab (`clt-recovered`), Apple's installer
            // running (`clt-installing`), or the tools
            // missing with no tab open and nothing installing them (`clt-missing`), where the
            // row offers Install….
            let mut repositories = setting(Setting::Repositories, NeedsYou, None);
            repositories.note =
                Some(aterm_types::macos_setup::COMMAND_LINE_TOOLS_MISSING.to_owned());
            status.observed = with(in_place(), vec![untold_lan(), repositories]);
            status.developer_tools = match name {
                "clt-password" => Some(ToolsInstall::Password),
                "clt-recovered" => Some(ToolsInstall::Recovered),
                "clt-installing" => Some(ToolsInstall::Installing),
                _ => None,
            };
        }
        "review" => {
            // The first launch's offer on a fresh signed Mac ([`fixture_review`]): every group
            // set up, most rows to change, and Full Disk Access and Debugger access, which a
            // person grants once setup runs, read as what setup changes.
            status = fixture_status("fresh")?;
            let mut fda = access(Access::FullDiskAccess, NeedsYou, Some("Not confirmed"));
            fda.detail = Some(
                "Full Disk Access keeps macOS from interrupting programs you run here with \
                 privacy dialogs. Only you can switch it on, in System Settings."
                    .to_owned(),
            );
            let mut debugger = access(Access::Debugger, NeedsYou, Some("Off"));
            debugger.detail = Some(
                "Turning it on lets debuggers attach without a password. macOS asks for an \
                 administrator password in a dialog named SamplingTools."
                    .to_owned(),
            );
            status.observed = with(status.observed, vec![fda, debugger]);
        }
        _ => return None,
    }
    status.observed_generation = 1;
    Some(status)
}

/// How a scenario shows an offer still waiting for its answer (`review`, `review-widened`,
/// `widen-review`): the page is offered the plan a launch offers, and draws the scenario's
/// status under it until a person is at this Mac (a headless instance has none). Its answer
/// goes nowhere, as every action of a scenario is inert. Debug and dev-seam builds only.
#[cfg(any(debug_assertions, feature = "dev-seams"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FixtureReview {
    /// The launch's offer waits for its answer.
    Waiting,
    /// Offered Permissions alone, the offer widens in place to the whole profile once the
    /// ALab signal is heard (§3.3).
    Widened,
    /// The review of the groups the ALab signal adds to a plan that set up Permissions alone
    /// (§3.5).
    Widen,
}

/// The review the scenario `name` shows, if one ([`FixtureReview`]).
#[cfg(any(debug_assertions, feature = "dev-seams"))]
pub(crate) fn fixture_review(name: &str) -> Option<FixtureReview> {
    match name {
        "review" => Some(FixtureReview::Waiting),
        "review-widened" => Some(FixtureReview::Widened),
        "widen-review" => Some(FixtureReview::Widen),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
