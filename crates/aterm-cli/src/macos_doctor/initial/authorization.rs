// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One startup review's authority, including its own interrupted writes.

use super::*;

/// Authorization bound to the records observed before a startup offer. Carry
/// the same ticket through persistence errors; never recapture a baseline for
/// an already accepted choice. It contains no password and probes no OS settings.
pub struct InitialSetupAuthorization {
    revision: u64,
    /// The vault this Mac was in when the offer was loaded ([`Self::offer`]), which the run
    /// it admits keeps; none known before an offer is.
    vault: AdmittedVault,
    state: AuthorizationState,
}

enum AuthorizationState {
    Bound(InitialSetupObservation),
    Writing(AuthorizationCommit),
    Ended,
}

struct AuthorizationCommit {
    plan: Plan,
    revision: u64,
    writes: Vec<AuthorizationWrite>,
    prefixes: Vec<Records>,
    publications: observation::Publications,
}

enum AuthorizationWrite {
    Plan(Plan),
    Progress(Progress),
    Workstation(Plan),
}

impl AuthorizationWrite {
    fn persist(&self, root: &Path) -> Result<(), String> {
        match self {
            Self::Plan(plan) => save_plan(root, plan),
            Self::Progress(progress) => save_progress(root, *progress),
            Self::Workstation(plan) => save_workstation_authorization(root, plan),
        }
    }

    fn update(&self, records: &mut Records) {
        match self {
            Self::Plan(plan) => records.plan = Some((plan.clone(), true)),
            Self::Progress(progress) => records.progress = Some(*progress),
            Self::Workstation(plan) => {
                records.decision = if plan.selection.workstation().is_empty() {
                    Decision::Applied(true)
                } else {
                    Decision::Pending
                };
            }
        }
    }
}

impl AuthorizationCommit {
    fn new(
        baseline: InitialSetupObservation,
        plan: &Plan,
        vault: AdmittedVault,
    ) -> Result<Self, String> {
        validate_plan(plan)?;
        let previous = baseline.records.plan.as_ref().map(|(plan, _)| plan);
        let prior = baseline.records.progress;
        let mut next =
            if previous == Some(plan) && prior.is_some_and(|progress| progress.belongs_to(plan)) {
                prior.unwrap()
            } else {
                Progress::for_plan(plan)
            };
        next.revision = next_revision(prior.unwrap_or_default().revision)?;
        next.completed = plan.selection.is_empty();
        next.admitted = false;
        next.running = None;
        next.admitted_vault = vault;
        let mut writes = vec![
            AuthorizationWrite::Plan(plan.clone()),
            AuthorizationWrite::Progress(next),
        ];
        if next.done & Stage::Workstation.bit() == 0 {
            writes.push(AuthorizationWrite::Workstation(plan.clone()));
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
    /// Observe existing setup records before loading or displaying an offer
    /// ([`Self::offer`]). An unreadable or mismatched run cannot be assigned a guessed
    /// revision.
    pub fn capture() -> Result<Self, String> {
        Self::capture_at(&state_root()?)
    }

    pub(super) fn capture_at(root: &Path) -> Result<Self, String> {
        let baseline = observation_at(root).map_err(|error| error.to_string())?;
        let plan = baseline.records.plan.as_ref().map(|(plan, _)| plan);
        let revision = plan_revision(baseline.records.progress, plan)?;
        Ok(Self {
            revision,
            vault: AdmittedVault::Unknown,
            state: AuthorizationState::Bound(baseline),
        })
    }

    /// Load the offer: the saved first-launch plan in the vault this Mac is configured for
    /// now ([`ConfiguredVault::present`]; with no saved plan every item is selected, and
    /// with no configured vault the LAN default is used), reading only aterm's own
    /// configuration and durable setup records. The run this ticket admits keeps that
    /// vault, so one joined while the offer is shown stays, and a vault typed in the offer
    /// while this Mac is in another moves it.
    pub fn offer(&mut self) -> Result<Plan, String> {
        self.offer_reading(&state_root()?, live_configured_vault)
    }

    /// [`Self::offer`] from `root`, `read` saying which vault this Mac is in now.
    pub(super) fn offer_reading(
        &mut self,
        root: &Path,
        read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
    ) -> Result<Plan, String> {
        let configured = read();
        let plan = offered_plan(saved_plan_record(root)?, configured.clone())?;
        self.vault = AdmittedVault::of(configured);
        Ok(plan)
    }

    /// The revision from the same coherent observation that bound this ticket.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether this bound choice can still be persisted or confirmed.
    pub fn pending(&self) -> bool {
        !matches!(self.state, AuthorizationState::Ended)
    }

    /// Some confirms this ticket's exact admitted revision, including an empty
    /// plan. None retires a stale or already-ended review without changing state.
    /// Retry errors only while pending, keeping the original plan and password.
    pub fn persist_observed(&mut self, plan: &Plan) -> InitialSetupObserved<Option<u64>> {
        observe_person_mutation(
            Err("another doctor profile pass is running".into()),
            |root| {
                self.persist_with(root, plan, AuthorizationWrite::persist, |root| {
                    observation_at(root).map_err(|error| error.to_string())
                })
            },
        )
    }

    #[cfg(test)]
    pub(super) fn persist_at(
        &mut self,
        root: &Path,
        plan: &Plan,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                self.persist_with(root, plan, AuthorizationWrite::persist, |root| {
                    observation_at(root).map_err(|error| error.to_string())
                })
            },
        )
    }

    fn persist_with(
        &mut self,
        root: &Path,
        plan: &Plan,
        mut write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> Result<Option<u64>, String> {
        let publications = match &self.state {
            AuthorizationState::Bound(baseline) => baseline.publications(),
            AuthorizationState::Writing(commit) => commit.publications.clone(),
            AuthorizationState::Ended => return Ok(None),
        };
        if !owns_publications(root, &publications)? {
            self.state = AuthorizationState::Ended;
            return Ok(None);
        }
        let current = observe(root)?;
        match &self.state {
            AuthorizationState::Bound(baseline) => {
                if current != *baseline || records_finished(&current.records) {
                    self.state = AuthorizationState::Ended;
                    return Ok(None);
                }
                let owned = baseline.clone();
                self.state = AuthorizationState::Ended;
                self.state =
                    AuthorizationState::Writing(AuthorizationCommit::new(owned, plan, self.vault)?);
            }
            AuthorizationState::Writing(commit) => {
                if current.publications() != commit.publications
                    || commit.plan != *plan
                    || !commit.prefixes.contains(&current.records)
                {
                    self.state = AuthorizationState::Ended;
                    return Ok(None);
                }
                if commit.prefixes.last() == Some(&current.records) {
                    let revision = commit.revision;
                    self.state = AuthorizationState::Ended;
                    return Ok(Some(revision));
                }
            }
            AuthorizationState::Ended => return Ok(None),
        }
        let AuthorizationState::Writing(commit) = &self.state else {
            unreachable!()
        };
        let revision = commit.revision;
        let result = commit.writes.iter().try_for_each(|step| write(step, root));
        if let Err(why) = result {
            match observation::publications(root) {
                Ok(publications) => {
                    let AuthorizationState::Writing(commit) = &mut self.state else {
                        unreachable!()
                    };
                    commit.publications = publications;
                }
                Err(_) => self.state = AuthorizationState::Ended,
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
            progress.admitted
                && progress.completed
                && records
                    .plan
                    .as_ref()
                    .is_some_and(|(plan, _)| progress.belongs_to(plan))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(settings: &[Setting]) -> Plan {
        let mut selection = Selection::none();
        for setting in settings {
            selection.set(*setting, true);
        }
        Plan::from(selection)
    }

    fn observe(root: &Path) -> Result<InitialSetupObservation, String> {
        observation_at(root).map_err(|error| error.to_string())
    }

    fn attempt(
        ticket: &mut InitialSetupAuthorization,
        root: &Path,
        plan: &Plan,
        write: impl FnMut(&AuthorizationWrite, &Path) -> Result<(), String>,
        read: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
    ) -> InitialSetupObserved<Option<u64>> {
        observe_mutation_at(
            root,
            observation_at(root).ok(),
            Err("busy".into()),
            |root| {
                assert!(HeldLock::try_take(root)?.is_none());
                ticket.persist_with(root, plan, write, read)
            },
        )
    }

    #[test]
    fn every_owned_write_boundary_recovers_the_same_revision_including_mismatched_prefixes() {
        for fault in 0..4 {
            for published in [false, true] {
                let root = aterm_tempfile::tempdir().unwrap();
                let previous = selected(&[Setting::Widgets, Setting::Secrets]);
                let original = authorize_at(root.path(), &previous).unwrap().unwrap();
                let mut chosen = selected(&[Setting::Desktop, Setting::Secrets]);
                chosen.vault_url = "https://vault.example/v1/reviewed".into();
                let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                assert_eq!(ticket.revision(), original);
                let mut index = 0;
                let failed = attempt(
                    &mut ticket,
                    root.path(),
                    &chosen,
                    |step, root| {
                        let current = index;
                        index += 1;
                        if current == fault {
                            if published {
                                step.persist(root)?;
                            }
                            return Err("injected namespace flush failure".into());
                        }
                        step.persist(root)
                    },
                    observe,
                );
                assert!(failed.result.is_err());
                assert!(ticket.pending());
                let after_error = observation_at(root.path()).unwrap();
                let writes = std::cell::Cell::new(0);
                let recovered = attempt(
                    &mut ticket,
                    root.path(),
                    &chosen,
                    |step, root| {
                        writes.set(writes.get() + 1);
                        step.persist(root)
                    },
                    observe,
                );
                assert_eq!(recovered.result.unwrap(), Some(original + 1));
                assert!(!ticket.pending());
                assert_eq!(revision_at(root.path()).unwrap(), original + 1);
                assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen));
                assert!(progress(root.path()).unwrap().unwrap().admitted);
                if fault == 3 && published {
                    assert_eq!(writes.get(), 0);
                    assert_eq!(observation_at(root.path()).unwrap(), after_error);
                }
            }
        }
    }

    #[test]
    fn repeated_failures_keep_the_bound_choice_and_same_plan_stage_history() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Desktop, Setting::Spotlight, Setting::Secrets]);
        let original = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut history = progress(root.path()).unwrap().unwrap();
        history.done = Stage::Spotlight.bit();
        save_progress(root.path(), history).unwrap();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        for published in [false, true] {
            let mut index = 0;
            assert!(
                attempt(
                    &mut ticket,
                    root.path(),
                    &plan,
                    |step, root| {
                        if index == 1 {
                            if published {
                                step.persist(root)?;
                            }
                            return Err("injected repeated staged save failure".into());
                        }
                        index += 1;
                        step.persist(root)
                    },
                    observe
                )
                .result
                .is_err()
            );
            assert!(ticket.pending());
        }
        let owned = observation_at(root.path()).unwrap();
        assert!(
            attempt(
                &mut ticket,
                root.path(),
                &plan,
                |_, _| panic!("read failure cannot write"),
                |_| Err("injected owned read failure".into())
            )
            .result
            .is_err()
        );
        assert!(ticket.pending());
        assert_eq!(observation_at(root.path()).unwrap(), owned);
        assert_eq!(
            ticket.persist_at(root.path(), &plan).result.unwrap(),
            Some(original + 1)
        );
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done,
            Stage::Spotlight.bit()
        );
    }

    #[test]
    fn empty_admission_is_confirmed_only_from_this_tickets_final_publication() {
        for replace in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[]);
            let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
            let mut index = 0;
            assert!(
                attempt(
                    &mut ticket,
                    root.path(),
                    &plan,
                    |step, root| {
                        step.persist(root)?;
                        index += 1;
                        if index == 4 {
                            Err("injected final publication flush failure".into())
                        } else {
                            Ok(())
                        }
                    },
                    observe
                )
                .result
                .is_err()
            );
            assert!(ticket.pending());
            let owned = observation_at(root.path()).unwrap();
            if replace {
                restart_at(root.path(), &plan).unwrap();
            }
            let recovered = attempt(
                &mut ticket,
                root.path(),
                &plan,
                |_, _| panic!("confirmation cannot rewrite admission"),
                observe,
            );
            assert_eq!(
                recovered.result.unwrap(),
                if replace { None } else { Some(1) }
            );
            assert!(!ticket.pending());
            if !replace {
                assert_eq!(observation_at(root.path()).unwrap(), owned);
                assert!(admission_at(root.path()).unwrap().unwrap().completed);
            }
        }
    }

    #[test]
    fn stopped_or_replaced_publications_end_bound_and_partially_written_tickets() {
        for started in [false, true] {
            for replacement in 0..5 {
                let root = aterm_tempfile::tempdir().unwrap();
                let plan = selected(&[Setting::Secrets]);
                authorize_at(root.path(), &plan).unwrap();
                let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                if started {
                    assert!(
                        attempt(
                            &mut ticket,
                            root.path(),
                            &plan,
                            |step, root| {
                                step.persist(root)?;
                                Err("injected own write failure".into())
                            },
                            observe
                        )
                        .result
                        .is_err()
                    );
                }
                {
                    let _writer = HeldLock::take(root.path()).unwrap();
                    match replacement {
                        0 | 1 => {
                            write_json(&root.path().join("first-run.json"), &json!({"profile":PROFILE,"state":if replacement == 0 {"cancelled"} else {"restored"}})).unwrap();
                            let path = root.path().join("initial-setup.json");
                            std::fs::remove_file(&path).unwrap();
                            std::fs::create_dir(path).unwrap();
                        }
                        2 => {
                            restart_at(root.path(), &plan).unwrap();
                        }
                        3 => {
                            save_plan(root.path(), &plan).unwrap();
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
                let plan_bytes = std::fs::read(root.path().join("automatic-plan.json")).unwrap();
                let decision = std::fs::read(root.path().join("first-run.json")).unwrap();
                let result = attempt(
                    &mut ticket,
                    root.path(),
                    &plan,
                    |_, _| panic!("a stale ticket cannot write"),
                    observe,
                );
                assert_eq!(result.result.unwrap(), None);
                assert!(!ticket.pending());
                assert_eq!(
                    std::fs::read(root.path().join("automatic-plan.json")).unwrap(),
                    plan_bytes
                );
                assert_eq!(
                    std::fs::read(root.path().join("first-run.json")).unwrap(),
                    decision
                );
            }
        }
    }

    #[test]
    fn lost_raw_publication_binding_never_adopts_a_later_readable_run() {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert!(
            attempt(
                &mut ticket,
                root.path(),
                &plan,
                |step, root| {
                    step.persist(root)?;
                    let path = root.join("first-run.json");
                    std::fs::create_dir(path).unwrap();
                    Err("injected unpinnable publication".into())
                },
                observe
            )
            .result
            .is_err()
        );
        assert!(!ticket.pending());
        std::fs::remove_dir(root.path().join("first-run.json")).unwrap();
        restart_at(root.path(), &plan).unwrap();
        let current = observation_at(root.path()).unwrap();
        assert_eq!(ticket.persist_at(root.path(), &plan).result.unwrap(), None);
        assert_eq!(observation_at(root.path()).unwrap(), current);
    }

    #[test]
    fn capture_uses_one_coherent_revision_and_leaves_absent_state_uncreated() {
        let root = aterm_tempfile::tempdir().unwrap();
        let absent = root.path().join("absent");
        let ticket = InitialSetupAuthorization::capture_at(&absent).unwrap();
        assert_eq!(ticket.revision(), 0);
        assert!(!absent.exists());
        let plan = selected(&[Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let before = observation_at(root.path()).unwrap();
        let ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(ticket.revision(), revision);
        assert_eq!(observation_at(root.path()).unwrap(), before);
        let mut replacement = plan;
        replacement.vault_url = "https://vault.example/v1/other".into();
        save_plan(root.path(), &replacement).unwrap();
        assert!(InitialSetupAuthorization::capture_at(root.path()).is_err());
        std::fs::remove_file(root.path().join("automatic-plan.json")).unwrap();
        assert!(InitialSetupAuthorization::capture_at(root.path()).is_err());
    }
}
