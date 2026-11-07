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

    /// The actual bring-forward writer, under the profile lock, as a launch runs it: the run it
    /// staged for the launch's offer, if any. A startup bound after it reviews what it brought
    /// forward ([`Self::startup`]).
    pub fn bring_forward(&self) -> Result<Option<u64>, String> {
        let _lock = HeldLock::take(self.root)?;
        bring_forward_restored_observing_using(self.root, |_| Ok(()), |_| {}, save_progress)
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

    /// Choose the scratch plan that the real admission writer will persist.
    #[must_use]
    pub fn with_plan(mut self, plan: Plan) -> Self {
        self.plan = plan;
        self
    }

    /// Run the real coordinator and native preference stage over inert supplied ports. Only
    /// a workstation-only fixture is admitted; every other stage refuses without live I/O.
    pub fn native_workstation_pass(
        &self,
        revision: u64,
        system: impl Fn() -> Box<dyn aterm_machines::port::System>,
    ) -> Result<Option<(Report, SetupNext)>, String> {
        if self.plan.selection != preferences(self.plan.selection) {
            return Err("the inert native coordinator accepts preferences alone".into());
        }
        let mut host = NativeWorkstationInitial {
            sys: system(),
            workstation: NativeHost::over(
                system(),
                Box::new(aterm_machines::init::prepare_workstation_selected),
            ),
        };
        let _lock = HeldLock::take(self.root)?;
        execute_initial_for_revision(&mut host, self.root, revision, &mut None, SetupHooks::NONE)
    }

    /// The actual accepted-setting reader over these inert records.
    pub fn input_activation_revision(&self) -> Result<Option<u64>, String> {
        input_activation::revision_at(self.root)
    }

    /// The production scoped-task admission over these inert records.
    pub fn pending_inputs(&self) -> Result<Option<PendingInputAdmission>, String> {
        input_pass::capture_at(self.root)
    }

    /// The production scoped coordinator, retaining the original plan while refusing all
    /// other stages through the inert host.
    pub fn native_input_pass(
        &self,
        expected: &PendingInputAdmission,
        system: impl Fn() -> Box<dyn aterm_machines::port::System>,
        hooks: SetupHooks,
    ) -> Result<Option<InitialInputPass>, String> {
        let mut host = NativeWorkstationInitial {
            sys: system(),
            workstation: NativeHost::over(
                system(),
                Box::new(aterm_machines::init::prepare_workstation_selected),
            ),
        };
        let _lock = HeldLock::take(self.root)?;
        input_pass::run_at(&mut host, self.root, expected, hooks)
    }

    /// A scoped mark using only the supplied inert console and service facts.
    pub fn input_park_mark(
        &self,
        expected: &PendingInputAdmission,
        sys: &dyn aterm_machines::port::System,
    ) -> InitialSetupParkMark {
        input_pass::park_mark(Some(self.root), expected, ledger::now_secs())
            .with_console(|| Some(!aterm_machines::console::remote(sys)))
            .with_keyboard_services(|| sys.keyboard_service_ids())
    }

    /// The same inert native input ports shared by coordinator and headless startup tests.
    /// The first observation lacks a service model; the attached observation can verify both
    /// current keyboard interfaces without invoking a setter.
    pub fn keyboard_system(attached: bool) -> aterm_machines::port::Fake {
        use aterm_machines::items::keyboard::{Model, caps_to_control};
        use aterm_machines::model::{PValue, User};
        use aterm_machines::port::{Fake as Machine, Ran, Stat};

        let user = User {
            name: "alice".into(),
            uid: 501,
            gid: 20,
            home: "/Users//alice".into(),
            shell: "/bin/zsh".into(),
            admin: false,
        };
        let mut sys = Machine::root();
        sys.euid = user.uid;
        sys.logged_in(&user);
        sys.keyboard_services = Some(Ok(if attached { vec![41, 42] } else { vec![41] }));
        sys.stat_of(
            "/dev/console",
            Stat {
                uid: user.uid,
                gid: user.gid,
                mode: 0o600,
                dir: false,
                symlink: false,
                len: 0,
            },
        );
        let census = format!(
            "Services:\n{:<12}{:<12}{:<16}{:<24}Built-In\n{:<12}{:<12}{:<16}{:<24}0\n{:<12}{:<12}{:<16}{:<24}0\n\nDevices:\n",
            "VendorID",
            "ProductID",
            "RegistryID",
            "Product",
            "0xfffe",
            "0xa9",
            "0x29",
            "Fixture keyboard",
            "0xfffe",
            "0xa9",
            "0x2a",
            "Fixture keyboard"
        );
        let filter = |id| {
            format!(
                "<dict><key>IORegistryEntryID</key><integer>{id}</integer>\
                 <key>PrimaryUsagePage</key><integer>1</integer>\
                 <key>PrimaryUsage</key><integer>6</integer>\
                 <key>ServiceFilterDebug</key><array><dict><key>plugin</key><dict>\
                 <key>Class</key><string>IOHIDKeyboardFilter</string>\
                 <key>HIDKeyboardModifierMappingPairs</key><array/>\
                 <key>UserKeyMapping</key><array><dict>\
                 <key>Src</key><integer>30064771129</integer>\
                 <key>Dst</key><integer>30064771296</integer></dict></array>\
                 <key>FnFunctionUsageMap</key><array/>\
                 <key>FnKeyboardUsageMap</key><array/>\
                 <key>NumLockKeyboardUsageMap</key><array/>\
                 </dict></dict></array></dict>"
            )
        };
        let dump = format!(
            "<plist version=\"1.0\"><dict><key>ServiceRecords</key><array>{}{}</array></dict></plist>",
            filter(41),
            filter(42)
        );
        sys.simulate(move |_, cmd| {
            let args: Vec<_> = cmd.args.iter().filter_map(|arg| arg.to_str()).collect();
            match cmd.program.file_name().and_then(|name| name.to_str()) {
                Some("hidutil") if args.first() == Some(&"list") => Some(Ran::ok(if attached {
                    &census
                } else {
                    "Services:\n\nDevices:\n"
                })),
                Some("hidutil") if args.first() == Some(&"property") => Some(Ran::ok(
                    "RegistryID Key Value\n29 UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n2a UserKeyMapping ( { HIDKeyboardModifierMappingSrc = 30064771129; HIDKeyboardModifierMappingDst = 30064771296; } )\n"
                )),
                Some("hidutil") if args.first() == Some(&"dump") => Some(Ran::ok(&dump)),
                Some("defaults") => {
                    let entries = if args.contains(&"-g") {
                        [Model { vendor: 0, product: 0 }, Model { vendor: 65534, product: 169 }]
                            .into_iter()
                            .map(|model| (model.key(), PValue::Array(vec![caps_to_control()])))
                            .collect()
                    } else {
                        Vec::new()
                    };
                    Some(Ran::ok(&aterm_machines::plist::document(&PValue::Dict(entries))))
                }
                _ => None,
            }
        });
        sys
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

    /// The startup review's answer, a Join of the vault at `vault_url` with the Secrets group
    /// on, written while this Mac's vault configuration cannot be read (a root-owned
    /// `source.toml`), and whether the ticket admitted it without its Join
    /// ([`InitialSetupAuthorization::join_unread`]).
    pub fn persist_join_over_unread_vault(
        &mut self,
        vault_url: &str,
    ) -> (Result<Option<u64>, String>, bool) {
        self.plan.selection.set(Setting::Secrets, true);
        self.plan.vault_url = vault_url.to_owned();
        let Owner::Startup(ticket) = &mut self.owner else {
            panic!("the scratch owner is a choice, not a startup review");
        };
        let result = observe_mutation_at(self.root, None, Err("busy".into()), |root| {
            ticket.persist_join_with(
                root,
                &self.plan,
                || Err("source.toml: Permission denied".into()),
                |step, root| step.persist(root),
                |root| observation_at(root).map_err(|error| error.to_string()),
            )
        })
        .result;
        (result, ticket.join_unread())
    }

    /// The Cancel an older aterm's first-launch review recorded of this scratch plan
    /// ([`save_older_cancel`]), which this aterm still reads: no aterm records one since the
    /// review was deleted (init-on-by-default design §6.1).
    pub fn older_cancel(&self) -> Result<(), String> {
        let _lock = HeldLock::take(self.root)?;
        save_older_cancel(self.root, &self.plan)
    }

    /// Set up now, as System State sends it (no group changed), through the real writer.
    pub fn set_up_now(&self) -> Result<Option<u64>, String> {
        observe_mutation_at(self.root, None, Err("busy".into()), |root| {
            InitialSetupRestart::default()
                .changing(Some(Selection::none()))
                .persist_with(
                    root,
                    &Plan::from(Selection::none()),
                    false,
                    || Ok(None),
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
        })
        .result
    }

    /// A group choice of `settings` set up, sent as System State sends one after a Cancel (the
    /// page's plan with the pressed group, that group changed), through the real writer.
    pub fn choose_group(&self, settings: &[Setting]) -> Result<Option<u64>, String> {
        let mut changed = Selection::none();
        for setting in settings {
            changed.set(*setting, true);
        }
        // A page still showing every group at Set up sends them all: only the press counts.
        let plan = Plan::default();
        observe_mutation_at(self.root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen()
                .changing(Some(changed))
                .persist_with(
                    root,
                    &plan,
                    false,
                    || Ok(None),
                    save_progress,
                    |root| observation_at(root).map_err(|error| error.to_string()),
                )
        })
        .result
    }

    /// The plan the saved records hold, as written (not as presented).
    pub fn saved_plan(&self) -> Result<Option<Plan>, String> {
        saved_plan(self.root)
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

struct NativeWorkstationInitial {
    sys: Box<dyn aterm_machines::port::System>,
    workstation: NativeHost,
}

impl InitialHost for NativeWorkstationInitial {
    fn legacy(&mut self, _root: &Path) -> Result<Option<Report>, String> {
        Err("the inert native coordinator accepts modern preferences alone".into())
    }

    fn workstation(
        &mut self,
        root: &Path,
        only: Option<Selection>,
    ) -> Result<Option<Report>, String> {
        apply_part_with(
            root,
            self.sys.as_ref(),
            &mut self.workstation,
            "native-input-fixture",
            std::time::UNIX_EPOCH + std::time::Duration::from_secs(self.sys.now()),
            Part::Preferences { only },
        )
    }

    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        Err("no Spotlight port in the native preference fixture".into())
    }

    fn permissions(&mut self, _plan: &Plan, _revision: u64) -> Result<Vec<Row>, String> {
        Err("no permissions port in the native preference fixture".into())
    }

    fn secrets(
        &mut self,
        _plan: &Plan,
        _replaces: Replaces,
        _password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        Err("no vault port in the native preference fixture".into())
    }

    fn secrets_pace(&mut self) -> Option<Pace> {
        None
    }

    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        Err("no vault port in the native preference fixture".into())
    }

    fn current_vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
        Err("no vault port in the native preference fixture".into())
    }

    fn current_repositories(&mut self, _vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        Err("no repository port in the native preference fixture".into())
    }

    fn repositories(&mut self, _vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        Err("no repository port in the native preference fixture".into())
    }

    fn accounts(
        &mut self,
        _root: &Path,
        _on_screen: fn() -> bool,
    ) -> Result<Option<Report>, String> {
        Err("no account port in the native preference fixture".into())
    }

    fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
        Ok(None)
    }

    fn boot_session(&mut self) -> Option<u128> {
        None
    }

    fn now_secs(&mut self) -> u64 {
        self.sys.now()
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
