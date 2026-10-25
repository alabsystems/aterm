// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Scratch-only admission publication failures for the binary's Stop handoff regressions.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitialSetupAdmissionFault {
    None,
    BeforePlan,
    BeforeFinal,
    AfterFinal,
    ProgressAfter,
    StagedCleanup,
    BeforeFinalPublicationUnknown,
    FinalPublicationUnknown,
    ProgressPublicationUnknown,
    PlanPublicationUnknown,
}

enum Owner {
    Startup(Box<InitialSetupAuthorization>),
    Restart(Box<InitialSetupRestart>),
}

/// Real startup/restart record writers at a supplied scratch root, without any live ports.
pub struct InitialSetupAdmissionStopTest<'a> {
    root: &'a Path,
    plan: Plan,
    owner: Owner,
}

impl<'a> InitialSetupAdmissionStopTest<'a> {
    /// A real older Restore before any run, ready for the bring-forward path.
    pub fn restored_before_run(root: &'a Path) -> Result<Self, String> {
        {
            let _lock = HeldLock::take(root)?;
            write_json(
                &root.join("first-run.json"),
                &json!({"profile": PROFILE, "state": "restored"}),
            )?;
        }
        Self::startup(root)
    }

    /// The actual bring-forward writer with a refused write, or a visible rename
    /// whose namespace flush fails. No service or OS preference port is reached.
    pub fn bring_forward_progress_error(
        &self,
        after_rename: bool,
        published: impl FnMut(u64),
    ) -> Result<BroughtForward, String> {
        let _lock = HeldLock::take(self.root)?;
        bring_forward_restored_observing_using(
            self.root,
            |_| Ok(()),
            published,
            |root, progress| progress_publication_error(root, progress, after_rename),
        )
        .map(|run| run.map_or(BroughtForward::Nothing, BroughtForward::Run))
    }

    pub fn startup(root: &'a Path) -> Result<Self, String> {
        let mut ticket = InitialSetupAuthorization::capture_at(root)?;
        ticket.offer_reading(root, || Ok(None))?;
        Ok(Self::new(root, Owner::Startup(Box::new(ticket))))
    }

    pub fn restart(root: &'a Path) -> Result<Self, String> {
        Ok(Self::new(
            root,
            Owner::Restart(Box::new(InitialSetupRestart::chosen())),
        ))
    }

    fn new(root: &'a Path, owner: Owner) -> Self {
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        Self {
            root,
            plan: Plan::from(selection),
            owner,
        }
    }

    pub fn plan(&self) -> Plan {
        self.plan.clone()
    }

    /// Admit and complete a scratch vault plan, without running its host stages.
    pub fn complete_vault_setup(&mut self) -> Result<u64, String> {
        self.plan.selection.set(Setting::Secrets, true);
        let revision = self
            .persist(InitialSetupAdmissionFault::None)?
            .ok_or("the scratch vault plan was refused")?;
        let _lock = HeldLock::take(self.root)?;
        let mut progress = progress(self.root)?.ok_or("the scratch run is missing")?;
        progress.done = Stage::ALL
            .into_iter()
            .filter(|stage| stage.selected(&self.plan))
            .fold(0, |done, stage| done | stage.bit());
        progress.completed = true;
        save_progress(self.root, progress)?;
        Ok(revision)
    }

    /// Move the real retained restart into the binary's live retry coordinator.
    pub fn into_restart(self) -> InitialSetupRestart {
        match self.owner {
            Owner::Restart(ticket) => *ticket,
            Owner::Startup(_) => panic!("the scratch owner is a startup review"),
        }
    }

    pub fn pending(&self) -> bool {
        match &self.owner {
            Owner::Startup(ticket) => ticket.pending(),
            Owner::Restart(ticket) => ticket.pending(),
        }
    }

    pub fn stop_target(&self) -> Result<Option<u64>, String> {
        match &self.owner {
            Owner::Startup(ticket) => ticket.stop_target(),
            Owner::Restart(ticket) => ticket.stop_target(),
        }
    }

    pub fn persist(&mut self, fault: InitialSetupAdmissionFault) -> Result<Option<u64>, String> {
        use InitialSetupAdmissionFault::{
            AfterFinal, BeforeFinal, BeforeFinalPublicationUnknown, BeforePlan,
            FinalPublicationUnknown, PlanPublicationUnknown, ProgressAfter,
            ProgressPublicationUnknown, StagedCleanup,
        };
        let root = self.root;
        let result = observe_mutation_at(root, None, Err("busy".into()), |root| {
            if fault == StagedCleanup {
                assert!(matches!(self.owner, Owner::Restart(_)));
                std::fs::create_dir(root.join("automatic-attempt.json"))
                    .map_err(|error| error.to_string())?;
            }
            match &mut self.owner {
                Owner::Startup(ticket) => {
                    let mut at = 0;
                    ticket.persist_with(
                        root,
                        &self.plan,
                        Admission::Automatic,
                        |step, root| {
                            let index = at;
                            at += 1;
                            if fault == BeforePlan && index == 0 {
                                return Err("injected unpublished plan failure".into());
                            }
                            if index == 3
                                && matches!(fault, BeforeFinal | BeforeFinalPublicationUnknown)
                            {
                                if fault == BeforeFinalPublicationUnknown {
                                    lose_publication(root)?;
                                }
                                return Err("injected final admission failure".into());
                            }
                            step.persist(root)?;
                            if (index == 0 && fault == PlanPublicationUnknown)
                                || (index == 1 && fault == ProgressPublicationUnknown)
                                || (index == 3 && fault == FinalPublicationUnknown)
                            {
                                lose_publication(root)?;
                                return Err("injected unpinnable publication".into());
                            }
                            if index == 3 && fault == AfterFinal {
                                return Err("injected final admission flush failure".into());
                            }
                            if index == 1 && fault == ProgressAfter {
                                return Err("injected staged progress flush failure".into());
                            }
                            Ok(())
                        },
                        |root| observation_at(root).map_err(|error| error.to_string()),
                    )
                }
                Owner::Restart(ticket) => ticket.persist_with(
                    root,
                    &self.plan,
                    false,
                    || Ok(None),
                    |root, progress| {
                        if matches!(fault, BeforeFinal | BeforeFinalPublicationUnknown) {
                            if fault == BeforeFinalPublicationUnknown {
                                lose_publication(root)?;
                            }
                            return Err("injected final restart failure".into());
                        }
                        save_progress(root, progress)?;
                        if fault == FinalPublicationUnknown {
                            lose_publication(root)?;
                            return Err("injected unpinnable restart publication".into());
                        }
                        if fault == AfterFinal {
                            return Err("injected final restart flush failure".into());
                        }
                        assert_eq!(
                            fault,
                            InitialSetupAdmissionFault::None,
                            "this fault belongs to startup's ordered writes"
                        );
                        Ok(())
                    },
                    |root| observation_at(root).map_err(|error| error.to_string()),
                ),
            }
        })
        .result;
        if fault == StagedCleanup {
            std::fs::remove_dir(root.join("automatic-attempt.json"))
                .map_err(|error| error.to_string())?;
        }
        let held = root.join("held-admission-plan.json");
        if held.exists() {
            std::fs::remove_dir(root.join("automatic-plan.json"))
                .map_err(|error| error.to_string())?;
            std::fs::rename(held, root.join("automatic-plan.json"))
                .map_err(|error| error.to_string())?;
        }
        result
    }

    pub fn snapshot(&self) -> Result<InitialSetupSnapshot, InitialSetupSnapshotError> {
        snapshot_at(self.root, Plan::default())
    }

    pub fn revision(&self) -> Result<u64, String> {
        revision_at(self.root)
    }

    pub fn admission(&self) -> Result<Option<InitialSetupAdmission>, String> {
        admission_at(self.root)
    }

    pub fn recover(&self) -> Result<u64, String> {
        if finished_at(self.root)? {
            return Err("the saved setup has ended".into());
        }
        if let Some(admission) = admission_at(self.root)? {
            return Ok(admission.revision);
        }
        let mut ticket = InitialSetupAuthorization::capture_at(self.root)?;
        let plan = ticket.offer_reading(self.root, || Ok(None))?;
        observe_mutation_at(self.root, None, Err("busy".into()), |root| {
            ticket.persist_with(
                root,
                &plan,
                Admission::Automatic,
                |step, root| step.persist(root),
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result?
        .ok_or_else(|| "the startup review was refused".into())
    }

    pub fn choose(&self) -> Result<u64, String> {
        observe_mutation_at(self.root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &self.plan,
                false,
                || Ok(None),
                save_progress,
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result?
        .ok_or_else(|| "the person's choice was refused".into())
    }

    pub fn stop_revision(&self, revision: u64) -> Result<(), String> {
        request_stop_at(self.root, revision)
    }
}

fn lose_publication(root: &Path) -> Result<(), String> {
    std::fs::rename(
        root.join("automatic-plan.json"),
        root.join("held-admission-plan.json"),
    )
    .map_err(|error| error.to_string())?;
    std::fs::create_dir(root.join("automatic-plan.json")).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_stop_target_survives_terminal_publication_capture_loss() {
        for restart in [false, true] {
            for fault in [
                InitialSetupAdmissionFault::BeforeFinalPublicationUnknown,
                InitialSetupAdmissionFault::FinalPublicationUnknown,
            ] {
                let root = aterm_tempfile::tempdir().unwrap();
                let mut owner = if restart {
                    InitialSetupAdmissionStopTest::restart(root.path()).unwrap()
                } else {
                    InitialSetupAdmissionStopTest::startup(root.path()).unwrap()
                };
                assert!(owner.persist(fault).is_err());
                assert!(!owner.pending());
                let revision = owner.snapshot().unwrap().revision;
                assert_eq!(owner.stop_target().unwrap(), Some(revision));
                let before = owner.snapshot().unwrap().observation;
                assert_eq!(
                    owner.persist(InitialSetupAdmissionFault::None).unwrap(),
                    None
                );
                assert_eq!(owner.snapshot().unwrap().observation, before);
                let replacement = owner.choose().unwrap();
                assert!(replacement > revision);
                owner.stop_revision(revision).unwrap();
                assert!(!owner.snapshot().unwrap().finished);
                assert_eq!(owner.recover().unwrap(), replacement);
            }
        }
    }

    #[test]
    fn admission_stop_before_a_confirmed_progress_never_guesses_a_target() {
        for fault in [
            InitialSetupAdmissionFault::PlanPublicationUnknown,
            InitialSetupAdmissionFault::ProgressPublicationUnknown,
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
            assert!(owner.persist(fault).is_err());
            assert!(!owner.pending());
            assert!(owner.stop_target().is_err());
            let revision = owner.choose().unwrap();
            if fault == InitialSetupAdmissionFault::PlanPublicationUnknown {
                assert_eq!(
                    revision, 1,
                    "a later choice can use the unreserved next revision"
                );
            }
            assert!(owner.stop_target().is_err());
            let before = owner.snapshot().unwrap().observation;
            assert_eq!(
                owner.persist(InitialSetupAdmissionFault::None).unwrap(),
                None
            );
            assert_eq!(owner.snapshot().unwrap().observation, before);
        }
    }

    #[test]
    fn admission_stop_knows_an_ordinary_failed_final_save_without_admitting_again() {
        for restart in [false, true] {
            for fault in [
                InitialSetupAdmissionFault::BeforeFinal,
                InitialSetupAdmissionFault::AfterFinal,
            ] {
                let root = aterm_tempfile::tempdir().unwrap();
                let mut owner = if restart {
                    InitialSetupAdmissionStopTest::restart(root.path()).unwrap()
                } else {
                    InitialSetupAdmissionStopTest::startup(root.path()).unwrap()
                };
                assert!(owner.persist(fault).is_err());
                assert!(owner.pending());
                let revision = owner.snapshot().unwrap().revision;
                assert_eq!(owner.stop_target().unwrap(), Some(revision));
                owner.stop_revision(revision).unwrap();
                let before = owner.snapshot().unwrap().observation;
                assert!(owner.recover().is_err());
                assert_eq!(owner.snapshot().unwrap().observation, before);
            }
        }
    }

    #[test]
    fn admission_stop_of_a_staged_target_holds_a_fresh_startup_without_reauthorization() {
        for (stopped, empty) in [(false, false), (true, false), (true, true)] {
            let root = aterm_tempfile::tempdir().unwrap();
            let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
            if empty {
                owner.plan.selection = Selection::none();
            }
            assert!(
                owner
                    .persist(InitialSetupAdmissionFault::BeforeFinalPublicationUnknown)
                    .is_err()
            );
            let target = owner.snapshot().unwrap().revision;
            if stopped {
                owner.stop_revision(target).unwrap();
                let before = owner.snapshot().unwrap().observation;
                assert!(
                    owner.recover().is_err(),
                    "a stopped staged run was admitted again"
                );
                let snapshot = owner.snapshot().unwrap();
                assert!(snapshot.finished && !snapshot.permissions_allowed);
                assert_eq!(snapshot.revision, target);
                assert_eq!(snapshot.observation, before);
            } else {
                assert!(
                    owner.recover().unwrap() > target,
                    "unstopped staging remains recoverable"
                );
            }
        }
    }

    #[test]
    fn admission_stop_confirms_only_the_owned_pinned_failed_progress_publication() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut owner = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
        assert!(
            owner
                .persist(InitialSetupAdmissionFault::ProgressAfter)
                .is_err()
        );
        assert!(owner.pending());
        let target = owner.snapshot().unwrap().revision;
        assert_eq!(owner.stop_target().unwrap(), Some(target));
        let newer = owner.choose().unwrap();
        assert!(newer > target);
        let before = owner.snapshot().unwrap().observation;
        assert_eq!(
            owner.persist(InitialSetupAdmissionFault::None).unwrap(),
            None
        );
        assert_eq!(owner.stop_target().unwrap(), Some(target));
        assert_eq!(owner.snapshot().unwrap().observation, before);
    }

    #[test]
    fn admission_stop_restart_cleanup_failure_keeps_its_successfully_staged_target() {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut owner = InitialSetupAdmissionStopTest::restart(root.path()).unwrap();
        assert!(
            owner
                .persist(InitialSetupAdmissionFault::StagedCleanup)
                .is_err()
        );
        assert!(!owner.pending());
        let target = owner.snapshot().unwrap().revision;
        assert_eq!(owner.stop_target().unwrap(), Some(target));
        owner.stop_revision(target).unwrap();
        let before = owner.snapshot().unwrap().observation;
        assert!(owner.recover().is_err());
        assert!(owner.snapshot().unwrap().finished);
        assert_eq!(
            owner.persist(InitialSetupAdmissionFault::None).unwrap(),
            None
        );
        assert_eq!(owner.snapshot().unwrap().observation, before);
    }
}
