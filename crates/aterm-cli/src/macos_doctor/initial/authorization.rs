// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One startup review's authority, including its own interrupted writes.

use super::*;

#[cfg(any(test, debug_assertions))]
#[path = "admission_stop_test.rs"]
mod admission_stop_test;
#[cfg(any(test, debug_assertions))]
pub use admission_stop_test::{InitialSetupAdmissionFault, InitialSetupAdmissionStopTest};

#[cfg(any(test, debug_assertions))]
#[derive(Clone, Copy)]
enum WatchFault {
    Write(usize, bool),
    PublicationRead(usize, bool),
}

/// Authorization bound to the records observed before a startup offer.
pub struct InitialSetupAuthorization {
    revision: u64,
    vault: AdmittedVault,
    state: AuthorizationState,
    stop: StopReservation,
    /// This ticket's Join was admitted without its Join: aterm could not read which vault this
    /// Mac is in ([`Self::join_unread`]).
    join_unread: bool,
}

enum AuthorizationState {
    Bound(InitialSetupObservation),
    Writing(AuthorizationCommit),
    /// A write failed and its publication identity could not be captured. This
    /// owns no further writes, but cannot certify that it published nothing.
    Unconfirmed,
    Ended,
}

struct AuthorizationCommit {
    plan: Plan,
    revision: u64,
    writes: Vec<AuthorizationWrite>,
    prefixes: Vec<Records>,
    publications: observation::Publications,
}

/// Who admits a plan through a ticket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Admission {
    /// A person's reviewed choice: the plan is theirs and is never widened.
    Person,
    /// The window's own answer of the plan it offered, with no press, where that plan is not
    /// aterm's provisional one: measured as a person's answer is, but nothing new is chosen, so
    /// the run it admits keeps the link that lets a Stop of the run it carries on hold it
    /// ([`Progress::continues`]), and System State's note that aterm set its records aside stays.
    Offered,
    /// An explicit Join: the chooser's plan, whose vault-dependent steps run again.
    Join,
    /// aterm's own first-launch plan (System State design §3.5): provisional.
    Automatic,
    /// A provisional plan widened on a later launch that found the ALab signal.
    /// Completed preferences carry forward while newly selected preferences still run;
    /// another stage keeps completion only when its full selection and source are unchanged.
    Upgrade,
}

enum AuthorizationWrite {
    /// The plan, and whether aterm chose it by itself.
    Plan(Plan, bool),
    /// An upgrade's plan and recoverable, unadmitted progress share one rename.
    UpgradePlan(Plan, scope::UpgradeProgress),
    Progress(Progress),
    /// The first-run decision ([`save_admitted_decision`]): what of a Restore still stands, and
    /// whether the Mac settings read applied ([`admitted_applied`]).
    Workstation(Option<Restored>, bool),
}

impl AuthorizationWrite {
    fn stop_target(&self) -> Option<u64> {
        match self {
            Self::Progress(progress) => Some(progress.revision),
            Self::UpgradePlan(_, recovery) => Some(recovery.pending.revision),
            Self::Plan(..) | Self::Workstation(..) => None,
        }
    }

    fn persist(&self, root: &Path) -> Result<(), String> {
        match self {
            Self::Plan(plan, provisional) => save_plan_as(root, plan, *provisional),
            Self::UpgradePlan(plan, recovery) => recovery.save(root, plan),
            Self::Progress(progress) => save_progress(root, *progress),
            Self::Workstation(restored, applied) => {
                save_admitted_decision(root, *restored, *applied)
            }
        }
    }

    fn update(&self, records: &mut Records) {
        match self {
            Self::Plan(plan, provisional) => {
                records.plan = Some((plan.clone(), true));
                records.provisional = *provisional;
            }
            Self::UpgradePlan(plan, recovery) => {
                records.plan = Some((plan.clone(), true));
                records.provisional = true;
                records.progress = Some(recovery.pending);
            }
            Self::Progress(progress) => records.progress = Some(*progress),
            Self::Workstation(restored, applied) => {
                records.decision = if *applied {
                    Decision::Applied(true)
                } else {
                    Decision::Pending
                };
                records.restored = *restored;
            }
        }
    }
}

impl AuthorizationCommit {
    fn published_target(&self, root: &Path) -> Result<Option<u64>, String> {
        if observation::publications(root)? != self.publications {
            return Ok(None);
        }
        let current = observation_at(root).map_err(|error| error.to_string())?;
        if current.publications() != self.publications
            || !self
                .prefixes
                .iter()
                .any(|expected| self.same_records(&current.records, expected))
            || !current.records.progress.is_some_and(|progress| {
                progress.revision == self.revision && progress.belongs_to(&self.plan)
            })
        {
            return Ok(None);
        }
        Ok(Some(self.revision))
    }

    fn same_records(&self, current: &Records, expected: &Records) -> bool {
        if !matches!(
            self.writes.first(),
            Some(AuthorizationWrite::UpgradePlan(..))
        ) {
            return current == expected;
        }
        let without_spent_stop = |records: &Records| {
            let mut records = records.clone();
            // An unfinished predecessor's Stop also holds its immediate upgrade.
            // Other older numeric Stops cannot revoke this owned retry. Keep the
            // request itself and every other record/publication check unchanged.
            if let Some(request @ StopRequest::Revision(revision)) = records.stop_request
                && revision < self.revision
                && !self.prefixes[0]
                    .progress
                    .is_some_and(|prior| prior.stopped_by(Some(request)))
            {
                records.stop_request = None;
            }
            records
        };
        without_spent_stop(current) == without_spent_stop(expected)
    }

    /// The writes that admit `plan` over the `baseline` records, with the vault this Mac is in
    /// (`vault`); `waiting`: a Restore's put-back waits for Full Disk Access
    /// ([`restore_waits`]).
    fn new(
        baseline: InitialSetupObservation,
        plan: &Plan,
        admission: Admission,
        vault: AdmittedVault,
        waiting: bool,
        revision_floor: u64,
    ) -> Result<Self, String> {
        validate_plan(plan)?;
        let previous = baseline.records.plan.as_ref().map(|(plan, _)| plan);
        let prior = baseline.records.progress;
        let same_plan =
            previous == Some(plan) && prior.is_some_and(|progress| progress.belongs_to(plan));
        let mut next = if same_plan {
            prior.unwrap()
        } else {
            Progress::for_plan(plan)
        };
        let restored = baseline.records.restored;
        let mut still_restored = None;
        if restored.is_some()
            || (!same_plan
                && matches!(
                    admission,
                    Admission::Person | Admission::Join | Admission::Offered | Admission::Upgrade
                ))
        {
            // A person's answer, or any answer after a Restore (no startup answer is Set up now
            // then), is a choice: whichever plan it admits (the one a choice cut short staged,
            // a group choice made while the offer waited, a Join, or the plan presented in a
            // vault joined since), it is measured as System State measures one, from the run
            // the records keep ([`Progress::staged`]), or the one pass an older aterm made of its
            // plan ([`older_run`]). Automatic widening carries the same completed preferences
            // and owes the newly selected ones: no Dock reset for another group, no completion
            // inherited by a new sign-out, and a Join runs only the vault steps again.
            let restart = Restart {
                chosen: true,
                joins: admission == Admission::Join,
                automatic: admission == Admission::Upgrade,
            };
            (next.done, next.only, still_restored) = admitted_keeps(
                restart,
                baseline.records.plan.as_ref(),
                prior.or_else(|| {
                    older_run(baseline.records.plan.as_ref(), baseline.records.decision)
                }),
                restored,
                waiting,
                plan,
            );
        }
        if admission == Admission::Join {
            next.done &= !(Stage::Secrets.bit() | Stage::Repositories.bit());
        }
        next.access = carried_access(previous, prior, plan);
        next.revision = next_revision(prior.unwrap_or_default().revision.max(revision_floor))?;
        next.completed = plan.selection.is_empty();
        next.admitted = false;
        next.running = None;
        // An admission is a run of its own: an older run's Stop holds it no more. The window's own
        // answer of the run it offered chooses nothing new: that run keeps its link.
        if !(same_plan && admission == Admission::Offered) {
            next.continues = None;
        }
        next.admitted_vault = vault;
        let first = if admission == Admission::Upgrade {
            AuthorizationWrite::UpgradePlan(
                plan.clone(),
                scope::UpgradeProgress::new(previous.cloned(), prior, next),
            )
        } else {
            AuthorizationWrite::Plan(
                plan.clone(),
                matches!(admission, Admission::Automatic | Admission::Upgrade),
            )
        };
        let mut writes = vec![first, AuthorizationWrite::Progress(next)];
        // A Restore's record is always written again: it now measures from this plan. So is a
        // decision no admitted run reads under, such as an older aterm's applied setup whose
        // sign-outs it never verified.
        if next.done & Stage::Workstation.bit() == 0
            || restored.is_some()
            || !matches!(
                baseline.records.decision,
                Decision::Pending | Decision::Applied(true)
            )
        {
            writes.push(AuthorizationWrite::Workstation(
                still_restored,
                admitted_applied(plan, still_restored, next.done),
            ));
        }
        next.admitted = true;
        writes.push(AuthorizationWrite::Progress(next));
        let mut records = baseline.records.clone();
        let mut prefixes = vec![records.clone()];
        for write in &writes {
            write.update(&mut records);
            prefixes.push(records.clone());
        }
        Ok(Self {
            plan: plan.clone(),
            revision: next.revision,
            writes,
            prefixes,
            publications: baseline.publications(),
        })
    }
}

impl InitialSetupAuthorization {
    /// The exact revision this attempt reserved, for Stop only, never admission or replay.
    pub fn stop_target(&self) -> Result<Option<u64>, String> {
        self.stop.target()
    }

    /// An upgrade target this attempt actually published, never its unwritten next revision.
    pub(super) fn published_upgrade_at(
        &self,
        root: &Path,
    ) -> Result<Option<scope::SetupUpgrade>, String> {
        if self.publication_unknown() {
            // Scope owns this upgrade ticket. A successful earlier checkpoint
            // reserved its exact revision even though write ownership was lost.
            return self.stop_target().map(|target| {
                target.map(|to| scope::SetupUpgrade {
                    from: self.revision,
                    to,
                })
            });
        }
        let AuthorizationState::Writing(commit) = &self.state else {
            return Ok(None);
        };
        if !matches!(
            commit.writes.first(),
            Some(AuthorizationWrite::UpgradePlan(..))
        ) {
            return Ok(None);
        }
        Ok(commit
            .published_target(root)?
            .map(|to| scope::SetupUpgrade {
                from: self.revision,
                to,
            }))
    }

    pub(super) const fn publication_unknown(&self) -> bool {
        matches!(self.state, AuthorizationState::Unconfirmed)
    }

    /// Whether this ticket's write failed part-way with its publication identified: the next
    /// admission through it finishes that write from where it stopped, never a fresh one.
    pub(super) const fn write_unfinished(&self) -> bool {
        matches!(self.state, AuthorizationState::Writing(_))
    }

    /// Observe existing setup records before loading or displaying an offer ([`Self::offer`]).
    pub fn capture() -> Result<Self, String> {
        Self::capture_at(&state_root()?)
    }

    pub(super) fn capture_at(root: &Path) -> Result<Self, String> {
        let baseline = observation_at(root).map_err(|error| error.to_string())?;
        let revision = review_revision(&baseline.records)?;
        Ok(Self {
            revision,
            vault: AdmittedVault::Unknown,
            state: AuthorizationState::Bound(baseline),
            stop: StopReservation::default(),
            join_unread: false,
        })
    }

    /// Loads the saved first-launch offer for the configured vault ([`ConfiguredVault::present`]).
    pub fn offer(&mut self) -> Result<Plan, String> {
        self.offer_reading(&state_root()?, live_configured_vault)
    }

    /// [`Self::offer`] from `root`, `read` saying which vault this Mac is in now.
    pub(super) fn offer_reading(
        &mut self,
        root: &Path,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    ) -> Result<Plan, String> {
        // An offer must know the vault its eventual Secrets choice may replace. A vault
        // configuration aterm cannot read (a root-owned or newer-format `source.toml`) holds
        // nothing back: it reads as none here, so the offer goes ahead, Permissions and every
        // other group with it (System State design §3.3). The run's Secrets stage reads it
        // again before it joins anything, and with this admission's vault recorded as none it
        // never replaces a vault it did not read ([`AdmittedVault::stands_in`]): one that reads
        // later stays as it is unless it is the plan's own.
        let configured = read().unwrap_or(None);
        let records = observation_at(root)
            .map_err(|error| error.to_string())?
            .records;
        // A plan an older aterm saved predates the Permissions group, so nobody turned it off:
        // it is presented with aterm's own permissions, approved by default as on every Mac
        // (System State design §3.5, §6; [`Records::present`]). Only a person's Cancel or Stop
        // holds them, and either ends setup before anything is offered.
        let plan = offered_plan(records.presented_plan_record(), Ok(configured.clone()));
        self.vault = AdmittedVault::of(Ok(configured));
        Ok(plan)
    }

    /// The revision from the same coherent observation that bound this ticket.
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether this bound choice can still be persisted or confirmed.
    pub const fn pending(&self) -> bool {
        matches!(
            self.state,
            AuthorizationState::Bound(_) | AuthorizationState::Writing(_)
        )
    }

    /// Some confirms this ticket's exact admitted revision, including an empty plan. A person
    /// answered: System State's note that aterm set its records aside goes, as after a choice.
    pub fn persist_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        self.persist_as(plan, Admission::Person)
    }

    /// [`Self::persist_observed`] for the window's own answer of the plan it offered, with no
    /// press, where that plan is not aterm's provisional one ([`Admission::Offered`]).
    pub fn persist_offered_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        self.persist_as(plan, Admission::Offered)
    }

    /// An explicit Join answered through this bound startup review. Its original
    /// doctor records must still be current; the vault this intentional join may
    /// replace is read once as that review starts writing, under the profile lock.
    /// Where that read fails, the answer is admitted without its Join ([`Self::join_unread`]).
    pub fn persist_join_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        observe_person_choice(|root| {
            self.persist_join_with(
                root,
                plan,
                live_configured_vault,
                AuthorizationWrite::persist,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
    }

    /// Whether this ticket's Join was admitted without its Join ([`Self::persist_join_observed`]):
    /// aterm could not read which vault this Mac is in (a root-owned or newer-format
    /// `source.toml`), and that is the vault a Join replaces. Holding the whole first-launch
    /// answer back for it would hold aterm's own permissions back with it, and try the same
    /// answer again and again, so the answer is admitted as the person's choice of groups, as
    /// a group choice is ([`InitialSetupRestart::persist_observed`]), with no vault recorded:
    /// Permissions and every other group run, and the Secrets step waits with its calm note,
    /// joining nothing and replacing no vault it did not read ([`AdmittedVault::stands_in`]).
    /// System State says the vault was not joined and why, and the person can Join once the
    /// settings read again.
    pub const fn join_unread(&self) -> bool {
        self.join_unread
    }

    fn persist_join_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
        write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        let mut unread = false;
        let admitted = self.persist_with_vault(
            root,
            plan,
            Admission::Join,
            |_, admission| match read() {
                Ok(configured) => Ok((AdmittedVault::of(Ok(configured)), admission)),
                Err(_) => {
                    unread = true;
                    Ok((AdmittedVault::None, Admission::Person))
                }
            },
            write,
            observe,
        );
        self.join_unread |= unread;
        admitted
    }

    /// [`Self::persist_observed`] for the plan aterm chose by itself at first
    /// launch ([`Self::offer_scoped`]): saved as provisional, so the launch
    /// that finds the ALab signal, this one or a later one, may widen it.
    pub fn persist_automatic_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        self.persist_as(plan, Admission::Automatic)
    }

    /// Widen the provisional plan this ticket observed to `plan`, even after its
    /// run finished. The caller checked the observed records allow it. A review admitted it, so
    /// it waits for the profile lock while another holds it, as a choice does
    /// ([`observe_person_choice`]), saying `waits` the moment it starts waiting
    /// ([`noting_first`]); records another writer changed meanwhile are read again
    /// ([`Self::rebind_upgrade`]).
    pub(super) fn upgrade_observed(
        &mut self,
        plan: &Plan,
        waits: impl FnMut(),
    ) -> InitialSetupObserved<Option<u64>> {
        observe_person(|root, before| {
            self.upgrade_waiting(root, before, noting_first(waits, choice_lock_wait()), plan)
        })
    }

    /// [`Self::upgrade_observed`] at `root`: `wait` waits a moment while another holds the
    /// profile lock and says whether to ask for it again ([`observe_choice_at`]).
    pub(super) fn upgrade_waiting(
        &mut self,
        root: &Path,
        before: Option<InitialSetupObservation>,
        wait: impl FnMut() -> bool,
        plan: &Plan,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_choice_at(root, before, wait, |root| {
            self.persist_with(
                root,
                plan,
                Admission::Upgrade,
                AuthorizationWrite::persist,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
    }

    /// An upgrade a review admitted meets records another writer changed since this ticket
    /// observed them (a pass that saved its progress while the upgrade waited for the profile
    /// lock): it still widens the plan in force when that is the same provisional plan, with no
    /// Stop, Cancel or Restore since ([`scope::upgrade_of`]), and is then admitted against the
    /// records as they are now. Otherwise the ticket stays bound to what it observed, and the
    /// admission ends with nothing written. Under the profile lock, before anything is written.
    fn rebind_upgrade(&mut self, root: &Path) -> Result<(), String> {
        let AuthorizationState::Bound(baseline) = &self.state else {
            return Ok(());
        };
        let current = observation_at(root).map_err(|error| error.to_string())?;
        if current == *baseline
            || current.records.plan != baseline.records.plan
            || stopped_at(root)?.is_some()
            || scope::upgrade_of(&current.records, || true).is_none()
        {
            return Ok(());
        }
        let Ok(revision) = review_revision(&current.records) else {
            return Ok(());
        };
        if plan_revision(
            current.records.progress,
            current.records.plan.as_ref().map(|(plan, _)| plan),
        )
        .is_err()
        {
            return Ok(());
        }
        self.revision = revision;
        self.state = AuthorizationState::Bound(current);
        Ok(())
    }

    /// The records this ticket observed when it was captured.
    pub(super) fn baseline_records(&self) -> Result<Records, String> {
        match &self.state {
            AuthorizationState::Bound(baseline) => Ok(baseline.records.clone()),
            _ => Err("the setup review was already used".into()),
        }
    }

    /// The answer to the offer waits for the profile lock while another holds it, as a
    /// choice does ([`observe_person_choice`]); so does an upgrade ([`Self::upgrade_observed`]).
    fn persist_as(
        &mut self,
        plan: &Plan,
        admission: Admission,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_person_choice(|root| {
            self.persist_with(root, plan, admission, AuthorizationWrite::persist, |root| {
                observation_at(root).map_err(|error| error.to_string())
            })
        })
    }

    #[cfg(test)]
    pub(super) fn persist_at(
        &mut self,
        root: &Path,
        plan: &Plan,
    ) -> InitialSetupObserved<Option<u64>> {
        self.persist_at_as(root, plan, Admission::Person)
    }

    /// [`Self::persist_offered_observed`] at `root`.
    #[cfg(test)]
    pub(super) fn persist_offered_at(
        &mut self,
        root: &Path,
        plan: &Plan,
    ) -> InitialSetupObserved<Option<u64>> {
        self.persist_at_as(root, plan, Admission::Offered)
    }

    /// [`Self::persist_join_observed`] at `root`, in the vault `configured` this Mac is in now.
    #[cfg(test)]
    pub(super) fn persist_join_at(
        &mut self,
        root: &Path,
        plan: &Plan,
        configured: Option<ConfiguredVault>,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                self.persist_join_with(
                    root,
                    plan,
                    || Ok(configured),
                    AuthorizationWrite::persist,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
            },
        )
    }

    /// [`Self::upgrade_observed`] at `root`, refused [`SETUP_BUSY`] at once, without waiting,
    /// while another holds the profile lock.
    #[cfg(test)]
    pub(super) fn upgrade_at(
        &mut self,
        root: &Path,
        plan: &Plan,
    ) -> InitialSetupObserved<Option<u64>> {
        self.upgrade_waiting(root, observation_at(root).ok(), || false, plan)
    }

    #[cfg(test)]
    fn persist_at_as(
        &mut self,
        root: &Path,
        plan: &Plan,
        admission: Admission,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                self.persist_with(root, plan, admission, AuthorizationWrite::persist, |root| {
                    observation_at(root).map_err(|error| error.to_string())
                })
            },
        )
    }

    fn persist_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        admission: Admission,
        write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        self.persist_with_vault(
            root,
            plan,
            admission,
            |vault, admission| Ok((vault, admission)),
            write,
            observe,
        )
    }

    #[cfg(any(test, debug_assertions))]
    pub(super) fn persist_for_watch_test(
        &mut self,
        root: &Path,
        plan: &Plan,
        upgrade: bool,
        fault: Option<(usize, bool)>,
    ) -> Result<Option<u64>, String> {
        self.persist_watch_fault(
            root,
            plan,
            upgrade,
            fault.map(|(at, after)| WatchFault::Write(at, after)),
        )
    }

    #[cfg(any(test, debug_assertions))]
    pub(super) fn lose_publication_for_watch_test(
        &mut self,
        root: &Path,
        plan: &Plan,
        at: usize,
        published: bool,
    ) -> Result<Option<u64>, String> {
        let result = self.persist_watch_fault(
            root,
            plan,
            true,
            Some(WatchFault::PublicationRead(at, published)),
        );
        std::fs::remove_dir(root.join("automatic-plan.json")).map_err(|error| error.to_string())?;
        std::fs::rename(
            root.join("held-upgrade-plan.json"),
            root.join("automatic-plan.json"),
        )
        .map_err(|error| error.to_string())?;
        result
    }

    #[cfg(any(test, debug_assertions))]
    fn persist_watch_fault(
        &mut self,
        root: &Path,
        plan: &Plan,
        upgrade: bool,
        fault: Option<WatchFault>,
    ) -> Result<Option<u64>, String> {
        let mut index = 0;
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            self.persist_with(
                root,
                plan,
                if upgrade {
                    Admission::Upgrade
                } else {
                    Admission::Automatic
                },
                |step, root| {
                    let current = index;
                    index += 1;
                    if let Some(WatchFault::PublicationRead(at, published)) = fault
                        && current == at
                    {
                        if published {
                            step.persist(root)?;
                        }
                        std::fs::rename(
                            root.join("automatic-plan.json"),
                            root.join("held-upgrade-plan.json"),
                        )
                        .map_err(|error| error.to_string())?;
                        std::fs::create_dir(root.join("automatic-plan.json"))
                            .map_err(|error| error.to_string())?;
                        return Err("injected post-publication identity failure".into());
                    }
                    if let Some(WatchFault::Write(at, published)) = fault
                        && current == at
                    {
                        if published {
                            step.persist(root)?;
                        }
                        return Err("injected upgrade publication failure".into());
                    }
                    step.persist(root)
                },
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result
    }

    /// Admit `plan` as `admission` says. A person's answer or Join, once admitted, takes System
    /// State's note that aterm set its records aside away, as a choice does
    /// ([`InitialSetupRestart::persist_observed`]); one left only keeps that calm note up.
    /// `read` says, as the ticket starts writing, the vault the admission records and who
    /// admits it: a Join whose read fails is the person's answer ([`Self::join_unread`]).
    fn persist_with_vault(
        &mut self,
        root: &Path,
        plan: &Plan,
        admission: Admission,
        read: impl FnOnce(AdmittedVault, Admission) -> Result<(AdmittedVault, Admission), String>,
        write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        let admitted = self.admit_with_vault(root, plan, admission, read, write, observe);
        if matches!(admitted, Ok(Some(_)))
            && matches!(admission, Admission::Person | Admission::Join)
        {
            let _ = set_aside::forget_at(root);
        }
        admitted
    }

    fn admit_with_vault(
        &mut self,
        root: &Path,
        plan: &Plan,
        admission: Admission,
        read: impl FnOnce(AdmittedVault, Admission) -> Result<(AdmittedVault, Admission), String>,
        mut write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        if admission == Admission::Upgrade {
            self.rebind_upgrade(root)?;
        }
        let publications = match &self.state {
            AuthorizationState::Bound(baseline) => baseline.publications(),
            AuthorizationState::Writing(commit) => commit.publications.clone(),
            AuthorizationState::Unconfirmed | AuthorizationState::Ended => return Ok(None),
        };
        if !owns_publications(root, &publications)? {
            self.state = AuthorizationState::Ended;
            return Ok(None);
        }
        let current = observe(root)?;
        match &self.state {
            AuthorizationState::Bound(baseline) => {
                let finished = records_finished(&current.records);
                if current != *baseline
                    || (finished && admission != Admission::Upgrade)
                    || (admission == Admission::Upgrade
                        && scope::upgrade_of(&current.records, || true).is_none())
                {
                    self.state = AuthorizationState::Ended;
                    return Ok(None);
                }
                let owned = baseline.clone();
                let (vault, admission) = read(self.vault, admission)?;
                let waiting = owned.records.restored.is_some() && restore_waits(root);
                let revision_floor = revision::floor(root)?;
                self.state = AuthorizationState::Ended;
                self.state = AuthorizationState::Writing(AuthorizationCommit::new(
                    owned,
                    plan,
                    admission,
                    vault,
                    waiting,
                    revision_floor,
                )?);
            }
            AuthorizationState::Writing(commit) => {
                if current.publications() != commit.publications
                    || commit.plan != *plan
                    || !commit
                        .prefixes
                        .iter()
                        .any(|expected| commit.same_records(&current.records, expected))
                {
                    self.state = AuthorizationState::Ended;
                    return Ok(None);
                }
                if commit
                    .prefixes
                    .last()
                    .is_some_and(|expected| commit.same_records(&current.records, expected))
                {
                    let revision = commit.revision;
                    self.stop = StopReservation::Known(revision);
                    self.state = AuthorizationState::Ended;
                    return Ok(Some(revision));
                }
            }
            AuthorizationState::Unconfirmed | AuthorizationState::Ended => return Ok(None),
        }
        let AuthorizationState::Writing(commit) = &self.state else {
            unreachable!()
        };
        let revision = commit.revision;
        let before = commit.publications.clone();
        let previous_stop = self.stop;
        self.stop.writing();
        let result = commit.writes.iter().try_for_each(|step| {
            write(step, root)?;
            if let Some(revision) = step.stop_target() {
                self.stop = StopReservation::Known(revision);
            }
            Ok::<_, String>(())
        });
        if let Err(why) = result {
            match observation::publications(root) {
                Ok(publications) => {
                    let unchanged = publications == before;
                    let AuthorizationState::Writing(commit) = &mut self.state else {
                        unreachable!()
                    };
                    commit.publications = publications;
                    if !matches!(self.stop, StopReservation::Known(_)) {
                        if unchanged {
                            self.stop = previous_stop;
                        } else if let Ok(Some(revision)) = commit.published_target(root) {
                            self.stop = StopReservation::Known(revision);
                        }
                    }
                }
                Err(_) => self.state = AuthorizationState::Unconfirmed,
            }
            return Err(why);
        }
        self.state = AuthorizationState::Ended;
        Ok(Some(revision))
    }
}

fn records_finished(records: &Records) -> bool {
    match records.progress {
        None => records.decision == Decision::Applied(true),
        Some(progress) => {
            (progress.stopped_by(records.stop_request) || (progress.admitted && progress.completed))
                && records
                    .plan
                    .as_ref()
                    .is_some_and(|(plan, _)| progress.belongs_to(plan))
        }
    }
}

#[cfg(test)]
#[path = "authorization_tests.rs"]
mod tests;
