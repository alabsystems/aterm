// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

mod deferred_mac_deadline_tests;
mod embedded_restore_tests;
mod ledger_conformance;
mod pending_local_inputs_tests;
mod pending_repository_inputs_tests;
mod resume_tests;
pub(super) mod run_ledger_tests;
mod upgrade_completion_tests;

type StageReading = (bool, Vec<Setting>, Vec<Subject>);
type AccountsEnd = Box<dyn FnOnce(&mut Fake)>;

/// A pass nobody stops; acquisition no longer requires a particular page to be visible.
const AT_THE_PAGE: SetupHooks = SetupHooks::NONE;

#[derive(Default)]
pub(super) struct Fake {
    calls: Vec<Stage>,
    software_epoch: Option<ledger::SoftwareEpoch>,
    pending: Option<Stage>,
    /// A stage whose row waits on a person ([`WAITING`]).
    waiting: Option<Stage>,
    failure: Option<Stage>,
    password_seen: bool,
    observe: Option<PathBuf>,
    vault_checks: usize,
    vault_blocked: bool,
    vault_error: bool,
    /// A local input changes after the sign-outs verified their effects.
    accounts_end: Option<AccountsEnd>,
    /// The access revision the last Permissions stage read.
    access_read: Option<u64>,
    /// While this stage runs, a person stops the run (System State's Stop)
    /// and the process ends: [`Self::relaunch`] is what the next launch reads.
    stop_during: Option<(Stage, PathBuf)>,
    relaunch: Option<Relaunch>,
    vault_url: Option<String>,
    pace: Option<Pace>,
    configured: Option<ConfiguredVault>,
    configured_unreadable: bool,
    checked_url: Option<String>,
    native_repositories: Option<(aterm_machines::port::Fake, aterm_machines::items::Ctx)>,
    native_repository_readback:
        Option<std::sync::Arc<aterm_machines::init_github::RepositoryReadback>>,
    /// The live Accounts stage adapter over an inert account machine.
    native_accounts: Option<(aterm_machines::port::Fake, NativeHost)>,
    native_accounts_now: Option<std::time::SystemTime>,
    /// The live preference stage adapter over an inert preference machine.
    native_workstation: Option<NativeHost>,
    /// The reason its failed part gave ([`repositories::failure_note`]).
    repositories_failure: Option<&'static str>,
    /// Whether it stopped for want of a GitHub token ([`repositories::want_token`]).
    token_wanted: bool,
    /// What each Workstation stage was given to set ([`Progress::only`]).
    only: Vec<Option<Selection>>,
    /// A stage whose every run ends the process mid-stage (a panic the test catches), as a
    /// crash or a kill there leaves its records.
    crash: Option<Stage>,
    /// A stage that wedges: the test reads the run while it runs, then the process is
    /// killed there.
    wedge: Option<(Stage, PathBuf)>,
    /// What the readings of the run said while the wedged stage ran: now, and two hours
    /// on ([`tests::run_ledger_tests::wedged_rows`]).
    wedged_rows: Vec<Vec<Row>>,
    /// The Secrets stage's rows, in place of the injected outcome.
    secrets_rows: Option<Vec<Row>>,
    /// The Permissions stage's rows, in place of the injected outcome.
    permissions_rows: Option<Vec<Row>>,
    /// Whether each Repositories stage was handed the plan's vault.
    repositories_vault: Vec<bool>,
    /// aterm quits as each stage of the run under this root is about to begin: the quit's
    /// note is taken ([`ledger::interrupt_at`]) as shutdown goes on and the pass with it.
    quit_at_begin: Option<PathBuf>,
}

/// What a launch reads of a run whose process ended mid-stage.
#[derive(Debug)]
struct Relaunch {
    finished: bool,
    admission: Option<InitialSetupAdmission>,
    snapshot_finished: bool,
}

impl Fake {
    pub(super) fn with_workstation(mut self, host: NativeHost) -> Self {
        self.native_workstation = Some(host);
        self
    }

    fn stage(&mut self, stage: Stage) -> Result<Vec<Row>, String> {
        self.calls.push(stage);
        assert_ne!(
            self.crash,
            Some(stage),
            "injected: the process ends in this stage"
        );
        if let Some((wedged, root)) = &self.wedge
            && *wedged == stage
        {
            self.wedged_rows = tests::run_ledger_tests::wedged_rows(root);
            panic!("injected: the wedged process is killed in this stage");
        }
        if let Some((during, root)) = &self.stop_during
            && *during == stage
        {
            let revision = progress(root).unwrap().unwrap().revision;
            request_stop_at(root, revision).unwrap();
            self.relaunch = Some(Relaunch {
                finished: finished_at(root).unwrap(),
                admission: admission_at(root).unwrap(),
                snapshot_finished: status_at(root, Plan::default()).unwrap().1,
            });
        }
        if let Some(root) = &self.observe {
            let (_, finished, rows) = status_at(root, Plan::default()).unwrap();
            assert!(!finished);
            assert!(
                rows.iter()
                    .any(|row| row.name == stage.name() && row.status == "running")
            );
        }
        if self.failure == Some(stage) {
            return Err("offline; retry later".into());
        }
        if self.waiting == Some(stage) {
            return Ok(vec![Row {
                name: stage.name().into(),
                status: WAITING.into(),
                detail: "injected wait for a person".into(),
                elapsed_ms: None,
            }]);
        }
        Ok(vec![readiness_row(
            stage.name(),
            self.pending != Some(stage),
            "injected stage outcome".into(),
        )])
    }
}

impl InitialHost for Fake {
    fn software_epoch(&mut self) -> Option<ledger::SoftwareEpoch> {
        self.software_epoch
    }
    fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.workstation(root, None)
    }
    fn now_secs(&mut self) -> u64 {
        // The pass reads the clock for a stage's begin, just before it.
        if let Some(root) = &self.quit_at_begin {
            ledger::interrupt_at(root).unwrap();
        }
        ledger::now_secs()
    }
    fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String> {
        if let Some((sys, host)) = &mut self.native_accounts {
            self.calls.push(Stage::Accounts);
            return apply_part_with(
                root,
                sys,
                host,
                "inert-init-completion",
                self.native_accounts_now.unwrap_or(
                    std::time::SystemTime::UNIX_EPOCH
                        + std::time::Duration::from_secs(1_800_000_000),
                ),
                Part::Accounts,
            );
        }
        // As the real sign-out pass marks and records its pass ([`record_pending`],
        // [`record_applied`]).
        record_pending(root)?;
        let rows = self.stage(Stage::Accounts)?;
        if rows_ready(&rows) {
            let planned = authorized_selection(root)?.unwrap_or_default();
            record_applied(root, accounts(planned))?;
            if let Some(changed) = self.accounts_end.take() {
                changed(self);
            }
        }
        Ok(Some(Report {
            action: Action::Apply,
            rows,
            manual_steps: Vec::new(),
            backup: root.join("original-preferences.json"),
            restore: None,
            page: None,
        }))
    }
    fn workstation(
        &mut self,
        root: &Path,
        only: Option<Selection>,
    ) -> Result<Option<Report>, String> {
        if let Some(host) = &mut self.native_workstation {
            self.calls.push(Stage::Workstation);
            // Each pass a minute after the one before, as the run's own pause spaces them
            // (`SETUP_RETRY_WAIT`), so the automatic gate, dated by the pass's own clock,
            // lets each one through.
            let passes = self
                .calls
                .iter()
                .filter(|stage| **stage == Stage::Workstation)
                .count() as u64;
            return apply_part_with(
                root,
                &super::super::tests::Inert::default(),
                host,
                "inert-init-preference-completion",
                std::time::UNIX_EPOCH
                    + std::time::Duration::from_secs(
                        1_800_000_000 + (passes - 1) * super::super::AUTOMATIC_PAUSE_SECS,
                    ),
                Part::Preferences { only },
            );
        }
        if first_run_complete(root)? {
            return Ok(None);
        }
        self.only.push(only);
        // As the real pass marks its pass pending before it sets anything, and records
        // it: over the plan's preferences, or `only` those a choice narrowed it to.
        record_pending(root)?;
        let rows = self.stage(Stage::Workstation)?;
        if rows_ready(&rows) {
            let mut set = preferences(authorized_selection(root)?.unwrap_or_default());
            if let Some(only) = only {
                for setting in Setting::ALL {
                    set.set(setting, set.contains(setting) && only.contains(setting));
                }
            }
            record_applied(root, set)?;
        }
        Ok(Some(Report {
            action: Action::Apply,
            rows,
            manual_steps: Vec::new(),
            backup: root.join("original-preferences.json"),
            restore: None,
            page: None,
        }))
    }
    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        self.stage(Stage::Spotlight)
    }
    fn permissions(&mut self, _: &Plan, access: u64) -> Result<Vec<Row>, String> {
        self.access_read = Some(access);
        if let Some(rows) = &self.permissions_rows {
            self.calls.push(Stage::Permissions);
            return Ok(rows.clone());
        }
        self.stage(Stage::Permissions)
    }
    fn secrets(
        &mut self,
        plan: &Plan,
        _replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        self.password_seen = password.is_some();
        self.vault_url = Some(plan.vault_url.clone());
        if let Some(rows) = &self.secrets_rows {
            self.calls.push(Stage::Secrets);
            return Ok(rows.clone());
        }
        self.stage(Stage::Secrets)
    }
    fn secrets_pace(&mut self) -> Option<Pace> {
        self.pace.clone()
    }
    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        if self.configured_unreadable {
            return Err("source.toml: no name".into());
        }
        Ok(self.configured.clone())
    }
    fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        self.repositories_vault.push(vault.is_some());
        if let Some((sys, ctx)) = &self.native_repositories {
            self.calls.push(Stage::Repositories);
            self.native_repository_readback = None;
            let rows = aterm_machines::init_github::run_initial_repositories(
                sys,
                ctx,
                aterm_machines::model::Mode::Apply,
            )?;
            self.repositories_failure = repositories::failure_note(&rows);
            self.token_wanted = repositories::want_token(&rows);
            self.native_repository_readback = rows.readback.map(std::sync::Arc::new);
            return Ok(item_rows(rows.rows));
        }
        self.stage(Stage::Repositories)
    }

    fn repositories_failure(&mut self) -> Option<&'static str> {
        self.repositories_failure
    }

    fn current_repositories(&mut self, _vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        match &self.native_repositories {
            Some((sys, ctx)) => {
                // This host injects vault readiness separately. Observe the same native
                // local inputs its repositories stage applies; the repositories module's
                // joined-vault fixtures exercise the actual materialization boundary.
                aterm_machines::init_github::run_initial_repositories_captured(
                    sys,
                    ctx,
                    aterm_machines::model::Mode::Plan,
                    aterm_secrets::converge::RepositoryWork::Repair,
                    aterm_machines::init_github::InitialRepositoryInputs::capture_ready(sys, ctx)?
                        .with_repository_readback(self.native_repository_readback.clone()),
                    || Ok(true),
                )
                .map(|run| item_rows(run.rows))
            }
            None => Ok(vec![readiness_row(
                Stage::Repositories.name(),
                true,
                "injected current repository readiness".into(),
            )]),
        }
    }

    fn repositories_want_token(&mut self) -> bool {
        self.token_wanted
    }

    fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
        Ok(None)
    }

    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        self.vault_checks += 1;
        self.checked_url = Some(plan.vault_url.clone());
        if self.vault_error {
            return Err("injected local vault readiness failure".into());
        }
        Ok(vec![readiness_row(
            Stage::Secrets.name(),
            !self.vault_blocked,
            "injected current vault readiness".into(),
        )])
    }
}

fn selected(settings: &[Setting]) -> Plan {
    let mut selection = Selection::none();
    for setting in settings {
        selection.set(*setting, true);
    }
    Plan::from(selection)
}

/// [`selected`] `settings` and every setting of `groups`.
fn plan_of(settings: &[Setting], groups: &[aterm_types::macos_setup::Group]) -> Plan {
    let mut plan = selected(settings);
    for group in groups {
        for setting in group.settings() {
            plan.selection.set(*setting, true);
        }
    }
    plan
}

/// Bytes a setup record cannot be read from.
const CORRUPT: &[u8] = b"{\"profile\":";

#[test]
fn an_old_startup_offer_cannot_replace_a_newer_pending_doctor_plan() {
    let root = aterm_tempfile::tempdir().unwrap();
    let offered = Plan::default();
    let baseline = observation_at(root.path()).unwrap();
    let mut authorization = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), 0);
    std::fs::write(
        root.path().join("original-preferences.json"),
        b"invalid backup",
    )
    .unwrap();
    assert!(super::super::tests::restore_without_effects(root.path()).is_err());
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let doctor_plan = saved_plan(root.path()).unwrap().unwrap();
    // With no plan on record the explicit plan is aterm's own base plan with the profile;
    // the Restore withdrew no permission, so it keeps aterm's permissions.
    let explicit = aterm_types::macos_setup::BASE_PLAN.union(Selection::all().workstation());
    assert!(explicit.contains(Setting::Permissions));
    assert_eq!(doctor_plan.selection, explicit);
    assert_eq!(revision_at(root.path()).unwrap(), 1);
    assert!(!finished_at(root.path()).unwrap());
    let before = observation_at(root.path()).unwrap();
    assert_ne!(before, baseline);

    let accepted = authorization
        .persist_at(root.path(), &offered)
        .result
        .unwrap();
    let mut old_worker = Fake {
        failure: Some(Stage::Workstation),
        ..Fake::default()
    };
    if let Some(revision) = accepted {
        execute_initial_for_revision(
            &mut old_worker,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
    }
    assert!(
        old_worker.calls.is_empty(),
        "a stale offer ran stages from its older plan"
    );
    assert_eq!(accepted, None);
    assert_eq!(revision_at(root.path()).unwrap(), 1);
    assert_eq!(saved_plan(root.path()).unwrap(), Some(doctor_plan));
    assert_eq!(observation_at(root.path()).unwrap(), before);
}

fn restart_attempt(
    attempt: &mut InitialSetupRestart,
    root: &Path,
    plan: &Plan,
    commit: impl FnOnce(&Path, Progress) -> Result<(), String>,
    observe: impl FnOnce(&Path) -> Result<InitialSetupObservation, String>,
) -> InitialSetupObserved<Option<u64>> {
    observe_mutation_at(
        root,
        observation_at(root).ok(),
        Err("busy".into()),
        |root| {
            assert!(HeldLock::try_take(root)?.is_none());
            attempt.persist_with(root, plan, true, || Ok(None), commit, observe)
        },
    )
}

fn observe_restart(root: &Path) -> Result<InitialSetupObservation, String> {
    observation_at(root).map_err(|error| error.to_string())
}

fn system_state_apply(
    root: &Path,
    plan: &Plan,
    vault_typed: bool,
    read: impl FnOnce() -> Result<Option<ConfiguredVault>, String>,
) -> u64 {
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::default().persist_with(
            root,
            plan,
            vault_typed,
            read,
            save_progress,
            observe_restart,
        )
    })
    .result
    .unwrap()
    .expect("admitted")
}

/// System State's group choice of `plan`, which sends the vault the worker holds, in a
/// scratch home that is in no vault: the admitted run's revision.
fn system_state_choice(root: &Path, plan: &Plan) -> u64 {
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            plan,
            false,
            || Ok(None),
            save_progress,
            observe_restart,
        )
    })
    .result
    .unwrap()
    .expect("admitted")
}

/// System State's Join of `plan`, its vault typed, in a scratch home that is in no
/// vault: the admitted run's revision.
fn system_state_join(root: &Path, plan: &Plan) -> u64 {
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            plan,
            true,
            || Ok(None),
            save_progress,
            observe_restart,
        )
    })
    .result
    .unwrap()
    .expect("admitted")
}

#[test]
fn restart_final_save_recovers_before_or_after_publication_without_a_new_revision() {
    for empty in [false, true] {
        for published in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(if empty { &[] } else { &[Setting::Secrets] });
            let original = authorize_at(root.path(), &plan).unwrap().unwrap();
            let mut attempt = InitialSetupRestart::default();
            let failed = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    if published {
                        save_progress(root, next)?;
                    }
                    Err("injected final namespace flush failure".into())
                },
                observe_restart,
            );
            assert!(failed.result.is_err());
            assert!(attempt.pending());
            let staged = progress(root.path()).unwrap().unwrap();
            assert_eq!(staged.revision, original + 1);
            assert_eq!(staged.admitted, published);
            let before = observation_at(root.path()).unwrap();
            let commits = std::cell::Cell::new(0);
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    commits.set(commits.get() + 1);
                    save_progress(root, next)
                },
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), Some(staged.revision));
            assert!(!attempt.pending());
            assert_eq!(commits.get(), usize::from(!published));
            assert_eq!(revision_at(root.path()).unwrap(), staged.revision);
            assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
            if published {
                assert_eq!(observation_at(root.path()).unwrap(), before);
            }
            let password = SecretBytes::from(b"test-only-retained-password".as_slice());
            let mut host = Fake::default();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                staged.revision,
                &mut Some(password.clone()),
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(
                host.calls,
                if empty {
                    Vec::new()
                } else {
                    vec![Stage::Secrets]
                }
            );
            assert_eq!(host.password_seen, !empty);
            assert!(finished_at(root.path()).unwrap());
        }
    }
}

fn a_stopped_run_before_its_stop_writer_saves() -> (aterm_tempfile::TempDir, Plan, u64) {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    let original = authorize_at(root.path(), &plan).unwrap().unwrap();
    // The running pass sees the window's immediate Stop at its boundary;
    // the independent Stop writer has not published its request yet.
    let writer = HeldLock::take(root.path()).unwrap();
    let mut prior = progress(root.path()).unwrap().unwrap();
    assert!(
        stop_at_boundary(
            root.path(),
            &mut prior,
            SetupHooks {
                stop: |_| true,
                ..SetupHooks::NONE
            },
        )
        .unwrap()
    );
    assert_eq!(stop_request_at(root.path()), None);
    drop(writer);
    (root, plan, original)
}

fn recover_restart_after_a_delayed_old_stop(published: bool) {
    let (root, plan, original) = a_stopped_run_before_its_stop_writer_saves();
    let mut attempt = InitialSetupRestart::chosen();
    let failed = restart_attempt(
        &mut attempt,
        root.path(),
        &plan,
        |root, next| {
            if published {
                save_progress(root, next)?;
            }
            // This separate lock is available while the new admission holds
            // profile.lock: the old asynchronous save can arrive this late.
            request_stop_at(root, original)?;
            Err("injected final admission flush failure".into())
        },
        observe_restart,
    );
    assert!(failed.result.is_err());
    assert!(attempt.pending());
    let staged = progress(root.path()).unwrap().unwrap();
    assert_eq!(staged.revision, original + 1);
    assert_eq!(staged.admitted, published);
    assert_eq!(
        stop_request_at(root.path()),
        Some(StopRequest::Revision(original))
    );
    let before = observation_at(root.path()).unwrap();
    let commits = std::cell::Cell::new(0);
    let recovered = restart_attempt(
        &mut attempt,
        root.path(),
        &plan,
        |root, next| {
            commits.set(commits.get() + 1);
            save_progress(root, next)
        },
        observe_restart,
    );
    assert_eq!(
        recovered.result.unwrap(),
        Some(staged.revision),
        "a delayed Stop for the old run must not refuse the new choice"
    );
    assert!(!attempt.pending());
    assert_eq!(commits.get(), usize::from(!published));
    if published {
        assert_eq!(observation_at(root.path()).unwrap(), before);
    }
    let mut host = Fake::default();
    let mut password = Some(SecretBytes::from(b"retained-choice-password".as_slice()));
    execute_initial_for_revision(
        &mut host,
        root.path(),
        staged.revision,
        &mut password,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the confirmed new choice executes");
    assert_eq!(host.calls, [Stage::Secrets]);
    assert!(host.password_seen);
    assert_eq!(revision_at(root.path()).unwrap(), staged.revision);
    assert!(finished_at(root.path()).unwrap());
    assert!(
        execute_initial_for_revision(
            &mut host,
            root.path(),
            staged.revision,
            &mut password,
            SetupHooks::NONE,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(host.calls, [Stage::Secrets], "the stage ran once");
}

#[test]
fn owned_restart_stop_recovers_before_final_publication() {
    recover_restart_after_a_delayed_old_stop(false);
}

#[test]
fn owned_restart_stop_recovers_after_final_publication() {
    recover_restart_after_a_delayed_old_stop(true);
}

#[test]
fn owned_restart_stop_keeps_current_requests_and_new_publications_authoritative() {
    for published in [false, true] {
        for replacement in ["current Stop", "future Stop", "unreadable Stop", "new plan"] {
            let (root, plan, original) = a_stopped_run_before_its_stop_writer_saves();
            let mut attempt = InitialSetupRestart::chosen();
            assert!(
                restart_attempt(
                    &mut attempt,
                    root.path(),
                    &plan,
                    |root, next| {
                        if published {
                            save_progress(root, next)?;
                        }
                        Err("injected final admission flush failure".into())
                    },
                    observe_restart,
                )
                .result
                .is_err()
            );
            {
                let _writer = HeldLock::take(root.path()).unwrap();
                match replacement {
                    "current Stop" => request_stop_at(root.path(), original + 1).unwrap(),
                    "future Stop" => request_stop_at(root.path(), original + 2).unwrap(),
                    "unreadable Stop" => {
                        write_json(&root.path().join(STOP_REQUEST), &json!({"invalid":true}))
                            .unwrap();
                    }
                    _ => {
                        let newer = selected(&[Setting::Dock, Setting::Secrets]);
                        save_plan_as(root.path(), &newer, false).unwrap();
                    }
                }
            }
            let before = observation_at(root.path()).unwrap();
            let recovered = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |_, _| panic!("superseded admission cannot commit"),
                observe_restart,
            );
            assert_eq!(recovered.result.unwrap(), None, "{replacement}");
            assert!(!attempt.pending());
            assert_eq!(observation_at(root.path()).unwrap(), before);
            if replacement != "future Stop" {
                let mut host = Fake::default();
                let _ = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    original + 1,
                    &mut None,
                    SetupHooks::NONE,
                );
                assert!(host.calls.is_empty(), "{replacement}");
            }
        }
    }
}

#[test]
fn repeated_restart_commit_and_confirmation_errors_retain_the_owned_publications() {
    for published in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let mut attempt = InitialSetupRestart::default();
        assert!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    if published {
                        save_progress(root, next)?;
                    }
                    Err("injected final flush failure".into())
                },
                observe_restart,
            )
            .result
            .is_err()
        );
        let owned = observation_at(root.path()).unwrap();
        for _ in 0..2 {
            let failed = restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |_, _| Err("injected repeated final flush failure".into()),
                |root| {
                    if published {
                        Err("injected confirmation directory flush failure".into())
                    } else {
                        observe_restart(root)
                    }
                },
            );
            assert!(failed.result.is_err());
            assert!(attempt.pending());
            assert_eq!(observation_at(root.path()).unwrap(), owned);
        }
        let recovered = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            save_progress,
            observe_restart,
        );
        assert_eq!(recovered.result.unwrap(), Some(1));
        assert!(!attempt.pending());
        assert_eq!(revision_at(root.path()).unwrap(), 1);
    }
}

#[test]
fn restart_prefinal_failures_never_confirm_an_older_same_plan_admission() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let mut prior = progress(root.path()).unwrap().unwrap();
    prior.revision = u64::MAX;
    save_progress(root.path(), prior).unwrap();
    let before = observation_at(root.path()).unwrap();
    let mut attempt = InitialSetupRestart::default();
    let failed = restart_attempt(
        &mut attempt,
        root.path(),
        &plan,
        |_, _| panic!("a preflight failure cannot reach final admission"),
        observe_restart,
    );
    assert!(failed.result.is_err());
    assert!(!attempt.pending());
    assert_eq!(observation_at(root.path()).unwrap(), before);
    assert_eq!(
        admission_at(root.path()).unwrap().unwrap().revision,
        u64::MAX
    );
    assert_eq!(
        restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            save_progress,
            observe_restart
        )
        .result
        .unwrap(),
        None
    );
    assert_eq!(observation_at(root.path()).unwrap(), before);

    let staged = aterm_tempfile::tempdir().unwrap();
    let mut attempt = InitialSetupRestart::default();
    assert!(
        restart_attempt(
            &mut attempt,
            staged.path(),
            &plan,
            |_, _| panic!("an unconfirmed staged run cannot reach final admission"),
            |_| Err("injected staged observation failure".into()),
        )
        .result
        .is_err()
    );
    assert!(!attempt.pending());
    assert!(admission_at(staged.path()).unwrap().is_none());
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, staged.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
}

#[test]
fn restart_recovery_retires_after_a_stop_new_run_or_same_content_publication() {
    for replacement in 0..5 {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets]);
        let mut attempt = InitialSetupRestart::default();
        assert!(
            restart_attempt(
                &mut attempt,
                root.path(),
                &plan,
                |root, next| {
                    save_progress(root, next)?;
                    Err("injected published admission failure".into())
                },
                observe_restart,
            )
            .result
            .is_err()
        );
        {
            let _writer = HeldLock::take(root.path()).unwrap();
            match replacement {
                0 | 1 => {
                    write_json(
                        &root.path().join("first-run.json"),
                        &json!({"profile":PROFILE,"state":if replacement == 0 {"cancelled"} else {"restored"}}),
                    ).unwrap();
                    let progress = root.path().join("initial-setup.json");
                    std::fs::remove_file(&progress).unwrap();
                    std::fs::create_dir(progress).unwrap();
                }
                2 => {
                    restart_at(root.path(), &plan).unwrap();
                }
                3 => {
                    save_progress(root.path(), progress(root.path()).unwrap().unwrap()).unwrap();
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
        let before = observation_at(root.path()).ok();
        let progress_bytes = std::fs::read(root.path().join("initial-setup.json")).ok();
        let decision_bytes = std::fs::read(root.path().join("first-run.json")).unwrap();
        let recovered = restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |_, _| panic!("supersession cannot write the old admission"),
            observe_restart,
        );
        assert_eq!(recovered.result.unwrap(), None);
        assert!(!attempt.pending());
        assert_eq!(observation_at(root.path()).ok(), before);
        assert_eq!(
            std::fs::read(root.path().join("initial-setup.json")).ok(),
            progress_bytes
        );
        if replacement <= 1 {
            assert!(root.path().join("initial-setup.json").is_dir());
        }
        assert_eq!(
            std::fs::read(root.path().join("first-run.json")).unwrap(),
            decision_bytes
        );
    }
}

#[test]
fn failed_restart_publication_capture_cannot_bind_a_later_readable_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    let mut attempt = InitialSetupRestart::default();
    let final_progress = std::cell::Cell::new(None);
    assert!(
        restart_attempt(
            &mut attempt,
            root.path(),
            &plan,
            |root, next| {
                final_progress.set(Some(next));
                let path = root.join("initial-setup.json");
                std::fs::remove_file(&path).unwrap();
                std::fs::create_dir(path).unwrap();
                Err("injected inaccessible publication".into())
            },
            observe_restart,
        )
        .result
        .is_err()
    );
    assert!(!attempt.pending());
    std::fs::remove_dir(root.path().join("initial-setup.json")).unwrap();
    save_progress(root.path(), final_progress.get().unwrap()).unwrap();
    let before = observation_at(root.path()).unwrap();
    let retired = restart_attempt(
        &mut attempt,
        root.path(),
        &plan,
        |_, _| panic!("an unbound failure cannot adopt a later publication"),
        observe_restart,
    );
    assert_eq!(retired.result.unwrap(), None);
    assert_eq!(observation_at(root.path()).unwrap(), before);
}

#[test]
fn admitted_run_observation_preserves_the_source_revision_and_stage_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
    plan.vault_url = "https://vault.example/v1/team".into();
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.done = Stage::Workstation.bit();
    saved.running = Some(Stage::Secrets);
    save_progress(root.path(), saved).unwrap();
    let before = observation_at(root.path()).unwrap();

    assert_eq!(
        admission_at(root.path()).unwrap(),
        Some(InitialSetupAdmission {
            plan: plan.clone(),
            revision,
            completed: false,
        })
    );
    assert_eq!(observation_at(root.path()).unwrap(), before);
    assert_eq!(progress(root.path()).unwrap(), Some(saved));
    assert!(!root.path().join("profile.lock").exists());

    saved.done |= Stage::Secrets.bit() | Stage::Repositories.bit();
    saved.running = None;
    saved.completed = true;
    save_progress(root.path(), saved).unwrap();
    assert_eq!(
        admission_at(root.path()).unwrap(),
        Some(InitialSetupAdmission {
            plan,
            revision,
            completed: true,
        })
    );
}

#[test]
fn staged_mismatched_legacy_and_missing_choices_are_not_admitted_runs() {
    let absent = aterm_tempfile::tempdir().unwrap();
    let missing_root = absent.path().join("not-created");
    assert_eq!(admission_at(&missing_root).unwrap(), None);
    assert!(!missing_root.exists());

    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let saved = progress(root.path()).unwrap().unwrap();
    save_progress(
        root.path(),
        Progress {
            admitted: false,
            ..saved
        },
    )
    .unwrap();
    assert_eq!(admission_at(root.path()).unwrap(), None);

    save_progress(root.path(), saved).unwrap();
    let mut replacement = plan.clone();
    replacement.vault_url = "https://vault.example/v1/other".into();
    save_plan(root.path(), &replacement).unwrap();
    assert_eq!(admission_at(root.path()).unwrap(), None);

    save_automatic_plan(root.path(), plan.selection).unwrap();
    assert_eq!(admission_at(root.path()).unwrap(), None);
    save_plan(root.path(), &plan).unwrap();
    for decision in [Decision::Missing, Decision::Applied(false)] {
        let path = root.path().join("first-run.json");
        if decision == Decision::Missing {
            std::fs::remove_file(&path).unwrap();
        } else {
            write_json(
                &path,
                &json!({"profile":PROFILE,"state":"applied","accounts_verified":false}),
            )
            .unwrap();
        }
        assert_eq!(admission_at(root.path()).unwrap(), None);
    }
    save_workstation_authorization(root.path(), &plan).unwrap();
    for record in ["automatic-plan.json", "initial-setup.json"] {
        let path = root.path().join(record);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);
        std::fs::write(path, bytes).unwrap();
    }
    assert!(admission_at(root.path()).unwrap().is_some());
}

#[test]
fn an_empty_admission_published_before_an_error_can_be_confirmed_without_rewriting_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let empty = selected(&[]);
    assert!(
        authorize_with(
            root.path(),
            &empty,
            AdmittedVault::None,
            |root, progress| {
                save_progress(root, progress)?;
                Err("injected failure after publishing the final admission".into())
            }
        )
        .is_err()
    );
    let before = observation_at(root.path()).unwrap();
    let admitted = admission_at(root.path()).unwrap().unwrap();
    assert_eq!(admitted.plan, empty);
    assert!(admitted.completed);
    assert_eq!(admitted.revision, revision_at(root.path()).unwrap());
    assert_eq!(observation_at(root.path()).unwrap(), before);
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());

    for state in ["cancelled", "restored"] {
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state}),
        )
        .unwrap();
        assert_eq!(admission_at(root.path()).unwrap(), None);
    }
    let next_plan = selected(&[Setting::Secrets]);
    let next_revision = restart_at(root.path(), &next_plan).unwrap();
    assert_eq!(
        admission_at(root.path()).unwrap(),
        Some(InitialSetupAdmission {
            plan: next_plan,
            revision: next_revision,
            completed: false,
        })
    );
    assert_ne!(next_revision, admitted.revision);

    let unpublished = aterm_tempfile::tempdir().unwrap();
    assert!(
        authorize_with(unpublished.path(), &empty, AdmittedVault::None, |_, _| {
            Err("injected failure before publishing the final admission".into())
        })
        .is_err()
    );
    assert_eq!(admission_at(unpublished.path()).unwrap(), None);
    assert!(!finished_at(unpublished.path()).unwrap());
}

#[test]
fn unreadable_admission_records_return_errors_instead_of_absence() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    for record in [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ] {
        let path = root.path().join(record);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"invalid setup record").unwrap();
        assert!(admission_at(root.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
        std::fs::write(path, bytes).unwrap();
    }
    assert!(admission_at(root.path()).unwrap().is_some());
}

#[test]
fn a_durable_stop_ends_admission_even_when_plan_or_progress_is_unreadable() {
    for record in ["automatic-plan.json", "initial-setup.json"] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
        let path = root.path().join(record);
        std::fs::write(&path, b"invalid setup record").unwrap();
        assert!(admission_at(root.path()).is_err());
        for state in ["cancelled", "restored"] {
            write_json(
                &root.path().join("first-run.json"),
                &json!({"profile":PROFILE,"state":state}),
            )
            .unwrap();
            assert!(finished_at(root.path()).unwrap());
            assert_eq!(admission_at(root.path()).unwrap(), None);
            assert_eq!(std::fs::read(&path).unwrap(), b"invalid setup record");
        }
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"pending"}),
        )
        .unwrap();
        assert!(admission_at(root.path()).is_err());
    }
}

#[test]
fn observations_are_read_only_and_unchanged_records_remain_equal() {
    let root = aterm_tempfile::tempdir().unwrap();
    let absent = observation_at(root.path()).unwrap();
    assert_eq!(observation_at(root.path()).unwrap(), absent);
    assert!(std::fs::read_dir(root.path()).unwrap().next().is_none());
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let current = observation_at(root.path()).unwrap();
    assert_ne!(current, absent);
    assert_eq!(current.clone(), observation_at(root.path()).unwrap());
    assert_eq!(
        snapshot_at(root.path(), Plan::default())
            .unwrap()
            .observation,
        current
    );
}

#[test]
fn observed_read_only_pass_expires_across_a_pending_stage_aba() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let original = Records::read(root.path()).unwrap();
    let before = observation_at(root.path()).unwrap();
    let stable = observe_read_only(Ok(root.path().into()), |_| Ok(7));
    assert_eq!(stable.result.unwrap(), 7);
    assert_eq!(stable.observation, Some(before.clone()));

    let changed = observe_read_only(Ok(root.path().into()), |root| {
        let _writer = HeldLock::take(root)?;
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root, &mut None)?.unwrap();
        assert!(report.needs_attention());
        assert_eq!(host.calls, [Stage::Secrets]);
        Ok(7)
    });
    assert_eq!(changed.result.unwrap(), 7);
    assert!(changed.observation.is_none());
    assert_eq!(Records::read(root.path()).unwrap(), original);
    assert_ne!(observation_at(root.path()).unwrap(), before);
}

#[test]
fn observed_check_cannot_bind_during_a_profile_pass_with_unchanged_records() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let context = observation_at(root.path()).unwrap();
    let writer = HeldLock::take(root.path()).unwrap();
    let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
    assert_eq!(observed.result.unwrap(), 7);
    assert!(observed.observation.is_none());
    assert_eq!(observation_at(root.path()).unwrap(), context);
    drop(writer);
    let observed = observe_read_only(Ok(root.path().into()), |_| Ok(7));
    assert_eq!(observed.result.unwrap(), 7);
    assert_eq!(observed.observation, Some(context));
}

#[test]
fn observed_restore_error_is_bound_under_lock_and_expires_after_a_new_run() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    // A run an older aterm cancelled.
    let before = observation_at(root.path()).unwrap();
    write_legacy_cancel_at(root.path());
    let cancelled = observation_at(root.path()).unwrap();
    assert_ne!(cancelled, before);
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    std::fs::write(
        root.path().join("original-preferences.json"),
        b"invalid backup",
    )
    .unwrap();
    let restored = observe_mutation_at(
        root.path(),
        Some(cancelled.clone()),
        Err("busy".into()),
        |root| {
            assert!(HeldLock::try_take(root)?.is_none());
            super::super::tests::restore_without_effects(root)
        },
    );
    assert!(restored.result.is_err());
    let restored = restored.observation.unwrap();
    assert_ne!(restored, cancelled);
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.finished);
    assert_eq!(snapshot.observation, restored);

    restart_at(root.path(), &plan).unwrap();
    assert_ne!(observation_at(root.path()).unwrap(), restored);
}

#[test]
fn observed_busy_failure_keeps_only_its_pre_attempt_context() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let before = observation_at(root.path()).unwrap();
    let writer = HeldLock::take(root.path()).unwrap();
    let refused: InitialSetupObserved<()> = observe_mutation_at(
        root.path(),
        Some(before.clone()),
        Err("busy".into()),
        |_| panic!("a busy profile must not run the operation"),
    );
    assert!(refused.result.is_err());
    assert_eq!(refused.observation, Some(before));
    drop(writer);
    restart_at(root.path(), &plan).unwrap();
    assert_ne!(
        refused.observation,
        Some(observation_at(root.path()).unwrap())
    );
}

#[test]
fn observation_read_failures_never_relabel_an_operation_result() {
    for directory in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let path = root.path().join("first-run.json");
        if directory {
            std::fs::create_dir(&path).unwrap();
        } else {
            std::fs::write(&path, b"invalid decision").unwrap();
        }
        assert!(matches!(
            observation_at(root.path()),
            Err(InitialSetupSnapshotError::Read(_))
        ));
        let observed: InitialSetupObserved<()> =
            observe_read_only(Ok(root.path().into()), |_| Err("original failure".into()));
        assert_eq!(observed.result.unwrap_err(), "original failure");
        assert!(observed.observation.is_none());
    }
}

#[test]
fn a_saved_running_stage_needs_a_live_profile_pass() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.running = Some(Stage::Secrets);
    save_progress(root.path(), saved).unwrap();
    {
        let _active = HeldLock::take(root.path()).unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(snapshot.rows[0].status, "running");
    }
    for missing_lock in [false, true] {
        if missing_lock {
            std::fs::remove_file(root.path().join("profile.lock")).unwrap();
        }
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(
            snapshot.rows[0].status, "pending",
            "an ended worker cannot remain live after restart"
        );
        assert!(!snapshot.rows[0].detail.is_empty());
        assert!(!snapshot.finished);
        assert_eq!(snapshot.revision, saved.revision);
        assert_eq!(progress(root.path()).unwrap().unwrap(), saved);
        if missing_lock {
            assert!(!root.path().join("profile.lock").exists());
        }
    }
}

#[test]
fn a_snapshot_samples_profile_activity_after_its_record_reads() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let mut saved = progress(root.path()).unwrap().unwrap();
    saved.running = Some(Stage::Secrets);
    save_progress(root.path(), saved).unwrap();
    let reads = std::cell::Cell::new(0);
    let writer = std::cell::RefCell::new(None);
    let snapshot = snapshot_with(
        Plan::default(),
        || {
            assert_eq!(reads.get(), 2);
            HeldLock::profile_active(root.path())
        },
        || {
            let record = Records::read(root.path())?;
            reads.set(reads.get() + 1);
            if reads.get() == 2 {
                *writer.borrow_mut() = Some(HeldLock::take(root.path())?);
            }
            Ok(record)
        },
    )
    .unwrap();
    assert_eq!(snapshot.rows[0].status, "running");
    drop(writer.into_inner());
    assert!(!HeldLock::profile_active(root.path()).unwrap());
    let _next_writer = HeldLock::take(root.path()).unwrap();
}

#[test]
fn explicit_mac_apply_retires_workers_when_it_revives_a_stopped_modern_run() {
    for stop in ["cancelled", "restored"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets, Setting::Permissions]);
        authorize_at(root.path(), &plan).unwrap();
        let original = revision_at(root.path()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":stop}),
        )
        .unwrap();
        save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
        let current = revision_at(root.path()).unwrap();
        assert!(
            current > original,
            "{stop}: explicit apply revived an older worker's password"
        );
        let _profile = HeldLock::take(root.path()).unwrap();
        let password = SecretBytes::from(b"test-only-old-run-password".as_slice());
        let mut host = Fake::default();
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                original,
                &mut Some(password.clone()),
                SetupHooks::NONE
            )
            .unwrap()
            .is_none()
        );
        assert!(host.calls.is_empty());
        assert!(!host.password_seen);
        execute_initial_for_revision(&mut host, root.path(), current, &mut None, SetupHooks::NONE)
            .unwrap();
        // A Restore withdraws no permission, so the explicit plan keeps the ones the plan
        // held; a Cancel does.
        let mut calls = vec![Stage::Workstation, Stage::Secrets, Stage::Accounts];
        if stop == "restored" {
            calls.insert(1, Stage::Permissions);
        }
        assert_eq!(host.calls, calls, "{stop}");
    }
}

#[test]
fn explicit_mac_apply_retires_legacy_run_zero_after_an_earlier_stop() {
    for stop in ["cancelled", "restored"] {
        let root = aterm_tempfile::tempdir().unwrap();
        assert_eq!(revision_at(root.path()).unwrap(), 0);
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":stop}),
        )
        .unwrap();
        save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
        assert_ne!(
            revision_at(root.path()).unwrap(),
            0,
            "{stop}: legacy run zero survived"
        );
    }
}

#[test]
fn explicit_workstation_completion_does_not_complete_independent_stages() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions, Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let explicit =
        save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    complete_explicit_workstation_plan_for_test(root.path(), explicit).unwrap();
    let current = progress(root.path()).unwrap().unwrap();
    assert_eq!(current.revision, revision);
    // The explicit pass signs out with its preferences: both stages finish.
    assert_eq!(
        current.done,
        Stage::Workstation.bit() | Stage::Accounts.bit()
    );
    assert!(!current.completed);
    assert!(!finished_at(root.path()).unwrap());
}

/// A whole `aterm init` over a run the window finished, whose plan it leaves as it is,
/// reopens nothing as it starts: an init that ends before it verifies (a Ctrl-C, a closed
/// terminal, a preference that did not take, a sign-out hold another setup kept) leaves the
/// run finished, its sign-outs done, with no worker needed to finish it again. NEGATIVE
/// CONTROL: a plan the pass widens is a run not finished, whose new groups nothing set yet.
#[test]
fn a_whole_init_over_a_finished_run_reopens_nothing_it_does_not_widen() {
    for widens in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::default();
        if widens {
            plan.selection.set(Setting::Dock, false);
        }
        authorize_at(root.path(), &plan).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap(), "{widens}");
        let before = progress(root.path()).unwrap().unwrap();
        save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
        // The init ends here, before it verifies: nothing completes the run again.
        let after = progress(root.path()).unwrap().unwrap();
        assert_eq!(finished_at(root.path()).unwrap(), !widens, "{widens}");
        assert_eq!(
            after.done & Stage::Accounts.bit() != 0,
            !widens,
            "{widens}: {after:?}"
        );
        if !widens {
            assert_eq!(after, before);
        }
    }
}

/// An explicit pass (`aterm doctor --apply`, a whole `aterm init`) that finishes the last
/// stages of the window's run publishes it completed only through the read-back the window's
/// own completion takes: a Secrets stage an earlier pass finished, whose vault no longer
/// reads ready, reopens instead, and the window's next pass repairs it without signing out
/// or resetting the Dock again. NEGATIVE CONTROL: a vault that still reads ready completes.
#[test]
fn an_explicit_completion_rereads_network_stages_an_earlier_pass_finished() {
    for regressed in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            waiting: Some(Stage::Accounts),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        let done = progress(root.path()).unwrap().unwrap().done;
        assert_ne!(done & Stage::Secrets.bit(), 0);
        assert_ne!(done & Stage::Repositories.bit(), 0);
        assert_eq!(done & Stage::Accounts.bit(), 0);
        assert!(!finished_at(root.path()).unwrap());
        // The vault regresses while the run waits on its sign-outs.
        host.vault_blocked = regressed;
        let checks = host.vault_checks;
        let explicit =
            save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
        let reopened =
            complete_explicit_workstation_plan_with(&mut host, root.path(), explicit).unwrap();
        let after = progress(root.path()).unwrap().unwrap();
        assert_ne!(
            after.done & Stage::Accounts.bit(),
            0,
            "{regressed}: the explicit sign-outs stay recorded"
        );
        assert!(
            host.vault_checks > checks,
            "{regressed}: completion reads the vault back"
        );
        assert_eq!(finished_at(root.path()).unwrap(), !regressed, "{regressed}");
        assert_eq!(
            reopened.iter().any(|row| row.name == Stage::Secrets.name()),
            regressed,
            "{regressed}: {reopened:?}"
        );
        if regressed {
            host.vault_blocked = false;
            host.waiting = None;
            host.calls.clear();
            execute_initial(&mut host, root.path(), &mut None).unwrap();
            assert!(finished_at(root.path()).unwrap());
            assert!(
                !host.calls.contains(&Stage::Accounts) && !host.calls.contains(&Stage::Workstation),
                "no Dock reset or sign-out repeats: {:?}",
                host.calls
            );
        }
    }
}

/// Automatic setup starts selected permission steps without a second opt-in.
#[test]
fn automatic_setup_starts_selected_permission_steps() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions, Setting::Secrets]);
    authorize_reading(root.path(), &plan, || Ok(None)).unwrap();
    assert_eq!(
        permission_steps_at(root.path()),
        Ok(PermissionSteps::Allowed),
        "the admitted plan starts its steps automatically"
    );
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.plan.selection.contains(Setting::Permissions));
    assert!(snapshot.permissions_allowed);
    note_person_asked_at(root.path()).unwrap();
    assert_eq!(
        permission_steps_at(root.path()),
        Ok(PermissionSteps::Allowed),
        "CONTROL: the press admits them, on this Mac's records"
    );
}

/// The records as an older aterm left them: its progress record carries no
/// [`PROGRESS_BY_PRESS`] mark and there is no [`PERSON_ACCESS`] record.
fn as_an_older_aterm_left_it(root: &Path) {
    let path = root.join("initial-setup.json");
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove(PROGRESS_BY_PRESS);
    write_json(&path, &record).unwrap();
    let _ = std::fs::remove_file(root.join(PERSON_ACCESS));
}

/// An update stops no step a person approved: an older aterm's admitted Permissions group
/// keeps the steps allowed, and this build's next choice carries that over as a person's
/// press. Negative control: an older run whose plan left Permissions out carries nothing,
/// and this build's choice that adds the group asks for no grant.
#[test]
fn an_older_aterms_approval_outlives_its_records() {
    let root = aterm_tempfile::tempdir().unwrap();
    let older = selected(&[Setting::Permissions, Setting::Secrets]);
    authorize_reading(root.path(), &older, || Ok(None)).unwrap();
    as_an_older_aterm_left_it(root.path());
    assert_eq!(person_access_at(root.path()), PersonAccess::Older);
    assert!(permissions_allowed(root.path()), "the older rule stands");
    system_state_choice(
        root.path(),
        &selected(&[Setting::Permissions, Setting::Spotlight]),
    );
    assert_eq!(person_access_at(root.path()), PersonAccess::Asked);
    assert!(permissions_allowed(root.path()), "carried over");

    let root = aterm_tempfile::tempdir().unwrap();
    authorize_reading(root.path(), &selected(&[Setting::Secrets]), || Ok(None)).unwrap();
    as_an_older_aterm_left_it(root.path());
    assert!(!permissions_allowed(root.path()));
    system_state_choice(
        root.path(),
        &selected(&[Setting::Permissions, Setting::Secrets]),
    );
    assert_eq!(person_access_at(root.path()), PersonAccess::NotAsked);
    assert!(
        permissions_allowed(root.path()),
        "selected permissions start automatically"
    );
}

/// A historical press belongs to its hardware UUID. Automatic acquisition follows this
/// Mac's admitted plan regardless of that historical press.
#[test]
fn automatic_permission_steps_do_not_depend_on_another_macs_press() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_reading(
        root.path(),
        &selected(&[Setting::Permissions, Setting::Secrets]),
        || Ok(None),
    )
    .unwrap();
    write_json(
        &root.path().join(PERSON_ACCESS),
        &json!({"profile":PROFILE,"asked":true,"host":"00000000-0000-0000-0000-000000000000"}),
    )
    .unwrap();
    assert_eq!(person_access_at(root.path()), PersonAccess::NotAsked);
    assert!(permissions_allowed(root.path()));
    note_person_asked_at(root.path()).unwrap();
    assert_eq!(person_access_at(root.path()), PersonAccess::Asked);
    assert!(
        permissions_allowed(root.path()),
        "CONTROL: this Mac's press"
    );
}

/// NEGATIVE CONTROL: an unreadable or blank hardware UUID never matches a `null` or missing
/// `host` field, and a symlink, FIFO or non-private `person-access.json` (or special
/// `initial-setup.json`) fails closed without blocking.
#[test]
fn a_missing_host_uuid_or_unsafe_person_access_file_never_admits_a_prompt() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = aterm_tempfile::tempdir().unwrap();
    assert!(note_person_access_at_with_host(root.path(), true, None).is_err());
    assert!(note_person_access_at_with_host(root.path(), true, Some("   ")).is_err());

    for payload in [
        json!({"profile":PROFILE,"asked":true,"host":null}),
        json!({"profile":PROFILE,"asked":true}),
        json!({"profile":PROFILE,"asked":true,"host":""}),
    ] {
        write_json(&root.path().join(PERSON_ACCESS), &payload).unwrap();
        assert_eq!(
            person_access_at_with_host(root.path(), None),
            PersonAccess::NotAsked
        );
        assert_eq!(
            person_access_at_with_host(root.path(), Some("")),
            PersonAccess::NotAsked
        );
    }

    let host = "11111111-2222-3333-4444-555555555555";
    note_person_access_at_with_host(root.path(), true, Some(host)).unwrap();
    assert_eq!(
        person_access_at_with_host(root.path(), Some(host)),
        PersonAccess::Asked,
        "CONTROL: 0600 regular file with matching host UUID admits"
    );

    let record = root.path().join(PERSON_ACCESS);
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        person_access_at_with_host(root.path(), Some(host)),
        PersonAccess::NotAsked,
        "world-readable person-access.json must fail closed"
    );
    std::fs::remove_file(&record).unwrap();

    let target = root.path().join("target-access.json");
    write_json(
        &target,
        &json!({"profile":PROFILE,"asked":true,"host":host}),
    )
    .unwrap();
    std::os::unix::fs::symlink(&target, &record).unwrap();
    assert_eq!(
        person_access_at_with_host(root.path(), Some(host)),
        PersonAccess::NotAsked,
        "symlinked person-access.json must fail closed"
    );
    std::fs::remove_file(&record).unwrap();

    assert!(
        aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&record)
            .status()
            .unwrap()
            .success()
    );
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        person_access_at_with_host(root.path(), Some(host)),
        PersonAccess::NotAsked,
        "FIFO person-access.json must return immediately without blocking"
    );
    std::fs::remove_file(&record).unwrap();

    let progress_path = root.path().join("initial-setup.json");
    assert!(
        aterm_uds::spawnfd::command("/usr/bin/mkfifo")
            .arg(&progress_path)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        person_access_at_with_host(root.path(), Some(host)),
        PersonAccess::NotAsked,
        "FIFO initial-setup.json must fail closed without blocking"
    );
}

/// NEGATIVE CONTROL: `aterm doctor --apply` over an older unverified (`Applied(false)`) setup
/// never promotes its Permissions group to allowed, even if cut short right after rewriting
/// `first-run.json` to pending.
#[test]
fn an_explicit_apply_over_an_unverified_older_setup_never_promotes_permissions() {
    for cut_at_plan in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let older = selected(&[Setting::Permissions, Setting::Dock]);
        authorize_reading(root.path(), &older, || Ok(None)).unwrap();
        as_an_older_aterm_left_it(root.path());
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":"applied","accounts_verified":false}),
        )
        .unwrap();
        assert!(!permissions_allowed(root.path()));

        let outcome = save_explicit_workstation_plan_with(
            root.path(),
            Selection::all().workstation(),
            |write| {
                if cut_at_plan && write == ExplicitWrite::Plan {
                    Err("cut after decision".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(outcome.is_err(), cut_at_plan);
        assert_eq!(person_access_at(root.path()), PersonAccess::NotAsked);
        assert!(permissions_allowed(root.path()), "{cut_at_plan}");
    }

    let verified = aterm_tempfile::tempdir().unwrap();
    let older = selected(&[Setting::Permissions, Setting::Dock]);
    authorize_reading(verified.path(), &older, || Ok(None)).unwrap();
    as_an_older_aterm_left_it(verified.path());
    write_json(
        &verified.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
    )
    .unwrap();
    assert!(permissions_allowed(verified.path()));
    save_explicit_workstation_plan(verified.path(), Selection::all().workstation()).unwrap();
    assert_eq!(person_access_at(verified.path()), PersonAccess::Asked);
    assert!(
        permissions_allowed(verified.path()),
        "CONTROL: verified older approval survives explicit apply"
    );
}

#[test]
fn permission_policy_requires_authorization_without_creating_missing_state() {
    let root = aterm_tempfile::tempdir().unwrap();
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!snapshot.permissions_allowed);
    assert!(!snapshot.finished);
    assert_eq!(snapshot.revision, 0);
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);

    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"applied","accounts_verified":true}),
    )
    .unwrap();
    save_plan(root.path(), &Plan::default()).unwrap();
    assert!(
        !snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );
}

#[test]
fn permission_policy_survives_completed_selected_setup_without_rerunning_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Permissions]);
    authorize_at(root.path(), &plan).unwrap();
    let pending = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(pending.permissions_allowed);
    assert!(!pending.finished);
    assert_ne!(pending.revision, 0);

    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    let completed = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(completed.permissions_allowed);
    assert!(completed.finished);
    assert_eq!(completed.revision, pending.revision);
    assert_eq!(completed.plan, plan);
    assert!(completed.rows.iter().all(|row| row.status == "applied"));
}

#[test]
fn permission_policy_honors_stops_and_the_latest_explicit_selection() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions, Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let revision = snapshot_at(root.path(), Plan::default()).unwrap().revision;
    // A Cancel holds aterm's permissions; a Restore puts back Mac preferences and
    // withdraws none of them.
    for (state, allowed) in [("cancelled", false), ("restored", true)] {
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state}),
        )
        .unwrap();
        let stopped = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(stopped.permissions_allowed, allowed, "{state}");
        assert!(stopped.finished);
        assert_eq!(stopped.revision, revision);
    }

    restart_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let unchecked = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!unchecked.permissions_allowed);
    assert!(unchecked.revision > revision);
    restart_at(root.path(), &plan).unwrap();
    let selected = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(selected.permissions_allowed);
    assert!(selected.revision > unchecked.revision);
}

/// Whether System State's access steps run, wait or are held by a person's choice, as the
/// page's snapshot and the Commons runtime's light read both say: a Mac with no records waits
/// for its first admission; an admitted run runs them when its plan selects Permissions and
/// holds them when it leaves Permissions out; a Cancel or a Stop holds them (a Restore's record
/// a Cancel holds among them); a Restore keeps them running.
#[test]
fn permission_steps_split_what_waits_from_what_a_person_holds() {
    let with = selected(&[Setting::Permissions, Setting::Secrets]);
    let without = selected(&[Setting::Secrets]);
    // Each case's records, written into a fresh state root.
    type SetUp<'a> = &'a dyn Fn(&Path);
    let cases: [(&str, SetUp<'_>, PermissionSteps); 8] = [
        ("no records", &|_| {}, PermissionSteps::NotYet),
        (
            "a saved plan no run admitted",
            &|root| save_plan(root, &with).unwrap(),
            PermissionSteps::NotYet,
        ),
        (
            "admitted with Permissions",
            &|root| {
                authorize_at(root, &with).unwrap();
            },
            PermissionSteps::Allowed,
        ),
        (
            "admitted without Permissions",
            &|root| {
                authorize_at(root, &without).unwrap();
            },
            PermissionSteps::Off,
        ),
        (
            "an older review's Cancel",
            &|root| save_older_cancel(root, &Plan::default()).unwrap(),
            PermissionSteps::Off,
        ),
        (
            "a Restore a Cancel holds",
            &|root| {
                save_older_cancel(root, &Plan::default()).unwrap();
                save_restore(root).unwrap();
                leave_restored_as_is(root).unwrap();
            },
            PermissionSteps::Off,
        ),
        (
            "a Stop",
            &|root| {
                let revision = authorize_at(root, &with).unwrap().unwrap();
                request_stop_at(root, revision).unwrap();
            },
            PermissionSteps::Off,
        ),
        (
            "a Restore",
            &|root| {
                authorize_at(root, &with).unwrap();
                write_json(
                    &root.join("first-run.json"),
                    &json!({"profile":PROFILE,"state":"restored"}),
                )
                .unwrap();
            },
            PermissionSteps::Allowed,
        ),
    ];
    for (case, set_up, expected) in cases {
        let root = aterm_tempfile::tempdir().unwrap();
        set_up(root.path());
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(snapshot.access_steps, expected, "{case}");
        assert_eq!(
            permission_steps_at(root.path()),
            Ok(expected),
            "{case}: the light read agrees with the page"
        );
        assert_eq!(
            snapshot.permissions_allowed,
            expected == PermissionSteps::Allowed,
            "{case}"
        );
    }
}

/// Check again looks at the LAN only in a person's scope and while the root LAN broker
/// answers, as the Secrets stage waits for it. NEGATIVE CONTROL: with both, it looks.
#[test]
fn check_again_looks_only_while_the_broker_answers() {
    let looked = |scoped: bool, broker: Option<&str>| {
        let mut looked = false;
        super::look_again_when(scoped, || broker.map(str::to_owned), || looked = true);
        looked
    };
    assert!(looked(true, None), "the control: scoped, broker answers");
    assert!(!looked(true, Some("no root LAN broker answers")));
    assert!(!looked(false, None));
}

/// The launch's startup as `prepare_setup` reads the records: nothing when setup is finished,
/// the run already admitted, else the offer answered as the page answers it, admitted.
fn launch_startup(root: &Path) -> Option<u64> {
    if finished_at(root).unwrap() {
        return None;
    }
    if let Some(admission) = admission_at(root).unwrap() {
        return (!admission.completed).then_some(admission.revision);
    }
    let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
    let offered = startup.offer_reading(root, || Ok(None)).unwrap();
    startup.persist_at(root, &offered).result.unwrap()
}

pub(super) fn resume_native_accounts_after_init_for_test(
    root: &Path,
    account: &std::sync::Arc<crate::macos_doctor::accounts_visibility_tests::Account>,
) -> Result<Option<Report>, String> {
    use crate::macos_doctor::accounts_visibility_tests::system;

    let Some(revision) = launch_startup(root) else {
        return Ok(None);
    };
    let mut host = Fake {
        native_accounts: Some((
            system(None, account),
            NativeHost::over(
                Box::new(system(None, account)),
                Box::new(aterm_machines::init::prepare_workstation_selected),
                complete_explicit_workstation_plan_for_test,
            ),
        )),
        ..Fake::default()
    };
    execute_initial_for_revision(&mut host, root, revision, &mut None, AT_THE_PAGE)
        .map(|pass| pass.map(|(report, _)| report))
}

/// A person's Cancel of the first-launch review holds until they ask again (System State
/// design §3.3): every group reads Leave as is, aterm's own permissions with them, no launch
/// offers the review again, and a Restore after it never brings the review back. Set up now
/// applies the plan the review showed; a group choice sets up that group alone.
#[test]
fn an_older_cancel_holds_until_a_person_asks_again() {
    use aterm_types::macos_setup::Group;
    let reviewed = Plan::default();
    let cancelled = || {
        let root = aterm_tempfile::tempdir().unwrap();
        save_older_cancel(root.path(), &reviewed).unwrap();
        root
    };
    let restart = |root: &Path, mut restart: InitialSetupRestart, plan: &Plan| {
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            restart.persist_with(
                root,
                plan,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("admitted")
    };

    let root = cancelled();
    assert!(holds_at(root.path()).stopped_revision.is_none());
    let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
    assert!(page.cancelled && page.finished && !page.permissions_allowed);
    assert!(page.plan.selection.is_empty(), "every group at Leave as is");
    assert_eq!(
        launch_startup(root.path()),
        None,
        "no launch offers it again"
    );
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    assert_eq!(
        launch_startup(root.path()),
        None,
        "a Restore after a Cancel never brings the review back"
    );
    let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
    assert!(!page.permissions_allowed && page.plan.selection.is_empty());

    // Set up now, as System State sends it: the plan the review showed.
    let root = cancelled();
    restart(
        root.path(),
        InitialSetupRestart::default().changing(Some(Selection::none())),
        &Plan::from(Selection::none()),
    );
    assert_eq!(saved_plan(root.path()).unwrap(), Some(reviewed.clone()));
    let page = snapshot_at(root.path(), reviewed.clone()).unwrap();
    // Set up now also starts the selected permission steps.
    assert!(!page.cancelled && page.permissions_allowed);
    note_person_asked_at(root.path()).unwrap();
    assert!(
        snapshot_at(root.path(), reviewed.clone())
            .unwrap()
            .permissions_allowed
    );

    // A group press, from a page that still sends every group at Set up: that group alone.
    let root = cancelled();
    let desktop = group_selection(Group::Desktop);
    restart(
        root.path(),
        InitialSetupRestart::chosen().changing(Some(desktop)),
        &reviewed,
    );
    assert_eq!(
        saved_plan(root.path()).unwrap().map(|plan| plan.selection),
        Some(desktop)
    );
    assert!(
        !snapshot_at(root.path(), reviewed.clone())
            .unwrap()
            .cancelled
    );
}

/// The Cancel an older aterm's review recorded over what a Restore brought forward reads and
/// behaves as any other Cancel (design §3.3): every group at Leave as is, aterm's own
/// permissions held, no launch offering it again, and Set up now applying the plan the review
/// showed. The Restore's record stays, held: nothing automatic sets again the groups it put
/// back, and Set up now, which shows none of them, leaves them restored. An aterm older still
/// reads past the plan the record names, and still holds.
#[test]
fn an_older_cancel_of_a_restore_brought_forward_holds_as_any_cancel() {
    let cancelled = || {
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), Some(Selection::all().workstation()));
        bring_forward_restored_at(root.path())
            .unwrap()
            .expect("brought forward");
        let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let reviewed = startup.offer_reading(root.path(), || Ok(None)).unwrap();
        assert!(reviewed.selection.contains(Setting::Permissions));
        assert!(preferences(reviewed.selection).is_empty());
        save_older_cancel(root.path(), &reviewed).unwrap();
        (root, reviewed)
    };
    let (root, reviewed) = cancelled();
    let restored = Records::read(root.path()).unwrap().restored.unwrap();
    assert!(restored.held, "the Restore's record stays, held");
    assert_eq!(restored.left, preferences(Selection::all()));
    let page = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(page.cancelled, "an older aterm's Cancel");
    assert!(page.finished && !page.permissions_allowed && !page.provisional);
    assert!(page.plan.selection.is_empty(), "every group at Leave as is");
    assert_eq!(launch_startup(root.path()), None, "nothing offers it again");
    assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    let record: Value =
        aterm_json::from_slice(&std::fs::read(root.path().join("first-run.json")).unwrap())
            .unwrap();
    assert_eq!(
        (&record["state"], &record["held"], &record["format"]),
        (&json!("restored"), &json!(true), &json!(FORMAT)),
        "a held Restore, as an aterm older still reads it"
    );

    // Set up now, as System State sends it: the plan the review showed.
    observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        InitialSetupRestart::default()
            .changing(Some(Selection::none()))
            .persist_with(
                root,
                &Plan::from(Selection::none()),
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
    })
    .result
    .unwrap()
    .expect("admitted");
    assert_eq!(saved_plan(root.path()).unwrap(), Some(reviewed.clone()));
    let page = snapshot_at(root.path(), Plan::default()).unwrap();
    // Set up now starts the selected permission steps.
    assert!(!page.cancelled && page.permissions_allowed);
    let records = Records::read(root.path()).unwrap();
    assert_eq!(
        records.restored.map(|restored| restored.left),
        Some(preferences(Selection::all())),
        "the groups the Restore put back stay put back"
    );

    // An aterm older still reads past `reviewed`: the held Restore it knew, offering nothing.
    let (root, _) = cancelled();
    let path = root.path().join("first-run.json");
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("reviewed");
    write_json(&path, &record).unwrap();
    let page = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!page.cancelled && !page.permissions_allowed);
    assert!(page.plan.selection.is_empty());
    assert_eq!(launch_startup(root.path()), None);
}

/// An older aterm's verified completion keeps aterm's own permissions across the update, and
/// through this build's next admission ([`carry_older_person_access`]). A pending, unverified,
/// cancelled or restored one never had them allowed: the launch's offer adds the group to the
/// plan aterm saved, and its grants are on offer, since an automatic admission asks for none.
#[test]
fn permission_policy_carries_only_what_an_older_aterm_allowed() {
    for (state, verified, allowed) in [
        ("applied", true, true),
        ("applied", false, true),
        ("pending", false, true),
        ("cancelled", true, false),
        ("restored", true, true),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
        )
        .unwrap();
        let before = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(
            before.permissions_allowed,
            state == "applied" && verified,
            "{state}/{verified}: only a verified completion before the launch"
        );
        assert_eq!(before.revision, 0);
        bring_forward_restored_at(root.path()).unwrap();
        let admitted = launch_startup(root.path());
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(snapshot.permissions_allowed, allowed, "{state}/{verified}");
        assert_eq!(
            snapshot.revision,
            admitted.unwrap_or(0),
            "{state}/{verified}"
        );
        if admitted.is_some() {
            assert!(snapshot.plan.selection.contains(Setting::Permissions));
        }
        if state != "cancelled" {
            note_person_asked_at(root.path()).unwrap();
            assert!(
                snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .permissions_allowed,
                "CONTROL {state}/{verified}: a person's press here allows them"
            );
        }
    }
}

/// The groups an older aterm's restored setup had: none without a plan, else its plan's.
fn older_restored_setup(root: &Path, saved: Option<Selection>) {
    if let Some(saved) = saved {
        save_automatic_plan(root, saved).unwrap();
    }
    write_json(
        &root.join("first-run.json"),
        &json!({"profile":PROFILE,"state":"restored"}),
    )
    .unwrap();
}

/// A Restore an older aterm made, before any run was admitted here, left aterm's own
/// permissions with no run to hold them, so nothing asked for them again (design §3.4: a
/// Restore withdraws none). The launch brings it forward, and its startup's offer admits a
/// run that reads aterm's permissions alone, their grants on offer (no press asked for them):
/// the restored groups stay at Leave as is, Messages & FaceTime as the plan had them, and no
/// preference, sign-out or Spotlight step runs.
#[test]
fn a_setup_restored_before_any_run_offers_aterms_permissions_and_nothing_else() {
    let sign_outs = accounts(Selection::all());
    for case in [
        "no plan",
        "an older plan",
        "an older plan without sign-outs",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let kept = match case {
            "no plan" => {
                older_restored_setup(root.path(), None);
                Selection::none()
            }
            "an older plan" => {
                older_restored_setup(root.path(), Some(Selection::all().workstation()));
                sign_outs
            }
            _ => {
                older_restored_setup(root.path(), Some(preferences(Selection::all())));
                Selection::none()
            }
        };
        let before = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(before.finished, "{case}");
        assert!(!before.permissions_allowed, "{case}: the gap");
        assert_eq!(launch_startup(root.path()), None, "{case}: nothing offered");

        let staged = bring_forward_restored_at(root.path())
            .unwrap()
            .expect("brought forward");
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!page.finished, "{case}");
        assert!(
            !page.permissions_allowed,
            "{case}: until the offer admits it"
        );
        assert_eq!(page.revision, staged, "{case}");
        let mut wanted = kept;
        wanted.set(Setting::Permissions, true);
        assert_eq!(page.plan.selection, wanted, "{case}");
        assert!(!saved_plan_provisional(root.path()).unwrap(), "{case}");

        let revision = launch_startup(root.path()).expect("the offer admits the run");
        let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(admitted.permissions_allowed, "{case}: automatic steps");
        assert!(!admitted.finished, "{case}");
        assert_eq!(admitted.plan.selection, wanted, "{case}");
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Permissions], "{case}");
        let done = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(done.finished && done.permissions_allowed, "{case}");
        assert_eq!(launch_startup(root.path()), None, "{case}");
        assert_eq!(
            bring_forward_restored_at(root.path()).unwrap(),
            None,
            "{case}"
        );
    }
}

/// This aterm's Restore of an older aterm's verified setup (its Dock reset, Messages and
/// FaceTime signed out, no progress record), from System State or from a terminal. The
/// Restore keeps the one pass that older aterm made ([`keep_older_pass`]): aterm's own
/// permissions stay allowed and shown throughout, nothing waits for a launch, and a choice
/// after it (Desktop & Dock back to Set up) sets that group without signing out again. An
/// older setup that was pending or unverified kept no pass to hold them: the Restore stages
/// them for the offer at once, which sets them alone.
#[test]
fn a_restore_of_an_older_setup_keeps_its_permissions_and_its_sign_outs() {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
    save_workstation_decision(root.path(), true).unwrap();
    assert!(permissions_allowed(root.path()));
    save_restore(root.path()).unwrap();
    assert!(permissions_allowed(root.path()), "while it puts back");
    leave_restored_as_is(root.path()).unwrap();
    let page = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(page.finished && page.permissions_allowed);
    assert!(page.plan.selection.contains(Setting::Permissions));
    assert!(preferences(page.plan.selection).is_empty());
    assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    assert_eq!(
        launch_startup(root.path()),
        None,
        "nothing waits for a launch"
    );
    let mut plan = page.plan;
    for setting in Group::Desktop.settings() {
        plan.selection.set(*setting, true);
    }
    let revision = system_state_choice(root.path(), &plan);
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    assert!(
        !host.calls.contains(&Stage::Accounts),
        "no sign-out again: {:?}",
        host.calls
    );

    for (state, verified) in [("pending", false), ("applied", false)] {
        let root = aterm_tempfile::tempdir().unwrap();
        save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
        write_json(
            &root.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state,"accounts_verified":verified}),
        )
        .unwrap();
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert!(!finished_at(root.path()).unwrap(), "{state}: the offer");
        let revision = launch_startup(root.path()).expect("the offer admits it");
        assert!(permissions_allowed(root.path()), "{state}: automatic steps");
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Permissions], "{state}");
    }

    // A run number reserved here before (a progress write that failed after its
    // reservation) is never used again: the older pass takes the number after it.
    let reserved = aterm_tempfile::tempdir().unwrap();
    save_automatic_plan(reserved.path(), Selection::all().workstation()).unwrap();
    save_workstation_decision(reserved.path(), true).unwrap();
    revision::reserve(reserved.path(), 4).unwrap();
    save_restore(reserved.path()).unwrap();
    let kept = progress_file(reserved.path())
        .unwrap()
        .expect("the older pass");
    assert_eq!(kept.revision, 5);
    assert!(kept.admitted);
    leave_restored_as_is(reserved.path()).unwrap();
    let page = snapshot_at(reserved.path(), Plan::default()).unwrap();
    assert!(page.finished && page.permissions_allowed);
}

/// Bringing a restored setup forward writes the run it stages into the Restore's record, then
/// its plan, then its staged progress, then the Restore's record, pending. Cut short before
/// any of them, the records still read as the Restore's own decision: nothing runs, nothing
/// is admitted, no permission is allowed; and the next launch finishes the writes as one
/// never cut short does.
#[test]
fn bringing_a_restored_setup_forward_finishes_wherever_it_was_cut_short() {
    let reference = aterm_tempfile::tempdir().unwrap();
    older_restored_setup(reference.path(), Some(Selection::all().workstation()));
    bring_forward_restored_at(reference.path())
        .unwrap()
        .unwrap();
    let whole = Records::read(reference.path()).unwrap();
    for cut in [
        RestoreWrite::Run,
        RestoreWrite::Plan,
        RestoreWrite::Progress,
        RestoreWrite::Record,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), Some(Selection::all().workstation()));
        let _lock = HeldLock::take(root.path()).unwrap();
        let mut writes = Vec::new();
        let result = bring_forward_restored_with(root.path(), |write| {
            writes.push(write);
            if write == cut {
                Err("injected end before this write".into())
            } else {
                Ok(())
            }
        });
        drop(_lock);
        assert!(result.is_err(), "{cut:?}");
        assert_eq!(writes.last(), Some(&cut));
        assert!(finished_at(root.path()).unwrap(), "{cut:?}");
        assert_eq!(admission_at(root.path()).unwrap(), None, "{cut:?}");
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed,
            "{cut:?}"
        );
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty(), "{cut:?}");
        assert_eq!(launch_startup(root.path()), None, "{cut:?}");

        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), Some(1));
        assert_eq!(Records::read(root.path()).unwrap(), whole, "{cut:?}");
    }
    let all = [
        RestoreWrite::Run,
        RestoreWrite::Plan,
        RestoreWrite::Progress,
        RestoreWrite::Record,
    ];
    let root = aterm_tempfile::tempdir().unwrap();
    older_restored_setup(root.path(), Some(Selection::all().workstation()));
    let mut writes = Vec::new();
    bring_forward_restored_with(root.path(), |write| {
        writes.push(write);
        Ok(())
    })
    .unwrap();
    assert_eq!(writes, all, "in this order");
}

#[test]
fn bringing_forward_reports_its_checkpoint_even_when_the_pending_record_returns_an_error() {
    for published_record in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        older_restored_setup(root.path(), None);
        let lock = HeldLock::take(root.path()).unwrap();
        let mut target = None;
        let result = bring_forward_restored_observing_with(
            root.path(),
            |write| {
                if !published_record && write == RestoreWrite::Progress {
                    return Err("injected unpublished checkpoint failure".into());
                }
                if published_record && write == RestoreWrite::Record {
                    // The real final publication lands, but the caller receives
                    // its later durability error instead of Some(revision).
                    let restored = decision_record(root.path()).unwrap().1.unwrap();
                    save_restored_decision(
                        root.path(),
                        RestoredState::Pending,
                        Restored {
                            run: None,
                            ..restored
                        },
                    )?;
                    return Err("injected pending-decision flush failure".into());
                }
                Ok(())
            },
            |revision| target = Some(revision),
        );
        drop(lock);
        assert!(result.is_err());
        assert_eq!(target.is_some(), published_record);
        if let Some(target) = target {
            let before = Records::read(root.path()).unwrap();
            assert_eq!(
                bring_forward_waiting_observing(
                    root.path(),
                    || false,
                    |_| { panic!("finding an earlier run is not a new publication") }
                )
                .unwrap(),
                BroughtForward::Run(target)
            );
            assert_eq!(Records::read(root.path()).unwrap(), before);
            request_stop_at(root.path(), target).unwrap();
            let before = Records::read(root.path()).unwrap();
            assert!(finished_at(root.path()).unwrap());
            assert_eq!(launch_startup(root.path()), None);
            assert_eq!(Records::read(root.path()).unwrap(), before);
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none()
            );
            assert!(host.calls.is_empty());
        }
    }
}

#[test]
fn finishing_a_restore_reports_its_rebound_run_before_late_metadata_failure() {
    let root = restored_after(&Plan::default());
    let previous = progress(root.path()).unwrap().unwrap().revision;
    let lock = HeldLock::take(root.path()).unwrap();
    let mut target = None;
    assert!(
        bring_forward_restored_observing_with(
            root.path(),
            |write| {
                if write == RestoreWrite::Run {
                    Err("injected late Restore-run metadata failure".into())
                } else {
                    Ok(())
                }
            },
            |revision| target = Some(revision),
        )
        .is_err()
    );
    let target = target.expect("the rebound Progress was published before the failure");
    assert!(target > previous);
    assert_eq!(progress(root.path()).unwrap().unwrap().revision, target);
    assert_eq!(decision_record(root.path()).unwrap().1.unwrap().run, None);

    request_stop_at(root.path(), target).unwrap();
    assert!(Records::read(root.path()).unwrap().stopped_run());
    assert_eq!(
        bring_forward_restored_observing_with(
            root.path(),
            |_| Ok(()),
            |_| { panic!("finishing metadata without a rebound writes no checkpoint") }
        )
        .unwrap(),
        Some(target)
    );
    assert_eq!(
        decision_record(root.path()).unwrap().1.unwrap().run,
        Some(target)
    );
    drop(lock);
    assert!(Records::read(root.path()).unwrap().stopped_run());
    assert_eq!(launch_startup(root.path()), None);
}

fn progress_namespace_error(finishes_restore: bool) {
    for after_rename in [false, true] {
        let root = if finishes_restore {
            restored_after(&Plan::default())
        } else {
            let root = aterm_tempfile::tempdir().unwrap();
            older_restored_setup(root.path(), None);
            root
        };
        let before = progress_file(root.path()).unwrap();
        let lock = HeldLock::take(root.path()).unwrap();
        let mut reported = None;
        assert!(
            bring_forward_restored_observing_using(
                root.path(),
                |_| Ok(()),
                |revision| reported = Some(revision),
                |root, progress| progress_publication_error(root, progress, after_rename),
            )
            .is_err(),
            "the failed flush never becomes successful admission"
        );
        let visible = progress_file(root.path()).unwrap();
        if !after_rename {
            assert_eq!(visible, before);
            assert_eq!(reported, None);
            continue;
        }
        let visible = visible.expect("the actual Progress rename landed before the error");
        assert!(before.is_none_or(|before| before.revision < visible.revision));
        if finishes_restore {
            // This retry only finishes Restore.run metadata. It cannot
            // recover a callback the original Progress writer omitted.
            let mut retry_target = None;
            assert_eq!(
                bring_forward_restored_observing_with(
                    root.path(),
                    |_| Ok(()),
                    |revision| {
                        retry_target = Some(revision);
                    }
                )
                .unwrap(),
                Some(visible.revision)
            );
            assert_eq!(retry_target, None);
        }
        drop(lock);
        request_stop_at(root.path(), visible.revision).unwrap();
        assert!(Records::read(root.path()).unwrap().stopped_run());
        assert!(
            !snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        assert_eq!(launch_startup(root.path()), None);
        assert_eq!(
            reported,
            Some(visible.revision),
            "a visible exact checkpoint needs Stop evidence despite its original error"
        );
    }
}

#[test]
fn progress_namespace_error_retains_a_staged_restore_target() {
    progress_namespace_error(false);
}

#[test]
fn progress_namespace_error_retains_a_rebound_restore_target_before_metadata_only_retry() {
    progress_namespace_error(true);
}

#[test]
fn progress_namespace_error_compares_the_canonical_candidate_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan::default();
    let candidate = Progress {
        revision: 8,
        access: Some(8),
        ..Progress::for_plan(&plan)
    };
    let _lock = HeldLock::take(root.path()).unwrap();
    let mut target = None;
    assert!(
        save_progress_observing_with(
            root.path(),
            candidate,
            &mut |revision| target = Some(revision),
            |root, progress| progress_publication_error(root, progress, true),
        )
        .is_err()
    );
    let visible = progress_file(root.path()).unwrap().unwrap();
    assert_eq!(
        visible.access, None,
        "the canonical record omits self access"
    );
    assert_eq!(progress_record(visible), progress_record(candidate));
    assert_eq!(target, Some(candidate.revision));
}

#[test]
fn progress_namespace_error_cannot_claim_a_newer_replacement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let candidate = Progress {
        revision: 8,
        ..Progress::for_plan(&Plan::default())
    };
    let newer = Progress {
        revision: 9,
        ..candidate
    };
    let _lock = HeldLock::take(root.path()).unwrap();
    let mut target = None;
    assert!(
        save_progress_observing_with(
            root.path(),
            candidate,
            &mut |revision| target = Some(revision),
            |root, progress| {
                save_progress(root, progress)?;
                save_progress(root, newer)?;
                Err("a newer checkpoint replaced the attempted publication".into())
            },
        )
        .is_err()
    );
    assert_eq!(progress_file(root.path()).unwrap(), Some(newer));
    assert_eq!(target, None);
}

/// Only a Restore before any run was admitted is brought forward. A person's Cancel holds
/// aterm's permissions, a Restore of an admitted run keeps that run's own approval, and a Mac
/// with no setup records gets none.
#[test]
fn nothing_else_is_brought_forward() {
    let cancelled = aterm_tempfile::tempdir().unwrap();
    save_automatic_plan(cancelled.path(), Selection::all().workstation()).unwrap();
    write_legacy_cancel_at(cancelled.path());
    let before = Records::read(cancelled.path()).unwrap();
    assert_eq!(bring_forward_restored_at(cancelled.path()).unwrap(), None);
    assert_eq!(Records::read(cancelled.path()).unwrap(), before);
    assert!(!cancelled.path().join("initial-setup.json").exists());
    assert_eq!(launch_startup(cancelled.path()), None);
    assert!(
        !snapshot_at(cancelled.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );

    let admitted = restored_after(&Plan::default());
    leave_restored_as_is(admitted.path()).unwrap();
    let before = Records::read(admitted.path()).unwrap();
    assert_eq!(bring_forward_restored_at(admitted.path()).unwrap(), None);
    assert_eq!(Records::read(admitted.path()).unwrap(), before);
    assert!(
        snapshot_at(admitted.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );

    let absent = aterm_tempfile::tempdir().unwrap();
    let missing = absent.path().join("not-created");
    assert_eq!(bring_forward_restored_at(&missing).unwrap(), None);
    assert!(!missing.exists());
}

#[test]
fn permission_policy_never_takes_a_snapshot_that_changed_during_observation() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions]);
    authorize_at(root.path(), &plan).unwrap();
    let before = Records::read(root.path()).unwrap();
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"cancelled"}),
    )
    .unwrap();
    let after = Records::read(root.path()).unwrap();
    let unchanged = snapshot_with(Plan::default(), || Ok(false), || Ok(before.clone()));
    assert!(unchanged.unwrap().permissions_allowed, "the policy before");
    // The observation that met the write is taken again: the one after it is the snapshot.
    let mut reads = [before.clone()]
        .into_iter()
        .chain(std::iter::repeat(after.clone()));
    let settled = snapshot_with(Plan::default(), || Ok(false), || Ok(reads.next().unwrap()));
    assert!(!settled.unwrap().permissions_allowed);
    // Records that keep changing are refused.
    let mut reads = [before, after].into_iter().cycle();
    assert!(matches!(
        snapshot_with(Plan::default(), || Ok(false), || Ok(reads.next().unwrap())),
        Err(InitialSetupSnapshotError::Updating)
    ));
    assert!(matches!(
        snapshot_with(
            Plan::default(),
            || Ok(false),
            || Err("injected read failure".into())
        ),
        Err(InitialSetupSnapshotError::Read(_))
    ));
    assert!(
        !snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );
}

#[test]
fn permission_policy_waits_for_the_final_admission_commit_including_empty_choices() {
    for plan in [selected(&[Setting::Permissions]), selected(&[])] {
        let root = aterm_tempfile::tempdir().unwrap();
        assert!(
            authorize_with(root.path(), &plan, AdmittedVault::None, |_, _| {
                Err("injected final admission failure".into())
            })
            .is_err()
        );
        let staged = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(!staged.permissions_allowed);
        assert!(!staged.finished);
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty());

        assert!(authorize_at(root.path(), &plan).unwrap().is_some());
        let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(
            admitted.permissions_allowed,
            plan.selection.contains(Setting::Permissions)
        );
        assert_eq!(admitted.finished, plan.selection.is_empty());
        assert!(admitted.revision > staged.revision);
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        let completed = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(completed.finished);
        assert!(
            authorize_at(root.path(), &Plan::default())
                .unwrap()
                .is_none()
        );
        assert_eq!(revision_at(root.path()).unwrap(), completed.revision);
    }
}

#[test]
fn permission_policy_cannot_reuse_old_admission_during_a_plan_restart() {
    let root = aterm_tempfile::tempdir().unwrap();
    let old = selected(&[Setting::Widgets]);
    authorize_at(root.path(), &old).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    let next = selected(&[Setting::Widgets, Setting::Permissions]);

    save_plan(root.path(), &next).unwrap();
    let published = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!published.permissions_allowed);
    assert_eq!(published.revision, 0);
    assert!(
        restart_with(
            root.path(),
            &next,
            Restart::default(),
            AdmittedVault::None,
            |_, _| { Err("injected restart admission failure".into()) }
        )
        .is_err()
    );
    let uncommitted = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(!uncommitted.permissions_allowed);
    assert!(!uncommitted.finished);
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());

    restart_at(root.path(), &next).unwrap();
    let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(admitted.permissions_allowed);
    assert!(admitted.revision > uncommitted.revision);
}

#[test]
fn permission_policy_migrates_completed_history_but_pending_history_needs_authorization() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Permissions]);
    authorize_at(root.path(), &plan).unwrap();
    let path = root.path().join("initial-setup.json");
    let remove_admission = || {
        let bytes = std::fs::read(&path).unwrap();
        let mut record: Value = aterm_json::from_slice(&bytes).unwrap();
        record.as_object_mut().unwrap().remove("admitted");
        write_json(&path, &record).unwrap();
    };
    remove_admission();
    assert!(
        !snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(authorize_at(root.path(), &plan).unwrap().is_some());
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    remove_admission();
    let completed = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(completed.permissions_allowed);
    assert!(completed.finished);
}

#[test]
fn unified_setup_orders_sync_before_repositories_and_never_persists_the_password() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan::default();
    authorize_at(root.path(), &plan).unwrap();
    let _profile = HeldLock::take(root.path()).unwrap();
    let password = SecretBytes::from(b"test-only-join-password".as_slice());
    let mut host = Fake {
        observe: Some(root.path().into()),
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut Some(password.clone()))
        .unwrap()
        .unwrap();
    assert!(!report.needs_attention());
    assert_eq!(host.calls, Stage::ALL);
    assert!(host.password_seen);
    assert!(finished_at(root.path()).unwrap());
    for path in [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ] {
        let bytes = std::fs::read(root.path().join(path)).unwrap();
        assert!(
            !bytes
                .windows(password.len())
                .any(|window| window == password.as_slice())
        );
    }
}

/// A VAULT THIS NETWORK DOES NOT HAVE NEVER HOLDS THE REPOSITORIES: a Secrets stage that left
/// the ALab vault absent or unanswered runs Repositories in the same pass, on this Mac's own
/// credentials, which wait for the vault's token only when this Mac has none; one waiting
/// for an admin, or failing otherwise, keeps them waiting for the vault's token.
#[test]
fn an_absent_or_unanswered_vault_never_holds_the_repositories() {
    let row = |status: &str, detail: &str| Row {
        name: Stage::Secrets.name().into(),
        status: status.into(),
        detail: detail.into(),
        elapsed_ms: None,
    };
    for (secrets, runs) in [
        (row(UNREACHED, scope::NOT_REACHED), true),
        (row(ABSENT, scope::NO_VAULT), true),
        (
            row(ABSENT, aterm_machines::init_secrets::NO_VAULT_NOW),
            true,
        ),
        // A waiting row is a person's wait whatever its words: never off the network.
        (row(WAITING, scope::NO_VAULT), false),
        (
            row(
                WAITING,
                "waiting to be let in: press Allow on any Mac in ALab",
            ),
            false,
        ),
        (
            row("blocked", "the subscriber could not be installed"),
            false,
        ),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            secrets_rows: Some(vec![secrets.clone()]),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert_eq!(
            host.calls.contains(&Stage::Repositories),
            runs,
            "{secrets:?}: {}",
            report.to_text()
        );
        if runs {
            assert_eq!(
                host.repositories_vault,
                [false],
                "this Mac's own credentials"
            );
        } else {
            assert!(
                report
                    .rows
                    .iter()
                    .any(|row| row.name == Stage::Repositories.name()
                        && row
                            .detail
                            .starts_with("waiting for the selected secrets vault")),
                "{secrets:?}: {}",
                report.to_text()
            );
        }
    }
    // The live stage on this Mac's own credentials: a token here signs in (GitHub unreached
    // is the stage's own row); with none, the stage waits for the vault's token as it would
    // have, never a sign-in asked of the person for a token the vault keeps.
    for token in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            secrets_rows: Some(vec![row(ABSENT, scope::NO_VAULT)]),
            ..native_repositories(true, token)
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        let repositories: Vec<&Row> = report
            .rows
            .iter()
            .filter(|row| row.name == Stage::Repositories.name())
            .collect();
        let wait = vault_token_wait(SetupNext::Park);
        let waits = matches!(repositories.as_slice(), [row]
            if (&row.status, &row.detail) == (&wait.status, &wait.detail));
        assert_eq!(waits, !token, "token: {token}: {}", report.to_text());
        assert_eq!(host.repositories_vault, [false], "token: {token}");
    }
}

/// EVERY ROW OF A SETUP PASS CARRIES ITS TIME in the JSON: a stage's rows, its error and
/// the repositories' wait on the vault alike (presence only, never a budget).
#[test]
fn every_setup_row_carries_its_time_in_the_json() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        failure: Some(Stage::Permissions),
        pending: Some(Stage::Secrets),
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    for (name, status) in [
        (Stage::Permissions.name(), "error"),
        (Stage::Repositories.name(), "blocked"),
    ] {
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.name == name && row.status == status),
            "{name}: {}",
            report.to_text()
        );
    }
    let json: aterm_json::Value = aterm_json::from_str(&report.to_json()).unwrap();
    let rows = json["settings"].as_array().unwrap();
    assert_eq!(rows.len(), report.rows.len());
    assert!(
        rows.iter().all(|row| row["elapsed_ms"].as_u64().is_some()),
        "{json}"
    );
}

#[test]
fn pending_vault_blocks_only_repositories_and_retries_without_repeating_finished_stages() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        pending: Some(Stage::Secrets),
        ..Fake::default()
    };
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap()
            .needs_attention()
    );
    assert_eq!(host.calls, [&Stage::ALL[..4], &[Stage::Accounts]].concat());
    assert!(first_run_complete(root.path()).unwrap());
    assert!(!finished_at(root.path()).unwrap());
    assert!(claim_automatic_setup_at(root.path()).unwrap().is_some());
    host.calls.clear();
    host.pending = None;
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn a_later_incomplete_vault_blocks_repository_retries_and_reopens_only_its_stage() {
    for readiness_error in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        let mut host = Fake {
            failure: Some(Stage::Repositories),
            ..Fake::default()
        };
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, Stage::ALL);
        assert_eq!(host.vault_checks, 1);
        let completed = progress(root.path()).unwrap().unwrap().done;
        assert_ne!(completed & Stage::Secrets.bit(), 0);
        assert_eq!(completed & Stage::Repositories.bit(), 0);
        let mut interrupted = progress(root.path()).unwrap().unwrap();
        interrupted.running = Some(Stage::Repositories);
        save_progress(root.path(), interrupted).unwrap();

        host.calls.clear();
        host.failure = None;
        host.vault_blocked = !readiness_error;
        host.vault_error = readiness_error;
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(host.calls.is_empty());
        assert_eq!(host.vault_checks, 2);
        assert!(report.needs_attention());
        assert_eq!(
            progress(root.path()).unwrap().unwrap().done,
            completed & !Stage::Secrets.bit()
        );
        assert_eq!(progress(root.path()).unwrap().unwrap().running, None);
        assert!(!finished_at(root.path()).unwrap());

        host.vault_blocked = false;
        host.vault_error = false;
        execute_initial(&mut host, root.path(), &mut None).unwrap();
        assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
        // One read precedes repository effects and another verifies completion.
        assert_eq!(host.vault_checks, 4);
        assert!(finished_at(root.path()).unwrap());
    }
}

/// A host whose Repositories stage is the live one over an inert machine for one console
/// account: Apple's Command Line Tools there or not (`tools`), the token the vault's sync
/// writes there or not (`token`), and GitHub unreached for the account's first two reads.
fn native_repositories(tools: bool, token: bool) -> Fake {
    use aterm_machines::items::Ctx;
    use aterm_machines::model::{HostFacts, User};
    use aterm_machines::port::{Fake as Native, GithubFailure, Ran, Stat};
    let user = User {
        name: "tester".into(),
        uid: 501,
        gid: 20,
        home: "/test-repository-home".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Native::root();
    sys.euid = user.uid;
    let stat = |dir, len| Stat {
        uid: user.uid,
        gid: user.gid,
        mode: if dir { 0o700 } else { 0o600 },
        dir,
        symlink: false,
        len,
    };
    const TOKEN: &str = "inert_repository_tools_token";
    sys.stat_of(&user.home, stat(true, 0));
    if tools {
        sys.executable(aterm_machines::items::github::CLT_GIT, b"inert git");
    }
    if token {
        let token = format!("{}/.secrets/gh_access_token", user.home);
        sys.file(&token, TOKEN.as_bytes())
            .stat_of(&token, stat(false, TOKEN.len() as u64));
    }
    for _ in 1..=2 {
        sys.fail_github_api(&user, "/user", GithubFailure::Unreached);
    }
    sys.simulate(|_, command| {
        let command = command.display();
        if command.contains(" config --null --show-origin --get-regexp ") {
            Some(Ran::ok(""))
        } else {
            panic!("unexpected repository effect: {command}");
        }
    });
    let ctx = Ctx {
        facts: HostFacts::default(),
        users: vec![user.clone()],
        console: Some(user),
        host_name: "fixture".into(),
        cache_dir: PathBuf::from("/test-cache"),
        disrupt: false,
    };
    Fake {
        native_repositories: Some((sys, ctx)),
        ..Fake::default()
    }
}

/// Git readiness is a retryable repository prerequisite. A pass never asks the window to
/// install developer tools merely because managed Git has not arrived yet.
#[test]
fn a_repository_git_readiness_wait_remains_owed_without_a_developer_tools_request() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &selected(&[Setting::Repositories]))
        .unwrap()
        .unwrap();
    let mut host = native_repositories(false, true);
    for _ in 0..2 {
        let (_, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SetupNext::Retry);
        let saved = progress(root.path()).unwrap().unwrap();
        assert!(!saved.completed);
        assert_eq!(saved.done & Stage::Repositories.bit(), 0);
    }
    assert_eq!(host.calls, [Stage::Repositories, Stage::Repositories]);
}

#[test]
fn completed_repositories_are_read_again_after_a_parked_accounts_stage() {
    use aterm_machines::port::{Ran, Stat, System as _};

    for change in [
        "inventory",
        "credential-route",
        "launcher-permission",
        "profile-path",
        "inventory-during-accounts",
        "credential-route-during-accounts",
        "launcher-permission-during-accounts",
        "unchanged",
        "stopped",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Repositories, Setting::Messages]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut host = native_repositories(true, true);
        let (sys, ctx) = host.native_repositories.as_ref().unwrap();
        let user = &ctx.users[0];
        sys.logged_in(user);
        let (uid, gid) = (user.uid, user.gid);
        let stat = |dir, len| Stat {
            uid,
            gid,
            mode: if dir { 0o700 } else { 0o600 },
            dir,
            symlink: false,
            len,
        };
        let home = user.home.clone();
        let inventory = format!("{home}/.secrets/github-repositories.json");
        let metadata = |name| {
            aterm_json::to_vec(&aterm_json::json!({
                "schema":2,"account":"tester","repositories":[{
                    "id":1,"name":name,"owner":{"login":"tester"},
                    "full_name":format!("tester/{name}"),"private":true,
                    "clone_url":format!("https://github.com/tester/{name}.git"),
                    "default_branch":"main"
                }]
            }))
            .unwrap()
        };
        let before = metadata("before");
        sys.file(&inventory, &before)
            .stat_of(&inventory, stat(false, before.len() as u64));
        let launcher = format!("{home}/.local/bin/aterm");
        sys.executable(&launcher, b"launcher");
        sys.stat_of(&format!("{home}/before"), stat(true, 0));
        sys.file(&format!("{home}/before/.git/index"), b"DIRC");
        // The new inventory's destination contains personal work, so readback must
        // reopen the stage while apply preserves the file without cloning over it.
        sys.file(&format!("{home}/after"), b"personal work")
            .stat_of(&format!("{home}/after"), stat(false, 13));
        let route = launcher.clone();
        let worktree = format!("{home}/before");
        sys.simulate(move |sys, command| {
            let committed_head = command.args.windows(2).any(|pair| pair[0] == "-C" && pair[1] == worktree.as_str())
                && command.args.windows(4).any(|args| args[0] == "rev-parse" && args[1] == "--verify" && args[2] == "--quiet" && args[3] == "HEAD^{commit}");
            let command = command.display();
            Some(if command.ends_with(" credential fill") {
                if sys.read_file(Path::new(&route)).is_ok_and(|bytes| bytes == b"launcher") {
                    Ran::ok("protocol=https\nhost=github.com\nusername=tester\npassword=inert_repository_tools_token\n")
                } else {
                    Ran::exit(128, "credential is no longer available")
                }
            } else if command.contains(" config --null --show-origin --get-regexp ") {
                Ran::ok("")
            } else if command.ends_with(
                " rev-parse --is-inside-work-tree --show-prefix --show-toplevel --git-path index",
            ) {
                Ran::ok(&format!("true\n\n{worktree}\n.git/index\n"))
            } else if command.ends_with(" config --get-all remote.origin.url") {
                Ran::ok("https://github.com/tester/before.git\n")
            } else if committed_head {
                Ran::ok("1111111111111111111111111111111111111111\n")
            } else {
                panic!("unexpected repository effect: {command}")
            })
        });
        host.pending = Some(Stage::Accounts);
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        let prior = progress(root.path()).unwrap().unwrap();
        assert!(!prior.completed);
        assert_ne!(prior.done & Stage::Repositories.bit(), 0);
        let profile = format!("{home}/.config/aterm/init.toml");
        let custom = |path| format!("[github]\naccount='tester'\ntoken_file='{path}'\n");
        let old_source = format!("{home}/old/token");
        let new_source = format!("{home}/new/token");
        if change == "profile-path" {
            let (sys, _) = host.native_repositories.as_ref().unwrap();
            for path in [&old_source, &new_source] {
                sys.file(path, b"unsafe").stat_of(
                    path,
                    Stat {
                        mode: 0o644,
                        ..stat(false, 6)
                    },
                );
            }
            let bytes = custom(&old_source);
            sys.file(&profile, bytes.as_bytes())
                .stat_of(&profile, stat(false, bytes.len() as u64));
        }
        let parked = |host: &Fake| {
            let (sys, _) = host.native_repositories.as_ref().unwrap();
            let calls = sys.calls();
            let mark = initial_setup_park_mark_with(Some(root.path()), None, 1_000, || None)
                .with_repository_inputs(|| {
                    aterm_machines::init_github::repository_input_mark(sys).ok()
                });
            assert!(
                sys.calls()[calls.len()..]
                    .iter()
                    .all(|call| call == "identity"),
                "park sampling starts no helper or other program"
            );
            mark
        };
        let before_park = parked(&host);
        assert!(before_park.watches().secrets && before_park.watches().git);

        match change {
            "inventory" => {
                let after = metadata("after");
                let (sys, _) = host.native_repositories.as_ref().unwrap();
                sys.file(&inventory, &after)
                    .stat_of(&inventory, stat(false, after.len() as u64));
            }
            "credential-route" => {
                let (sys, _) = host.native_repositories.as_ref().unwrap();
                sys.file(&launcher, b"badexec!");
            }
            "launcher-permission" => {
                let (sys, _) = host.native_repositories.as_ref().unwrap();
                let before = sys.stat(Path::new(&launcher)).unwrap();
                sys.stat_of(
                    &launcher,
                    Stat {
                        mode: 0o600,
                        ..before
                    },
                );
            }
            "profile-path" => {
                let (sys, _) = host.native_repositories.as_ref().unwrap();
                let bytes = custom(&new_source);
                sys.file(&profile, bytes.as_bytes())
                    .stat_of(&profile, stat(false, bytes.len() as u64));
                let paths = aterm_machines::init_github::repository_input_watch_paths(sys).unwrap();
                assert!(paths.contains(&PathBuf::from(&new_source)));
                assert!(!paths.contains(&PathBuf::from(&old_source)));
            }
            "inventory-during-accounts" => {
                let inventory = inventory.clone();
                let after = metadata("after");
                let changed = stat(false, after.len() as u64);
                host.accounts_end = Some(Box::new(move |host| {
                    let (sys, _) = host.native_repositories.as_ref().unwrap();
                    sys.file(&inventory, &after).stat_of(&inventory, changed);
                }));
            }
            "credential-route-during-accounts" => {
                let launcher = launcher.clone();
                host.accounts_end = Some(Box::new(move |host| {
                    let (sys, _) = host.native_repositories.as_ref().unwrap();
                    sys.file(&launcher, b"badexec!");
                }));
            }
            "launcher-permission-during-accounts" => {
                let launcher = launcher.clone();
                host.accounts_end = Some(Box::new(move |host| {
                    let (sys, _) = host.native_repositories.as_ref().unwrap();
                    let before = sys.stat(Path::new(&launcher)).unwrap();
                    sys.stat_of(
                        &launcher,
                        Stat {
                            mode: 0o600,
                            ..before
                        },
                    );
                }));
            }
            "stopped" => request_stop_at(root.path(), revision).unwrap(),
            "unchanged" => {}
            _ => unreachable!(),
        }
        let after_park = parked(&host);
        assert_eq!(
            after_park.facts_changed_since(&before_park),
            matches!(
                change,
                "inventory" | "credential-route" | "launcher-permission" | "profile-path"
            ),
            "{change}: actual local input changes must wake the still-pending run"
        );
        if after_park.facts_changed_since(&before_park) {
            host.calls.clear();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            let reopened = progress(root.path()).unwrap().unwrap();
            assert_eq!(host.pending, Some(Stage::Accounts), "Accounts still waits");
            assert_eq!(reopened.done & Stage::Accounts.bit(), 0);
            assert_eq!(reopened.done & Stage::Repositories.bit(), 0);
            assert!(!reopened.completed);
            assert!(
                host.calls.contains(&Stage::Repositories),
                "{change}: input wake ran readback"
            );
        }
        host.pending = None;
        host.calls.clear();
        let pass = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        let saved = progress(root.path()).unwrap().unwrap();
        match change {
            "unchanged" => {
                assert!(saved.completed);
                assert_eq!(host.calls, [Stage::Accounts]);
                let retired = initial_setup_park_mark_with(Some(root.path()), None, 1_000, || None)
                    .with_repository_inputs(|| {
                        panic!("completed setup reads no repository inputs")
                    });
                assert!(!retired.facts_changed_since(&before_park));
                assert!(retired.watches().none());
            }
            "stopped" => {
                assert!(pass.is_none());
                assert!(host.calls.is_empty());
                assert!(saved.stopped);
            }
            _ => {
                assert!(
                    !saved.completed,
                    "{change}: old repository readiness cannot complete a changed setup"
                );
                assert_eq!(saved.done & Stage::Repositories.bit(), 0, "{change}");
                let (report, next) = pass.unwrap();
                assert!(!rows_ready(&report.rows), "{change}");
                assert_ne!(next, SetupNext::Done, "{change}");
            }
        }
        if change.ends_with("during-accounts") || change == "launcher-permission" {
            assert_ne!(saved.done & Stage::Accounts.bit(), 0);
            let (sys, _) = host.native_repositories.as_ref().unwrap();
            if change.starts_with("launcher-permission") {
                assert_eq!(sys.read_file(Path::new(&launcher)).unwrap(), b"launcher");
                let damaged = sys.stat(Path::new(&launcher)).unwrap();
                sys.stat_of(
                    &launcher,
                    Stat {
                        mode: 0o755,
                        ..damaged
                    },
                );
            } else {
                sys.file(&inventory, &before)
                    .stat_of(&inventory, stat(false, before.len() as u64));
                sys.executable(&launcher, b"launcher");
            }
            host.calls.clear();
            let repaired = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            assert!(rows_ready(&repaired.0.rows));
            assert_eq!(repaired.1, SetupNext::Done);
            assert_eq!(
                host.calls,
                [Stage::Repositories],
                "sign-outs stay completed"
            );
            assert!(progress(root.path()).unwrap().unwrap().completed);
        }
        let (sys, _) = host.native_repositories.as_ref().unwrap();
        assert!(
            sys.calls().iter().all(|call| !call.contains(" clone ")
                && !call.contains(" agh GET ")
                && !call.contains("credential approve")),
            "{change}: readback and preservation must have no repository effects: {:?}",
            sys.calls()
        );
    }
}

fn repositories_recover_after_an_interface_change(with_secrets: bool) {
    use aterm_machines::items::Ctx;
    use aterm_machines::model::{HostFacts, User};
    use aterm_machines::port::{Fake as Native, Ran, Stat};

    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = selected(&[Setting::Repositories]);
    plan.selection.set(Setting::Secrets, with_secrets);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let user = User {
        name: "tester".into(),
        uid: 501,
        gid: 20,
        home: "/test-repository-home".into(),
        shell: "/bin/zsh".into(),
        admin: false,
    };
    let mut sys = Native::root();
    sys.euid = user.uid;
    let stat = |dir, len| Stat {
        uid: user.uid,
        gid: user.gid,
        mode: if dir { 0o700 } else { 0o600 },
        dir,
        symlink: false,
        len,
    };
    const TOKEN: &str = "inert_repository_recovery_token";
    sys.stat_of(&user.home, stat(true, 0));
    sys.executable(aterm_machines::items::github::CLT_GIT, b"inert git");
    sys.executable(&format!("{}/.local/bin/aterm", user.home), b"launcher");
    let token = format!("{}/.secrets/gh_access_token", user.home);
    sys.file(&token, TOKEN.as_bytes())
        .stat_of(&token, stat(false, TOKEN.len() as u64));
    // Metadata has not arrived from the private vault on this network yet. Default
    // repository setup waits without enumerating GitHub, then wakes on a new attachment.
    const INVENTORY: &[u8] = b"{\"schema\":2,\"account\":\"tester\",\"repositories\":[]}";
    let inventory_path = format!("{}/.secrets/github-repositories.json", user.home);
    let inventory_stat = stat(false, INVENTORY.len() as u64);
    // The local helper already returns this token for all repositories.
    // A complete empty inventory therefore needs no credential writes or clones.
    sys.simulate(|_, command| {
        let command = command.display();
        if command.ends_with(" credential fill") {
            Some(Ran::ok(&format!(
                "protocol=https\nhost=github.com\nusername=tester\npassword={TOKEN}\n"
            )))
        } else if command.contains(" config --null --show-origin --get-regexp ") {
            Some(Ran::ok(
                "file:/test-repository-home/.gitconfig\0credential.helper\ninert-helper\0",
            ))
        } else {
            panic!("unexpected repository effect: {command}");
        }
    });
    let ctx = Ctx {
        facts: HostFacts::default(),
        users: vec![user.clone()],
        console: Some(user),
        host_name: "fixture".into(),
        cache_dir: PathBuf::from("/test-cache"),
        disrupt: false,
    };
    let mut host = Fake {
        native_repositories: Some((sys, ctx)),
        ..Fake::default()
    };
    for _ in 0..10 {
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(next, SetupNext::Retry);
        assert!(report.rows.iter().any(|row| row.status == "blocked"));
    }
    let attempts =
        |host: &Fake, stage| host.calls.iter().filter(|called| **called == stage).count();
    assert_eq!(attempts(&host, Stage::Secrets), usize::from(with_secrets));
    assert_eq!(attempts(&host, Stage::Repositories), 10);
    let progress = progress(root.path()).unwrap().unwrap();
    assert_eq!(progress.done & Stage::Repositories.bit(), 0);
    if with_secrets {
        assert_ne!(progress.done & Stage::Secrets.bit(), 0);
    }
    let mark = |address| {
        initial_setup_park_mark_with(Some(root.path()), None, 1_800_000_600, || {
            Some(format!("en0 192.0.2.{address} 00:11:22:33:44:55"))
        })
    };
    let parked = mark(1);
    assert_eq!(parked.retry_due(), None);
    assert_eq!(parked.mac_settings_retry_due(), None);
    assert_eq!(parked, mark(1), "an unchanged network stays parked");
    let reattached = mark(2);

    // The new network delivers the vault's complete private inventory. Its local account
    // and repository list suffice; no GitHub identity or inventory request is needed.
    let (sys, _) = host.native_repositories.as_ref().unwrap();
    sys.file(&inventory_path, INVENTORY)
        .stat_of(&inventory_path, inventory_stat);

    // A worker woken by the changed local attachment can finish the same
    // admitted run, without repeating Secrets or creating a new revision.
    let (report, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert!(rows_ready(&report.rows), "{:?}", report.rows);
    assert_eq!(next, SetupNext::Done);
    assert_eq!(attempts(&host, Stage::Secrets), usize::from(with_secrets));
    assert_eq!(attempts(&host, Stage::Repositories), 11);
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert!(
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(attempts(&host, Stage::Repositories), 11);
    let (sys, _) = host.native_repositories.as_ref().unwrap();
    assert!(sys.calls().iter().all(|call| !call.contains(" agh GET ")));
    assert!(sys.fed().is_empty());
    assert!(sys.calls().iter().all(|call| !call.contains(" clone ")
        && !call.contains("credential approve")
        && !call.contains("--replace-all")));

    let no_interface_read = || {
        initial_setup_park_mark_with(Some(root.path()), None, 1_800_000_600, || {
            panic!("finished or stopped repositories must not inspect interfaces")
        })
    };
    no_interface_read();
    let stopped_revision = restart_at(root.path(), &plan).unwrap();
    request_stop_at(root.path(), stopped_revision).unwrap();
    no_interface_read();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        stopped_revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(attempts(&host, Stage::Repositories), 11);
    assert_ne!(
        parked, reattached,
        "a local interface change must wake unfinished repositories"
    );
}

#[test]
fn a_network_change_wakes_repositories_after_secrets_finishes() {
    repositories_recover_after_an_interface_change(true);
}

#[test]
fn a_network_change_wakes_a_repositories_only_run() {
    repositories_recover_after_an_interface_change(false);
}

#[test]
fn subset_and_empty_plans_never_visit_unselected_stages() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Repositories]);
    authorize_at(root.path(), &plan).unwrap();
    assert_eq!(saved_plan(root.path()).unwrap(), Some(plan));
    let mut host = Fake::default();
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Repositories]);
    assert!(!host.password_seen);
    assert_eq!(host.vault_checks, 0);

    let empty = aterm_tempfile::tempdir().unwrap();
    authorize_at(empty.path(), &Plan::from(Selection::none())).unwrap();
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, empty.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(finished_at(empty.path()).unwrap());
}

struct WithholdingStages {
    doctor: super::super::tests::Withholding,
    rest: Fake,
    grant_in_permissions: bool,
}

impl WithholdingStages {
    fn over(doctor: super::super::tests::Withholding) -> Self {
        Self {
            doctor,
            rest: Fake::default(),
            grant_in_permissions: false,
        }
    }
}

impl InitialHost for WithholdingStages {
    fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.doctor.pass(root)
    }
    fn workstation(&mut self, root: &Path, _: Option<Selection>) -> Result<Option<Report>, String> {
        self.doctor.pass(root)
    }
    fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.rest.accounts(root)
    }
    fn spotlight(&mut self) -> Result<Vec<Row>, String> {
        self.rest.spotlight()
    }
    fn permissions(&mut self, plan: &Plan, access: u64) -> Result<Vec<Row>, String> {
        if self.grant_in_permissions {
            self.doctor.grant();
        }
        self.rest.permissions(plan, access)
    }
    fn secrets(
        &mut self,
        plan: &Plan,
        replaces: Replaces,
        password: &mut Option<SecretBytes>,
    ) -> Result<Vec<Row>, String> {
        self.rest.secrets(plan, replaces, password)
    }
    fn secrets_pace(&mut self) -> Option<Pace> {
        self.rest.secrets_pace()
    }
    fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
        self.rest.configured_vault()
    }
    fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
        self.rest.current_vault(plan)
    }
    fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        self.rest.repositories(vault)
    }
    fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
        self.rest.current_repositories(vault)
    }
    fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
        self.doctor.follow_up(root)
    }
    fn withheld_owed(&mut self, root: &Path) -> Vec<Setting> {
        self.doctor.owed(root)
    }
}

#[test]
fn the_grant_that_arrives_while_the_stage_is_pending_clears_notification_center_once() {
    for granted in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Dock]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(
            super::super::tests::Withholding::widgets_and_a_dock_that_never_verifies(),
        );
        let retry = |stages: &mut WithholdingStages| {
            let _ = execute_initial_for_revision(
                stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("setup still runs");
        };
        retry(&mut stages);
        assert_eq!(stages.doctor.window_manager_writes(), 1);
        assert_eq!(stages.doctor.notification_center_writes(), 0);
        stages.doctor.show_desktop_widgets();
        // Each retry a minute on: the Dock's second try ten minutes after its first, its
        // third twenty after that. The first sets the widgets once more (relaunching the
        // Dock, which reads them), since nothing says who showed them; shown again after
        // that, they stay shown.
        retry(&mut stages);
        assert_eq!(stages.doctor.window_manager_writes(), 2);
        assert!(!stages.doctor.desktop_widgets_shown());
        stages.doctor.show_desktop_widgets();
        let docks = stages.doctor.dock_relaunches();
        for _ in 0..28 {
            retry(&mut stages);
        }
        assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
        assert_eq!(stages.doctor.window_manager_writes(), 2);
        assert!(stages.doctor.desktop_widgets_shown());
        assert_eq!(stages.doctor.notification_center_writes(), 0);
        assert_eq!(
            stages.doctor.dock_relaunches(),
            docks + 2,
            "the Dock's second try: its relaunch and its rollback's"
        );
        let docks = stages.doctor.dock_relaunches();
        if granted {
            stages.doctor.grant();
        }
        for _ in 0..30 {
            retry(&mut stages);
        }
        assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
        assert_eq!(stages.doctor.notification_center_cleared(), granted);
        assert_eq!(
            stages.doctor.notification_center_writes(),
            usize::from(granted)
        );
        assert_eq!(
            stages.doctor.notification_center_relaunches(),
            usize::from(granted)
        );
        assert_eq!(stages.doctor.window_manager_writes(), 2);
        assert!(stages.doctor.desktop_widgets_shown());
        assert_eq!(
            stages.doctor.dock_relaunches(),
            docks + 2,
            "the Dock's third try, tried again by itself"
        );
        assert_eq!(
            super::super::tests::Withholding::withheld(root.path()).is_empty(),
            granted
        );
    }
}

#[test]
fn the_widgets_row_never_says_aterm_set_the_list_the_follow_up_still_owes() {
    use super::super::tests::{WIDGETS, Withholding, nc_widgets};
    let list = nc_widgets().describe();
    let owed = format!("{list}; left as it is: {SETTLED_OWED}");
    for keeps in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Dock]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages =
            WithholdingStages::over(Withholding::widgets_and_a_dock_that_never_verifies());
        if !keeps {
            stages.doctor.notification_center_never_keeps();
        }
        let retry = |stages: &mut WithholdingStages| {
            execute_initial_for_revision(stages, root.path(), revision, &mut None, SetupHooks::NONE)
                .unwrap()
                .expect("setup still runs")
                .0
        };
        retry(&mut stages);
        stages.doctor.grant();
        for at in 0..15 {
            let report = retry(&mut stages);
            assert!(
                !report.rows.iter().any(|row| row.detail.contains(SETTLED)),
                "{keeps} {at}: {}",
                report.to_text()
            );
            let followed_up = !keeps || at == 0;
            let named = report
                .rows
                .iter()
                .any(|row| row.name == WIDGETS && row.status == "ok" && row.detail == owed);
            assert_eq!(named, followed_up, "{keeps} {at}: {}", report.to_text());
            assert_eq!(
                report.rows.iter().filter(|row| row.name == WIDGETS).count(),
                1 + usize::from(followed_up),
                "the Workstation stage's row, and the follow-up's: {keeps} {at}: {}",
                report.to_text()
            );
        }
        assert!(!finished_at(root.path()).unwrap(), "the stage is pending");
        assert_eq!(stages.doctor.window_manager_writes(), 1);
        assert_eq!(stages.doctor.notification_center_cleared(), keeps);
        assert_eq!(
            stages.doctor.notification_center_writes(),
            if keeps { 1 } else { 4 },
            "a clear that takes once; else two tries and their rollbacks"
        );
        assert_eq!(Withholding::withheld(root.path()).is_empty(), keeps);
    }
}

#[test]
fn the_grant_that_arrives_during_the_permissions_stage_is_followed_up_as_setup_completes() {
    for granted in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages {
            rest: Fake {
                pending: Some(Stage::Permissions),
                ..Fake::default()
            },
            ..WithholdingStages::over(super::super::tests::Withholding::widgets())
        };
        for _ in 0..3 {
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
        }
        assert_eq!(stages.doctor.notification_center_writes(), 0);
        assert!(!finished_at(root.path()).unwrap());
        stages.grant_in_permissions = granted;
        stages.rest.pending = None;
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the retry runs");
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(stages.doctor.notification_center_cleared(), granted);
        assert_eq!(
            stages.doctor.notification_center_writes(),
            usize::from(granted)
        );
        assert_eq!(
            super::super::tests::Withholding::withheld(root.path()).is_empty(),
            granted
        );
        for _ in 0..2 {
            assert!(
                execute_initial_for_revision(
                    &mut stages,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE
                )
                .unwrap()
                .is_none()
            );
        }
        assert_eq!(
            stages.doctor.notification_center_writes(),
            usize::from(granted)
        );
    }
}

#[test]
fn a_stop_before_pending_follow_up_leaves_withheld_preferences_untouched() {
    use super::super::tests::Withholding;
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP: AtomicBool = AtomicBool::new(false);
    fn after_permissions(event: StageEvent) {
        if !event.running && event.settings == Stage::Permissions.settings() {
            STOP.store(true, Ordering::SeqCst);
        }
    }

    for (pending, request) in [(true, false), (true, true), (false, true)] {
        STOP.store(false, Ordering::SeqCst);
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Permissions]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages {
            rest: Fake {
                pending: pending.then_some(Stage::Permissions),
                ..Fake::default()
            },
            grant_in_permissions: true,
            ..WithholdingStages::over(Withholding::widgets())
        };
        let hooks = SetupHooks {
            stop: |_| STOP.load(Ordering::SeqCst),
            stage: if request { after_permissions } else { |_| {} },
            ..SetupHooks::NONE
        };
        let (_, next) =
            execute_initial_for_revision(&mut stages, root.path(), revision, &mut None, hooks)
                .unwrap()
                .expect("the admitted run makes a pass");

        assert_eq!(STOP.load(Ordering::SeqCst), request);
        assert!(
            !root.path().join(STOP_REQUEST).exists(),
            "the save has not run"
        );
        let stopped = pending && request;
        assert_eq!(
            stages.doctor.notification_center_writes(),
            usize::from(!stopped),
            "a Stop holds before follow-up while work is pending; a finished last stage keeps its completion"
        );
        assert_eq!(progress(root.path()).unwrap().unwrap().stopped, stopped);
        assert_eq!(finished_at(root.path()).unwrap(), !pending || request);
        assert_eq!(
            Withholding::withheld(root.path()).is_empty(),
            !stopped,
            "a stopped clear remains withheld"
        );
        assert_eq!(
            next,
            if pending && !request {
                SetupNext::Retry
            } else {
                SetupNext::Done
            }
        );
    }
}

#[test]
fn a_stopped_run_follows_nothing_up() {
    use super::super::tests::{WIDGETS, Withholding};
    for stop in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Widgets, Setting::Secrets]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        // The vault is not joined yet: the run is still going.
        stages.rest.pending = Some(Stage::Secrets);
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert!(!finished_at(root.path()).unwrap());
        if stop {
            request_stop_at(root.path(), revision).unwrap();
        }
        assert_eq!(stopped(root.path()).unwrap(), stop);
    }
}

/// A later choice that leaves the widgets row as is, the vault-alone one included, withdraws
/// the clear setup still owes; a restart that keeps the row withdraws nothing, and once the
/// grant arrives that run's follow-up clears Notification Center's widgets.
#[test]
fn a_choice_that_leaves_the_widgets_as_is_withdraws_the_clear_setup_owes() {
    use super::super::tests::{WIDGETS, Withholding};
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    for (what, next, cleared) in [
        ("the vault alone", selected(&[Setting::Secrets]), false),
        (
            "the Dock and the vault",
            selected(&[Setting::Dock, Setting::Secrets]),
            false,
        ),
        ("the same groups", plan.clone(), true),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS], "{what}");
        let revision = restart_at(root.path(), &next).unwrap();
        stages.doctor.grant();
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(
            stages.doctor.notification_center_cleared(),
            cleared,
            "{what}"
        );
        assert!(Withholding::withheld(root.path()).is_empty(), "{what}");
    }
}

/// A finished modern run left Notification Center's list for its follow-up.
fn widgets_owed_before_restore() -> (
    aterm_tempfile::TempDir,
    Plan,
    super::super::tests::Withholding,
) {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = Plan::from(group_selection(Group::Desktop));
    for setting in Group::Input.settings() {
        plan.selection.set(*setting, true);
    }
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    assert!(progress(root.path()).unwrap().unwrap().only.is_none());
    // This first Workstation stage owes its whole selection, so the
    // stage fixture's unrestricted pass is the production scope here.
    let mut stages = WithholdingStages::over(super::super::tests::Withholding::widgets());
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the initial modern run executes");
    assert!(finished_at(root.path()).unwrap());
    assert!(progress(root.path()).unwrap().unwrap().completed);
    let doctor = stages.doctor;
    assert_eq!(
        super::super::tests::Withholding::withheld(root.path()),
        [super::super::tests::WIDGETS]
    );
    assert_eq!(doctor.notification_center_writes(), 0);
    assert!(!doctor.notification_center_cleared());
    (root, plan, doctor)
}

#[test]
fn an_interrupted_restore_never_revives_its_old_widgets_follow_up_for_another_choice() {
    use super::super::tests::Withholding;
    use aterm_types::macos_setup::Group;

    for join in [false, true] {
        let (root, mut plan, mut doctor) = widgets_owed_before_restore();
        // Restore published its authoritative record, then the process ended
        // before its next write cleared the old withheld-preferences record.
        save_restore(root.path()).unwrap();
        assert!(stopped(root.path()).unwrap());
        if join {
            plan.selection.set(Setting::Secrets, true);
            system_state_join(root.path(), &plan);
        } else {
            for setting in Group::Input.settings() {
                plan.selection.set(*setting, false);
            }
            system_state_choice(root.path(), &plan);
        }
        assert!(!stopped(root.path()).unwrap());
        let (decision, restored) = decision_record(root.path()).unwrap();
        assert_eq!(decision, Decision::Pending);
        let restored = restored.expect("the unchanged Desktop group stands restored");
        assert!(restored.left.contains(Setting::Widgets));
        assert!(
            !restored
                .queue(progress(root.path()).unwrap(), Some(&plan))
                .contains(Setting::Widgets)
        );
        assert!(progress(root.path()).unwrap().unwrap().admitted);
        doctor.grant();
        let followed = doctor.follow_up(root.path()).unwrap();
        assert_eq!(doctor.notification_center_writes(), 0);
        assert!(
            followed.is_none(),
            "an unrelated choice or Join never authorizes restored Widgets"
        );
        assert!(!doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
    }
}

#[test]
fn a_widgets_follow_up_requires_fresh_authority_after_restore() {
    use super::super::tests::{WIDGETS, Withholding};
    use aterm_types::macos_setup::Group;

    for route in [
        "Restore stop",
        "Restore cleared deferred work",
        "Desktop set up",
        "Set up now",
    ] {
        let (root, mut plan, mut doctor) = widgets_owed_before_restore();
        if route == "Restore cleared deferred work" {
            doctor.restore(root.path()).unwrap();
            assert!(Withholding::withheld(root.path()).is_empty());
            for setting in Group::Input.settings() {
                plan.selection.set(*setting, false);
            }
            system_state_choice(root.path(), &plan);
        } else {
            save_restore(root.path()).unwrap();
            match route {
                "Desktop set up" => {
                    for setting in Group::Desktop.settings() {
                        plan.selection.set(*setting, false);
                    }
                    system_state_choice(root.path(), &plan);
                    for setting in Group::Desktop.settings() {
                        plan.selection.set(*setting, true);
                    }
                    system_state_choice(root.path(), &plan);
                    let restored = decision_record(root.path()).unwrap().1.unwrap();
                    assert!(
                        restored
                            .queue(progress(root.path()).unwrap(), Some(&plan))
                            .contains(Setting::Widgets)
                    );
                }
                "Set up now" => {
                    system_state_apply(root.path(), &plan, false, || Ok(None));
                    // The put-back still waits for Full Disk Access, so the record
                    // survives, queueing what the Workstation stage now sets.
                    let restored = decision_record(root.path()).unwrap().1.unwrap();
                    assert!(
                        restored
                            .queue(progress(root.path()).unwrap(), Some(&plan))
                            .contains(Setting::Widgets)
                    );
                }
                _ => assert!(stopped(root.path()).unwrap()),
            }
        }
        doctor.grant();
        let follows = matches!(route, "Desktop set up" | "Set up now");
        assert_eq!(
            doctor.follow_up(root.path()).unwrap().is_some(),
            follows,
            "{route}"
        );
        assert_eq!(
            doctor.notification_center_writes(),
            usize::from(follows),
            "{route}"
        );
        assert_eq!(doctor.notification_center_cleared(), follows, "{route}");
        assert_eq!(
            Withholding::withheld(root.path()),
            if route == "Restore stop" {
                vec![WIDGETS.to_owned()]
            } else {
                Vec::new()
            },
            "{route}"
        );
    }
}

#[test]
fn an_unadmitted_modern_widgets_follow_up_waits_for_its_own_final_commit() {
    use super::super::tests::{WIDGETS, Withholding};
    use aterm_types::macos_setup::Group;

    let (root, mut plan, mut doctor) = widgets_owed_before_restore();
    save_restore(root.path()).unwrap();
    for setting in Group::Desktop.settings() {
        plan.selection.set(*setting, false);
    }
    system_state_choice(root.path(), &plan);
    for setting in Group::Desktop.settings() {
        plan.selection.set(*setting, true);
    }
    let mut attempt = InitialSetupRestart::chosen();
    let staged = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        attempt.persist_with(
            root,
            &plan,
            false,
            || Ok(None),
            |_, _| Err("injected failure before the final admission save".into()),
            observe_restart,
        )
    });
    assert!(staged.result.is_err());
    assert!(attempt.pending());
    let pending = progress(root.path()).unwrap().unwrap();
    assert!(pending.belongs_to(&plan));
    assert!(!pending.admitted);
    let restored = decision_record(root.path()).unwrap().1.unwrap();
    assert!(
        restored
            .queue(Some(pending), Some(&plan))
            .contains(Setting::Widgets)
    );
    let before = observation_at(root.path()).unwrap();
    doctor.grant();
    let followed = doctor.follow_up(root.path()).unwrap();
    assert_eq!(doctor.notification_center_writes(), 0);
    assert!(
        followed.is_none(),
        "a staged queue is not a durable admission"
    );
    assert!(!doctor.notification_center_cleared());
    assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
    assert_eq!(observation_at(root.path()).unwrap(), before);

    let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        attempt.persist_with(
            root,
            &plan,
            false,
            || Ok(None),
            save_progress,
            observe_restart,
        )
    })
    .result
    .unwrap();
    assert_eq!(admitted, Some(pending.revision));
    assert!(!attempt.pending());
    assert!(doctor.follow_up(root.path()).unwrap().is_some());
    assert_eq!(doctor.notification_center_writes(), 1);
    assert!(doctor.notification_center_cleared());
    assert!(Withholding::withheld(root.path()).is_empty());
    assert!(doctor.follow_up(root.path()).unwrap().is_none());
    assert_eq!(doctor.notification_center_writes(), 1);
}

/// `aterm doctor --apply` over a choice whose admission never landed admits that run at its
/// last write, as the launch's offer would have: the plan the person chose, with the stages
/// the apply ran recorded finished, so the next launch runs them no more. Its Widgets change,
/// withheld for want of Full Disk Access, follows the grant, once.
#[test]
fn an_explicit_apply_over_a_staged_choice_admits_it_and_its_widgets_follow_the_grant() {
    use super::super::tests::{WIDGETS, Withholding};

    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Permissions]);
    let mut attempt = InitialSetupRestart::chosen();
    let staged = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        attempt.persist_with(
            root,
            &plan,
            false,
            || Ok(None),
            |_, _| Err("injected failure before the final admission save".into()),
            observe_restart,
        )
    });
    assert!(staged.result.is_err());
    assert!(attempt.pending());
    assert!(Withholding::withheld(root.path()).is_empty());
    let mut doctor = Withholding::widgets();
    doctor.explicit_apply(root.path()).unwrap();
    let expanded = saved_plan(root.path()).unwrap().unwrap();
    assert!(expanded.selection.contains(Setting::Secrets));
    assert!(expanded.selection.contains(Setting::Permissions));
    let admitted = progress(root.path()).unwrap().unwrap();
    assert!(admitted.belongs_to(&expanded));
    assert!(admitted.admitted);
    assert_ne!(admitted.done & Stage::Workstation.bit(), 0);
    assert!(admission_at(root.path()).unwrap().is_some());
    assert!(
        permissions_allowed(root.path()),
        "an explicit apply starts selected permission steps"
    );
    assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
    assert_eq!(doctor.notification_center_writes(), 0);

    doctor.grant();
    assert!(doctor.follow_up(root.path()).unwrap().is_some());
    assert_eq!(doctor.notification_center_writes(), 1);
    assert!(doctor.notification_center_cleared());
    assert!(Withholding::withheld(root.path()).is_empty());
    assert!(doctor.follow_up(root.path()).unwrap().is_none());
    assert_eq!(doctor.notification_center_writes(), 1);
    drop(attempt);
}

#[test]
fn a_widgets_follow_up_waits_for_its_saved_plan_to_match_existing_progress() {
    use super::super::tests::{WIDGETS, Withholding};

    for legacy_mismatch in [false, true] {
        let (root, _, mut doctor) = widgets_owed_before_restore();
        let withheld = root.path().join("withheld-preferences.json");
        let owed = std::fs::read(&withheld).unwrap();
        let path = root.path().join("automatic-plan.json");
        let retained = root.path().join("automatic-plan-retained.json");
        std::fs::rename(&path, &retained).unwrap();
        if legacy_mismatch {
            // A selection-only plan is not fallback authority over the
            // modern run whose progress it has not reset yet.
            super::super::save_automatic_plan(root.path(), selected(&[Setting::Widgets]).selection)
                .unwrap();
        }
        assert!(progress(root.path()).unwrap().unwrap().admitted);
        doctor.grant();
        assert!(
            doctor.follow_up(root.path()).is_err(),
            "an unconfirmed run must not reset its deferred retry record"
        );
        assert_eq!(doctor.notification_center_writes(), 0);
        assert!(!doctor.notification_center_cleared());
        assert_eq!(std::fs::read(&withheld).unwrap(), owed);

        std::fs::rename(retained, path).unwrap();
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        assert!(doctor.follow_up(root.path()).unwrap().is_some());
        assert_eq!(doctor.notification_center_writes(), 1);
        assert!(doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
        assert!(doctor.follow_up(root.path()).unwrap().is_none());
        assert_eq!(doctor.notification_center_writes(), 1);
    }
}

#[test]
fn a_new_setup_run_starts_the_follow_ups_tries_afresh() {
    use super::super::{load_withheld, record_withheld};
    let root = aterm_tempfile::tempdir().unwrap();
    let widgets = super::super::tests::WIDGETS.to_owned();
    let spend = |root: &Path| {
        record_withheld(root, std::slice::from_ref(&widgets), &[], &[], ("b", 0)).unwrap();
        for at in [0, 600] {
            record_withheld(root, &[], &[], std::slice::from_ref(&widgets), ("b", at)).unwrap();
        }
        let owed = load_withheld(root).unwrap();
        assert_eq!(owed.failed.len(), 1, "{:?}", owed.failed);
        assert_eq!(owed.failed[0].tries, 2);
    };
    spend(root.path());
    let plan = selected(&[Setting::Widgets]);
    authorize_at(root.path(), &plan).unwrap().unwrap();
    let owed = load_withheld(root.path()).unwrap();
    assert_eq!(owed.items, std::slice::from_ref(&widgets));
    assert!(owed.failed.is_empty(), "{:?}", owed.failed);
    spend(root.path());
    restart_at(root.path(), &plan).unwrap();
    let owed = load_withheld(root.path()).unwrap();
    assert_eq!(owed.items, [widgets]);
    assert!(owed.failed.is_empty(), "{:?}", owed.failed);
}

/// Notification Center's clear that never keeps is tried again by itself, less often after
/// each try ([`super::super::RETRY`]), whichever pass makes each try, and never held.
#[test]
fn notification_centers_clear_is_tried_again_less_often_whichever_pass_makes_each_try() {
    use super::super::tests::Withholding;
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages =
        WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
    let retry = |stages: &mut WithholdingStages| {
        let _ = execute_initial_for_revision(
            stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("setup still runs");
    };
    retry(&mut stages);
    assert_eq!(stages.doctor.notification_center_writes(), 0);
    assert_eq!(
        Withholding::withheld(root.path()),
        [super::super::tests::WIDGETS]
    );
    stages.doctor.grant();
    stages.doctor.notification_center_never_keeps();
    let mut tries = Vec::new();
    for _ in 0..40 {
        let writes = stages.doctor.notification_center_writes();
        retry(&mut stages);
        match stages.doctor.notification_center_writes() - writes {
            0 => {}
            2 => tries.push(stages.doctor.now()),
            more => panic!("a try and its rollback in one retry, not {more} writes"),
        }
    }
    assert!(!finished_at(root.path()).unwrap(), "the stage stays open");
    assert_eq!(tries.len(), 3, "{tries:?}");
    for (pause, pair) in tries.windows(2).enumerate() {
        let apart = pair[1].duration_since(pair[0]).unwrap();
        let pause = super::super::RETRY.after(u32::try_from(pause + 1).unwrap());
        assert!(apart >= pause, "{apart:?} after a pause of {pause:?}");
    }
}

/// A rerun (Apply in System State) keeps the clear owed and tries it on the same pauses.
#[test]
fn a_rerun_keeps_a_failing_clear_owed_and_tries_it_again() {
    use super::super::tests::Withholding;
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets]);
    let mut revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages =
        WithholdingStages::over(Withholding::widgets_whose_first_try_does_not_verify());
    let retry = |stages: &mut WithholdingStages, revision: u64| {
        let writes = stages.doctor.notification_center_writes();
        let _ = execute_initial_for_revision(
            stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("setup still runs");
        stages.doctor.notification_center_writes() - writes
    };
    retry(&mut stages, revision);
    stages.doctor.grant();
    stages.doctor.notification_center_never_keeps();
    for run in 0..2 {
        let tried: usize = (0..40).map(|_| retry(&mut stages, revision)).sum();
        assert_eq!(
            tried, 6,
            "run {run}: three tries in 40 passes a minute apart, each with its rollback"
        );
        assert_eq!(
            Withholding::withheld(root.path()),
            [super::super::tests::WIDGETS],
            "run {run}: still owed"
        );
        revision = restart_at(root.path(), &plan).unwrap();
        assert_eq!(
            Withholding::withheld(root.path()),
            [super::super::tests::WIDGETS],
            "run {run}: the restart keeps it owed"
        );
    }
}

/// A person's Check again starts a Mac setting's tries afresh: the run's next pass tries a
/// setting whose pause has not ended at once, counted as its first try, so the pause after
/// it is the first one again ([`tries_afresh`]).
#[test]
fn check_again_tries_a_paused_mac_setting_at_once_and_counts_afresh() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Dock]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(
        super::super::tests::Withholding::widgets_and_a_dock_that_never_verifies(),
    );
    let pass = |stages: &mut WithholdingStages| {
        execute_initial_for_revision(stages, root.path(), revision, &mut None, SetupHooks::NONE)
            .unwrap()
            .expect("the run is unfinished");
    };
    let tries = || Automatic::load(root.path(), revision).unwrap().failed[0].tries;
    for _ in 0..3 {
        pass(&mut stages);
    }
    assert_eq!(tries(), 1, "tried once, then put off for ten minutes");
    let relaunches = stages.doctor.dock_relaunches();
    pass(&mut stages);
    assert_eq!(stages.doctor.dock_relaunches(), relaunches, "the control");
    assert!(ledger::retry_parked_at(root.path()).unwrap(), "pressed");
    pass(&mut stages);
    assert_eq!(
        stages.doctor.dock_relaunches(),
        relaunches + 2,
        "tried at once, and rolled back"
    );
    assert_eq!(tries(), 1, "counted afresh");
    pass(&mut stages);
    assert_eq!(
        stages.doctor.dock_relaunches(),
        relaunches + 2,
        "put off for its first pause again"
    );
}

#[test]
fn a_choice_runs_only_the_steps_it_changed() {
    let root = aterm_tempfile::tempdir().unwrap();
    let allowed = || {
        snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    };
    let run = |revision: u64| {
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert!(first_run_complete(root.path()).unwrap());
        host.calls
    };
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    assert!(allowed());

    let mut joined = Plan {
        vault_url: "http://team.local:7484/v1/team".into(),
        ..Plan::default()
    };
    let revision = system_state_join(root.path(), &joined);
    assert!(!finished_at(root.path()).unwrap(), "the vault's steps run");
    assert!(first_run_complete(root.path()).unwrap());
    assert!(allowed());
    assert_eq!(run(revision), [Stage::Secrets, Stage::Repositories]);
    assert!(allowed(), "the permission steps stay allowed");

    joined.selection.set(Setting::Spotlight, false);
    assert!(run(system_state_choice(root.path(), &joined)).is_empty());
    joined.selection.set(Setting::Spotlight, true);
    let revision = system_state_choice(root.path(), &joined);
    assert_eq!(run(revision), [Stage::Spotlight]);
    joined.selection.set(Setting::Secrets, false);
    assert!(run(system_state_choice(root.path(), &joined)).is_empty());
    joined.selection.set(Setting::Secrets, true);
    let revision = system_state_join(root.path(), &joined);
    assert_eq!(run(revision), [Stage::Secrets, Stage::Repositories]);

    let revision = system_state_apply(root.path(), &joined, false, || Ok(None));
    let calls = run(revision);
    assert!(calls.contains(&Stage::Workstation), "{calls:?}");
    assert!(calls.contains(&Stage::Accounts), "{calls:?}");
}

#[test]
fn a_workstation_stage_that_never_verifies_is_tried_again_less_often_across_worker_retries() {
    struct Stages {
        doctor: super::super::tests::NeverVerifies,
        rest: Fake,
    }
    impl InitialHost for Stages {
        fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.doctor.pass(root)
        }
        fn workstation(
            &mut self,
            root: &Path,
            _: Option<Selection>,
        ) -> Result<Option<Report>, String> {
            self.doctor.pass(root)
        }
        fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.rest.accounts(root)
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.rest.spotlight()
        }
        fn permissions(&mut self, plan: &Plan, access: u64) -> Result<Vec<Row>, String> {
            self.rest.permissions(plan, access)
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.rest.secrets(plan, replaces, password)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            self.rest.secrets_pace()
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            self.rest.configured_vault()
        }
        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            self.rest.current_vault(plan)
        }
        fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.rest.repositories(vault)
        }
        fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.rest.current_repositories(vault)
        }
        fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.rest.withheld(root)
        }
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Dock, Setting::Widgets, Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = Stages {
        doctor: super::super::tests::NeverVerifies::dock(),
        rest: Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        },
    };
    // Each pass a minute on; the pause before each next try of the Dock grows (10, 20 min).
    let retries = |stages: &mut Stages, count: usize| {
        for retry in 0..count {
            let report = execute_initial_for_revision(
                stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap()
            .0;
            assert!(!report.complete(), "retry {retry}");
            assert!(
                report
                    .rows
                    .iter()
                    .any(|row| row.status == super::super::DEFERRED
                        && row.detail.ends_with(super::super::LATER)),
                "retry {retry}: {}",
                report.to_text()
            );
        }
    };
    let first = execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap()
    .0;
    assert!(first.needs_attention());
    let once = (stages.doctor.changes(), stages.doctor.relaunches());
    assert_eq!(once.1, 1);
    retries(&mut stages, 9);
    assert_eq!(
        (stages.doctor.changes(), stages.doctor.relaunches()),
        once,
        "nothing between the tries"
    );
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    let twice = (stages.doctor.changes(), stages.doctor.relaunches());
    assert_eq!(twice.1, 2);
    assert!(twice.0 > once.0);
    retries(&mut stages, 19);
    assert_eq!((stages.doctor.changes(), stages.doctor.relaunches()), twice);
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(
        stages.doctor.relaunches(),
        3,
        "tried again by itself, later"
    );
    assert_eq!(
        stages
            .rest
            .calls
            .iter()
            .filter(|stage| **stage == Stage::Secrets)
            .count(),
        31,
        "the pending vault keeps retrying"
    );
    assert!(!finished_at(root.path()).unwrap());
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
        0,
        "a setting not made yet keeps its stage open"
    );
}

#[test]
fn a_rerun_retires_an_unreadable_held_record() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Dock, Setting::Widgets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    std::fs::write(root.path().join(HELD_RECORD), b"not json").unwrap();
    let mut doctor = super::super::tests::NeverVerifies::dock();
    assert!(
        doctor.pass(root.path()).is_err(),
        "an unreadable record holds the pass"
    );
    let rerun = restart_at(root.path(), &plan).unwrap();
    assert_ne!(rerun, revision);
    assert!(!root.path().join(HELD_RECORD).exists());
    let report = doctor
        .pass(root.path())
        .expect("the rerun's pass runs")
        .expect("its setup is pending");
    assert!(report.needs_attention(), "{}", report.to_text());
    assert_eq!(doctor.relaunches(), 1, "it tried the Dock once");
}

#[test]
fn failed_stages_remain_pending_and_cancel_or_restore_stops_every_stage() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[
        Setting::Permissions,
        Setting::Secrets,
        Setting::Repositories,
    ]);
    authorize_at(root.path(), &plan).unwrap();
    let mut host = Fake {
        failure: Some(Stage::Secrets),
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    assert!(report.needs_attention());
    assert_eq!(host.calls, [Stage::Permissions, Stage::Secrets]);
    assert!(!finished_at(root.path()).unwrap());
    assert!(first_run_complete(root.path()).unwrap());
    write_legacy_cancel_at(root.path());
    assert!(finished_at(root.path()).unwrap());
    let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("initial-setup.json")).unwrap(),
        before
    );
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"restored"}),
    )
    .unwrap();
    assert!(finished_at(root.path()).unwrap());
}

/// A stage that fails as a whole is said on the page under the rows it covers (design §3.6):
/// a grant for Permissions, a setting the plan selects for the others, never a grant a stage
/// only needs (the sign-outs' Accessibility), and none of the stage's own words, which
/// `aterm doctor` keeps under the stage's own name.
#[test]
fn a_stage_that_fails_as_a_whole_is_paged_under_its_rows() {
    use aterm_types::macos_setup::Access;
    for (stage, settings, said) in [
        (
            Stage::Spotlight,
            vec![Setting::Spotlight],
            vec![Subject::Setting(Setting::Spotlight)],
        ),
        (
            Stage::Permissions,
            vec![Setting::Permissions],
            [
                Access::FullDiskAccess,
                Access::DeveloperTools,
                Access::Debugger,
            ]
            .map(Subject::Access)
            .to_vec(),
        ),
        (
            Stage::Workstation,
            vec![Setting::CapsLock, Setting::Dock],
            vec![
                Subject::Setting(Setting::CapsLock),
                Subject::Setting(Setting::Dock),
            ],
        ),
        (
            Stage::Accounts,
            vec![Setting::Messages, Setting::FaceTime],
            vec![
                Subject::Setting(Setting::Messages),
                Subject::Setting(Setting::FaceTime),
            ],
        ),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &selected(&settings)).unwrap();
        let mut host = Fake {
            failure: Some(stage),
            ..Fake::default()
        };
        let report = execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .unwrap();
        assert!(
            report.rows.iter().any(|row| row.name == stage.name()
                && row.status == "error"
                && row.detail == "offline; retry later"),
            "{stage:?}: `aterm doctor` keeps the stage's own words"
        );
        let page: Vec<(&str, &str, &str)> = report
            .page
            .as_ref()
            .expect("a pass has a page")
            .rows
            .iter()
            .filter(|row| row.status == "error")
            .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
            .collect();
        let want: Vec<(&str, &str, &str)> = said
            .iter()
            .map(|subject| (subject.name(), "error", ""))
            .collect();
        assert_eq!(page, want, "{stage:?}");
    }
}

/// The vault's stage says a failed try on the page as the vault's row, in the page's words
/// (design §3.6): what aterm does next, and never the subscriber, a path or the vault's own
/// words, which `aterm doctor` keeps.
#[test]
fn a_failed_vault_try_is_paged_in_the_pages_words() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let mut host = Fake {
        failure: Some(Stage::Secrets),
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    assert!(
        report
            .rows
            .iter()
            .any(|row| row.detail.starts_with("offline; retry later")),
        "`aterm doctor` keeps the stage's own words"
    );
    let page: Vec<(&str, &str, &str)> = report
        .page
        .as_ref()
        .expect("a pass has a page")
        .rows
        .iter()
        .filter(|row| row.status == "error")
        .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
        .collect();
    assert_eq!(
        page,
        [("Secrets vault", "error", secrets_retry::PAGE_RETRIES)]
    );
}

#[test]
fn an_explicit_profile_sets_up_permissions_unless_setup_was_cancelled() {
    // No setup record yet: the explicit plan is the profile and the permissions.
    let fresh = aterm_tempfile::tempdir().unwrap();
    save_explicit_workstation_plan(fresh.path(), Selection::all().workstation()).unwrap();
    let plan = saved_plan(fresh.path()).unwrap().unwrap();
    assert!(plan.selection.contains(Setting::Permissions));
    for (state, permissions) in [("cancelled", false), ("restored", true)] {
        // A legacy Cancel: the explicit pass does not re-open aterm's permissions. A
        // Restore never withdrew them (design §3.4), so the plan keeps them.
        let legacy = aterm_tempfile::tempdir().unwrap();
        write_json(
            &legacy.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state}),
        )
        .unwrap();
        save_explicit_workstation_plan(legacy.path(), Selection::all().workstation()).unwrap();
        let plan = saved_plan(legacy.path()).unwrap().unwrap();
        assert_eq!(
            plan.selection.contains(Setting::Permissions),
            permissions,
            "{state}"
        );
        assert_eq!(plan.selection.workstation(), Selection::all().workstation());
    }
    // A modern run an older aterm cancelled: Permissions comes off, even
    // where it was set up.
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    write_legacy_cancel_at(root.path());
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let plan = saved_plan(root.path()).unwrap().unwrap();
    assert!(!plan.selection.contains(Setting::Permissions));
    assert_eq!(plan.selection.workstation(), Selection::all().workstation());
}

#[test]
fn explicit_mac_apply_retains_modern_stages_and_legacy_completion_stays_honored() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = selected(&[Setting::Widgets, Setting::Secrets, Setting::Repositories]);
    plan.vault_url = "https://vault.example/v1/team".into();
    authorize_at(root.path(), &plan).unwrap();
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let revised = saved_plan(root.path()).unwrap().unwrap();
    assert_eq!(
        revised.selection.workstation(),
        Selection::all().workstation()
    );
    assert!(revised.selection.contains(Setting::Secrets));
    assert!(revised.selection.contains(Setting::Repositories));
    // aterm's own permissions stay as the person's plan had them: left as they are.
    assert!(!revised.selection.contains(Setting::Permissions));
    assert_eq!(revised.vault_url, plan.vault_url);

    let legacy = aterm_tempfile::tempdir().unwrap();
    for state in ["cancelled", "restored", "applied"] {
        write_json(
            &legacy.path().join("first-run.json"),
            &json!({"profile":PROFILE,"state":state,"accounts_verified":true}),
        )
        .unwrap();
        authorize_at(legacy.path(), &Plan::default()).unwrap();
        assert!(finished_at(legacy.path()).unwrap());
        assert!(!legacy.path().join("initial-setup.json").exists());
    }
}

#[test]
fn restore_stops_completed_workstation_reporting_without_undoing_other_stage_history() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert!(finished);
    assert!(rows.iter().all(|row| row.status == "applied"));
    let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();

    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"restored"}),
    )
    .unwrap();
    let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert!(finished);
    assert_eq!(rows.len(), Stage::ALL.len());
    for (stage, row) in Stage::ALL.iter().zip(rows) {
        assert_eq!(
            row.status,
            if *stage == Stage::Workstation {
                "stopped"
            } else {
                "applied"
            }
        );
    }
    assert_eq!(
        std::fs::read(root.path().join("initial-setup.json")).unwrap(),
        history
    );
}

fn restore_preflight_failure_stops_every_stage(record: &str) {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[
        Setting::Widgets,
        Setting::Permissions,
        Setting::Secrets,
        Setting::Repositories,
    ]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let history = std::fs::read(root.path().join("initial-setup.json")).unwrap();
    let invalid = root.path().join(record);
    std::fs::write(&invalid, b"invalid recovery record").unwrap();
    assert!(super::super::tests::restore_without_effects(root.path()).is_err());

    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.finished, "Restore did not stop the durable run");
    assert!(
        snapshot.permissions_allowed,
        "a Restore never withdraws aterm's permissions"
    );
    assert_eq!(snapshot.plan, plan);
    assert_eq!(snapshot.revision, revision);
    assert!(snapshot.rows.iter().all(|row| row.status == "stopped"));
    assert_eq!(
        std::fs::read(root.path().join("initial-setup.json")).unwrap(),
        history
    );
    assert_eq!(std::fs::read(&invalid).unwrap(), b"invalid recovery record");
    let mut automatic = Fake::default();
    assert!(
        execute_initial(&mut automatic, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(automatic.calls.is_empty());
    assert!(authorize_at(root.path(), &plan).unwrap().is_none());
}

#[test]
fn restore_stops_automatic_setup_before_reading_a_corrupt_backup() {
    restore_preflight_failure_stops_every_stage("original-preferences.json");
}

#[test]
fn restore_stops_automatic_setup_before_reading_corrupt_activation() {
    restore_preflight_failure_stops_every_stage("pending-refresh.json");
}

#[test]
fn restore_stop_write_failure_refuses_recovery_preflight_and_effects() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(
        root.path(),
        &selected(&[Setting::Widgets, Setting::Secrets]),
    )
    .unwrap();
    let backup = root.path().join("original-preferences.json");
    save_backup(
        &backup,
        &[Saved {
            target: Target::new("com.apple.dock", "show-recents"),
            value: Some(PValue::Bool(true)),
            granted: true,
        }],
    )
    .unwrap();
    let before = std::fs::read(&backup).unwrap();
    let decision = root.path().join("first-run.json");
    std::fs::remove_file(&decision).unwrap();
    std::fs::create_dir(&decision).unwrap();
    assert!(super::super::tests::restore_without_effects(root.path()).is_err());
    assert!(
        !root.path().join("pending-refresh.json").exists(),
        "a refused stop must precede recovery preparation"
    );
    assert_eq!(std::fs::read(&backup).unwrap(), before);
    let mut automatic = Fake::default();
    assert!(execute_initial(&mut automatic, root.path(), &mut None).is_err());
    assert!(automatic.calls.is_empty());
}

#[test]
fn an_older_cancel_keeps_applied_preferences_and_stops_only_unfinished_stages() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let mut host = Fake {
        pending: Some(Stage::Secrets),
        ..Fake::default()
    };
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    write_legacy_cancel_at(root.path());
    let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert!(finished);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].status, "applied");
    assert_eq!(rows[1].status, "stopped");
}

#[test]
fn failed_explicit_preflight_cannot_accept_the_earlier_subset_completion() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let mut host = Fake {
        pending: Some(Stage::Secrets),
        ..Fake::default()
    };
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert!(first_run_complete(root.path()).unwrap());
    let revision = revision_at(root.path()).unwrap();

    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    host.calls.clear();
    host.failure = Some(Stage::Workstation);
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    assert!(report.needs_attention());
    // The person's plan left aterm's permissions as they are, and the explicit profile
    // keeps them so: their stage does not run.
    assert_eq!(
        host.calls,
        [Stage::Workstation, Stage::Secrets, Stage::Accounts]
    );
    assert!(!first_run_complete(root.path()).unwrap());
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
        0
    );
    assert_eq!(revision_at(root.path()).unwrap(), revision);

    host.calls.clear();
    host.failure = None;
    host.pending = None;
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn an_explicit_restart_reopens_stopped_stages_and_preserves_the_original_backup() {
    let root = aterm_tempfile::tempdir().unwrap();
    let original = root.path().join("original-preferences.json");
    std::fs::write(&original, b"the original preference snapshot").unwrap();
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"cancelled"}),
    )
    .unwrap();
    write_json(
        &root.path().join("automatic-attempt.json"),
        &json!({"profile":PROFILE,"accessibility_ready":false}),
    )
    .unwrap();
    let plan = selected(&[Setting::Spotlight, Setting::Secrets]);
    restart_at(root.path(), &plan).unwrap();
    assert_eq!(
        std::fs::read(&original).unwrap(),
        b"the original preference snapshot"
    );
    assert!(!root.path().join("automatic-attempt.json").exists());
    let (saved, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert_eq!(saved, plan);
    assert!(!finished);
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|row| row.status == "pending"));
    let mut host = Fake::default();
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    let (_, finished, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert!(finished);
    assert!(rows.iter().all(|row| row.status == "applied"));
    assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
}

fn configure_vault(paths: &aterm_secrets::client::Paths, url: &str) -> ConfiguredVault {
    use std::os::unix::fs::PermissionsExt;
    let name = url.rsplit('/').next().unwrap();
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let source = paths.dir.join("source.toml");
    std::fs::write(
        &source,
        format!(
            "url = \"{url}\"\nname = \"{name}\"\nsigner = \"{}\"\nhost = \"m99\"\n",
            aterm_secrets::b64::encode_padded(&[7; 32])
        ),
    )
    .unwrap();
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600)).unwrap();
    ConfiguredVault::read(paths).unwrap().expect("configured")
}

#[test]
fn an_unedited_apply_resumes_the_configured_vault_instead_of_the_saved_one() {
    use aterm_secrets::client::{Membership, Paths, Status};
    let root = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = Paths::for_home(home.path());
    let alab = aterm_types::macos_setup::DEFAULT_VAULT_URL;
    let team = "http://vault.example:7484/v1/team";
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(ConfiguredVault::read(&paths).unwrap(), None);
    let unconfigured = snapshot_in_vault(root.path(), Ok(None)).unwrap();
    assert_eq!(unconfigured.plan, Plan::default());

    let vault = configure_vault(&paths, team);
    let configured = Some(vault.clone());
    let snapshot = snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
    assert_eq!(snapshot.plan.vault_url, team);
    assert_eq!(snapshot.plan.selection, Plan::default().selection);
    assert_eq!(
        (
            snapshot.finished,
            snapshot.revision,
            format!("{:?}", snapshot.rows)
        ),
        (
            unconfigured.finished,
            unconfigured.revision,
            format!("{:?}", unconfigured.rows)
        ),
        "the rows read the saved plan's progress"
    );
    assert!(snapshot.rows.iter().all(|row| row.status == "applied"));
    let saved = || saved_plan_record(root.path()).unwrap();
    assert_eq!(
        offered_plan(saved(), Ok(configured.clone())).vault_url,
        team
    );
    assert_eq!(
        offered_plan(saved(), Err("unreadable source.toml".into())),
        Plan::default()
    );
    assert_eq!(
        offered_plan(None, Err("unreadable source.toml".into())),
        Plan::default(),
        "an unreadable vault configuration reads as none"
    );

    let revision = restart_at(root.path(), &snapshot.plan).unwrap();
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    let ran = host.vault_url.expect("the Secrets stage ran");
    assert_eq!(ran, team);
    let status = Status {
        config: aterm_secrets::client::config(&paths).unwrap(),
        machine: None,
        membership: Membership::In { seq: 3, heard: 3 },
        names: Vec::new(),
    };
    let reads = |url: &str| {
        aterm_machines::init_secrets::selected_vault(url, home.path(), status.config.as_ref())
            .unwrap()
            .is_some()
    };
    assert!(reads(&ran), "resumes {ran}");
    assert!(!reads(alab), "joins {alab}");

    let pinned = Plan {
        vault_url: format!("{team}#{}", aterm_secrets::b64::encode_padded(&[7; 32])),
        ..Plan::default()
    };
    assert_eq!(vault.present(pinned.clone()), pinned);
    let file = ConfiguredVault {
        config: aterm_secrets::client::Config {
            source: aterm_secrets::transport::Source::Path(home.path().join("team.vault")),
            mirrors: Vec::new(),
            name: "team".into(),
            signer: None,
            followed: Vec::new(),
            host: "m99".into(),
        },
        home: home.path().to_path_buf(),
    };
    let typed = Plan {
        vault_url: "~/team.vault".into(),
        ..Plan::default()
    };
    assert_eq!(file.present(typed.clone()), typed);
    assert_eq!(
        file.present(Plan::default()).vault_url,
        home.path().join("team.vault").to_string_lossy()
    );
}

#[test]
fn a_vault_typed_in_system_state_stays_while_setup_still_joins_it() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let team = "http://vault.example:7484/v1/team";
    let other = "http://other.example:7484/v1/other";
    let configured = Some(configure_vault(&paths, team));
    let shown = || snapshot_in_vault(root.path(), Ok(configured.clone())).unwrap();
    let apply = |plan: &Plan| {
        let typed = plan.vault_url != shown().plan.vault_url;
        system_state_apply(root.path(), plan, typed, || Ok(configured.clone()))
    };
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert_eq!(shown().plan.vault_url, team);

    let typed = Plan {
        vault_url: other.into(),
        ..Plan::default()
    };
    let failing = || Fake {
        failure: Some(Stage::Secrets),
        ..Fake::default()
    };
    let revision = apply(&typed);
    assert_eq!(shown().plan, typed);
    for pass in 0..2 {
        let mut host = failing();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.vault_url.as_deref(), Some(other), "pass {pass}");
        let snapshot = shown();
        assert!(!snapshot.finished);
        assert_eq!(snapshot.plan, typed, "pass {pass}");
        assert!(
            snapshot
                .rows
                .iter()
                .any(|row| row.name == Stage::Secrets.name() && row.status == "pending"),
            "{:?}",
            snapshot.rows
        );
    }
    let revision = apply(&shown().plan);
    let mut host = failing();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.vault_url.as_deref(), Some(other));
    assert_eq!(shown().plan, typed);

    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"cancelled"}),
    )
    .unwrap();
    assert_eq!(shown().plan.vault_url, team);
}

#[test]
fn an_unchecked_vault_source_cannot_block_selected_preferences() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = selected(&[Setting::Widgets]);
    plan.vault_url = "unfinished source".into();
    authorize_at(root.path(), &plan).unwrap();
    let mut host = Fake::default();
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Workstation]);
    assert!(finished_at(root.path()).unwrap());
    let before = [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ]
    .map(|name| std::fs::read(root.path().join(name)).unwrap());
    plan.selection.set(Setting::Secrets, true);
    assert!(restart_at(root.path(), &plan).is_err());
    let after = [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ]
    .map(|name| std::fs::read(root.path().join(name)).unwrap());
    assert_eq!(
        before, after,
        "a selected invalid vault cannot authorize changes"
    );
}

#[test]
fn an_invalid_vault_source_changes_no_authorization_and_its_error_contains_no_value() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan {
        vault_url: "https://user:fixture-private@vault.example/v1/team".into(),
        ..Plan::default()
    };
    assert!(validate_initial_setup_plan(&plan).is_err());
    let error = authorize_at(root.path(), &plan).unwrap_err();
    assert!(!error.contains("fixture-private"));
    assert!(!root.path().join("automatic-plan.json").exists());
    assert!(!root.path().join("initial-setup.json").exists());
}

#[test]
fn a_plan_published_before_a_failed_progress_reset_requires_authorization() {
    let root = aterm_tempfile::tempdir().unwrap();
    let old = selected(&[Setting::Widgets, Setting::Spotlight]);
    authorize_at(root.path(), &old).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    let new = selected(&[Setting::Dock, Setting::Secrets]);
    save_plan(root.path(), &new).unwrap();
    assert!(!finished_at(root.path()).unwrap());
    let (_, _, rows) = status_at(root.path(), Plan::default()).unwrap();
    assert!(rows.iter().all(|row| row.status == "pending"));
    let mut host = Fake::default();
    assert!(
        execute_initial(&mut host, root.path(), &mut None)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(!finished_at(root.path()).unwrap());
    assert!(authorize_at(root.path(), &new).unwrap().is_some());
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn identical_plan_restarts_retire_older_workers_but_stage_updates_keep_the_revision() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), 0);
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let first = revision_at(root.path()).unwrap();
    assert_eq!(first, 1);
    let mut host = Fake {
        pending: Some(Stage::Secrets),
        ..Fake::default()
    };
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), first);
    authorize_at(root.path(), &plan).unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), first + 1);
    restart_at(root.path(), &plan).unwrap();
    let rerun = revision_at(root.path()).unwrap();
    assert_eq!(rerun, first + 2);
    host.pending = None;
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(revision_at(root.path()).unwrap(), rerun);
    restart_at(root.path(), &plan).unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), rerun + 1);
    assert!(!finished_at(root.path()).unwrap());
}

#[test]
fn revision_reads_refuse_interrupted_plan_resets_and_counters_never_wrap() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap();
    let next = selected(&[Setting::Repositories]);
    save_plan(root.path(), &next).unwrap();
    assert!(revision_at(root.path()).is_err());
    authorize_at(root.path(), &next).unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), 2);
    let mut latest = progress(root.path()).unwrap().unwrap();
    latest.revision = u64::MAX;
    save_progress(root.path(), latest).unwrap();
    let before = std::fs::read(root.path().join("initial-setup.json")).unwrap();
    assert!(restart_at(root.path(), &plan).is_err());
    assert_eq!(
        std::fs::read(root.path().join("initial-setup.json")).unwrap(),
        before
    );
    assert_eq!(saved_plan(root.path()).unwrap(), Some(next));
    assert_eq!(revision_at(root.path()).unwrap(), u64::MAX);
}

#[test]
fn a_stop_between_stages_ends_the_revision_and_a_new_plan_runs_normally() {
    use std::sync::Mutex;
    static ENDED: Mutex<Vec<Vec<Setting>>> = Mutex::new(Vec::new());
    fn stage(event: StageEvent) {
        if !event.running {
            ENDED.lock().unwrap().push(event.settings.to_vec());
        }
    }
    // Stop once the first stage has ended.
    fn stop(_: u64) -> bool {
        !ENDED.lock().unwrap().is_empty()
    }
    let hooks = SetupHooks {
        stop,
        stage,
        ..SetupHooks::NONE
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake::default();
    let (report, next) =
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks)
            .unwrap()
            .unwrap();
    assert_eq!(host.calls, [Stage::Workstation]);
    assert_eq!(next, SetupNext::Done);
    assert!(report.rows.iter().any(|row| row.status == "stopped"));
    let stopped = progress(root.path()).unwrap().unwrap();
    assert!(stopped.stopped && stopped.admitted && !stopped.completed);
    assert_eq!(stopped.done, Stage::Workstation.bit());
    // The revision ended without cancelling the plan: nothing more runs for it.
    assert!(finished_at(root.path()).unwrap());
    assert!(admission_at(root.path()).unwrap().unwrap().completed);
    assert_eq!(saved_plan(root.path()).unwrap(), Some(plan.clone()));
    assert!(
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks)
            .unwrap()
            .is_none()
    );

    // A later admission is a new revision, and it runs normally.
    let next_revision = restart_at(root.path(), &plan).unwrap();
    assert_eq!(next_revision, revision + 1);
    assert!(!finished_at(root.path()).unwrap());
    host.calls.clear();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        next_revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
    assert!(finished_at(root.path()).unwrap());
    assert!(!progress(root.path()).unwrap().unwrap().stopped);
}

#[test]
fn a_row_waiting_on_a_person_parks_the_run_instead_of_retrying_it() {
    let row = |status: &str| Row {
        name: "row".into(),
        status: status.into(),
        detail: String::new(),
        elapsed_ms: None,
    };
    assert_eq!(SetupNext::of(&[row("ok"), row("applied")]), SetupNext::Done);
    assert_eq!(SetupNext::of(&[row("ok"), row(WAITING)]), SetupNext::Park);
    assert_eq!(SetupNext::of(&[row("ok"), row(UNREACHED)]), SetupNext::Park);
    assert_eq!(SetupNext::of(&[row("ok"), row(ABSENT)]), SetupNext::Park);
    assert_eq!(
        SetupNext::of(&[row(WAITING), row("blocked")]),
        SetupNext::Retry
    );
    assert_eq!(SetupNext::of(&[]), SetupNext::Retry);
    assert_eq!(SetupNext::Done.and(SetupNext::Park), SetupNext::Park);
    assert_eq!(SetupNext::Park.and(SetupNext::Retry), SetupNext::Retry);

    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions, Setting::Widgets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake {
        waiting: Some(Stage::Permissions),
        ..Fake::default()
    };
    let (_, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Park);
    let left = progress(root.path()).unwrap().unwrap();
    assert_eq!(left.done, Stage::Workstation.bit());
    assert!(!left.completed);
    // Once the person acts, the next pass finishes only the parked stage.
    host.calls.clear();
    host.waiting = None;
    let (_, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Done);
    assert_eq!(host.calls, [Stage::Permissions]);
    assert!(finished_at(root.path()).unwrap());
}

/// A grant its stage leaves to someone else ([`super::super::OFFERED`]) or could not read
/// ([`super::super::UNREAD`]) waits for nobody: the stage completes on it, and a run whose
/// Permissions stage says nothing else finishes, never parking or retrying, though neither
/// grant is in place. NEGATIVE CONTROL: beside a row waiting on a person, the stage parks.
#[test]
fn an_offered_or_unread_grant_completes_its_stage() {
    let row = |status: &str| Row {
        name: "row".into(),
        status: status.into(),
        detail: String::new(),
        elapsed_ms: None,
    };
    for status in [super::super::OFFERED, super::super::UNREAD] {
        assert_eq!(
            SetupNext::of(&[row("ok"), row(status)]),
            SetupNext::Done,
            "{status}"
        );
        assert_eq!(
            SetupNext::of(&[row(status), row(WAITING)]),
            SetupNext::Park,
            "CONTROL: {status}"
        );
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Permissions, Setting::Widgets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake {
        permissions_rows: Some(vec![row(super::super::OFFERED), row(super::super::UNREAD)]),
        ..Fake::default()
    };
    let (_, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Done);
    assert!(host.calls.contains(&Stage::Permissions), "{:?}", host.calls);
    let done = progress(root.path()).unwrap().unwrap();
    assert_ne!(done.done & Stage::Permissions.bit(), 0);
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn a_record_from_before_the_sign_out_stage_counts_its_finished_workstation_as_signed_out() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan::default();
    authorize_at(root.path(), &plan).unwrap();
    let mut done = progress(root.path()).unwrap().unwrap();
    done.done = Stage::Workstation.bit();
    save_progress(root.path(), done).unwrap();
    let path = root.path().join("initial-setup.json");
    let mut record: Value = aterm_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(record["stages"].as_u64(), Some(6));
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done,
        Stage::Workstation.bit()
    );
    record.as_object_mut().unwrap().remove("stages");
    write_json(&path, &record).unwrap();
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done,
        Stage::Workstation.bit() | Stage::Accounts.bit()
    );
}

#[test]
fn a_change_to_another_group_keeps_this_approvals_access_steps() {
    let root = aterm_tempfile::tempdir().unwrap();
    let record = || -> Value {
        aterm_json::from_slice(&std::fs::read(root.path().join("initial-setup.json")).unwrap())
            .unwrap()
    };
    let first = selected(&[Setting::Permissions, Setting::Widgets]);
    let approved = authorize_at(root.path(), &first).unwrap().unwrap();
    assert_eq!(access_revision_at(root.path()).unwrap(), approved);
    assert!(record().get("access_revision").is_none());

    // Another group changes at once: a new revision, the same approval of the
    // grants, so nothing is handed to macOS again (§3.3, §6).
    let wider = selected(&[Setting::Permissions, Setting::Widgets, Setting::Dock]);
    let revision = restart_at(root.path(), &wider).unwrap();
    assert!(revision > approved);
    assert_eq!(access_revision_at(root.path()).unwrap(), approved);
    assert_eq!(record()["access_revision"].as_u64(), Some(approved));
    let mut host = Fake::default();
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.access_read, Some(approved));
    assert_eq!(
        admission_at(root.path()).unwrap().unwrap().revision,
        revision
    );

    // Permissions off and on again is a new approval: this revision's own.
    restart_at(root.path(), &selected(&[Setting::Widgets])).unwrap();
    let again = restart_at(root.path(), &first).unwrap();
    assert_eq!(access_revision_at(root.path()).unwrap(), again);
    assert!(record().get("access_revision").is_none());

    // A record never names an access revision after its own.
    let mut bad = record();
    bad.as_object_mut()
        .unwrap()
        .insert("access_revision".into(), Value::from(again + 1));
    write_json(&root.path().join("initial-setup.json"), &bad).unwrap();
    assert!(progress(root.path()).is_err());
}

/// The sign-outs need no System State in front and no person at the page: a pass with the
/// bare hooks ([`SetupHooks::NONE`]) runs them, each reading its own session right before
/// its clicks.
#[test]
fn admitted_account_setup_does_not_wait_for_system_state_on_screen() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Messages]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake::default();
    let (_, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(host.calls, [Stage::Workstation, Stage::Accounts]);
    assert_eq!(next, SetupNext::Done);
    assert!(finished_at(root.path()).unwrap());
}

/// A pass with nobody at the page runs every stage of its plan at once, the sign-outs
/// included: no stage waits for input or for System State in front (init-on-by-default design
/// §6.1); a sign-out that finds its session locked or in use stands aside by itself
/// ([`aterm_machines::port::System::session_reach`]).
#[test]
fn an_admitted_mac_runs_every_possible_stage_without_an_input_requirement() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &Plan::default())
        .unwrap()
        .unwrap();
    let mut host = Fake::default();
    let nobody = SetupHooks::NONE;
    let (_, next) =
        execute_initial_for_revision(&mut host, root.path(), revision, &mut None, nobody)
            .unwrap()
            .unwrap();
    for stage in [
        Stage::Workstation,
        Stage::Spotlight,
        Stage::Secrets,
        Stage::Repositories,
    ] {
        assert!(
            host.calls.contains(&stage),
            "{stage:?} ran: {:?}",
            host.calls
        );
    }
    assert_eq!(next, SetupNext::Done);
    let done = progress(root.path()).unwrap().unwrap().done;
    assert_ne!(done & Stage::Accounts.bit(), 0, "the sign-outs complete");
    for stage in [
        Stage::Workstation,
        Stage::Spotlight,
        Stage::Secrets,
        Stage::Repositories,
    ] {
        assert_ne!(done & stage.bit(), 0, "{stage:?} done");
    }
}

#[test]
fn a_legacy_run_that_waits_on_a_person_parks_like_a_modern_one() {
    let root = aterm_tempfile::tempdir().unwrap();
    save_plan(root.path(), &Plan::default()).unwrap();
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"pending"}),
    )
    .unwrap();
    assert!(progress(root.path()).unwrap().is_none());
    let mut host = Fake {
        waiting: Some(Stage::Workstation),
        ..Fake::default()
    };
    let (report, next) = execute_pass(&mut host, root.path(), &mut None, SetupHooks::NONE)
        .unwrap()
        .unwrap();
    assert!(report.needs_attention());
    assert_eq!(next, SetupNext::Park);
    let mut host = Fake {
        failure: Some(Stage::Workstation),
        ..Fake::default()
    };
    assert!(execute_pass(&mut host, root.path(), &mut None, SetupHooks::NONE).is_err());
}

#[test]
fn the_same_plan_again_stays_provisional_and_a_new_one_is_the_persons() {
    let root = aterm_tempfile::tempdir().unwrap();
    let automatic = selected(&[Setting::Permissions]);
    save_plan_as(root.path(), &automatic, true).unwrap();
    restart_at(root.path(), &automatic).unwrap();
    assert!(saved_plan_provisional(root.path()).unwrap());
    let shaped = selected(&[Setting::Permissions, Setting::Widgets]);
    restart_at(root.path(), &shaped).unwrap();
    assert!(!saved_plan_provisional(root.path()).unwrap());
    // Shaping it back to what aterm chose is still the person's plan.
    restart_at(root.path(), &automatic).unwrap();
    assert!(!saved_plan_provisional(root.path()).unwrap());
}

#[test]
fn a_group_choice_back_to_the_provisional_plan_is_the_persons_and_set_up_now_is_not() {
    let root = aterm_tempfile::tempdir().unwrap();
    let automatic = selected(&[Setting::Permissions]);
    save_plan_as(root.path(), &automatic, true).unwrap();
    // Keyboard & Trackpad to Set up and back, while busy: the first choice
    // was overtaken unsaved, and the second admits aterm's own plan again.
    let mut choice = InitialSetupRestart::chosen();
    let admitted = restart_attempt(
        &mut choice,
        root.path(),
        &automatic,
        save_progress,
        observe_restart,
    );
    assert!(admitted.result.unwrap().is_some());
    let records = Records::read(root.path()).unwrap();
    assert_eq!(records.plan, Some((automatic.clone(), true)));
    assert!(
        !records.provisional,
        "a later ALab signal must not widen groups the person left as is"
    );
    assert_eq!(scope::upgrade_of(&records, || true), None);
    assert!(holds(&records).off.contains(Setting::Repositories));
    assert_eq!(plan_in_scope(root.path(), || true), automatic.selection);

    // Set up now runs the plan as it stands: a provisional one stays so.
    save_plan_as(root.path(), &automatic, true).unwrap();
    let mut again = InitialSetupRestart::default();
    let admitted = restart_attempt(
        &mut again,
        root.path(),
        &automatic,
        save_progress,
        observe_restart,
    );
    assert!(admitted.result.unwrap().is_some());
    assert!(saved_plan_provisional(root.path()).unwrap());
}

#[test]
fn a_stop_asked_for_while_a_stage_runs_is_never_resumed_after_a_quit() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Repositories, Setting::Messages]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake {
        stop_during: Some((Stage::Repositories, root.path().to_owned())),
        ..Fake::default()
    };
    let (report, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    // Had the process ended inside Repositories, the next launch would
    // read the run as over: the window's startup finishes instead of
    // resuming it, and the sign-outs never run.
    let relaunch = host.relaunch.take().expect("the Stop came mid-stage");
    assert!(relaunch.finished, "{relaunch:?}");
    assert!(relaunch.snapshot_finished, "{relaunch:?}");
    let admission = relaunch.admission.expect("the run stays admitted");
    assert_eq!(admission.revision, revision);
    assert!(admission.completed, "a stopped run has nothing to resume");
    // This process lived on: the pass stopped at the next boundary and
    // folded the request into the run's own record.
    assert_eq!(host.calls, [Stage::Workstation, Stage::Repositories]);
    assert_eq!(next, SetupNext::Done);
    assert!(report.rows.iter().any(|row| row.status == "stopped"));
    let stopped = progress(root.path()).unwrap().unwrap();
    assert!(stopped.stopped && !stopped.completed);
    assert_eq!(stop_request_at(root.path()), None);
    assert!(finished_at(root.path()).unwrap());
    assert!(admission_at(root.path()).unwrap().unwrap().completed);
}

#[test]
fn a_stop_request_is_folded_by_the_next_lock_holder_and_an_older_ones_is_spent() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Secrets]);
    let first = authorize_at(root.path(), &plan).unwrap().unwrap();
    // The Stop of a run whose process ended before any boundary: the next
    // pass folds it before it runs anything.
    request_stop_at(root.path(), first).unwrap();
    let mut host = Fake::default();
    assert!(
        execute_initial_for_revision(&mut host, root.path(), first, &mut None, SetupHooks::NONE)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(progress(root.path()).unwrap().unwrap().stopped);
    assert_eq!(stop_request_at(root.path()), None);

    // A request for a revision a person's later choice replaced is ignored
    // by every reader, and the next pass removes it and runs normally.
    let second = restart_at(root.path(), &plan).unwrap();
    request_stop_at(root.path(), first).unwrap();
    assert!(!finished_at(root.path()).unwrap());
    assert!(!admission_at(root.path()).unwrap().unwrap().completed);
    assert!(!status_at(root.path(), Plan::default()).unwrap().1);
    execute_initial_for_revision(&mut host, root.path(), second, &mut None, SetupHooks::NONE)
        .unwrap();
    assert_eq!(host.calls, [Stage::Workstation, Stage::Secrets]);
    assert_eq!(stop_request_at(root.path()), None);
    assert!(!progress(root.path()).unwrap().unwrap().stopped);

    // A restart folds a Stop of the run it replaces, then runs its own.
    let third = restart_at(root.path(), &plan).unwrap();
    request_stop_at(root.path(), third).unwrap();
    let fourth = restart_at(root.path(), &plan).unwrap();
    assert_eq!(fourth, third + 1);
    assert_eq!(stop_request_at(root.path()), None);
    assert!(!finished_at(root.path()).unwrap());
}

#[test]
fn a_late_old_stop_never_replaces_the_current_runs_stop() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    let first = authorize_at(root.path(), &plan).unwrap().unwrap();
    let second = restart_at(root.path(), &plan).unwrap();
    request_stop_at(root.path(), first).unwrap();
    assert!(
        !finished_at(root.path()).unwrap(),
        "an old Stop alone cannot stop a newer run"
    );
    request_stop_at(root.path(), second).unwrap();
    assert!(finished_at(root.path()).unwrap());
    request_stop_at(root.path(), first).unwrap();
    assert!(
        finished_at(root.path()).unwrap(),
        "a delayed old process must not erase the current Stop"
    );
    assert_eq!(
        stop_request_at(root.path()),
        Some(StopRequest::Revision(second))
    );
    let mut host = Fake::default();
    assert!(
        execute_initial_for_revision(&mut host, root.path(), second, &mut None, SetupHooks::NONE)
            .unwrap()
            .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(progress(root.path()).unwrap().unwrap().stopped);
}

#[test]
fn settling_an_old_stop_never_unlinks_a_new_request_published_meanwhile() {
    use std::sync::mpsc;
    use std::time::Duration;
    let root = aterm_tempfile::tempdir().unwrap();
    request_stop_at(root.path(), 1).unwrap();
    let path = root.path().to_owned();
    let (go, begin) = mpsc::sync_channel(0);
    let (entered, entering) = mpsc::sync_channel(1);
    let publisher = std::thread::spawn(move || {
        begin
            .recv_timeout(Duration::from_secs(60))
            .expect("settler reached its removal boundary");
        let lock = HeldLock::try_take_named(&path, STOP_REQUEST_LOCK).unwrap();
        if lock.is_some() {
            // No settler excludes this writer: publish in the open gap.
            drop(lock);
            request_stop_at(&path, 2).unwrap();
            entered.send(()).unwrap();
        } else {
            // The settler excludes publication until its compare/unlink
            // finishes. The actual writer then waits for that short lock.
            entered.send(()).unwrap();
            request_stop_at(&path, 2).unwrap();
        }
    });
    settle_stop_request_with(root.path(), StopRequest::Revision(1), || {
        go.send(()).unwrap();
        entering
            .recv_timeout(Duration::from_secs(60))
            .expect("publisher reached the Stop protocol");
        Ok(())
    })
    .unwrap();
    publisher.join().expect("new Stop published");
    assert_eq!(
        stop_request_at(root.path()),
        Some(StopRequest::Revision(2)),
        "settling the old request must leave the newer publication intact"
    );
}

#[test]
fn a_retained_stop_is_confirmed_durable_under_the_short_lock() {
    let root = aterm_tempfile::tempdir().unwrap();
    request_stop_at(root.path(), 9).unwrap();
    let original = std::fs::read(root.path().join(STOP_REQUEST)).unwrap();
    for revision in [8, 9] {
        let mut flushed = false;
        let result = request_stop_with_flush(root.path(), revision, |_, _| {
            assert!(
                HeldLock::try_take_named(root.path(), STOP_REQUEST_LOCK)
                    .unwrap()
                    .is_none(),
                "retained publication stays locked through durability confirmation"
            );
            flushed = true;
            Err(std::io::Error::other(
                "injected Stop directory flush failure",
            ))
        });
        assert!(
            result.is_err(),
            "a visible request alone is not a saved Stop"
        );
        assert!(flushed);
        assert_eq!(
            std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
            original
        );
    }
    request_stop_at(root.path(), 8).unwrap();
    assert_eq!(
        std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
        original
    );
}

#[test]
fn an_unreadable_stop_is_never_replaced_or_reported_saved() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let invalid = b"unreadable Stop request";
    aterm_machines::port::write_atomic(&root.path().join(STOP_REQUEST), invalid, 0o600).unwrap();
    assert!(request_stop_at(root.path(), revision).is_err());
    assert_eq!(
        std::fs::read(root.path().join(STOP_REQUEST)).unwrap(),
        invalid
    );
    assert!(
        finished_at(root.path()).unwrap(),
        "an unreadable Stop never releases setup"
    );
    let mut host = Fake::default();
    assert!(
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE
        )
        .unwrap()
        .is_none()
    );
    assert!(host.calls.is_empty());
    assert!(progress(root.path()).unwrap().unwrap().stopped);
}

#[test]
fn saving_a_stop_never_waits_for_the_running_profile_pass() {
    use std::sync::mpsc;
    use std::time::Duration;
    let root = aterm_tempfile::tempdir().unwrap();
    let profile = HeldLock::take(root.path()).unwrap();
    let path = root.path().to_owned();
    let (saved, saving) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        saved.send(request_stop_at(&path, 7)).unwrap();
    });
    saving
        .recv_timeout(Duration::from_secs(60))
        .expect("Stop is saved while the profile lock remains held")
        .unwrap();
    assert!(HeldLock::try_take(root.path()).unwrap().is_none());
    assert_eq!(stop_request_at(root.path()), Some(StopRequest::Revision(7)));
    drop(profile);
    worker.join().unwrap();
}

#[test]
fn a_stop_during_the_last_stage_of_a_run_that_finishes_comes_too_late() {
    // Permissions is in the plan, so the snapshot's permission policy says
    // whether the run reads as stopped (a stopped run allows no access step).
    let plan = selected(&[Setting::Widgets, Setting::Permissions, Setting::Messages]);
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake {
        stop_during: Some((Stage::Accounts, root.path().to_owned())),
        ..Fake::default()
    };
    let (report, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    // The sign-outs were already running when Stop was pressed, and they
    // finished: the run is completed, not stopped, and the request is spent.
    assert_eq!(
        host.calls,
        [Stage::Workstation, Stage::Permissions, Stage::Accounts]
    );
    assert_eq!(next, SetupNext::Done);
    assert!(report.rows.iter().all(|row| row.status != "stopped"));
    let done = progress(root.path()).unwrap().unwrap();
    assert!(done.completed && !done.stopped);
    assert_eq!(stop_request_at(root.path()), None);
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.finished);
    assert!(
        snapshot.permissions_allowed,
        "a completed run is not stopped"
    );
    // A Stop that lands after the last stage, before the worker lets the
    // run go, is as late: every reader still reads the run as completed.
    request_stop_at(root.path(), revision).unwrap();
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.finished && snapshot.permissions_allowed);
    assert!(admission_at(root.path()).unwrap().unwrap().completed);

    // A Stop during the last stage of a pass that leaves work (the stage
    // waits on a person) stops the run at the pass's end: no later pass of
    // it runs, and the request is folded in.
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake {
        stop_during: Some((Stage::Accounts, root.path().to_owned())),
        waiting: Some(Stage::Accounts),
        ..Fake::default()
    };
    let (report, next) = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap();
    assert_eq!(next, SetupNext::Done);
    assert!(report.rows.iter().any(|row| row.status == "stopped"));
    let stopped = progress(root.path()).unwrap().unwrap();
    assert!(stopped.stopped && !stopped.completed);
    assert_eq!(stop_request_at(root.path()), None);
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.finished && !snapshot.permissions_allowed);
}

mod vault {
    use super::*;
    use aterm_machines::init_secrets::SecretsOps;
    use aterm_secrets::client::{Config, JoinOutcome, Look, Membership, ProbeOutcome, Status};
    use aterm_secrets::service::State;
    use aterm_secrets::transport::Source;
    use std::sync::{Arc, Mutex};

    pub(super) const NO_ROUTE: &str =
        "fetching http://alab-secrets.local:7484/v1/alab: No route to host (os error 65)";

    pub(super) struct Lan {
        pub(super) answers: Mutex<bool>,
        pub(super) network: Mutex<String>,
        status: Mutex<Status>,
        subscribed: Mutex<bool>,
        pub(super) asked: Mutex<u32>,
        pub(super) refusal: Mutex<Option<String>>,
        pub(super) fleeting: Mutex<Option<PathBuf>>,
        install_error: Mutex<Option<String>>,
        pub(super) subscriber_error_once: Mutex<bool>,
        subscriber_error_after_install: Mutex<bool>,
        pub(super) status_error_once: Mutex<bool>,
        pub(super) routes_error_once: Mutex<bool>,
        log: Mutex<Vec<String>>,
    }

    impl Lan {
        pub(super) fn new(answers: bool, network: &str) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers),
                network: Mutex::new(network.into()),
                status: Mutex::new(Status {
                    config: None,
                    machine: None,
                    membership: Membership::Unconfigured,
                    names: Vec::new(),
                }),
                subscribed: Mutex::new(false),
                asked: Mutex::new(0),
                refusal: Mutex::new(None),
                fleeting: Mutex::new(None),
                install_error: Mutex::new(None),
                subscriber_error_once: Mutex::new(false),
                subscriber_error_after_install: Mutex::new(false),
                status_error_once: Mutex::new(false),
                routes_error_once: Mutex::new(false),
                log: Mutex::new(Vec::new()),
            })
        }

        pub(super) fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        pub(super) fn approve(&self, heard: u64) {
            let mut status = self.status.lock().unwrap();
            status.membership = Membership::In { seq: 7, heard };
            status.names = vec!["GH_TOKEN".into()];
        }

        pub(super) fn status_now(&self) -> Status {
            self.status.lock().unwrap().clone()
        }

        pub(super) fn set(&self, status: Status, subscribed: bool) {
            *self.status.lock().unwrap() = status;
            *self.subscribed.lock().unwrap() = subscribed;
        }

        pub(super) fn in_vault(&self, source: &str, heard: u64) {
            let mut status = member(source, Membership::In { seq: 7, heard });
            status.names = vec!["GH_TOKEN".into()];
            self.set(status, false);
        }
    }

    fn member(source: &str, membership: Membership) -> Status {
        let recipient = aterm_secrets::age::Identity::from_secret_bytes([31; 32]).recipient();
        let code = aterm_secrets::client::join_code(&recipient);
        let (source, pin) =
            aterm_secrets::transport::parse_source(source, Path::new("/test-home")).unwrap();
        Status {
            config: Some(Config {
                name: aterm_secrets::transport::vault_name(&source).unwrap(),
                source,
                mirrors: Vec::new(),
                signer: Some(pin.unwrap_or([3; 32])),
                followed: Vec::new(),
                host: "m22".into(),
            }),
            machine: Some((recipient, code)),
            membership,
            names: Vec::new(),
        }
    }

    pub(super) struct LanReads {
        pub(super) lan: Arc<Lan>,
        pub(super) vault: bool,
        pub(super) build: String,
        pub(super) now: u64,
    }

    impl secrets_retry::Reads for LanReads {
        fn build(&self) -> String {
            self.build.clone()
        }
        fn now(&self) -> u64 {
            self.now
        }
        fn routes(&self) -> Option<String> {
            if std::mem::take(&mut *self.lan.routes_error_once.lock().unwrap()) {
                return None;
            }
            Some(self.lan.network.lock().unwrap().clone())
        }
        fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
            self.vault
                .then(|| Arc::clone(&self.lan) as Arc<dyn SecretsOps>)
        }
    }

    struct MaterializationReads {
        lan: Arc<Lan>,
        paths: aterm_secrets::client::Paths,
    }

    impl MaterializationReads {
        fn new(paths: aterm_secrets::client::Paths) -> Self {
            Self {
                lan: Lan::new(true, "unchanged scratch interface"),
                paths,
            }
        }

        fn pace(&self) -> Pace {
            self.lan
                .set(aterm_secrets::client::status(&self.paths).unwrap(), true);
            secrets_retry::pace(self)
        }

        fn park(&self, root: &Path) -> InitialSetupParkMark {
            initial_setup_park_mark_at(Some(root), Some(&self.paths), 1_000, || None)
        }
    }

    impl secrets_retry::Reads for MaterializationReads {
        fn build(&self) -> String {
            "scratch-materialization-build".into()
        }
        fn now(&self) -> u64 {
            1_000
        }
        fn routes(&self) -> Option<String> {
            Some("unchanged scratch interface".into())
        }
        fn vault(&self) -> Option<Arc<dyn SecretsOps>> {
            Some(Arc::clone(&self.lan) as Arc<dyn SecretsOps>)
        }
        fn paths(&self) -> Option<aterm_secrets::client::Paths> {
            Some(self.paths.clone())
        }
    }

    fn hold_materialization_attempt(root: &Path, revision: u64, pace: &Pace) {
        for _ in 0..2 {
            let mut rows = vec![Row {
                name: Stage::Secrets.name().into(),
                status: "error".into(),
                detail: "injected failed write of the scratch vault".into(),
                elapsed_ms: None,
            }];
            secrets_retry::record(root, revision, pace, false, &mut rows).unwrap();
        }
        assert!(secrets_retry::paused(root, revision, pace).is_some());
    }

    fn without_heartbeat(mut status: Status) -> Status {
        if let Membership::In { heard, .. } = &mut status.membership {
            *heard = 0;
        }
        status
    }

    #[test]
    fn a_same_signed_version_materialization_change_releases_the_held_stage() {
        use aterm_secrets::client::{self, Paths};
        let scratch = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&scratch.path().join("home"));
        let source = scratch.path().join("scratch.vault");
        client::create(&paths, source.to_str().unwrap(), None, Some("scratch")).unwrap();
        let opened = client::admin_open(&paths).unwrap();
        let mut doc = opened.doc.clone();
        doc.set_entry(
            &aterm_secrets::text::Audience::Everyone,
            "gh_access_token",
            b"inert-signed-test-value",
        )
        .unwrap();
        client::admin_publish(&paths, &opened, &doc).unwrap();
        client::sync(&paths, None).unwrap();
        let tracked = paths.home.join(".secrets/gh_access_token");
        std::fs::remove_file(&tracked).unwrap();
        assert!(!client::materialization_ready(&paths).unwrap());
        let status = without_heartbeat(client::status(&paths).unwrap());
        assert!(matches!(status.membership, Membership::In { .. }));
        let root = scratch.path().join("doctor");
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let revision = authorize_at(&root, &plan).unwrap().unwrap();
        let reads = MaterializationReads::new(paths.clone());
        let before = reads.pace();
        hold_materialization_attempt(&root, revision, &before);
        let parked = reads.park(&root);
        let mut held = Fake {
            pace: Some(reads.pace()),
            ..Fake::default()
        };
        execute_initial_for_revision(&mut held, &root, revision, &mut None, SetupHooks::NONE)
            .unwrap();
        assert!(
            held.calls.is_empty(),
            "the unchanged failed try stays paused"
        );
        assert_eq!(parked, reads.park(&root));

        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.dir.join(".lock"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "a scratch writer excludes the observer; this File's Drop releases the lock"
        )]
        writer.lock().unwrap();
        let unknown = reads.pace();
        assert!(unknown.materialization.is_none(), "the writer is busy");
        assert!(secrets_retry::paused(&root, revision, &unknown).is_some());
        let mut contended = reads.park(&root);
        assert!(!contended.facts_changed_since(&parked));
        contended.retain_known_from(&parked);
        assert_eq!(parked, contended, "writer contention does not wake setup");
        drop(writer);
        assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_some());

        client::sync(&paths, None).unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        assert_eq!(status, without_heartbeat(client::status(&paths).unwrap()));
        assert_ne!(parked, reads.park(&root), "the real repair wakes setup");
        assert!(reads.park(&root).facts_changed_since(&parked));
        let mut repaired = Fake {
            pace: Some(reads.pace()),
            ..Fake::default()
        };
        let (_, next) = execute_initial_for_revision(
            &mut repaired,
            &root,
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            repaired.calls,
            [Stage::Secrets, Stage::Repositories],
            "a real same-version repair must release the old held outcome"
        );
        assert_eq!(next, SetupNext::Done);
        let mut completed = reads.park(&root);
        completed.retain_known_from(&parked);
        assert_ne!(completed, parked, "completion ends the old watch");
        assert!(completed.materialization.is_none());

        let stopped_root = scratch.path().join("stopped-doctor");
        let stopped = authorize_at(&stopped_root, &plan).unwrap().unwrap();
        let before_stop = reads.park(&stopped_root);
        request_stop_at(&stopped_root, stopped).unwrap();
        let mut after_stop = reads.park(&stopped_root);
        after_stop.retain_known_from(&before_stop);
        assert_ne!(after_stop, before_stop, "Stop ends the old watch");
        assert!(after_stop.materialization.is_none());
        let mut stopped_host = Fake {
            pace: Some(reads.pace()),
            ..Fake::default()
        };
        assert!(
            execute_initial_for_revision(
                &mut stopped_host,
                &stopped_root,
                stopped,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .is_none()
        );
        assert!(stopped_host.calls.is_empty(), "a repair never revives Stop");

        restart_at(&stopped_root, &plan).unwrap();
        let older_run = reads.park(&stopped_root);
        restart_at(&stopped_root, &plan).unwrap();
        let writer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(paths.dir.join(".lock"))
            .unwrap();
        #[expect(
            clippy::disallowed_methods,
            reason = "a scratch writer excludes the observer; this File's Drop releases the lock"
        )]
        writer.lock().unwrap();
        let mut newer_run = reads.park(&stopped_root);
        newer_run.retain_known_from(&older_run);
        assert_ne!(newer_run, older_run, "a new run ends the old watch");
        assert!(newer_run.materialization.is_none());
        drop(writer);
    }

    #[test]
    fn a_plain_same_entry_materialization_change_wakes_and_releases_the_pause() {
        use aterm_secrets::client::{self, JoinOptions, Paths};
        let scratch = aterm_tempfile::tempdir().unwrap();
        let source = scratch.path().join("scratch.secrets");
        aterm_machines::port::write_atomic(&source, b"gh_access_token=inert-plain-first\n", 0o600)
            .unwrap();
        let paths = Paths::for_home(&scratch.path().join("home"));
        client::join(
            &paths,
            &JoinOptions {
                source: source.to_string_lossy().into_owned(),
                host: Some("scratch".into()),
                interactive: false,
                wait: std::time::Duration::ZERO,
                confirm: &|_| false,
                password: aterm_secrets::client::JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        let status = client::status(&paths).unwrap();
        let root = scratch.path().join("doctor");
        let plan = Plan {
            vault_url: source.to_string_lossy().into_owned(),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let revision = authorize_at(&root, &plan).unwrap().unwrap();
        let reads = MaterializationReads::new(paths.clone());
        let before = reads.pace();
        hold_materialization_attempt(&root, revision, &before);
        let parked = reads.park(&root);
        client::sync(&paths, None).unwrap();
        assert_eq!(parked, reads.park(&root), "an unchanged sync stays parked");
        assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_some());

        aterm_machines::port::write_atomic(&source, b"gh_access_token=inert-plain-second\n", 0o600)
            .unwrap();
        client::sync(&paths, None).unwrap();
        assert!(client::materialization_ready(&paths).unwrap());
        assert_eq!(status, client::status(&paths).unwrap());
        assert_ne!(
            parked,
            reads.park(&root),
            "new credential bytes under the same entry name must wake setup"
        );
        assert!(reads.park(&root).facts_changed_since(&parked));
        assert!(secrets_retry::paused(&root, revision, &reads.pace()).is_none());
    }

    enum SubscriberReadFailure {
        None,
        InThePause,
        AfterFirstInstall,
        AfterNetworkChange,
    }

    fn subscriber_recovery_after_install_failures(read_failure: SubscriberReadFailure) {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = selected(&[Setting::Secrets, Setting::Repositories]);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let lan = Lan::new(true, "unchanged network");
        lan.in_vault(&plan.vault_url, START);
        *lan.install_error.lock().unwrap() = Some("injected subscriber install refusal".into());
        *lan.subscriber_error_after_install.lock().unwrap() =
            matches!(read_failure, SubscriberReadFailure::AfterFirstInstall);
        let mut host = Vaulted::new(&lan);
        let mut password = Some(join_password());
        // Three retry passes a minute apart: the first two reach the failed change, through
        // the actual Secrets item; two failures in a row pause the next try two minutes.
        for minute in 0..3 {
            host.now = START + minute * 60;
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            if minute == 0 && matches!(read_failure, SubscriberReadFailure::AfterFirstInstall) {
                assert!(host.pace_reads[0].is_some(), "the first inputs were known");
                assert!(host.pace_reads[1].is_none(), "the post-Pace read failed");
                let attempt: Value = aterm_json::from_slice(
                    &std::fs::read(root.path().join(secrets_retry::RECORD)).unwrap(),
                )
                .unwrap();
                assert_eq!(attempt["marks"].as_str(), host.pace_reads[0].as_deref());
                assert_eq!(attempt["failures"].as_u64(), Some(1));
            }
            assert!(report.needs_attention());
            assert_eq!(next, SetupNext::Retry);
        }
        assert_eq!(host.minutes(), [0, 1]);
        assert!(
            host.rest.calls.is_empty(),
            "repositories still wait for Secrets"
        );
        assert_eq!(
            lan.log().iter().filter(|line| *line == "install").count(),
            2
        );
        let status = lan.status_now();
        let before_pace = host.secrets_pace().unwrap();
        let mark_with = |subscriber| {
            initial_setup_park_mark_reading(
                Some(root.path()),
                Some(lan.status_now()),
                START + 150,
                || Some(String::new()),
                || None,
                || subscriber,
            )
        };
        let mark = || mark_with(lan.subscriber().ok().map(secrets_retry::subscriber_running));
        let parked = mark();
        assert_eq!(parked.retry_due(), None, "the next try is not due yet");
        assert_eq!(
            parked.next_due(),
            Some(START + 180),
            "two failures pause it, never hold it"
        );
        assert_eq!(parked, mark(), "stable service state must stay parked");
        let mut unknown = mark_with(None);
        assert!(!unknown.facts_changed_since(&parked));
        unknown.retain_known_from(&parked);
        assert_eq!(unknown, parked, "an unreadable service is no new fact");

        if matches!(read_failure, SubscriberReadFailure::InThePause) {
            // Only the pre-Pace read fails. The actual item's probe and
            // post-Pace see the same absent subscriber as both failed tries.
            *lan.subscriber_error_once.lock().unwrap() = true;
            host.now = START + 150;
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
            assert!(report.needs_attention());
            assert_eq!(next, SetupNext::Retry);
            assert!(!*lan.subscriber_error_once.lock().unwrap());
            assert_eq!(lan.subscriber().unwrap(), State::Absent);
            assert_eq!(lan.status_now(), status);
            assert_eq!(host.secrets_pace().unwrap().marks, before_pace.marks);
            assert_eq!(parked, mark());
            assert!(host.rest.calls.is_empty());
        }

        let expected_installs = if matches!(read_failure, SubscriberReadFailure::AfterNetworkChange)
        {
            *lan.network.lock().unwrap() = "new local network".into();
            // Inside the pause the old network's tries left (due at minute 3).
            for minute in 2..5 {
                *lan.subscriber_error_once.lock().unwrap() = true;
                host.now = START + minute * 60;
                let (report, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut password,
                    SetupHooks::NONE,
                )
                .unwrap()
                .unwrap();
                assert!(report.needs_attention());
                assert_eq!(next, SetupNext::Retry);
                assert!(!*lan.subscriber_error_once.lock().unwrap());
                assert_eq!(lan.status_now(), status);
                assert!(host.rest.calls.is_empty());
            }
            assert_eq!(
                lan.log().iter().filter(|line| *line == "install").count(),
                4,
                "the known new network permits two tries even while subscriber reads fail"
            );
            let known = host.secrets_pace().unwrap();
            assert!(secrets_retry::paused(root.path(), revision, &known).is_some());
            host.now = START + 270;
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(host.minutes(), [0, 1, 2, 3]);
            4
        } else {
            2
        };

        // launchd, the terminal's agent-on or the existing mend can start
        // this same subscriber without changing any vault files/version.
        *lan.install_error.lock().unwrap() = None;
        lan.set(status.clone(), true);
        assert_eq!(lan.status_now(), status);
        let after_pace = host.secrets_pace().unwrap();
        assert_eq!(before_pace.materialization, after_pace.materialization);
        assert_ne!(before_pace.marks, after_pace.marks);
        let repaired = mark();
        assert!(repaired.facts_changed_since(&parked));
        let mut unknown = mark_with(None);
        unknown.retain_known_from(&repaired);
        assert_eq!(
            unknown, repaired,
            "running stays known through a read failure"
        );

        // Before the pause ends: the repair, not the timer, lets this pass through.
        host.now = secrets_retry::retry_ahead(root.path(), revision, host.now)
            .expect("the next try is still ahead")
            - 1;
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut password,
            SetupHooks::NONE,
        )
        .unwrap()
        .unwrap();
        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(next, SetupNext::Done);
        assert_eq!(host.rest.calls, [Stage::Repositories]);
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(revision_at(root.path()).unwrap(), revision);
        assert!(
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut password,
                SetupHooks::NONE,
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(
            host.rest.calls,
            [Stage::Repositories],
            "no second repository pass"
        );
        assert_ne!(
            parked, repaired,
            "the service repair must wake the parked worker"
        );

        let no_pending_reads = || {
            initial_setup_park_mark_reading(
                Some(root.path()),
                Some(lan.status_now()),
                START + 600,
                || panic!("no pending Secrets interface read"),
                || panic!("no pending materialization read"),
                || panic!("no pending subscriber read"),
            )
        };
        let completed = no_pending_reads();
        let mut kept = completed.clone();
        kept.retain_known_from(&repaired);
        assert_eq!(
            kept, completed,
            "completion retires the service observation"
        );

        let next_revision = restart_at(root.path(), &plan).unwrap();
        assert!(next_revision > revision);
        let next = mark_with(None);
        let mut kept = next.clone();
        kept.retain_known_from(&repaired);
        assert_eq!(
            kept, next,
            "a new run cannot borrow the older service state"
        );
        request_stop_at(root.path(), next_revision).unwrap();
        let stopped = no_pending_reads();
        let mut kept = stopped.clone();
        kept.retain_known_from(&repaired);
        assert_eq!(kept, stopped, "Stop retires the service observation");
        assert!(password.is_some(), "no join consumes the retained password");
        assert!(lan.log().iter().all(|line| !line.starts_with("join ")));
        assert_eq!(
            lan.log().iter().filter(|line| *line == "install").count(),
            expected_installs,
            "an unreadable subscriber is no new fact permitting another failed install"
        );
    }

    #[test]
    fn subscriber_recovery_wakes_paused_secrets_and_unblocks_repositories() {
        subscriber_recovery_after_install_failures(SubscriberReadFailure::None);
    }

    #[test]
    fn subscriber_read_failure_does_not_release_a_paused_install() {
        subscriber_recovery_after_install_failures(SubscriberReadFailure::InThePause);
    }

    #[test]
    fn subscriber_read_failure_after_the_first_install_preserves_its_failure_count() {
        subscriber_recovery_after_install_failures(SubscriberReadFailure::AfterFirstInstall);
    }

    #[test]
    fn subscriber_read_failure_does_not_hide_a_known_network_change() {
        subscriber_recovery_after_install_failures(SubscriberReadFailure::AfterNetworkChange);
    }

    impl SecretsOps for Lan {
        fn status(&self) -> aterm_secrets::Result<Status> {
            if std::mem::take(&mut *self.status_error_once.lock().unwrap()) {
                return Err(aterm_secrets::Error::new(
                    "injected temporary vault-state read failure",
                ));
            }
            Ok(self.status.lock().unwrap().clone())
        }
        fn configured(&self) -> aterm_secrets::Result<Option<Config>> {
            Ok(self.status.lock().unwrap().config.clone())
        }
        fn subscriber(&self) -> aterm_secrets::Result<State> {
            if std::mem::take(&mut *self.subscriber_error_once.lock().unwrap()) {
                return Err(aterm_secrets::Error::new(
                    "injected temporary subscriber-state read failure",
                ));
            }
            Ok(if *self.subscribed.lock().unwrap() {
                State::Installed {
                    pid: Some(4242),
                    unit: PathBuf::from("/test-home/Library/LaunchAgents/subscriber.plist"),
                }
            } else {
                State::Absent
            })
        }
        fn terminal(&self) -> bool {
            false
        }
        fn probe_alab(&self) -> aterm_secrets::Result<ProbeOutcome> {
            Err(aterm_secrets::Error::new(
                "the setup never looks for a vault",
            ))
        }
        fn reachable(&self, _source: &Source) -> Look {
            *self.asked.lock().unwrap() += 1;
            if *self.answers.lock().unwrap() {
                Look::Answers
            } else {
                Look::Unreached(NO_ROUTE.into())
            }
        }
        fn keep(&self, into: &Path, replaces: &Replaces) -> aterm_secrets::Result<()> {
            if let Replaces::Read(expected) = replaces
                && self.status.lock().unwrap().config != *expected
            {
                return Err(aterm_secrets::Error::new("the fixture's vault changed"));
            }
            self.log
                .lock()
                .unwrap()
                .push(format!("keep {}", into.display()));
            Ok(())
        }
        fn join(
            &self,
            _sys: &dyn aterm_machines::port::System,
            (source, replaces): (&str, &Replaces),
            _password: Option<&SecretBytes>,
            interactive: bool,
            _wait: std::time::Duration,
            requested: &dyn Fn() -> bool,
            current_authority: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> aterm_secrets::Result<JoinOutcome> {
            current_authority().map_err(aterm_secrets::Error::new)?;
            assert!(!interactive, "the setup's join is unattended");
            self.log.lock().unwrap().push(format!("join {source}"));
            if let Some(why) = self.refusal.lock().unwrap().clone() {
                return Err(aterm_secrets::Error::new(why));
            }
            let status = member(source, Membership::Pending);
            let code = status.machine.as_ref().map(|(_, code)| code.clone());
            {
                let mut current = self.status.lock().unwrap();
                if let Replaces::Read(expected) = replaces
                    && current.config != *expected
                {
                    return Err(aterm_secrets::Error::new("the fixture's vault changed"));
                }
                *current = status;
            }
            requested();
            Ok(JoinOutcome::Pending {
                code: code.expect("a join makes a key"),
                host: "m22".into(),
            })
        }
        fn install_subscriber(&self) -> aterm_secrets::Result<String> {
            self.log.lock().unwrap().push("install".into());
            if let Some(exe) = self.fleeting() {
                aterm_secrets::service::service_program(&exe, &|_| false)?;
            }
            if let Some(why) = self.install_error.lock().unwrap().as_ref() {
                if std::mem::take(&mut *self.subscriber_error_after_install.lock().unwrap()) {
                    *self.subscriber_error_once.lock().unwrap() = true;
                }
                return Err(aterm_secrets::Error::new(why));
            }
            *self.subscribed.lock().unwrap() = true;
            Ok("installed the subscriber".into())
        }
        fn turned_off(&self) -> bool {
            false
        }
        /// The fixture's "home" network is the ALab LAN; it cannot tell where any other is.
        /// NEGATIVE CONTROL: answering `Unknown` everywhere fails
        /// `an_approval_is_picked_up_at_the_next_pass_and_within_the_longest_pause`, whose
        /// waiting row then gives the place-unknown words instead of `LET_IN`.
        fn join_place(&self) -> aterm_secrets::alab_join::JoinPlace {
            if *self.network.lock().unwrap() == "home" {
                aterm_secrets::alab_join::JoinPlace::Home
            } else {
                aterm_secrets::alab_join::JoinPlace::Unknown
            }
        }
        fn started(&self) -> Result<(), String> {
            Ok(())
        }
        fn fleeting(&self) -> Option<PathBuf> {
            self.fleeting.lock().unwrap().clone()
        }
        fn synced(&self) -> aterm_secrets::Result<bool> {
            Ok(true)
        }
        fn sync(
            &self,
            current: &(dyn Fn() -> Result<(), String> + Sync),
        ) -> aterm_secrets::Result<()> {
            current().map_err(aterm_secrets::Error::new)?;
            self.log.lock().unwrap().push("sync".into());
            Ok(())
        }
        fn say(&self, _line: &str) {}
    }

    struct Lines(Arc<Lan>, bool);

    impl aterm_converge::Journal for Lines {
        fn before_change(
            &mut self,
            id: &str,
            _from: &str,
            _to: &str,
            revert: Option<&str>,
        ) -> Result<(), String> {
            if self.1 {
                return Err("No space left on device (os error 28)".into());
            }
            assert!(
                revert.is_some(),
                "every injected vault change is reversible"
            );
            if !*self.0.subscribed.lock().unwrap() {
                assert!(revert.is_some_and(|revert| revert.contains("aterm secrets agent off")));
            }
            self.0.log.lock().unwrap().push(format!("line {id}"));
            Ok(())
        }
    }

    pub(super) const START: u64 = 1_800_000_000;

    pub(super) struct Vaulted {
        pub(super) lan: Arc<Lan>,
        rest: Fake,
        pub(super) refuse: bool,
        pub(super) paced: bool,
        pub(super) marks_vault: bool,
        pub(super) build: String,
        pub(super) now: u64,
        tries: Vec<u64>,
        pace_reads: Vec<Option<String>>,
        pub(super) joins_after_read: Option<&'static str>,
    }

    impl Vaulted {
        pub(super) fn new(lan: &Arc<Lan>) -> Self {
            Self {
                lan: Arc::clone(lan),
                rest: Fake::default(),
                refuse: false,
                paced: true,
                marks_vault: true,
                build: "1.0.0+gtest".into(),
                now: START,
                tries: Vec::new(),
                pace_reads: Vec::new(),
                joins_after_read: None,
            }
        }

        pub(super) fn minutes(&self) -> Vec<u64> {
            self.tries.iter().map(|at| (at - START) / 60).collect()
        }

        pub(super) fn reads(&self, vault: bool) -> LanReads {
            LanReads {
                lan: Arc::clone(&self.lan),
                vault,
                build: self.build.clone(),
                now: self.now,
            }
        }
    }

    fn context() -> aterm_machines::items::Ctx {
        let user = aterm_machines::model::User {
            name: "tester".into(),
            uid: 501,
            gid: 20,
            home: "/test-home".into(),
            shell: "/bin/zsh".into(),
            admin: true,
        };
        aterm_machines::items::Ctx {
            facts: aterm_machines::model::HostFacts::default(),
            users: vec![user.clone()],
            console: Some(user),
            host_name: "m22".into(),
            cache_dir: PathBuf::from("/test-cache"),
            disrupt: false,
        }
    }

    impl InitialHost for Vaulted {
        fn legacy(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.rest.legacy(root)
        }
        fn workstation(
            &mut self,
            root: &Path,
            only: Option<Selection>,
        ) -> Result<Option<Report>, String> {
            self.rest.workstation(root, only)
        }
        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            self.rest.spotlight()
        }
        fn permissions(&mut self, plan: &Plan, revision: u64) -> Result<Vec<Row>, String> {
            self.rest.permissions(plan, revision)
        }
        fn accounts(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.rest.accounts(root)
        }
        fn secrets(
            &mut self,
            plan: &Plan,
            replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            self.tries.push(self.now);
            let ops: Arc<dyn SecretsOps> = self.lan.clone();
            aterm_machines::init::run_initial_secrets_with(
                &super::super::super::tests::Inert::default(),
                &context(),
                ops,
                &plan.vault_url,
                replaces,
                password,
                APPROVAL_WAIT,
                &mut Lines(Arc::clone(&self.lan), self.refuse),
            )
            .map(secrets_rows)
        }
        fn secrets_pace(&mut self) -> Option<Pace> {
            let pace = self
                .paced
                .then(|| secrets_retry::pace(&self.reads(self.marks_vault)));
            self.pace_reads
                .push(pace.as_ref().and_then(|pace| pace.marks.clone()));
            pace
        }
        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            let read = self.lan.status_now().config.map(|config| ConfiguredVault {
                config,
                home: PathBuf::from("/test-home"),
            });
            if let Some(source) = self.joins_after_read.take() {
                self.lan.in_vault(source, START);
            }
            Ok(read)
        }
        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            let subscribed = if *self.lan.subscribed.lock().unwrap() {
                check::Subscribed::Running
            } else {
                check::Subscribed::Down
            };
            check::vault_rows(
                plan,
                Path::new("/test-home"),
                &self.lan.status_now(),
                |_| Ok(true),
                || Ok(subscribed),
            )
        }
        fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.rest.repositories(vault)
        }
        fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.rest.current_repositories(vault)
        }
        fn withheld(&mut self, root: &Path) -> Result<Option<Report>, String> {
            self.rest.withheld(root)
        }
    }

    #[test]
    fn a_completed_vault_is_rechecked_before_pending_accounts_finish() {
        for (subscriber_stopped, person_stopped, during_accounts) in [
            (false, false, false),
            (true, true, false),
            (true, false, false),
            (true, false, true),
        ] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = selected(&[Setting::Secrets, Setting::Messages]);
            let lan = Lan::new(true, "home");
            lan.in_vault(&plan.vault_url, START);
            let joined = lan.status_now();
            lan.set(joined.clone(), true);
            let mut host = Vaulted::new(&lan);
            host.rest.waiting = Some(Stage::Accounts);
            let admitted = host.configured_vault().unwrap();
            let revision = authorize_reading(root.path(), &plan, || Ok(admitted))
                .unwrap()
                .unwrap();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the first pass leaves Accounts pending");
            let saved = progress(root.path()).unwrap().unwrap();
            assert_ne!(saved.done & Stage::Secrets.bit(), 0);
            assert_eq!(saved.done & Stage::Accounts.bit(), 0);
            assert!(!finished_at(root.path()).unwrap());
            assert!(lan.log().is_empty());

            if subscriber_stopped && !during_accounts {
                lan.set(joined.clone(), false);
            }
            if during_accounts {
                let lan = Arc::clone(&lan);
                let joined = joined.clone();
                host.rest.accounts_end = Some(Box::new(move |_| lan.set(joined, false)));
            }
            if person_stopped {
                request_stop_at(root.path(), revision).unwrap();
            }
            host.rest.waiting = None;
            host.rest.calls.clear();
            let resumed = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            if person_stopped {
                assert!(resumed.is_none());
                assert!(!*lan.subscribed.lock().unwrap());
                assert!(lan.log().is_empty(), "Stop authorizes no subscriber repair");
            } else if during_accounts {
                let (report, next) = resumed.unwrap();
                assert!(!rows_ready(&report.rows), "{:?}", report.rows);
                assert_ne!(next, SetupNext::Done);
                assert!(!*lan.subscribed.lock().unwrap());
                assert!(
                    lan.log().is_empty(),
                    "readback does not repair the subscriber"
                );
                let saved = progress(root.path()).unwrap().unwrap();
                assert_eq!(saved.done & Stage::Secrets.bit(), 0);
                assert_ne!(saved.done & Stage::Accounts.bit(), 0);
                assert!(!finished_at(root.path()).unwrap());
                assert_eq!(host.rest.calls, [Stage::Accounts]);

                host.rest.calls.clear();
                let (repaired, next) = execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .expect("the ordinary Secrets stage repairs its withdrawn subscriber");
                assert!(rows_ready(&repaired.rows), "{:?}", repaired.rows);
                assert_eq!(next, SetupNext::Done);
                assert!(
                    host.rest.calls.is_empty(),
                    "verified sign-outs do not repeat"
                );
                assert_eq!(
                    lan.log().iter().filter(|entry| *entry == "install").count(),
                    1
                );
                assert!(finished_at(root.path()).unwrap());
            } else {
                assert!(resumed.is_some());
                assert!(
                    *lan.subscribed.lock().unwrap(),
                    "setup completed with its previously verified subscriber down"
                );
                assert_eq!(
                    lan.log().iter().filter(|entry| *entry == "install").count(),
                    usize::from(subscriber_stopped),
                    "only the changed subscriber needs repair"
                );
                assert!(finished_at(root.path()).unwrap());
            }
            assert_eq!(lan.status_now(), joined);
        }
    }

    #[test]
    fn a_same_source_vault_joined_under_another_signer_allows_repositories() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = "https://team.example/v1/team";
        let plan = Plan {
            vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let lan = Lan::new(true, "office");
        lan.in_vault(source, START);
        let joined = lan.status_now();
        lan.set(joined.clone(), true);
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        let (report, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the admitted run executes");

        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(next, SetupNext::Done);
        assert_eq!(host.rest.calls, [Stage::Repositories]);
        assert!(host.minutes().is_empty(), "no replacement join");
        assert!(lan.log().is_empty(), "{:?}", lan.log());
        assert_eq!(lan.status_now(), joined);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_same_source_authority_joined_after_admission_is_preserved() {
        for legacy in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let source = "https://team.example/v1/team";
            let plan = Plan {
                vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
                ..selected(&[Setting::Secrets, Setting::Repositories])
            };
            let lan = Lan::new(true, "office");
            lan.in_vault(&plan.vault_url, START);
            let admitted = ConfiguredVault {
                config: lan.status_now().config.unwrap(),
                home: PathBuf::from("/test-home"),
            };
            let revision = authorize_reading(root.path(), &plan, || Ok(Some(admitted)))
                .unwrap()
                .unwrap();
            if legacy {
                let mut saved = progress(root.path()).unwrap().unwrap();
                // The source/name-only stamp written before authority was included.
                saved.admitted_vault = AdmittedVault::Configured(aterm_digest::Sha256::digest(
                    format!("url\0{source}\0team").as_bytes(),
                ));
                save_progress(root.path(), saved).unwrap();
            }
            lan.in_vault(source, START);
            let joined = lan.status_now();
            lan.set(joined.clone(), true);
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let (report, next) = execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the admitted run executes");

            assert!(
                host.minutes().is_empty(),
                "legacy={legacy}: {:?}",
                lan.log()
            );
            assert!(lan.log().is_empty(), "legacy={legacy}: {:?}", lan.log());
            assert_eq!(lan.status_now(), joined);
            assert!(
                rows_ready(&report.rows),
                "legacy={legacy}: {:?}",
                report.rows
            );
            assert_eq!(host.rest.calls, [Stage::Repositories]);
            assert_eq!(next, SetupNext::Done);
            assert!(finished_at(root.path()).unwrap());
        }
    }

    #[test]
    fn a_verified_rotation_continues_the_admitted_vault_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = "https://team.example/v1/team";
        let plan = Plan {
            vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let lan = Lan::new(true, "office");
        lan.in_vault(&plan.vault_url, START);
        let admitted = ConfiguredVault {
            config: lan.status_now().config.unwrap(),
            home: PathBuf::from("/test-home"),
        };
        let identity = admitted.identity();
        let revision = authorize_reading(root.path(), &plan, || Ok(Some(admitted)))
            .unwrap()
            .unwrap();
        lan.in_vault(source, START);
        let mut rotated = lan.status_now();
        rotated.config.as_mut().unwrap().followed.push([9; 32]);
        lan.set(rotated.clone(), true);
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        assert!(!AdmittedVault::Configured(identity).stands_in(
            &Plan {
                vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[11; 32])),
                ..plan.clone()
            },
            &host.configured_vault().unwrap().unwrap(),
        ));
        let (report, _) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the admitted run executes");

        assert!(rows_ready(&report.rows), "{:?}", report.rows);
        assert_eq!(host.minutes(), [0], "normal member read, not left in place");
        assert_eq!(host.rest.calls, [Stage::Repositories]);
        assert!(lan.log().is_empty(), "no join or sync: {:?}", lan.log());
        assert_eq!(lan.status_now(), rotated);
        assert!(finished_at(root.path()).unwrap());
    }

    #[test]
    fn a_fresh_explicit_join_can_replace_a_same_source_authority() {
        let root = aterm_tempfile::tempdir().unwrap();
        let source = "https://team.example/v1/team";
        let plan = Plan {
            vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[9; 32])),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let lan = Lan::new(true, "office");
        lan.in_vault(source, START);
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        let revision = system_state_apply(root.path(), &plan, true, || host.configured_vault());
        let (_, next) = execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the explicit join executes");

        assert_eq!(host.minutes(), [0]);
        assert!(lan.log().contains(&format!("join {}", plan.vault_url)));
        assert_eq!(lan.status_now().config.unwrap().signer, Some([9; 32]));
        assert!(
            host.rest.calls.is_empty(),
            "approval still holds repositories"
        );
        assert_ne!(next, SetupNext::Done);
        assert!(!finished_at(root.path()).unwrap());
    }

    fn a_group_choice_after_a_terminal_join() -> (aterm_tempfile::TempDir, Plan, Arc<Lan>, Vaulted)
    {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan {
            vault_url: "http://older.example:7484/v1/team".into(),
            ..selected(&[Setting::Secrets])
        };
        let lan = Lan::new(true, "office");
        lan.set(member(&plan.vault_url, Membership::Pending), true);
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        let configured = host.configured_vault().unwrap();
        authorize_reading(root.path(), &plan, || Ok(configured)).unwrap();
        lan.set(
            member(
                "http://newer.example:7484/v1/team",
                Membership::In {
                    seq: 7,
                    heard: START,
                },
            ),
            true,
        );
        plan.selection.set(Setting::Dock, true);
        (root, plan, lan, host)
    }

    /// A group choice whose read of the vault configuration fails is saved, with the vault
    /// this Mac is in recorded as none: its run sets the group chosen and never replaces the
    /// vault a terminal joined since with the plan's older one.
    #[test]
    fn manual_vault_admission_read_failure_never_replaces_a_newer_terminal_join() {
        let (root, plan, lan, mut host) = a_group_choice_after_a_terminal_join();
        let joined = lan.status_now();
        let mut attempt = InitialSetupRestart::chosen();
        let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Err("injected temporary source.toml read failure".into()),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .expect("the person's choice is saved");
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            lan.log().is_empty(),
            "an admission read failure authorized the older vault: {:?}",
            lan.log()
        );
        assert_eq!(lan.status_now(), joined);
        assert_eq!(host.rest.calls, [Stage::Workstation]);
    }

    #[test]
    fn manual_vault_admission_read_errors_refuse_join_but_allow_unrelated_groups() {
        for secrets in [false, true] {
            let (root, mut plan, lan, mut host) = a_group_choice_after_a_terminal_join();
            plan.selection.set(Setting::Secrets, secrets);
            let joined = lan.status_now();
            let before = observation_at(root.path()).unwrap();
            let mut attempt = InitialSetupRestart::chosen();
            let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                attempt.persist_with(
                    root,
                    &plan,
                    secrets,
                    || Err("injected temporary source.toml read failure".into()),
                    save_progress,
                    observe_restart,
                )
            });
            if secrets {
                assert!(admitted.result.is_err());
                assert_eq!(observation_at(root.path()).unwrap(), before);
                assert!(host.rest.calls.is_empty());
            } else {
                execute_initial_for_revision(
                    &mut host,
                    root.path(),
                    admitted.result.unwrap().unwrap(),
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap();
                assert_eq!(host.rest.calls, [Stage::Workstation]);
            }
            assert_eq!(lan.status_now(), joined);
            assert!(lan.log().is_empty());
        }
    }

    #[test]
    fn manual_vault_admission_reads_the_newer_vault_for_an_unrelated_group_choice() {
        let (root, plan, lan, mut host) = a_group_choice_after_a_terminal_join();
        let joined = lan.status_now();
        let configured = host.configured_vault().unwrap();
        let mut attempt = InitialSetupRestart::chosen();
        let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            attempt.persist_with(
                root,
                &plan,
                false,
                || Ok(configured),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .unwrap();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.rest.calls, [Stage::Workstation]);
        assert_eq!(lan.status_now(), joined);
        assert!(lan.log().is_empty());
        assert_eq!(
            saved_plan(root.path()).unwrap().unwrap().vault_url,
            "http://newer.example:7484/v1/team"
        );
    }

    fn admit_page_vault_delta(
        root: &Path,
        host: &mut Vaulted,
        plan: &Plan,
        changed: Selection,
        typed: bool,
    ) -> u64 {
        let configured = host.configured_vault().unwrap();
        observe_mutation_at(root, None, Err("busy".into()), |root| {
            let restart = if typed || !changed.is_empty() {
                InitialSetupRestart::chosen()
            } else {
                InitialSetupRestart::default()
            };
            restart.changing(Some(changed)).persist_with(
                root,
                plan,
                typed,
                || Ok(configured),
                save_progress,
                observe_restart,
            )
        })
        .result
        .unwrap()
        .unwrap()
    }

    fn page_before_a_newer_pending_join(
        configured: bool,
    ) -> (aterm_tempfile::TempDir, Plan, Plan, Arc<Lan>, Vaulted) {
        let root = aterm_tempfile::tempdir().unwrap();
        let older = Plan {
            vault_url: "https://older.example/v1/team".into(),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        let lan = Lan::new(false, "office");
        if configured {
            lan.in_vault("https://prior.example/v1/team", START);
        }
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        let read = host.configured_vault().unwrap();
        system_state_apply(root.path(), &older, true, || Ok(read));
        // A different page explicitly chooses this vault, but its first
        // real pass cannot reach it, so source.toml still names no vault
        // (or the prior membership), and the newer intent lives in the plan.
        let newer = Plan {
            vault_url: "https://newer.example/v1/team".into(),
            ..older.clone()
        };
        let mut changed = Selection::none();
        changed.set(Setting::Secrets, true);
        let revision = admit_page_vault_delta(root.path(), &mut host, &newer, changed, true);
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(lan.log().is_empty(), "an offline join changes nothing");
        assert_eq!(saved_plan(root.path()).unwrap(), Some(newer.clone()));
        let pending = progress(root.path()).unwrap().unwrap();
        assert!(pending.admitted && !pending.completed);
        assert_eq!(pending.done & Stage::Secrets.bit(), 0);
        *lan.answers.lock().unwrap() = true;
        (root, older, newer, lan, host)
    }

    #[test]
    fn page_vault_delta_keeps_a_newer_pending_join_for_an_unrelated_group() {
        for configured in [false, true] {
            let (root, mut stale, newer, lan, mut host) =
                page_before_a_newer_pending_join(configured);
            stale.selection.set(Setting::Spotlight, true);
            let mut changed = Selection::none();
            changed.set(Setting::Spotlight, true);
            let revision = admit_page_vault_delta(root.path(), &mut host, &stale, changed, false);
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert_eq!(host.rest.calls, [Stage::Spotlight]);
            assert!(
                lan.log().contains(&format!("join {}", newer.vault_url)),
                "an unrelated group must keep the pending vault: {:?}",
                lan.log()
            );
            assert!(!lan.log().contains(&format!("join {}", stale.vault_url)));
            let saved = saved_plan(root.path()).unwrap().unwrap();
            assert_eq!(saved.vault_url, newer.vault_url);
            assert!(saved.selection.contains(Setting::Spotlight));
        }
    }

    #[test]
    fn page_vault_delta_set_up_now_runs_the_newer_pending_vault() {
        let (root, stale, newer, lan, mut host) = page_before_a_newer_pending_join(false);
        let revision =
            admit_page_vault_delta(root.path(), &mut host, &stale, Selection::none(), false);
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(host.rest.calls.is_empty());
        assert!(
            lan.log().contains(&format!("join {}", newer.vault_url)),
            "Set up now must run the in-force vault: {:?}",
            lan.log()
        );
        assert_eq!(saved_plan(root.path()).unwrap(), Some(newer));
    }

    #[test]
    fn page_vault_delta_explicit_join_can_replace_the_pending_vault() {
        for configured in [false, true] {
            let (root, typed, _, lan, mut host) = page_before_a_newer_pending_join(configured);
            let mut changed = Selection::none();
            changed.set(Setting::Secrets, true);
            let revision = admit_page_vault_delta(root.path(), &mut host, &typed, changed, true);
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(lan.log().contains(&format!("join {}", typed.vault_url)));
            assert_eq!(saved_plan(root.path()).unwrap(), Some(typed));
        }
    }

    #[test]
    fn manual_vault_admission_allows_a_first_join_and_a_fresh_explicit_switch() {
        for switch in [false, true] {
            let root = aterm_tempfile::tempdir().unwrap();
            let plan = Plan {
                vault_url: "http://selected.example:7484/v1/team".into(),
                ..selected(&[Setting::Secrets])
            };
            let lan = Lan::new(true, "office");
            if switch {
                lan.in_vault("http://other.example:7484/v1/team", START);
            }
            let mut host = Vaulted::new(&lan);
            host.paced = false;
            let configured = host.configured_vault().unwrap();
            let mut attempt = InitialSetupRestart::chosen();
            let revision = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                attempt.persist_with(
                    root,
                    &plan,
                    switch,
                    || Ok(configured),
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap()
            .unwrap();
            execute_initial_for_revision(
                &mut host,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
            assert!(lan.log().contains(&format!("join {}", plan.vault_url)));
            assert_eq!(
                lan.status_now().config.unwrap().source,
                Source::Url(plan.vault_url)
            );
        }
    }

    fn a_relaunched_staged_choice_after_a_terminal_join()
    -> (aterm_tempfile::TempDir, Plan, Arc<Lan>, Vaulted) {
        let root = aterm_tempfile::tempdir().unwrap();
        let plan = Plan {
            vault_url: "http://older.example:7484/v1/team".into(),
            ..selected(&[Setting::Secrets, Setting::Dock])
        };
        let lan = Lan::new(true, "office");
        lan.set(member(&plan.vault_url, Membership::Pending), true);
        let mut host = Vaulted::new(&lan);
        host.paced = false;
        let configured = host.configured_vault().unwrap();
        let mut interrupted = InitialSetupRestart::chosen();
        let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            interrupted.persist_with(
                root,
                &plan,
                false,
                || Ok(configured),
                |_, next| {
                    assert!(next.admitted, "the preceding real writes staged the plan");
                    Err("the process ends before publishing final admission".into())
                },
                observe_restart,
            )
        });
        assert!(failed.result.is_err());
        assert!(interrupted.pending());
        let staged = progress(root.path()).unwrap().unwrap();
        assert!(staged.belongs_to(&plan));
        assert!(!staged.admitted);
        assert!(admission_at(root.path()).unwrap().is_none());
        drop(interrupted);
        lan.set(
            member(
                "http://newer.example:7484/v1/team",
                Membership::In {
                    seq: 7,
                    heard: START,
                },
            ),
            true,
        );
        (root, plan, lan, host)
    }

    /// The startup's offer reads an unreadable vault configuration as none and goes ahead
    /// (design §3.3): the offer's answer is admitted with no vault recorded, so the run's
    /// Secrets stage, reading the newer terminal join by then, leaves it in place and runs
    /// the rest of the plan, never replacing a vault it did not read.
    #[test]
    fn startup_vault_admission_read_failure_admits_and_keeps_the_newer_join() {
        let (root, plan, lan, mut host) = a_relaunched_staged_choice_after_a_terminal_join();
        let joined = lan.status_now();
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let offered = ticket
            .offer_reading(root.path(), || {
                Err("injected temporary source.toml read failure".into())
            })
            .expect("the offer goes ahead");
        assert_eq!(offered, plan);
        let revision = ticket
            .persist_at(root.path(), &offered)
            .result
            .unwrap()
            .expect("the offer's answer is admitted");
        assert_eq!(
            progress(root.path()).unwrap().unwrap().admitted_vault,
            AdmittedVault::None
        );
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            lan.log().is_empty(),
            "a startup read failure replaced the newer join: {:?}",
            lan.log()
        );
        assert_eq!(lan.status_now(), joined);
        assert_eq!(host.rest.calls, [Stage::Workstation]);
    }

    #[test]
    fn one_shot_vault_admission_read_failure_keeps_the_newer_terminal_join() {
        let (root, plan, lan, mut host) = a_relaunched_staged_choice_after_a_terminal_join();
        let joined = lan.status_now();
        let before = observation_at(root.path()).unwrap();
        let admitted = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            restart_reading(root, &plan, || {
                Err("injected temporary source.toml read failure".into())
            })
        });
        if let Ok(revision) = &admitted.result {
            execute_initial_for_revision(
                &mut host,
                root.path(),
                *revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap();
        }
        assert!(
            lan.log().is_empty(),
            "a fresh restart read failure replaced the newer join: {:?}",
            lan.log()
        );
        assert_eq!(lan.status_now(), joined);
        assert!(host.rest.calls.is_empty());
        assert_eq!(observation_at(root.path()).unwrap(), before);
        assert!(admitted.result.is_err());
    }

    pub(super) fn authorized() -> (aterm_tempfile::TempDir, u64) {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &selected(&[Setting::Secrets]))
            .unwrap()
            .unwrap();
        (root, revision)
    }

    pub(super) fn passes(
        host: &mut Vaulted,
        root: &Path,
        revision: u64,
        password: &mut Option<SecretBytes>,
        minutes: std::ops::RangeInclusive<u64>,
        mut before: impl FnMut(u64, &mut Vaulted),
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        for minute in minutes {
            host.now = START + minute * 60;
            before(minute, host);
            let Some((report, _)) =
                execute_initial_for_revision(host, root, revision, password, SetupHooks::NONE)
                    .unwrap()
            else {
                break;
            };
            rows.extend(
                report
                    .rows
                    .into_iter()
                    .filter(|row| row.name == Stage::Secrets.name()),
            );
        }
        rows
    }
}

fn join_password() -> SecretBytes {
    SecretBytes::from(b"test-only-setup-join-password".as_slice())
}

#[test]
fn an_unreachable_vault_is_asked_at_growing_pauses_and_nothing_is_changed() {
    use vault::{Lan, NO_ROUTE, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(false, "away");
    let mut host = Vaulted::new(&lan);
    let mut password = Some(join_password());
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        0..=20,
        |_, _| {},
    );
    assert_eq!(host.minutes(), [0, 1, 3, 7, 11, 15, 19]);
    assert_eq!(*lan.asked.lock().unwrap(), 7);
    assert!(
        lan.log().is_empty(),
        "journaled or changed: {:?}",
        lan.log()
    );
    assert!(password.is_some(), "kept for a try that reaches the vault");
    assert_eq!(rows.len(), 21);
    let offline = format!("the vault does not answer, so nothing was changed: {NO_ROUTE}; ");
    assert!(
        rows.iter()
            .all(|row| row.status == "blocked" && row.detail.starts_with(&offline)),
        "{rows:?}"
    );
    assert!(rows[0].detail.ends_with(
        "aterm tries again by itself a minute after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
    ));
    assert_eq!(
        rows[2].detail, rows[1].detail,
        "a paused pass shows its try"
    );
    assert!(rows[2].detail.contains("2 minutes after this try"));
    assert!(rows[20].detail.contains("4 minutes after this try"));
    let distinct: std::collections::BTreeSet<&str> =
        rows.iter().map(|row| row.detail.as_str()).collect();
    assert_eq!(distinct.len(), 3, "{distinct:?}");

    let (root, revision) = authorized();
    let lan = Lan::new(false, "away");
    let mut unpaced = Vaulted::new(&lan);
    unpaced.paced = false;
    // Keep the same join admission; this control changes only pacing.
    passes(
        &mut unpaced,
        root.path(),
        revision,
        &mut password,
        0..=20,
        |_, _| {},
    );
    assert_eq!(unpaced.minutes(), (0..=20).collect::<Vec<_>>());
    assert_eq!(*lan.asked.lock().unwrap(), 21);
    assert!(lan.log().is_empty());
    assert!(
        password.is_some(),
        "the unpaced preflight also keeps its password"
    );
}

#[test]
fn a_paused_try_comes_at_once_on_a_new_network_build_or_run() {
    use vault::{Lan, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(false, "away");
    let mut host = Vaulted::new(&lan);
    passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=14,
        |minute, host| match minute {
            5 => *host.lan.network.lock().unwrap() = "office".into(),
            10 => host.build = "1.0.0+gnext".into(),
            _ => {}
        },
    );
    assert_eq!(host.minutes(), [0, 1, 3, 5, 6, 8, 10, 11, 13]);

    let plan = selected(&[Setting::Secrets]);
    let rerun = restart_at(root.path(), &plan).unwrap();
    assert!(!root.path().join(secrets_retry::RECORD).exists());
    let mut host = Vaulted::new(&lan);
    host.now = vault::START + 14 * 60;
    execute_initial_for_revision(&mut host, root.path(), rerun, &mut None, SetupHooks::NONE)
        .unwrap();
    assert_eq!(host.minutes(), [14], "the person's choice tries at once");
}

#[test]
fn an_approval_is_picked_up_at_the_next_pass_and_within_the_longest_pause() {
    use vault::{Lan, START, Vaulted, authorized, passes};
    for marks_vault in [true, false] {
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        let mut host = Vaulted::new(&lan);
        host.marks_vault = marks_vault;
        let mut password = Some(join_password());
        let approved = 8;
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut password,
            0..=30,
            |minute, host| {
                if minute == approved {
                    host.lan.approve(START + minute * 60);
                }
            },
        );
        let tries = host.minutes();
        let picked = *tries.last().unwrap();
        assert_eq!(tries[..4], [0, 1, 3, 7], "{tries:?}");
        if marks_vault {
            assert_eq!(picked, approved, "{tries:?}");
        } else {
            assert_eq!(picked, 11, "{tries:?}");
        }
        assert!(
            (picked - tries[tries.len() - 2]) * 60 <= secrets_retry::LONGEST.as_secs(),
            "{tries:?}"
        );
        let last = rows.last().unwrap();
        assert_eq!(last.status, "ok", "{rows:?}");
        assert!(finished_at(root.path()).unwrap());
        assert!(!root.path().join(secrets_retry::RECORD).exists());
        assert!(
            rows[1].status == WAITING
                && rows[1]
                    .detail
                    .contains(aterm_machines::init_secrets::LET_IN),
            "{rows:?}"
        );
        assert_eq!(
            lan.log(),
            [
                "line init.secrets@tester".to_owned(),
                format!("join {}", aterm_types::macos_setup::DEFAULT_VAULT_URL),
                "install".to_owned(),
            ]
        );
        assert!(password.is_none(), "the join's proof is with the vault");
        assert_eq!(*lan.asked.lock().unwrap(), 1, "only the join reached out");
    }
}

#[test]
fn the_live_host_paces_each_secrets_try_by_the_network_and_the_vault_state() {
    use aterm_secrets::client::{Membership, Status};
    use std::sync::Arc;
    use vault::{Lan, LanReads};
    let lan = Lan::new(false, "away");
    let reads = |vault: bool| LanReads {
        lan: Arc::clone(&lan),
        vault,
        build: "1.0.0+gtest".into(),
        now: vault::START,
    };
    let mut host = LiveInitial {
        reads: reads(true),
        repositories_failure: None,
        token_wanted: false,
        spotlight_readback: WorkstationReadback::EMPTY,
        repository_readback: None,
    };
    let first = host.secrets_pace().expect("the live host paces every try");
    assert_eq!(first, secrets_retry::pace(&reads(true)));
    assert_eq!(first.build, "1.0.0+gtest");
    assert_eq!(first.now, vault::START);
    // An old-format known digest must still hold after upgrading aterm.
    let old_facts = format!(
        "away; {:?}; subscriber running: Ok(false); runs from: None",
        lan.status_now()
    );
    let mut old = first.clone();
    old.inputs = None;
    old.marks = Some(
        aterm_digest::Sha256::digest(old_facts.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    );
    let root = aterm_tempfile::tempdir().unwrap();
    for _ in 0..2 {
        let mut failed = vec![Row {
            name: Stage::Secrets.name().into(),
            status: "error".into(),
            detail: "injected failed change before upgrade".into(),
            elapsed_ms: None,
        }];
        secrets_retry::record(root.path(), 7, &old, false, &mut failed).unwrap();
    }
    assert!(secrets_retry::paused(root.path(), 7, &first).is_some());

    let known = first.inputs.clone().unwrap();
    for (fault, expected) in [
        (
            &lan.routes_error_once,
            secrets_retry::Inputs {
                routes: None,
                ..known.clone()
            },
        ),
        (
            &lan.status_error_once,
            secrets_retry::Inputs {
                vault: None,
                ..known.clone()
            },
        ),
        (
            &lan.subscriber_error_once,
            secrets_retry::Inputs {
                subscriber: None,
                ..known.clone()
            },
        ),
    ] {
        *fault.lock().unwrap() = true;
        let unknown = host.secrets_pace().unwrap();
        assert!(unknown.marks.is_none());
        assert_eq!(unknown.inputs, Some(expected));
        assert!(secrets_retry::paused(root.path(), 7, &unknown).is_some());
        assert_eq!(host.secrets_pace().unwrap().marks, first.marks);
    }
    let mut host_marks = vec![host.secrets_pace().unwrap().marks];
    assert_eq!(host_marks[0], first.marks, "nothing changed");

    *lan.network.lock().unwrap() = "office".into();
    host_marks.push(secrets_retry::pace(&host.reads).marks);
    let pending = Status {
        membership: Membership::Pending,
        ..lan.status_now()
    };
    lan.set(pending, true);
    host_marks.push(secrets_retry::pace(&host.reads).marks);
    lan.approve(vault::START);
    host_marks.push(secrets_retry::pace(&host.reads).marks);
    lan.approve(vault::START + 600);
    assert_eq!(
        secrets_retry::pace(&host.reads).marks,
        *host_marks.last().unwrap()
    );
    let joined = lan.status_now();
    lan.set(joined, false);
    host_marks.push(secrets_retry::pace(&host.reads).marks);
    let distinct: std::collections::BTreeSet<_> = host_marks.iter().collect();
    assert_eq!(distinct.len(), host_marks.len(), "{host_marks:?}");

    let unread = || secrets_retry::pace(&reads(false)).marks;
    let before = unread();
    lan.approve(vault::START + 900);
    let pending = Status {
        membership: Membership::Pending,
        ..lan.status_now()
    };
    lan.set(pending, true);
    assert_eq!(unread(), before);
}

#[test]
fn builds_taking_turns_share_the_pause_of_a_vault_that_does_not_answer() {
    use vault::{Lan, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(false, "away");
    let mut host = Vaulted::new(&lan);
    // Both builds use the same password-backed join, whose offline preflight makes nothing.
    let mut password = Some(join_password());
    passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        0..=20,
        |minute, host| {
            host.build = if minute % 2 == 0 {
                "1.0.0+gold".into()
            } else {
                "1.0.0+gnew".into()
            };
        },
    );
    assert_eq!(host.minutes(), [0, 1, 2, 4, 8, 12, 16, 20]);
    assert_eq!(*lan.asked.lock().unwrap(), 8);
    assert!(lan.log().is_empty());

    passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        21..=21,
        |_, host| host.build = "1.0.0+gthird".into(),
    );
    assert_eq!(*host.minutes().last().unwrap(), 21, "the control");
    assert!(
        password.is_some(),
        "no offline build spends the join password"
    );
}

/// A join the vault keeps refusing is tried again by itself, quietly, less often after each
/// try (two failures in a row slow it to the hour-long pause), journaled once per try, never
/// given up on, and tried at once when this Mac's network changes.
#[test]
fn a_join_that_keeps_failing_is_tried_again_less_often_and_at_once_on_a_change() {
    use vault::{Lan, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(true, "home");
    *lan.refusal.lock().unwrap() = Some(
        "http://alab-secrets.local:7484/v1/alab is signed by 0123-4567, which nothing pins: run this in a terminal to confirm it, or add #<signer> to the URL".into(),
    );
    let mut host = Vaulted::new(&lan);
    let mut password = Some(join_password());
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        0..=1440,
        |_, _| {},
    );
    let day: Vec<u64> = [0, 1, 3, 7, 15, 31, 63]
        .into_iter()
        .chain((123..=1440).step_by(60))
        .collect();
    assert_eq!(host.minutes(), day);
    let lines = |lan: &Lan| lan.log().iter().filter(|e| e.starts_with("line ")).count();
    assert_eq!(lines(&lan), day.len(), "{:?}", lan.log());
    assert!(password.is_some(), "kept: no join spent it");
    let last = rows.last().unwrap();
    assert_eq!(last.status, "error", "{rows:?}");
    assert!(
        last.detail.ends_with(
            "aterm tries again by itself an hour after this try, and at once when this Mac's network or vault changes or on Set up now or Join in System State"
        ),
        "{last:?}"
    );

    passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        1441..=1500,
        |minute, host| {
            if minute == 1441 {
                *host.lan.network.lock().unwrap() = "office".into();
            }
        },
    );
    // Inside the hour after the day's last try: the new network tries at once, and its
    // tries start over from a minute.
    assert_eq!(
        host.minutes()[day.len()..],
        [1441, 1442, 1444, 1448, 1456, 1472]
    );
    assert_eq!(lines(&lan), day.len() + 6);

    let (root, revision) = authorized();
    let lan = Lan::new(false, "away");
    let mut host = Vaulted::new(&lan);
    passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        0..=60,
        |_, _| {},
    );
    assert_eq!(host.minutes().len(), 17, "{:?}", host.minutes());
    assert_eq!(lines(&lan), 0);
    assert!(
        password.is_some(),
        "an unreachable preflight changes nothing"
    );
}

#[test]
fn a_setup_from_a_disk_image_waits_and_tries_at_once_from_applications() {
    use vault::{Lan, START, Vaulted, authorized, passes};
    let image = PathBuf::from("/Volumes/aterm/aterm.app/Contents/MacOS/aterm");
    for opened in [Some(5), None] {
        let (root, revision) = authorized();
        let lan = Lan::new(true, "home");
        lan.in_vault(&selected(&[Setting::Secrets]).vault_url, START);
        *lan.fleeting.lock().unwrap() = Some(image.clone());
        let mut host = Vaulted::new(&lan);
        let rows = passes(
            &mut host,
            root.path(),
            revision,
            &mut None,
            0..=8,
            |minute, host| {
                if Some(minute) == opened {
                    *host.lan.fleeting.lock().unwrap() = None;
                }
            },
        );
        let waits = &rows[..opened.map_or(rows.len(), |at| at as usize)];
        assert!(
            waits.iter().all(|row| row.status == WAITING
                && row
                    .detail
                    .starts_with(&format!("this aterm runs from {}", image.display()))
                && !row.detail.contains("--program")),
            "{rows:?}"
        );
        let installs = lan.log().iter().filter(|event| *event == "install").count();
        if opened.is_some() {
            assert_eq!(host.minutes(), [0, 1, 3, 5]);
            assert_eq!(installs, 4);
            assert_eq!(rows.last().unwrap().status, "applied", "{rows:?}");
        } else {
            assert_eq!(host.minutes(), [0, 1, 3, 7]);
            assert_eq!(installs, 4);
            assert_eq!(rows.len(), 9);
        }
    }
}

#[test]
fn a_refused_journal_line_refuses_the_secrets_stages_change() {
    use vault::{Lan, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(true, "home");
    let mut host = Vaulted::new(&lan);
    host.refuse = true;
    let mut password = Some(join_password());
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        0..=0,
        |_, _| {},
    );
    assert_eq!(rows[0].status, "error", "{rows:?}");
    assert!(
        rows[0]
            .detail
            .starts_with("not applied: the journal refused the record"),
        "{rows:?}"
    );
    assert!(lan.log().is_empty(), "{:?}", lan.log());
    assert!(password.is_some());

    host.refuse = false;
    passes(
        &mut host,
        root.path(),
        revision,
        &mut password,
        1..=1,
        |_, _| {},
    );
    assert_eq!(lan.log().len(), 3, "{:?}", lan.log());
    assert!(lan.log()[0].starts_with("line "));
    assert!(password.is_none());
}

const JOINED_SINCE: &str = "http://vault.example:7484/v1/team?k=fixture-query";

#[test]
fn a_vault_joined_after_setup_began_stays_when_the_setup_vault_answers() {
    use vault::{Lan, START, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
    assert!(record.contains(r#""admitted_vault":null"#), "{record}");
    let lan = Lan::new(false, "away");
    let mut host = Vaulted::new(&lan);
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=0,
        |_, _| {},
    );
    assert_eq!(rows[0].status, "blocked", "{rows:?}");
    let bootstrap = lan.log();
    assert_eq!(bootstrap, ["line init.secrets@tester", "install"]);

    lan.in_vault(JOINED_SINCE, START);
    let joined = lan.status_now();
    let shown = snapshot_in_vault(root.path(), host.configured_vault()).unwrap();
    assert!(!shown.finished);
    assert_eq!(shown.plan.vault_url, JOINED_SINCE);

    *lan.answers.lock().unwrap() = true;
    host.now = START + 60;
    let report = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap()
    .0;
    assert_eq!(host.minutes(), [0], "the vault item ran again");
    assert_eq!(
        lan.log(),
        bootstrap,
        "the newer vault permits no further change"
    );
    assert_eq!(lan.status_now(), joined);
    let [left, vault, subscriber] = report.rows.as_slice() else {
        panic!("{:?}", report.rows)
    };
    assert_eq!(
        (left.name.as_str(), left.status.as_str()),
        ("Secrets vault", "ok")
    );
    assert_eq!(vault.status, "ok", "{vault:?}");
    assert_eq!(
        (subscriber.name.as_str(), subscriber.status.as_str()),
        ("Secrets subscriber", "blocked")
    );
    assert!(!finished_at(root.path()).unwrap());

    lan.set(joined.clone(), true);
    host.now = START + 120;
    let report = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap()
    .0;
    assert!(rows_ready(&report.rows), "{:?}", report.rows);
    assert_eq!(host.minutes(), [0]);
    assert_eq!(
        lan.log(),
        bootstrap,
        "the newer vault is read back without changes"
    );
    assert!(finished_at(root.path()).unwrap());
    assert!(!root.path().join(secrets_retry::RECORD).exists());
}

#[test]
fn a_vault_left_in_place_that_waits_for_approval_is_not_done() {
    use aterm_secrets::client::Membership;
    use vault::{Lan, START, Vaulted, authorized};
    let (root, revision) = authorized();
    let lan = Lan::new(true, "office");
    lan.in_vault(JOINED_SINCE, START);
    let mut waiting = lan.status_now();
    waiting.membership = Membership::Pending;
    lan.set(waiting, true);
    let mut host = Vaulted::new(&lan);
    let report = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap()
    .0;
    assert!(host.minutes().is_empty(), "the vault item ran");
    assert!(lan.log().is_empty(), "{:?}", lan.log());
    let vault = report
        .rows
        .iter()
        .find(|row| row.detail.starts_with("waiting for vault approval, code "))
        .unwrap_or_else(|| panic!("{:?}", report.rows));
    assert_eq!(
        (vault.name.as_str(), vault.status.as_str()),
        ("Secrets vault", "blocked")
    );
    assert!(!rows_ready(&report.rows));
    assert!(!finished_at(root.path()).unwrap());

    lan.approve(START);
    host.now = START + 60;
    let report = execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .unwrap()
    .0;
    assert!(rows_ready(&report.rows), "{:?}", report.rows);
    assert!(finished_at(root.path()).unwrap());
    assert!(lan.log().is_empty(), "{:?}", lan.log());
}

#[test]
fn presenting_the_configured_vault_preserves_only_admitted_signer_pins() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let source = "https://team.example/v1/team";
    let mut vault = configure_vault(&paths, source);
    vault.config.followed.push([9; 32]);
    for pin in [None, Some([7; 32]), Some([9; 32])] {
        let plan = Plan {
            vault_url: pin.map_or_else(
                || source.to_owned(),
                |pin| format!("{source}#{}", aterm_secrets::b64::encode_padded(&pin)),
            ),
            ..selected(&[Setting::Secrets, Setting::Repositories])
        };
        assert_eq!(vault.present(plan.clone()), plan);
    }

    let unrelated = Plan {
        vault_url: format!("{source}#{}", aterm_secrets::b64::encode_padded(&[11; 32])),
        ..selected(&[Setting::Secrets, Setting::Repositories])
    };
    let presented = vault.present(unrelated.clone());
    assert_eq!(presented.selection, unrelated.selection);
    assert!(
        aterm_machines::init_secrets::selected_vault(
            &presented.vault_url,
            &vault.home,
            Some(&vault.config),
        )
        .unwrap()
        .is_some(),
        "the presented source must admit the configured signer"
    );
}

#[test]
fn an_apply_from_a_tab_shown_before_a_terminal_join_keeps_that_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let alab = Plan::default().vault_url;
    configure_vault(&paths, &alab);
    let read = || ConfiguredVault::read(&paths);
    authorize_reading(root.path(), &selected(&[Setting::Secrets]), read).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    let shown = snapshot_in_vault(root.path(), read()).unwrap().plan;
    assert_eq!(shown.vault_url, alab);

    let team = "http://vault.example:7484/v1/team";
    configure_vault(&paths, team);
    let ticked = Plan {
        vault_url: shown.vault_url.clone(),
        ..selected(&[Setting::Secrets, Setting::Spotlight])
    };
    let revision = system_state_apply(root.path(), &ticked, false, read);
    assert_eq!(saved_plan(root.path()).unwrap().unwrap().vault_url, team);
    let mut host = Fake {
        configured: read().unwrap(),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.vault_url.as_deref(), Some(team));

    let other = "https://other.example/v1/other";
    let typed = Plan {
        vault_url: other.into(),
        ..ticked
    };
    let revision = system_state_apply(root.path(), &typed, true, read);
    let mut host = Fake {
        configured: read().unwrap(),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.vault_url.as_deref(), Some(other));
}

#[test]
fn a_vault_joined_after_the_stage_read_it_is_not_joined_over() {
    use vault::{Lan, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let lan = Lan::new(true, "office");
    let mut host = Vaulted::new(&lan);
    host.paced = false;
    host.joins_after_read = Some(JOINED_SINCE);
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=0,
        |_, _| {},
    );
    let [row] = rows.as_slice() else {
        panic!("{rows:?}")
    };
    assert_eq!(row.status, "unverified", "{row:?}");
    assert_eq!(
        row.detail,
        "this Mac's vault changed after it was read (now vault team (http://vault.example:7484/v1/team)), so nothing was changed; the next try reads it"
    );
    assert!(lan.log().is_empty(), "{:?}", lan.log());
    assert!(!finished_at(root.path()).unwrap());

    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        1..=1,
        |_, _| {},
    );
    assert_eq!(rows[0].status, "ok", "{rows:?}");
    assert!(lan.log().is_empty(), "{:?}", lan.log());
}

#[test]
fn repository_setup_reads_the_vault_the_secrets_stage_left_in_place() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let team = "http://vault.example:7484/v1/team";
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake {
        configured: Some(configure_vault(&paths, team)),
        ..Fake::default()
    };
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
    assert_eq!(host.vault_url, None, "joined");
    assert_eq!(host.checked_url.as_deref(), Some(team));
    assert!(
        host.calls.contains(&Stage::Repositories),
        "{:?}",
        host.calls
    );
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn background_bootstrap_reads_current_stop_off_source_and_unreadable_choices() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut selection = Selection::none();
    selection.set(Setting::Secrets, true);
    let mut selected = Plan::from(selection);
    assert!(automatic_vault_bootstrap_allowed_at(root.path()).unwrap());
    authorize_at(root.path(), &selected).unwrap();
    assert!(automatic_vault_bootstrap_allowed_at(root.path()).unwrap());
    request_stop_at(root.path(), revision_at(root.path()).unwrap()).unwrap();
    assert!(!automatic_vault_bootstrap_allowed_at(root.path()).unwrap());

    let root = aterm_tempfile::tempdir().unwrap();
    selected.selection.set(Setting::Secrets, false);
    authorize_at(root.path(), &selected).unwrap();
    assert!(!automatic_vault_bootstrap_allowed_at(root.path()).unwrap());
    selected.selection.set(Setting::Secrets, true);
    selected.vault_url = "https://chosen.example/v1/personal".into();
    save_plan(root.path(), &selected).unwrap();
    assert!(!automatic_vault_bootstrap_allowed_at(root.path()).unwrap());
    std::fs::write(root.path().join("automatic-plan.json"), b"invalid").unwrap();
    assert!(automatic_vault_bootstrap_allowed_at(root.path()).is_err());
}

/// An authorized join parks where the vault cannot yet answer:
/// a row stood aside because the vault did not answer is unreached, and one whose look heard
/// a server with no vault waits, each with the look's words, so the pass parks; any other row
/// is as [`secrets_rows`] has it. Negative control: the plain rows leave it blocked, which
/// retries the pass every minute.
#[test]
fn a_join_an_unanswered_probe_let_through_parks_where_the_vault_does_not_answer() {
    use aterm_converge::StoodAside;
    use aterm_machines::model::{Effect, ItemReport, Outcome};
    let item = |outcome: Outcome, stood_aside: Option<StoodAside>, why: &str| ItemReport {
        id: "secrets.vault".into(),
        family: "secrets".into(),
        title: "vault".into(),
        outcome,
        from: None,
        to: None,
        why: Some(why.to_owned()),
        revert: None,
        effect: Effect::Now,
        detail: None,
        layer: None,
        class: None,
        explain: None,
        stood_aside,
        waits_on: None,
        essential: false,
        elapsed_ms: None,
    };
    let offline = "the vault does not answer, so nothing was changed: no route";
    let no_vault = format!(
        "{}: a login page",
        aterm_machines::init_secrets::NO_VAULT_NOW
    );
    let reports = vec![
        item(
            Outcome::Blocked,
            Some(StoodAside::Offline(offline.into())),
            offline,
        ),
        item(
            Outcome::Blocked,
            Some(StoodAside::Waiting("an admin Mac".into())),
            "approve",
        ),
        item(Outcome::Changed, None, "joined"),
        item(
            Outcome::Blocked,
            Some(StoodAside::Absent(no_vault.clone())),
            &no_vault,
        ),
        // The kind decides, never the words: an unanswered look worded as an absent one is
        // still unanswered.
        item(
            Outcome::Blocked,
            Some(StoodAside::Offline(no_vault.clone())),
            &no_vault,
        ),
    ];
    let seen = |rows: Vec<Row>| {
        rows.into_iter()
            .map(|row| (row.name, row.status, row.detail))
            .collect::<Vec<_>>()
    };
    let secrets = Stage::Secrets.name().to_owned();
    assert_eq!(
        seen(unanswered_join_rows(reports.clone())),
        [
            (secrets.clone(), UNREACHED.to_owned(), offline.to_owned()),
            (secrets.clone(), WAITING.to_owned(), "approve".to_owned()),
            (secrets.clone(), "applied".to_owned(), "joined".to_owned()),
            (secrets.clone(), ABSENT.to_owned(), no_vault.clone()),
            (secrets.clone(), UNREACHED.to_owned(), no_vault.clone()),
        ]
    );
    assert_eq!(
        SetupNext::of(&unanswered_join_rows(reports.clone())[..1]),
        SetupNext::Park
    );
    assert_eq!(SetupNext::of(&secrets_rows(reports)[..1]), SetupNext::Retry);
}

/// A Mac where aterm saved no plan and the vault's `source.toml` cannot be read (a
/// root-owned file from a `sudo aterm secrets …`, a newer format): the snapshot reads it as
/// no vault and shows the plan in force, never "could not read what it saved about this
/// Mac's setup", since aterm saved nothing and the file it could not read is the vault's.
#[test]
fn a_snapshot_reads_an_unreadable_vault_config_as_no_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    let shown = snapshot_in_vault(root.path(), Err("source.toml: no name".into()))
        .expect("an unreadable vault config is no unread saved setup");
    assert_eq!(shown.plan, Plan::default());
    authorize_at(root.path(), &selected(&[Setting::Permissions])).unwrap();
    let shown = snapshot_in_vault(root.path(), Err("source.toml: no name".into())).unwrap();
    assert_eq!(shown.plan, selected(&[Setting::Permissions]));
}

#[test]
fn partial_repository_readback_cannot_finish_its_setup_stage() {
    use aterm_machines::init_observe::Reading;
    use aterm_machines::model::{Effect, ItemReport, Outcome};
    for outcome in [Outcome::Ok, Outcome::Changed] {
        let report = ItemReport {
            id: "init.github.native@alice".into(),
            family: "github".into(),
            title: Stage::Repositories.name().into(),
            outcome,
            from: None,
            to: None,
            why: None,
            revert: None,
            effect: Effect::Now,
            detail: Some(
                Reading::Repositories {
                    present: 0,
                    count: 53,
                    signed_in: true,
                }
                .token(),
            ),
            layer: None,
            class: None,
            explain: None,
            stood_aside: None,
            waits_on: None,
            essential: false,
            elapsed_ms: None,
        };
        let rows = item_rows(vec![report]);
        assert_eq!(rows[0].status, "drift");
        assert!(!rows_ready(&rows));
    }
}

/// A Repositories step that failed says on the page the reason its step decided
/// (`init_observe::failure_note`), under the page's row, in place of the words a change
/// that did not take carries or the stage's own line, which the log keeps.
#[test]
fn a_failed_repositories_step_says_the_reason_it_decided() {
    const REASON: &str = "GitHub can't be reached";
    let failed = aterm_types::macos_setup::Unfinished::Failed;
    let said = |page: &PassPage| {
        page.rows
            .iter()
            .map(|row| (row.name.clone(), row.status.clone(), row.detail.clone()))
            .collect::<Vec<_>>()
    };
    let reason = vec![(
        Setting::Repositories.name().to_owned(),
        "error".to_owned(),
        REASON.to_owned(),
    )];
    let item = Row {
        name: "GitHub repositories from synced development credentials".into(),
        status: "error".into(),
        detail: "GET /user: connection refused".into(),
        elapsed_ms: None,
    };
    let mut page = items_page(Stage::Repositories, &[item]).unwrap();
    assert_eq!(page.outcome.unfinished, [(Setting::Repositories, failed)]);
    page.failed_because(Setting::Repositories, REASON);
    assert!(page.outcome.unfinished.is_empty());
    assert_eq!(said(&page), reason);

    let stage = Row {
        name: Stage::Repositories.name().into(),
        status: "error".into(),
        detail: "the stage's own line".into(),
        elapsed_ms: None,
    };
    let mut page = items_page(Stage::Repositories, &[stage]).unwrap();
    assert_eq!(page.rows[0].detail, "", "no words of the stage's own");
    page.failed_because(Setting::Repositories, REASON);
    assert_eq!(said(&page), reason);

    let mut calm = PassPage::default();
    calm.failed_because(Setting::Repositories, REASON);
    assert!(calm.rows.is_empty() && calm.outcome.unfinished.is_empty());
}

/// The same Mac's first-launch offer goes ahead, so aterm's own permissions, which need no
/// vault, are set up with no press (design §6); its admission records no vault, so the
/// Secrets stage never replaces one it did not read.
#[test]
fn the_first_offer_goes_ahead_past_an_unreadable_vault_config() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let offered = ticket
        .offer_reading(root.path(), || Err("source.toml: no name".into()))
        .expect("the offer goes ahead");
    assert_eq!(offered, Plan::default());
    let revision = ticket.persist_at(root.path(), &offered).result.unwrap();
    assert!(revision.is_some(), "the offer's answer is admitted");
    assert_eq!(
        progress(root.path()).unwrap().unwrap().admitted_vault,
        AdmittedVault::None
    );
}

/// The Secrets stage that cannot read which vault this Mac is in says nothing of its own on
/// the page: the Secrets vault row's calm note says it ([`observe::VAULT_UNREAD`]), once.
#[test]
fn an_unreadable_vault_config_is_said_once_by_its_row() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let mut host = Fake {
        configured_unreadable: true,
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    assert!(
        report
            .rows
            .iter()
            .any(|row| row.detail.starts_with(VAULT_CONFIG_UNREAD)),
        "{:?}",
        report.rows
    );
    let page = report.page.expect("the pass's page");
    assert!(page.rows.is_empty(), "{:?}", page.rows);
    assert!(page.outcome.unfinished.is_empty());
}

#[test]
fn an_unreadable_vault_configuration_joins_nothing_and_counts_no_failure() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &selected(&[Setting::Secrets])).unwrap();
    let mut host = Fake {
        configured_unreadable: true,
        pace: Some(Pace {
            build: "1.0.0+gtest".into(),
            marks: Some("unreadable".into()),
            inputs: None,
            materialization: None,
            network: None,
            now: vault::START,
        }),
        ..Fake::default()
    };
    let report = execute_initial(&mut host, root.path(), &mut None)
        .unwrap()
        .unwrap();
    assert!(host.calls.is_empty(), "{:?}", host.calls);
    let [row] = report.rows.as_slice() else {
        panic!("{:?}", report.rows)
    };
    assert_eq!(row.status, "unverified");
    assert!(
        row.detail
            .starts_with("cannot read which vault this Mac is in: source.toml: no name; "),
        "{row:?}"
    );
    let attempt: Value =
        aterm_json::from_slice(&std::fs::read(root.path().join(secrets_retry::RECORD)).unwrap())
            .unwrap();
    assert_eq!(attempt["failures"].as_u64(), Some(0), "{attempt:?}");
    assert!(!finished_at(root.path()).unwrap());
    host.pace = None;

    let older = Progress {
        admitted_vault: AdmittedVault::Unknown,
        ..progress(root.path()).unwrap().unwrap()
    };
    save_progress(root.path(), older).unwrap();
    execute_initial(&mut host, root.path(), &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Secrets]);
}

/// A vault configuration aterm cannot read (a root-owned `source.toml`) refuses no choice:
/// the offer admits the whole profile, and a later group choice that keeps Secrets is saved
/// as the person's, with the vault this Mac is in recorded as none.
#[test]
fn an_unreadable_vault_configuration_never_refuses_a_group_choice() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
    let offered = startup
        .offer_reading(root, || Err("source.toml: Permission denied".into()))
        .unwrap();
    assert!(offered.selection.contains(Setting::Secrets));
    startup
        .persist_at(root, &offered)
        .result
        .unwrap()
        .expect("the offer is admitted");
    let mut plan = offered.clone();
    for setting in aterm_types::macos_setup::Group::Desktop.settings() {
        plan.selection.set(*setting, false);
    }
    let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen()
            .changing(Some(group_selection(
                aterm_types::macos_setup::Group::Desktop,
            )))
            .persist_with(
                root,
                &plan,
                false,
                || Err("source.toml: Permission denied".into()),
                save_progress,
                observe_restart,
            )
    })
    .result;
    assert!(
        matches!(chosen, Ok(Some(_))),
        "the person's press: {chosen:?}"
    );
    assert_eq!(
        progress(root).unwrap().unwrap().admitted_vault,
        AdmittedVault::None
    );
}

/// A choice of Secrets made while the vault configuration cannot be read: its Secrets step
/// only waits, joining and configuring nothing, and goes ahead by itself once the
/// configuration reads.
#[test]
fn a_choice_over_an_unreadable_vault_configuration_waits_and_joins_nothing() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let plan = selected(&[Setting::Secrets]);
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            &plan,
            false,
            || Err("source.toml: Permission denied".into()),
            save_progress,
            observe_restart,
        )
    })
    .result
    .unwrap()
    .expect("the person's choice is saved");
    let mut host = Fake {
        configured_unreadable: true,
        ..Fake::default()
    };
    execute_initial(&mut host, root, &mut None).unwrap();
    assert!(host.calls.is_empty(), "{:?}", host.calls);
    assert!(!finished_at(root).unwrap());
    host.configured_unreadable = false;
    execute_initial(&mut host, root, &mut None).unwrap();
    assert_eq!(host.calls, [Stage::Secrets]);
}

#[test]
fn a_vault_typed_in_system_state_while_in_another_still_moves_the_mac() {
    use vault::{Lan, START, Vaulted, passes};
    let root = aterm_tempfile::tempdir().unwrap();
    let lan = Lan::new(true, "office");
    lan.in_vault(JOINED_SINCE, START);
    let mut host = Vaulted::new(&lan);
    let admitted = AdmittedVault::of(host.configured_vault());
    assert!(matches!(admitted, AdmittedVault::Configured(_)));
    let plan = selected(&[Setting::Secrets]);
    let revision = restart_in_vault(root.path(), &plan, admitted).unwrap();
    let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
    assert!(record.contains("admitted_vault"), "{record}");
    assert!(!record.contains("vault.example") && !record.contains("fixture-query"));
    passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=0,
        |_, _| {},
    );
    assert_eq!(host.minutes(), [0]);
    let log = lan.log();
    assert!(log[0].starts_with("line "), "{log:?}");
    assert!(
        log.iter().any(|event| event.starts_with("keep ")),
        "{log:?}"
    );
    assert!(log.contains(&format!("join {}", plan.vault_url)), "{log:?}");
}

#[test]
fn a_run_whose_plan_names_the_vault_joined_since_reads_it_as_before() {
    use vault::{Lan, START, Vaulted, passes};
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan {
        vault_url: JOINED_SINCE.into(),
        ..selected(&[Setting::Secrets])
    };
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let lan = Lan::new(true, "office");
    lan.in_vault(JOINED_SINCE, START);
    let mut host = Vaulted::new(&lan);
    let rows = passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=0,
        |_, _| {},
    );
    assert_eq!(host.minutes(), [0]);
    let log = lan.log();
    assert!(log.iter().any(|event| event == "install"), "{log:?}");
    assert!(
        !log.iter().any(|event| event.starts_with("join ")),
        "{log:?}"
    );
    assert!(rows_ready(&rows), "{rows:?}");
    assert!(finished_at(root.path()).unwrap());
}

#[test]
fn a_run_an_older_build_admitted_joins_as_before() {
    use vault::{Lan, START, Vaulted, authorized, passes};
    let (root, revision) = authorized();
    let older = Progress {
        admitted_vault: AdmittedVault::Unknown,
        ..progress(root.path()).unwrap().unwrap()
    };
    save_progress(root.path(), older).unwrap();
    let record = std::fs::read_to_string(root.path().join("initial-setup.json")).unwrap();
    assert!(!record.contains("admitted_vault"), "{record}");
    assert_eq!(progress(root.path()).unwrap(), Some(older));
    let lan = Lan::new(true, "office");
    lan.in_vault(JOINED_SINCE, START);
    let mut host = Vaulted::new(&lan);
    passes(
        &mut host,
        root.path(),
        revision,
        &mut None,
        0..=0,
        |_, _| {},
    );
    assert_eq!(host.minutes(), [0]);
    let joined = format!("join {}", selected(&[Setting::Secrets]).vault_url);
    assert!(lan.log().contains(&joined), "{:?}", lan.log());
}

#[test]
fn every_admission_keeps_the_vault_this_mac_is_in() {
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    configure_vault(&paths, "http://vault.example:7484/v1/team");
    let read = || ConfiguredVault::read(&paths);
    let vault = AdmittedVault::of(read());
    assert!(matches!(vault, AdmittedVault::Configured(_)));
    let plan = selected(&[Setting::Secrets, Setting::Spotlight]);
    let kept = |root: &Path| progress(root).unwrap().unwrap().admitted_vault;
    for index in 0..4 {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = match index {
            0 => {
                let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
                ticket.offer_reading(root.path(), read).unwrap();
                ticket.persist_at(root.path(), &plan).result.unwrap()
            }
            1 => Some(restart_reading(root.path(), &plan, read).unwrap()),
            2 => observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                InitialSetupRestart::default().persist_with(
                    root,
                    &plan,
                    true,
                    read,
                    save_progress,
                    observe_restart,
                )
            })
            .result
            .unwrap(),
            _ => authorize_reading(root.path(), &plan, read).unwrap(),
        }
        .expect("admitted");
        assert_eq!(kept(root.path()), vault, "admission {index}");
        let mut host = Fake {
            pending: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert_eq!(host.calls, [Stage::Spotlight, Stage::Secrets]);
        assert_eq!(
            kept(root.path()),
            vault,
            "after a pass of admission {index}"
        );
    }
}

#[test]
fn the_launch_offer_keeps_the_vault_it_was_loaded_in() {
    let root = aterm_tempfile::tempdir().unwrap();
    let home = aterm_tempfile::tempdir().unwrap();
    let paths = aterm_secrets::client::Paths::for_home(home.path());
    let team = "http://vault.example:7484/v1/team";
    let other = "https://other.example/v1/other";
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    configure_vault(&paths, team);
    let read = || ConfiguredVault::read(&paths);
    let offered = ticket.offer_reading(root.path(), read).unwrap();
    assert_eq!(offered.vault_url, team);
    let typed = Plan {
        vault_url: other.into(),
        ..offered
    };
    let revision = ticket
        .persist_at(root.path(), &typed)
        .result
        .unwrap()
        .expect("admitted");
    assert_eq!(
        progress(root.path()).unwrap().unwrap().admitted_vault,
        AdmittedVault::of(read())
    );
    let mut host = Fake {
        configured: read().unwrap(),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert!(host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
    assert_eq!(host.vault_url.as_deref(), Some(other));

    let root = aterm_tempfile::tempdir().unwrap();
    let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
    let revision = ticket
        .persist_at(root.path(), &offered)
        .result
        .unwrap()
        .expect("admitted");
    let mut host = Fake {
        configured: read().unwrap(),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert!(!host.calls.contains(&Stage::Secrets), "{:?}", host.calls);
    assert!(finished_at(root.path()).unwrap());
}

/// The settings of one page group, as a selection.
fn group_selection(group: aterm_types::macos_setup::Group) -> Selection {
    let mut selection = Selection::none();
    for setting in group.settings() {
        selection.set(*setting, true);
    }
    selection
}

/// Keyboard & Trackpad turned to Leave as is keeps the finished Workstation stage finished,
/// so the Dock is not set again for another group's choice.
#[test]
fn a_group_turned_to_leave_as_is_keeps_the_finished_workstation_stage() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    let mut plan = Plan::default();
    for setting in aterm_types::macos_setup::Group::Input.settings() {
        plan.selection.set(*setting, false);
    }
    assert!(preferences(plan.selection).contains(Setting::Dock));
    let revision = system_state_choice(root.path(), &plan);
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert!(
        !host.calls.contains(&Stage::Workstation),
        "Keyboard & Trackpad to Leave as is sets the Desktop & Dock preferences again: {:?}",
        host.calls
    );
    assert!(finished_at(root.path()).unwrap());
}

/// A group turned to Set up after the Workstation stage finished is the only one it sets
/// again.
#[test]
fn a_group_turned_on_after_the_stage_finished_sets_only_that_groups_preferences() {
    use aterm_types::macos_setup::Group;
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
    fn stage(event: StageEvent) {
        EVENTS.lock().unwrap().push((
            event.running,
            event.settings.to_vec(),
            event.subjects.to_vec(),
        ));
    }
    let hooks = SetupHooks {
        stage,
        ..SetupHooks::NONE
    };
    let events = |group: Group| {
        let settings = group.settings().to_vec();
        let subjects = settings
            .iter()
            .copied()
            .map(Subject::Setting)
            .collect::<Vec<_>>();
        vec![
            (true, settings.clone(), subjects.clone()),
            (false, settings, subjects),
        ]
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let mut plan = Plan::default();
    for setting in Group::Desktop.settings() {
        plan.selection.set(*setting, false);
    }
    authorize_at(root.path(), &plan).unwrap();
    let mut first = Fake::default();
    execute_initial(&mut first, root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(
        first.only,
        [None],
        "a run of a whole plan sets all its preferences"
    );
    let revision = system_state_choice(root.path(), &Plan::default());
    let mut host = Fake::default();
    execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks).unwrap();
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    assert_eq!(
        *EVENTS.lock().unwrap(),
        events(Group::Desktop),
        "only the preferences this pass sets may appear changing or get read back"
    );
    EVENTS.lock().unwrap().clear();
    assert!(finished_at(root.path()).unwrap());
    // Turning Keyboard & Trackpad off and on again sets that group alone.
    let mut plan = Plan::default();
    for setting in Group::Input.settings() {
        plan.selection.set(*setting, false);
    }
    execute_initial_for_revision(
        &mut Fake::default(),
        root.path(),
        system_state_choice(root.path(), &plan),
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    let mut host = Fake::default();
    let revision = system_state_choice(root.path(), &Plan::default());
    execute_initial_for_revision(&mut host, root.path(), revision, &mut None, hooks).unwrap();
    assert_eq!(host.only, [Some(group_selection(Group::Input))]);
    assert_eq!(*EVENTS.lock().unwrap(), events(Group::Input));
}

#[test]
fn stage_events_for_one_account_exclude_the_unselected_account_and_keep_accessibility() {
    use aterm_types::macos_setup::Access;
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
    fn stage(event: StageEvent) {
        EVENTS.lock().unwrap().push((
            event.running,
            event.settings.to_vec(),
            event.subjects.to_vec(),
        ));
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Messages]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks {
            stage,
            ..SetupHooks::NONE
        },
    )
    .unwrap();
    assert_eq!(host.calls, [Stage::Accounts]);
    let settings = vec![Setting::Messages];
    let subjects = vec![
        Subject::Setting(Setting::Messages),
        Subject::Access(Access::Accessibility),
    ];
    assert_eq!(
        *EVENTS.lock().unwrap(),
        [
            (true, settings.clone(), subjects.clone()),
            (false, settings, subjects)
        ]
    );
}

/// An explicit full profile supersedes a group-scoped Workstation stage,
/// including when the process ends between authorization and OS effects. Over a
/// group choice that already finished the run it widens nothing, so it reopens
/// nothing and an Apply ended there leaves no run to resume (H34).
#[test]
fn an_interrupted_explicit_full_apply_resumes_every_preference_after_a_partial_choice() {
    use aterm_types::macos_setup::Group;
    for completed in [true, false] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut previous = Plan::default();
        for setting in Group::Desktop.settings() {
            previous.selection.set(*setting, false);
        }
        authorize_at(root.path(), &previous).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        let partial = system_state_choice(root.path(), &Plan::default());
        let desktop = group_selection(Group::Desktop);
        assert_eq!(progress(root.path()).unwrap().unwrap().only, Some(desktop));
        if completed {
            let mut chosen = Fake::default();
            execute_initial_for_revision(
                &mut chosen,
                root.path(),
                partial,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the chosen group applies");
            assert_eq!(
                chosen.only,
                [Some(desktop)],
                "a normal group choice keeps its scope"
            );
        }

        // The public explicit Apply stages its full profile before touching
        // preferences. Ending there leaves this same durable run to resume.
        save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
        let revision = revision_at(root.path()).unwrap();
        let selected = saved_plan(root.path()).unwrap().unwrap();
        assert_eq!(
            selected.selection.workstation(),
            Selection::all().workstation()
        );
        let mut resumed = Fake::default();
        if completed {
            assert!(finished_at(root.path()).unwrap());
            assert!(
                execute_initial_for_revision(
                    &mut resumed,
                    root.path(),
                    revision,
                    &mut None,
                    SetupHooks::NONE,
                )
                .unwrap()
                .is_none(),
                "a finished run the full Apply does not widen has nothing to resume"
            );
            assert!(resumed.only.is_empty(), "{:?}", resumed.only);
            continue;
        }
        execute_initial_for_revision(
            &mut resumed,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the interrupted explicit profile resumes");
        assert_eq!(
            resumed.only,
            [None],
            "the full Apply must retire the earlier group restriction"
        );
        assert!(finished_at(root.path()).unwrap());
    }
}

/// A finished run whose Mac preferences a Restore put back (`plan` first), with the
/// record Restore writes.
fn restored_after(plan: &Plan) -> aterm_tempfile::TempDir {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), plan).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    save_restore(root.path()).unwrap();
    assert_eq!(
        decision_at(root.path()).unwrap(),
        Decision::Stopped(Stop::Restored)
    );
    root
}

pub(super) fn staged_restored_choice() -> (aterm_tempfile::TempDir, Plan) {
    use aterm_types::macos_setup::Group;
    let root = restored_after(&leaving(&[Group::Desktop]));
    let plan = Plan::default();
    let mut attempt = InitialSetupRestart::chosen();
    let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        attempt.persist_with(
            root,
            &plan,
            false,
            || Ok(None),
            |_, _| Err("injected failure before final admission".into()),
            observe_restart,
        )
    });
    assert!(failed.result.is_err());
    assert!(attempt.pending());
    let staged = progress(root.path()).unwrap().unwrap();
    assert!(!staged.admitted);
    assert_eq!(staged.only, Some(group_selection(Group::Desktop)));
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
    drop(attempt);
    (root, plan)
}

#[test]
fn a_fresh_restored_startup_keeps_the_groups_no_pass_set_again() {
    use aterm_types::macos_setup::Group;
    let (root, plan) = staged_restored_choice();
    let original = revision_at(root.path()).unwrap();
    let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
    let revision = startup
        .persist_at(root.path(), &plan)
        .result
        .unwrap()
        .unwrap();
    assert_eq!(revision, original + 1);
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(group_selection(Group::Input))),
        "Keyboard & Trackpad were restored and no pass set them again"
    );
    assert_ne!(workstation_row(root.path()).status, "applied");
}

/// After a Restore, a choice turned Desktop & Dock on and its process ended before the
/// admission; the next launch's offer is answered with another plan than the one staged: a
/// group choice made while the offer waited, a Join, or, with no person at all, the staged
/// plan presented in a vault the Mac joined since. Each is a choice over the Restore, as
/// the same one made on System State is (design §3.4): it sets only the group the person
/// turned on, keeps the sign-outs and the access revision, and leaves Keyboard & Trackpad
/// restored.
#[test]
fn a_restored_startup_answer_of_another_plan_sets_only_what_the_person_turned_on() {
    use aterm_types::macos_setup::Group;
    let desktop = group_selection(Group::Desktop);
    for route in ["group choice", "join", "vault joined since"] {
        let (root, staged) = staged_restored_choice();
        let before = progress(root.path()).unwrap().unwrap();
        let home = aterm_tempfile::tempdir().unwrap();
        let url = "https://joined.example/v1/joined";
        let joined = configure_vault(&aterm_secrets::client::Paths::for_home(home.path()), url);
        let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let (plan, answer) = match route {
            "group choice" => {
                let offered = startup.offer_reading(root.path(), || Ok(None)).unwrap();
                let mut plan = offered;
                plan.selection.set(Setting::Spotlight, false);
                let answer = startup.persist_at(root.path(), &plan);
                (plan, answer)
            }
            "join" => {
                startup.offer_reading(root.path(), || Ok(None)).unwrap();
                let plan = Plan {
                    vault_url: url.into(),
                    ..staged.clone()
                };
                let answer = startup.persist_join_at(root.path(), &plan, Some(joined));
                (plan, answer)
            }
            _ => {
                let plan = startup
                    .offer_reading(root.path(), || Ok(Some(joined.clone())))
                    .unwrap();
                assert_ne!(plan, staged, "the offer is presented in the joined vault");
                let answer = startup.persist_at(root.path(), &plan);
                (plan, answer)
            }
        };
        let revision = answer.result.unwrap().expect("admitted");
        let admitted = progress(root.path()).unwrap().unwrap();
        assert!(admitted.belongs_to(&plan), "{route}");
        assert_eq!(admitted.only, Some(desktop), "{route}");
        assert_eq!(
            admitted.access_revision(),
            before.access_revision(),
            "{route}: the grants are not handed to macOS again"
        );
        assert_eq!(
            decision_record(root.path()).unwrap(),
            (
                Decision::Pending,
                Some(Restored {
                    left: preferences(Selection::all()),
                    queued: Some(desktop),
                    selection: Some(plan.selection),
                    run: None,
                    held: false,
                })
            ),
            "{route}: the Restore's record measures from the admitted plan"
        );
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(desktop)], "{route}");
        for kept in [Stage::Accounts, Stage::Permissions] {
            assert!(!host.calls.contains(&kept), "{route}: {:?}", host.calls);
        }
        assert!(finished_at(root.path()).unwrap(), "{route}");
        assert_eq!(
            restored_left(root.path()),
            (Decision::Pending, Some(group_selection(Group::Input))),
            "{route}: Keyboard & Trackpad were restored and nobody turned them on"
        );
        assert_ne!(workstation_row(root.path()).status, "applied", "{route}");
    }
}

/// The startup answer and System State's own choice of the same plan over the same
/// records admit the same run: one path admits a plan over a Restore's record.
#[test]
fn a_restored_startup_answer_admits_what_the_same_choice_on_system_state_does() {
    let mut no_spotlight = Plan::default();
    no_spotlight.selection.set(Setting::Spotlight, false);
    for plan in [Plan::default(), no_spotlight] {
        let (startup_root, _) = staged_restored_choice();
        let mut startup = InitialSetupAuthorization::capture_at(startup_root.path()).unwrap();
        startup
            .persist_at(startup_root.path(), &plan)
            .result
            .unwrap();
        let (window_root, _) = staged_restored_choice();
        system_state_choice(window_root.path(), &plan);
        let (by_startup, by_window) = (
            progress(startup_root.path()).unwrap().unwrap(),
            progress(window_root.path()).unwrap().unwrap(),
        );
        assert_eq!(
            (
                by_startup.done,
                by_startup.only,
                by_startup.access_revision()
            ),
            (by_window.done, by_window.only, by_window.access_revision()),
            "{plan:?}"
        );
        assert_eq!(
            decision_record(startup_root.path()).unwrap(),
            decision_record(window_root.path()).unwrap(),
            "{plan:?}"
        );
    }
}

/// The first-run decision, and the Mac preferences it keeps restored, if any.
fn restored_left(root: &Path) -> (Decision, Option<Selection>) {
    let (decision, restored) = decision_record(root).unwrap();
    (decision, restored.map(|restored| restored.left))
}

/// One pass of the admitted run `revision` on a fake host: what it set.
fn run_pass(root: &Path, revision: u64) -> Fake {
    let mut host = Fake::default();
    execute_initial_for_revision(&mut host, root, revision, &mut None, SetupHooks::NONE).unwrap();
    host
}

/// The default plan with `groups` turned to Leave as is.
fn leaving(groups: &[aterm_types::macos_setup::Group]) -> Plan {
    let mut plan = Plan::default();
    for group in groups {
        for setting in group.settings() {
            plan.selection.set(*setting, false);
        }
    }
    plan
}

/// The Workstation row of the run a snapshot shows.
fn workstation_row(root: &Path) -> Row {
    snapshot_at(root, Plan::default())
        .unwrap()
        .rows
        .into_iter()
        .find(|row| row.name == Stage::Workstation.name())
        .expect("the plan selects Mac settings")
}

/// After Restore, a choice of another group, a group turned to Leave as is, or a Join sets no
/// Mac preference again, and none is recorded applied by this choice or any later one before
/// a pass sets them.
#[test]
fn a_choice_or_a_join_after_restore_leaves_the_restored_preferences_alone() {
    use aterm_types::macos_setup::Group;
    let mut no_spotlight = Plan::default();
    no_spotlight.selection.set(Setting::Spotlight, false);
    let mut no_accounts = Plan::default();
    for setting in Group::Accounts.settings() {
        no_accounts.selection.set(*setting, false);
    }
    for how in ["another group", "leave as is", "join"] {
        let root = restored_after(&Plan::default());
        let revision = match how {
            "another group" => system_state_choice(root.path(), &no_spotlight),
            "leave as is" => system_state_choice(root.path(), &no_accounts),
            _ => system_state_join(root.path(), &Plan::default()),
        };
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Pending,
            "{how}: the run goes on, and the restored preferences are not applied"
        );
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{how}: {:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap(), "{how}");
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Pending,
            "{how}"
        );
        assert_ne!(workstation_row(root.path()).status, "applied", "{how}");
        // A later choice still knows the preferences stand restored.
        let revision = system_state_choice(root.path(), &Plan::default());
        let mut host = Fake::default();
        execute_initial_for_revision(
            &mut host,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(!host.calls.contains(&Stage::Workstation), "{how}");
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Pending,
            "{how}"
        );
    }
    // Set up now is the person asking for every preference again.
    let root = restored_after(&Plan::default());
    let revision = system_state_apply(root.path(), &Plan::default(), false, || Ok(None));
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.only, [None]);
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
}

/// A Restore leaves both groups it puts back (Keyboard & Trackpad, Desktop & Dock) at Leave
/// as is (design §3.4): the plan is saved as a person's choice is, a person's and bound to
/// its run's progress, which keeps every other stage finished and the run's admission, so
/// aterm's permissions go on by themselves, and starts no run. One later press of Set up
/// on one of those groups sets that group alone. With no saved plan there is none to
/// change.
#[test]
fn a_restore_leaves_the_groups_it_put_back_as_is_and_starts_nothing() {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    let before = progress(root.path()).unwrap().unwrap();
    assert!(before.completed);
    // What `macos_doctor::restore` writes around its effects.
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    let left = leaving(&[Group::Input, Group::Desktop]);
    assert_eq!(
        saved_plan_record(root.path()).unwrap(),
        Some((left.clone(), true))
    );
    assert!(!saved_plan_provisional(root.path()).unwrap());
    let after = progress(root.path()).unwrap().unwrap();
    assert!(after.belongs_to(&left));
    assert_eq!(after.selection, Some(left.selection));
    assert_eq!(after.revision, before.revision + 1);
    assert!(after.admitted, "a Restore withdraws no permission");
    assert!(
        snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed
    );
    assert_eq!(after.access_revision(), before.access_revision());
    assert_eq!(
        after.done, before.done,
        "every stage stays finished: a group left as is has nothing to set"
    );
    assert!(admission_at(root.path()).unwrap().is_none());
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(
        decision_at(root.path()).unwrap(),
        Decision::Stopped(Stop::Restored)
    );
    // One press of Set up on Keyboard & Trackpad sets that group alone.
    let mut input = left.clone();
    for setting in Group::Input.settings() {
        input.selection.set(*setting, true);
    }
    let revision = system_state_choice(root.path(), &input);
    let staged = progress(root.path()).unwrap().unwrap();
    assert_eq!(staged.only, Some(group_selection(Group::Input)));
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Input))]);
    for kept in [Stage::Accounts, Stage::Permissions, Stage::Secrets] {
        assert!(!host.calls.contains(&kept), "{:?}", host.calls);
    }
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(
        decision_at(root.path()).unwrap(),
        Decision::Applied(true),
        "every Mac setting the plan selects is set, and Desktop & Dock is left as is"
    );
    // With no saved plan, there is none to change, and no run to carry aterm's own
    // permissions: the Restore stages them alone for the offer, as a launch would.
    let bare = aterm_tempfile::tempdir().unwrap();
    save_restore(bare.path()).unwrap();
    leave_restored_as_is(bare.path()).unwrap();
    let mut permissions = Selection::none();
    permissions.set(Setting::Permissions, true);
    let staged = saved_plan(bare.path()).unwrap().expect("staged");
    assert_eq!(staged.selection, permissions);
    assert!(!progress(bare.path()).unwrap().unwrap().admitted);
    let revision = launch_startup(bare.path()).expect("the offer admits it");
    assert_eq!(run_pass(bare.path(), revision).calls, [Stage::Permissions]);
}

fn restore_plan_publication(interrupted: bool) -> aterm_tempfile::TempDir {
    let root = restored_after(&Plan::default());
    // Restore's actual final metadata path: its preferences are back, but the
    // plan publication can fail after the new decision is durable.
    let restored = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        leave_restored_as_is_with(root, |write| {
            if interrupted && write == RestoreWrite::Plan {
                Err("injected failure before Restore's plan publication".into())
            } else {
                Ok(())
            }
        })
    });
    assert_eq!(restored.result.is_err(), interrupted);
    root
}

#[test]
fn an_interrupted_restore_plan_does_not_turn_restored_groups_back_on() {
    use aterm_types::macos_setup::Group;

    for interrupted in [false, true] {
        let root = restore_plan_publication(interrupted);
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Stopped(Stop::Restored)
        );
        assert_eq!(
            decision_record(root.path()).unwrap().1.unwrap().selection,
            Some(leaving(&[Group::Input, Group::Desktop]).selection)
        );
        if interrupted {
            assert_eq!(saved_plan(root.path()).unwrap(), Some(Plan::default()));
        }

        // A new process reads only the retained records. The person changes an
        // unrelated group from that page, without turning any Mac setting on.
        let before = observation_at(root.path()).unwrap();
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(page.finished);
        assert!(
            page.permissions_allowed,
            "a Restore never withdraws aterm's permissions"
        );
        assert_eq!(page.observation, before);
        let prior = progress(root.path()).unwrap().unwrap();
        if interrupted {
            assert_eq!(page.revision, prior.revision);
            assert_eq!(page.access_revision, prior.access_revision());
            assert!(
                page.rows
                    .iter()
                    .any(|row| { row.name == Stage::Accounts.name() && row.status == "applied" })
            );
        }
        let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(
            startup.offer_reading(root.path(), || Ok(None)).unwrap(),
            page.plan
        );
        let left = decision_record(root.path()).unwrap().1.unwrap().left;
        assert!(!left.is_empty());
        let mut unrelated = page.plan.clone();
        unrelated.selection.set(Setting::Spotlight, false);
        let revision = system_state_choice(root.path(), &unrelated);
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "Restore's failed plan write turned Mac preferences back on: {:?}",
            host.calls
        );
        assert!(preferences(page.plan.selection).is_empty());
        // The preferences the Restore put back stay unset, so its record survives the
        // unrelated choice, which reads applied.
        assert_eq!(
            restored_left(root.path()),
            (Decision::Applied(true), Some(left))
        );
    }
}

#[test]
fn restore_presentation_keeps_early_decisions_and_later_explicit_groups_distinct() {
    use aterm_types::macos_setup::Group;

    // Restore writes this first decision before its OS work. It still names P,
    // so merely asking for Restore does not fabricate its final Q selection.
    let early = restored_after(&Plan::default());
    let page = snapshot_at(early.path(), Plan::default()).unwrap();
    assert_eq!(page.plan, Plan::default());
    let mut unrelated = page.plan;
    unrelated.selection.set(Setting::Spotlight, false);
    let revision = system_state_choice(early.path(), &unrelated);
    assert!(
        !run_pass(early.path(), revision)
            .calls
            .contains(&Stage::Workstation)
    );
    assert_eq!(
        restored_left(early.path()),
        (Decision::Pending, Some(preferences(Selection::all())))
    );

    // The failed final plan save leaves Q durable. A new explicit Input-on
    // choice from its recovered page still sets Input alone, never Desktop.
    let root = restore_plan_publication(true);
    let mut input = snapshot_at(root.path(), Plan::default()).unwrap().plan;
    for setting in Group::Input.settings() {
        input.selection.set(*setting, true);
    }
    let revision = system_state_choice(root.path(), &input);
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Input))]);
    assert_eq!(saved_plan(root.path()).unwrap(), Some(input));
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
}

#[test]
fn a_restore_without_progress_presents_its_final_choice() {
    for modern in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let original = Plan::default();
        if modern {
            save_plan(root.path(), &original).unwrap();
        } else {
            save_automatic_plan(root.path(), original.selection).unwrap();
        }
        save_restore(root.path()).unwrap();
        let restored = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            leave_restored_as_is_with(root, |write| {
                if write == RestoreWrite::Plan {
                    Err("injected failure before Restore's plan publication".into())
                } else {
                    Ok(())
                }
            })
        });
        assert!(restored.result.is_err());
        let page = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(page.finished);
        assert!(preferences(page.plan.selection).is_empty());
        assert_eq!(saved_plan(root.path()).unwrap(), Some(original));
        assert_eq!(progress(root.path()).unwrap(), None);
        let mut startup = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        assert_eq!(
            startup.offer_reading(root.path(), || Ok(None)).unwrap(),
            page.plan
        );
    }
}

/// After Restore, a group a choice turns to Set up is the only one set again; one whose
/// pass has not landed stays owed through a later choice of another group. The pass
/// that sets it records nothing applied while another group stays restored.
#[test]
fn a_group_turned_on_after_restore_is_the_only_one_set_again() {
    use aterm_types::macos_setup::Group;
    let mut no_desktop = Plan::default();
    for setting in Group::Desktop.settings() {
        no_desktop.selection.set(*setting, false);
    }
    let root = restored_after(&no_desktop);
    let revision = system_state_choice(root.path(), &Plan::default());
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
    let mut failing = Fake {
        failure: Some(Stage::Workstation),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut failing,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(failing.only, [Some(group_selection(Group::Desktop))]);
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Pending);
    let mut no_spotlight = Plan::default();
    no_spotlight.selection.set(Setting::Spotlight, false);
    let revision = system_state_choice(root.path(), &no_spotlight);
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    // Keyboard & Trackpad were never set again: the record still stands restored.
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(group_selection(Group::Input)))
    );
}

/// A group turned to Set up whose pass failed, then back to Leave as is, leaves the stage
/// finished: it owes nothing.
#[test]
fn a_group_turned_on_then_off_before_its_pass_finishes_the_stage() {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    assert!(finished_at(root.path()).unwrap());
    let mut no_desktop = Plan::default();
    for setting in Group::Desktop.settings() {
        no_desktop.selection.set(*setting, false);
    }
    let revision = system_state_choice(root.path(), &no_desktop);
    execute_initial_for_revision(
        &mut Fake::default(),
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    // Desktop & Dock back to Set up: its pass fails (offline Dock, say).
    let revision = system_state_choice(root.path(), &Plan::default());
    let mut failing = Fake {
        failure: Some(Stage::Workstation),
        ..Fake::default()
    };
    execute_initial_for_revision(
        &mut failing,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(failing.only, [Some(group_selection(Group::Desktop))]);
    // The person gives up on it: Desktop & Dock to Leave as is again.
    let revision = system_state_choice(root.path(), &no_desktop);
    let record = progress(root.path()).unwrap().unwrap();
    assert_ne!(record.done & Stage::Workstation.bit(), 0, "{record:?}");
    assert_eq!(record.only, None);
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert!(
        !host.calls.contains(&Stage::Workstation),
        "{:?}",
        host.calls
    );
    assert!(finished_at(root.path()).unwrap());
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
}

/// After Restore, a choice that turns Desktop & Dock and the sign-outs back to Set up
/// sets only that group of the Mac preferences, and neither its pass nor the sign-outs'
/// records anything applied: Keyboard & Trackpad, which no pass set again, still read
/// restored, so the Workstation row never reads applied (design §3.3: nothing restored
/// reads applied until a pass sets it).
#[test]
fn a_restored_group_no_pass_set_does_not_read_applied() {
    use aterm_types::macos_setup::Group;
    let mut no_desktop = Plan::default();
    for setting in Group::Desktop
        .settings()
        .iter()
        .chain(Group::Accounts.settings())
    {
        no_desktop.selection.set(*setting, false);
    }
    let root = restored_after(&no_desktop);
    let revision = system_state_choice(root.path(), &Plan::default());
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(
        host.only,
        [Some(group_selection(Group::Desktop))],
        "only Desktop was set"
    );
    assert!(host.calls.contains(&Stage::Accounts), "{:?}", host.calls);
    let row = workstation_row(root.path());
    assert_ne!(
        row.status,
        "applied",
        "Keyboard & Trackpad were restored and never set again: {row:?}, {:?}",
        decision_record(root.path())
    );
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(group_selection(Group::Input)))
    );
    // Set up now sets every group again, and only then is the profile applied.
    let revision = restart_at(root.path(), &Plan::default()).unwrap();
    let mut host = Fake::default();
    execute_initial_for_revision(
        &mut host,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap();
    assert_eq!(host.only, [None]);
    assert_eq!(decision_at(root.path()).unwrap(), Decision::Applied(true));
}

/// A choice after Restore cut short before its decision landed (a crash or an I/O
/// error after its plan was saved, or after its progress too, before admission) leaves
/// the Restore's record, which names the plan it was taken against. The next choice is
/// measured from that plan, not from the plan the cut-short choice saved, so Desktop &
/// Dock, which the person turned on, is set; and it is the only step that runs: the
/// run's other steps are measured from the plan its progress was staged for, so
/// Spotlight, Permissions (whose access revision stands), the vault and the sign-outs
/// stay as the run left them.
#[test]
fn a_choice_whose_decision_write_failed_is_not_lost() {
    use aterm_types::macos_setup::Group;
    for final_restore in [false, true] {
        for cut in ["after its plan", "after its progress"] {
            let root = restored_after(&leaving(&[Group::Desktop]));
            if final_restore {
                leave_restored_as_is(root.path()).unwrap();
            }
            let mut choice = saved_plan(root.path()).unwrap().unwrap();
            for setting in Group::Desktop.settings() {
                choice.selection.set(*setting, true);
            }
            let access = access_revision_at(root.path()).unwrap();
            let before = |name: &str| std::fs::read(root.path().join(name)).unwrap();
            let (decision, progress_record) =
                (before("first-run.json"), before("initial-setup.json"));
            system_state_choice(root.path(), &choice);
            // What that choice leaves when it stops at `cut`.
            std::fs::write(root.path().join("first-run.json"), &decision).unwrap();
            if cut == "after its plan" {
                std::fs::write(root.path().join("initial-setup.json"), &progress_record).unwrap();
            } else {
                let staged = progress(root.path()).unwrap().unwrap();
                save_progress(
                    root.path(),
                    Progress {
                        admitted: false,
                        ..staged
                    },
                )
                .unwrap();
            }
            assert_eq!(saved_plan(root.path()).unwrap(), Some(choice.clone()));
            let page = snapshot_at(root.path(), Plan::default()).unwrap();
            assert_eq!(page.plan, choice, "the later staged choice stays visible");
            let revision = system_state_choice(root.path(), &page.plan);
            assert_eq!(
                access_revision_at(root.path()).unwrap(),
                access,
                "{cut}: nothing is handed to macOS again"
            );
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, [Stage::Workstation], "{cut}");
            assert_eq!(
                host.only,
                [Some(group_selection(Group::Desktop))],
                "{cut}: the group the person turned on is set"
            );
        }
    }
}

/// A choice cut short once it saved its plan, before its progress landed, never ran:
/// the run is still the one its progress records, staged for the plan before it, which
/// the progress keeps. The next choice is measured from that plan, not from the one the
/// cut-short choice saved, so it runs only what it changed: Desktop & Dock turned on
/// sets that group alone, and a Join runs only the vault's steps. Spotlight, Permissions
/// (whose access revision stands, so nothing is handed to macOS again) and the
/// sign-outs (Messages and FaceTime are not signed out again) stay as the run left
/// them. NEGATIVE CONTROL: Set up now still runs every step.
#[test]
fn a_choice_cut_short_after_its_plan_runs_only_what_it_changed() {
    use aterm_types::macos_setup::Group;
    let joined = Plan {
        vault_url: "http://team.local:7484/v1/team".into(),
        ..Plan::default()
    };
    for (how, first, then, runs) in [
        (
            "a choice",
            leaving(&[Group::Desktop]),
            Plan::default(),
            &[Stage::Workstation][..],
        ),
        (
            "a join",
            Plan::default(),
            joined.clone(),
            &[Stage::Secrets, Stage::Repositories][..],
        ),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &first).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        assert!(finished_at(root.path()).unwrap(), "{how}");
        let access = access_revision_at(root.path()).unwrap();
        let choose = |root: &Path| {
            if how == "a join" {
                system_state_join(root, &then)
            } else {
                system_state_choice(root, &then)
            }
        };
        let before = |name: &str| std::fs::read(root.path().join(name)).unwrap();
        let (decision, progress_record) = (before("first-run.json"), before("initial-setup.json"));
        choose(root.path());
        // What that choice leaves when it stops after its plan.
        std::fs::write(root.path().join("first-run.json"), &decision).unwrap();
        std::fs::write(root.path().join("initial-setup.json"), &progress_record).unwrap();
        assert_eq!(
            saved_plan(root.path()).unwrap(),
            Some(then.clone()),
            "{how}"
        );
        let revision = choose(root.path());
        assert_eq!(
            access_revision_at(root.path()).unwrap(),
            access,
            "{how}: nothing is handed to macOS again"
        );
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, runs, "{how}");
        if how == "a choice" {
            assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
        }
        assert!(finished_at(root.path()).unwrap(), "{how}");
        let revision = system_state_apply(root.path(), &then, false, || Ok(None));
        let host = run_pass(root.path(), revision);
        assert!(
            host.calls.contains(&Stage::Workstation) && host.calls.contains(&Stage::Accounts),
            "{how}: Set up now runs every step: {:?}",
            host.calls
        );
    }
}

/// A choice staged but never admitted (its admission commit failed) and then a Restore:
/// the Restore stands over it. A later choice of another group sets no Mac preference
/// again, not even the group that choice turned on before the Restore.
#[test]
fn a_choice_staged_before_a_restore_is_not_set_after_it() {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &leaving(&[Group::Desktop])).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    let failed = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen().persist_with(
            root,
            &Plan::default(),
            false,
            || Ok(None),
            |_, _| Err("disk full".into()),
            observe_restart,
        )
    })
    .result;
    assert!(failed.is_err(), "{failed:?}");
    let staged = progress(root.path()).unwrap().unwrap();
    assert!(!staged.admitted, "{staged:?}");
    assert_eq!(staged.only, Some(group_selection(Group::Desktop)));
    save_restore(root.path()).unwrap();
    let mut no_spotlight = Plan::default();
    no_spotlight.selection.set(Setting::Spotlight, false);
    let revision = system_state_choice(root.path(), &no_spotlight);
    let host = run_pass(root.path(), revision);
    assert!(
        !host.calls.contains(&Stage::Workstation),
        "Desktop & Dock was set after the Restore: {:?}",
        host.calls
    );
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(preferences(Selection::all())))
    );
}

/// After a Restore, every group the plan selects set again piece by piece reads
/// applied, as Set up now does: Desktop & Dock turned on (its pass sets it), then
/// Keyboard & Trackpad turned to Leave as is and back to Set up (its pass sets it).
/// Each pass takes its group out of the restored record.
#[test]
fn every_group_set_again_since_a_restore_reads_applied() {
    use aterm_types::macos_setup::Group;
    let root = restored_after(&leaving(&[Group::Desktop]));
    let revision = system_state_choice(root.path(), &Plan::default());
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(group_selection(Group::Input)))
    );
    assert_ne!(workstation_row(root.path()).status, "applied");
    // Keyboard & Trackpad to Leave as is: nothing is set, and nothing the plan
    // selects stands restored any more.
    let revision = system_state_choice(root.path(), &leaving(&[Group::Input]));
    let host = run_pass(root.path(), revision);
    assert!(
        !host.calls.contains(&Stage::Workstation),
        "{:?}",
        host.calls
    );
    // And back to Set up: its pass sets it, and the profile reads applied.
    let revision = system_state_choice(root.path(), &Plan::default());
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Input))]);
    assert_eq!(restored_left(root.path()), (Decision::Applied(true), None));
    assert_eq!(workstation_row(root.path()).status, "applied");
}

/// A Restore record an older aterm wrote names no preferences, and reads as every
/// one restored; so does a pending record of a choice since with `"restored": true`,
/// which kept its queue in the run's progress, as before.
#[test]
fn an_older_restored_record_reads_every_preference_restored() {
    use aterm_types::macos_setup::Group;
    let root = restored_after(&leaving(&[Group::Desktop]));
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"restored"}),
    )
    .unwrap();
    assert_eq!(
        restored_left(root.path()),
        (
            Decision::Stopped(Stop::Restored),
            Some(preferences(Selection::all()))
        )
    );
    system_state_choice(root.path(), &Plan::default());
    // What an older aterm's choice wrote, its queue in the progress.
    write_json(
        &root.path().join("first-run.json"),
        &json!({"profile":PROFILE,"state":"pending","restored":true}),
    )
    .unwrap();
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(preferences(Selection::all())))
    );
    let mut no_spotlight = Plan::default();
    no_spotlight.selection.set(Setting::Spotlight, false);
    let revision = system_state_choice(root.path(), &no_spotlight);
    let host = run_pass(root.path(), revision);
    assert_eq!(host.only, [Some(group_selection(Group::Desktop))]);
    assert_eq!(
        restored_left(root.path()),
        (Decision::Pending, Some(group_selection(Group::Input)))
    );
}

/// System State's group choice from a page that changed only `changed` since it last took
/// the plan in force ([`InitialSetupRestart::changing`]): the admitted run's revision.
fn page_choice(root: &Path, plan: &Plan, changed: Selection) -> u64 {
    observe_mutation_at(root, None, Err("busy".into()), |root| {
        InitialSetupRestart::chosen()
            .changing(Some(changed))
            .persist_with(
                root,
                plan,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
    })
    .result
    .unwrap()
    .expect("admitted")
}

/// A choice takes only the groups the page changed onto the plan in force (design §3.3).
/// A press made while Restore ran was sent with the plan the page showed before it, where
/// Keyboard & Trackpad and Desktop & Dock were still Set up: admitted after the Restore,
/// it changes Messages & FaceTime alone, never sets the restored groups again, and keeps
/// the person's own press, a group pressed and pressed back included.
#[test]
fn a_choice_from_a_page_shown_before_restore_never_sets_the_restored_groups_again() {
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    let restored = leaving(&[Group::Input, Group::Desktop]);
    let stale = leaving(&[Group::Accounts]);
    let revision = page_choice(root.path(), &stale, group_selection(Group::Accounts));
    let saved = saved_plan(root.path()).unwrap().unwrap();
    assert_eq!(
        saved,
        leaving(&[Group::Input, Group::Desktop, Group::Accounts]),
        "the press is kept, and the restored groups stay as is"
    );
    let staged = progress(root.path()).unwrap().unwrap();
    assert_eq!(staged.only, None, "{staged:?}");
    let host = run_pass(root.path(), revision);
    assert!(
        !host.calls.contains(&Stage::Workstation),
        "{:?}",
        host.calls
    );
    // Pressed and pressed back while the first was admitted: the page's value stands.
    page_choice(root.path(), &restored, group_selection(Group::Accounts));
    assert_eq!(saved_plan(root.path()).unwrap(), Some(restored));
}

/// A Restore cut short at any of its writes leaves the records whole (design §3.4): a
/// choice made from the page as it shows the plan in force sets none of the restored
/// groups for a change of another group, and sets the group a press of Set up turns on.
#[test]
fn a_restore_cut_short_at_any_write_never_lets_another_choice_undo_it() {
    use aterm_types::macos_setup::Group;
    for cut in [
        Some(RestoreWrite::Record),
        Some(RestoreWrite::Plan),
        Some(RestoreWrite::Progress),
        None,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        authorize_at(root.path(), &Plan::default()).unwrap();
        execute_initial(&mut Fake::default(), root.path(), &mut None).unwrap();
        save_restore(root.path()).unwrap();
        let ended = leave_restored_as_is_with(root.path(), |write| {
            if Some(write) == cut {
                Err("the process ended here".into())
            } else {
                Ok(())
            }
        });
        assert_eq!(ended.is_err(), cut.is_some(), "{cut:?}");
        // The page shows the plan in force; Rust in Spotlight is turned to Leave as is.
        let mut shown = snapshot_at(root.path(), Plan::default()).unwrap().plan;
        shown.selection.set(Setting::Spotlight, false);
        let mut spotlight = Selection::none();
        spotlight.set(Setting::Spotlight, true);
        let host = run_pass(root.path(), page_choice(root.path(), &shown, spotlight));
        assert!(
            !host.calls.contains(&Stage::Workstation),
            "{cut:?}: an unrelated choice after Restore set {:?}",
            host.only
        );
        if shown.selection.contains(Setting::Dock) {
            continue;
        }
        for setting in Group::Desktop.settings() {
            shown.selection.set(*setting, true);
        }
        let host = run_pass(
            root.path(),
            page_choice(root.path(), &shown, group_selection(Group::Desktop)),
        );
        assert_eq!(
            host.only,
            [Some(group_selection(Group::Desktop))],
            "{cut:?}: Set up on Desktop & Dock sets that group"
        );
    }
}

/// A person's Stop, a Restore, then a plan record that cannot be read: the set-aside holds
/// the Stop in the Restore's decision ([`Restored::held`]), so no bring-forward stages
/// aterm's permissions over it and no launch runs them; a Restore pressed again keeps it.
#[test]
fn a_stop_a_set_aside_held_is_never_brought_forward_by_a_restore() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let plan = plan_of(
        &[Setting::Permissions],
        &[aterm_types::macos_setup::Group::Desktop],
    );
    let revision = system_state_choice(root, &plan);
    request_stop_at(root, revision).unwrap();
    save_restore(root).unwrap();
    leave_restored_as_is(root).unwrap();
    assert_eq!(
        bring_forward_restored_at(root).unwrap(),
        None,
        "the Stop holds"
    );
    assert_eq!(launch_startup(root), None);

    std::fs::write(root.join("automatic-plan.json"), CORRUPT).unwrap();
    set_aside_unreadable_setup_at(root, false)
        .unwrap()
        .expect("the damaged plan is set aside");
    assert!(saved_plan(root).unwrap().unwrap().selection.is_empty());
    let (decision, restored) = decision_record(root).unwrap();
    assert_eq!(decision, Decision::Stopped(Stop::Restored));
    assert!(
        restored.is_some_and(|restored| restored.held),
        "held in the decision"
    );
    assert_eq!(bring_forward_restored_at(root).unwrap(), None);
    assert_eq!(launch_startup(root), None, "nothing runs");
    assert!(!permissions_allowed(root));

    save_restore(root).unwrap();
    leave_restored_as_is(root).unwrap();
    assert!(
        decision_record(root)
            .unwrap()
            .1
            .is_some_and(|restored| restored.held)
    );
    assert_eq!(
        launch_startup(root),
        None,
        "a Restore pressed again keeps the hold"
    );
}

/// A Stop of a staged run nobody admitted, whose save lands after a Restore carried that run
/// on, holds the run it was carried on to ([`Progress::continues`]); so does one that lands
/// after the Restore staged aterm's permissions over it for the launch's offer.
#[test]
fn a_stop_saved_after_a_restore_carried_its_unadmitted_run_still_holds_it() {
    let stage = |root: &Path| {
        authorize_at(
            root,
            &plan_of(
                &[Setting::Permissions],
                &[aterm_types::macos_setup::Group::Desktop],
            ),
        )
        .unwrap();
        let mut staged = progress(root).unwrap().unwrap();
        staged.admitted = false;
        save_progress(root, staged).unwrap();
        staged.revision
    };
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let staged = stage(root);
    save_restore(root).unwrap();
    leave_restored_as_is_with(root, |write| {
        if write == RestoreWrite::Run {
            request_stop_at(root, staged)?;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(bring_forward_restored_with(root, |_| Ok(())).unwrap(), None);
    assert_eq!(launch_startup(root), None);

    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let staged = stage(root);
    save_restore(root).unwrap();
    leave_restored_as_is(root).unwrap();
    assert!(
        progress(root).unwrap().unwrap().revision > staged,
        "staged for the offer"
    );
    request_stop_at(root, staged).unwrap();
    assert_eq!(bring_forward_restored_at(root).unwrap(), None);
    assert_eq!(
        launch_startup(root),
        None,
        "the late Stop holds the staged run"
    );
}

/// A Restore never withdraws aterm's permissions (design §3.4, §6): the run it ends keeps
/// its admission and its access revision, so the access steps go on by themselves, while no
/// pass runs. A person's Stop of that run still holds them.
#[test]
fn a_restore_keeps_the_permissions_approval_and_a_persons_stop() {
    for stop in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &Plan::default())
            .unwrap()
            .unwrap();
        let mut first = Fake {
            waiting: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial(&mut first, root.path(), &mut None).unwrap();
        if stop {
            request_stop_at(root.path(), revision).unwrap();
        }
        let before = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(before.permissions_allowed, !stop);
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        let after = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(after.permissions_allowed, !stop, "stop={stop}");
        assert_eq!(after.access_revision, before.access_revision);
        assert!(after.finished);
        assert!(admission_at(root.path()).unwrap().is_none());
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty(), "a Restore starts no run");
    }
}

/// `aterm doctor --apply` after a Restore finishes with aterm's permissions kept (design
/// §6) and keeps the groups the Restore left as is: nothing of Desktop changes. Once the
/// person turns Desktop on again on the page, the next apply makes its Widgets change, which
/// waits for Full Disk Access and follows once the grant comes; the next launch runs nothing.
#[test]
fn an_explicit_apply_after_restore_keeps_what_it_left_and_a_group_turned_on_follows() {
    use super::super::tests::{WIDGETS, Withholding};
    use aterm_types::macos_setup::Group;
    for whole_restore in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::from(group_selection(Group::Desktop));
        for setting in Group::Input.settings() {
            plan.selection.set(*setting, true);
        }
        plan.selection.set(Setting::Permissions, true);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the run executes");
        assert!(finished_at(root.path()).unwrap());
        let mut doctor = stages.doctor;
        if whole_restore {
            doctor.restore(root.path()).unwrap();
        } else {
            save_restore(root.path()).unwrap();
        }
        let report = doctor.explicit_apply(root.path()).unwrap();
        assert!(
            finished_at(root.path()).unwrap(),
            "whole_restore={whole_restore}: {:?}",
            progress(root.path()).unwrap()
        );
        assert!(
            saved_plan(root.path())
                .unwrap()
                .unwrap()
                .selection
                .contains(Setting::Permissions)
        );
        assert!(
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        let off = report
            .rows
            .iter()
            .find(|row| row.status == super::super::OFF)
            .unwrap_or_else(|| {
                panic!(
                    "whole_restore={whole_restore}: no row for what the Restore left: {}",
                    report.to_text()
                )
            });
        assert!(off.detail.contains("System State"), "{}", off.detail);
        if whole_restore {
            assert!(
                Withholding::withheld(root.path()).is_empty(),
                "the Restore left Desktop as is"
            );
        }
        InitialSetupAdmissionStopTest::restart(root.path())
            .unwrap()
            .choose_group(Group::Desktop.settings())
            .unwrap()
            .expect("the page admits the group turned on");
        doctor.explicit_apply(root.path()).unwrap();
        assert!(finished_at(root.path()).unwrap());
        assert_eq!(Withholding::withheld(root.path()), [WIDGETS]);
        doctor.grant();
        assert!(doctor.follow_up(root.path()).unwrap().is_some());
        assert!(doctor.notification_center_cleared());
        assert!(Withholding::withheld(root.path()).is_empty());
        let mut host = Fake::default();
        assert!(
            execute_initial(&mut host, root.path(), &mut None)
                .unwrap()
                .is_none()
        );
        assert!(host.calls.is_empty(), "{:?}", host.calls);
    }
}

/// The Widgets change a pass makes once Full Disk Access arrives changes the row as a stage
/// does, so it is fenced the same way (design §3.5): the row paints busy while it runs, a
/// reading taken meanwhile is dropped, and the row is read again once it ends.
#[test]
fn a_runs_deferred_widgets_change_is_fenced_from_readings() {
    use super::super::tests::Withholding;
    use std::sync::Mutex;
    static EVENTS: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
    fn stage(event: StageEvent) {
        EVENTS.lock().unwrap().push((
            event.running,
            event.settings.to_vec(),
            event.subjects.to_vec(),
        ));
    }
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Widgets, Setting::Permissions]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(Withholding::widgets());
    stages.grant_in_permissions = true;
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks {
            stage,
            ..SetupHooks::NONE
        },
    )
    .unwrap()
    .expect("the run executes");
    assert!(stages.doctor.notification_center_cleared());
    let widgets = (
        vec![Setting::Widgets],
        vec![Subject::Setting(Setting::Widgets)],
    );
    let events = EVENTS.lock().unwrap().clone();
    assert_eq!(
        events[events.len() - 2..],
        [
            (true, widgets.0.clone(), widgets.1.clone()),
            (false, widgets.0, widgets.1)
        ],
        "{events:?}"
    );
}

/// A Restore's put-back that waits for Full Disk Access lets go of a group a person set up
/// again since: once the grant comes, the follow-up makes setup's change, never the
/// Restore's.
#[test]
fn a_waiting_restore_lets_go_of_a_group_set_up_again_since() {
    use super::super::tests::Withholding;
    use aterm_types::macos_setup::Group;
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = Plan::from(group_selection(Group::Desktop));
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let mut stages = WithholdingStages::over(Withholding::widgets());
    stages.doctor.grant();
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the run executes");
    assert!(stages.doctor.notification_center_cleared());
    stages.doctor.revoke();
    stages.doctor.restore(root.path()).unwrap();
    assert_eq!(
        stages.doctor.owed(root.path()),
        Vec::<Setting>::new(),
        "nothing to do without the grant"
    );
    let revision = page_choice(root.path(), &plan, group_selection(Group::Desktop));
    execute_initial_for_revision(
        &mut stages,
        root.path(),
        revision,
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("the chosen run executes");
    stages.doctor.grant();
    let followed = stages.doctor.follow_up(root.path()).unwrap();
    assert!(
        followed.is_none_or(|report| report.restore.is_none()),
        "the Restore's put-back is let go"
    );
    assert!(stages.doctor.notification_center_cleared());
    assert!(
        super::super::restore_waiting(root.path())
            .unwrap()
            .is_empty()
    );
}

/// A Restore's put-back that waits for Full Disk Access survives an admission that never
/// turned its group to Set up, whatever it is: a choice of another group, Set up now of the
/// plan the page shows, or a Join (design §3.4: "put back by itself once aterm has the grant
/// ... unless a choice since turned its group to Set up"). The record stays; that the plan
/// owes no Mac setting decides only that the decision reads applied.
#[test]
fn a_waiting_restore_survives_any_admission_that_leaves_its_group_alone() {
    use super::super::tests::Withholding;
    use aterm_types::macos_setup::Group;
    for how in ["choice", "set-up-now", "join"] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut selection = group_selection(Group::Desktop);
        selection.set(Setting::Permissions, true);
        selection.set(Setting::Spotlight, true);
        let plan = Plan::from(selection);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        stages.doctor.grant();
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the run executes");
        assert!(stages.doctor.notification_center_cleared());
        stages.doctor.revoke();
        stages.doctor.restore(root.path()).unwrap();
        assert!(
            !super::super::restore_waiting(root.path())
                .unwrap()
                .is_empty(),
            "{how}: the Widgets put-back waits for Full Disk Access"
        );
        // The page shows the plan in force: Desktop & Dock left as is by the Restore.
        let shown = snapshot_at(root.path(), plan.clone()).unwrap().plan;
        assert!(!shown.selection.contains(Setting::Widgets), "{how}");
        let revision = match how {
            "choice" => {
                // Rust in Spotlight turned to Leave as is.
                let mut sent = shown.clone();
                sent.selection.set(Setting::Spotlight, false);
                page_choice(root.path(), &sent, group_selection(Group::Developer))
            }
            "set-up-now" => observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
                InitialSetupRestart::default()
                    .changing(Some(Selection::none()))
                    .persist_with(
                        root,
                        &shown,
                        false,
                        || Ok(None),
                        save_progress,
                        observe_restart,
                    )
            })
            .result
            .unwrap()
            .expect("admitted"),
            _ => system_state_join(root.path(), &shown),
        };
        assert_eq!(
            decision_at(root.path()).unwrap(),
            Decision::Applied(true),
            "{how}: the plan owes no Mac setting"
        );
        assert!(
            decision_record(root.path()).unwrap().1.is_some(),
            "{how}: the Restore's record stays"
        );
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            stages.doctor.notification_center_cleared(),
            "{how}: nothing was put back before the grant"
        );
        stages.doctor.grant();
        stages.doctor.follow_up(root.path()).unwrap();
        assert!(
            !stages.doctor.notification_center_cleared(),
            "{how}: the grant arrived, and the Restore's waiting Widgets put-back is done \
             (first-run: {})",
            std::fs::read_to_string(root.path().join("first-run.json")).unwrap()
        );
        assert!(
            super::super::restore_waiting(root.path())
                .unwrap()
                .is_empty(),
            "{how}"
        );
    }
}

/// A choice cut short anywhere between its first write and its admission (design §3.3)
/// leaves setup resumable: the next launch's review binds whatever the records hold, offers
/// the plan saved, and admits its answer measured from the run the choice left, so the
/// stages it left alone stay finished and the person never reads an engine error. A pass
/// never runs while the saved plan and the run's progress disagree.
#[test]
fn the_next_launch_after_a_choice_cut_short_anywhere_resumes_setup() {
    for cut in [
        Some(RestartWrite::Plan),
        Some(RestartWrite::Progress),
        Some(RestartWrite::Reset),
        Some(RestartWrite::Decision),
        None,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &Plan::default())
            .unwrap()
            .unwrap();
        let mut first = Fake {
            waiting: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut first,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(first.calls.contains(&Stage::Workstation));
        assert!(first.calls.contains(&Stage::Accounts));
        // The page's choice: Rust in Spotlight to Leave as is, its process ended before
        // `cut` (`None`: before its admission's own save).
        let mut sent = Plan::default();
        sent.selection.set(Setting::Spotlight, false);
        let staged = stage_restart_with(
            root.path(),
            &sent,
            Restart {
                chosen: true,
                joins: false,
                automatic: false,
            },
            AdmittedVault::None,
            save_progress,
            |write| {
                if Some(write) == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            },
        );
        assert_eq!(staged.is_ok(), cut.is_none(), "{cut:?}");
        if cut == Some(RestartWrite::Progress) {
            assert!(
                revision_at(root.path()).is_err(),
                "no pass runs on a plan its progress does not belong to"
            );
        }
        assert!(!finished_at(root.path()).unwrap(), "{cut:?}");
        if cut == Some(RestartWrite::Plan) {
            // Nothing landed: the next launch resumes the run the choice would have replaced.
            let admission = admission_at(root.path()).unwrap().expect("admitted");
            assert_eq!((admission.revision, admission.completed), (revision, false));
            let host = run_pass(root.path(), revision);
            assert_eq!(host.calls, [Stage::Secrets, Stage::Repositories]);
            continue;
        }
        // The next launch: the choice's run is not admitted, so it offers the plan saved.
        assert_eq!(admission_at(root.path()).unwrap(), None, "{cut:?}");
        let mut ticket = InitialSetupAuthorization::capture_at(root.path())
            .unwrap_or_else(|why| panic!("{cut:?}: the next launch's review binds: {why}"));
        let records = observation_at(root.path()).unwrap().records;
        assert_eq!(ticket.revision(), review_revision(&records).unwrap());
        let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
        assert_eq!(offered, sent, "{cut:?}: the plan saved is offered");
        let revision = ticket
            .persist_at(root.path(), &offered)
            .result
            .unwrap()
            .unwrap_or_else(|| panic!("{cut:?}: the answer is admitted"));
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Workstation) && !host.calls.contains(&Stage::Accounts),
            "{cut:?}: the Dock and the sign-outs stay as the first run left them: {:?}",
            host.calls
        );
        assert_eq!(
            host.calls,
            [Stage::Secrets, Stage::Repositories],
            "{cut:?}: the vault steps the first run left waiting run"
        );
    }
}

/// The startup's answer after a choice cut short once its staged writes landed, whatever the
/// page answered while the offer waited (a group choice, or a Join), is measured as a choice
/// on the page is (design §3.3): no Dock reset and no sign-out again for another group, and
/// a Join runs only the vault steps again.
#[test]
fn a_startup_answer_after_a_choice_cut_short_keeps_the_dock_and_sign_outs() {
    use aterm_types::macos_setup::Group;
    for join in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &Plan::default())
            .unwrap()
            .unwrap();
        let mut first = Fake {
            waiting: Some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut first,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(first.calls.contains(&Stage::Workstation));
        assert!(first.calls.contains(&Stage::Accounts));
        // On the page: Rust in Spotlight to Leave as is, cut short before its final save.
        let mut sent = Plan::default();
        sent.selection.set(Setting::Spotlight, false);
        let mut spotlight = Selection::none();
        spotlight.set(Setting::Spotlight, true);
        let cut = observe_mutation_at(root.path(), None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen()
                .changing(Some(spotlight))
                .persist_with(
                    root,
                    &sent,
                    false,
                    || Ok(None),
                    |_, _| Err("the process ended here".into()),
                    observe_restart,
                )
        });
        assert!(cut.result.is_err());
        // The next launch offers the saved plan; the person answers it.
        let mut ticket = InitialSetupAuthorization::capture_at(root.path()).unwrap();
        let offered = ticket.offer_reading(root.path(), || Ok(None)).unwrap();
        assert_eq!(offered, sent);
        let revision = if join {
            let mut typed = offered.clone();
            typed.vault_url = "https://vault.example/v1/team".into();
            ticket.persist_join_at(root.path(), &typed, None)
        } else {
            // Keyboard & Trackpad turned to Leave as is while the offer waited.
            let mut answer = offered.clone();
            for setting in Group::Input.settings() {
                answer.selection.set(*setting, false);
            }
            ticket.persist_at(root.path(), &answer)
        }
        .result
        .unwrap()
        .expect("admitted");
        let host = run_pass(root.path(), revision);
        assert!(
            !host.calls.contains(&Stage::Accounts) && !host.calls.contains(&Stage::Workstation),
            "join={join}: stages run again {:?}",
            host.calls
        );
        assert_eq!(
            host.calls,
            [Stage::Secrets, Stage::Repositories],
            "join={join}"
        );
    }
}

/// A person's Stop is never lost to a Restore: one saved after the Restore wrote its run's
/// next revision still holds that run (it carries on the run the Stop named), so aterm's
/// permissions stay held after a relaunch, as before the Restore. A Stop that came after the
/// run finished stays spent, and a choice made after it starts a run the Stop does not hold.
#[test]
fn a_stop_saved_after_a_restore_wrote_its_revision_still_holds_the_run() {
    for finished in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = authorize_at(root.path(), &Plan::default())
            .unwrap()
            .unwrap();
        let mut first = Fake {
            waiting: (!finished).then_some(Stage::Secrets),
            ..Fake::default()
        };
        execute_initial_for_revision(
            &mut first,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap();
        assert!(
            snapshot_at(root.path(), Plan::default())
                .unwrap()
                .permissions_allowed
        );
        // Restore, while the person's Stop of `revision` is still being saved.
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert_eq!(
            progress(root.path()).unwrap().unwrap().revision,
            revision + 1
        );
        request_stop_at(root.path(), revision).unwrap();
        // The next launch.
        let held = !snapshot_at(root.path(), Plan::default())
            .unwrap()
            .permissions_allowed;
        assert_eq!(held, !finished, "finished: {finished}");
        assert_eq!(Records::read(root.path()).unwrap().stopped_run(), !finished);
        let before_fold = holds_at(root.path());
        assert_eq!(
            before_fold.stopped_revision,
            (!finished).then_some(revision)
        );
        fold_stop_request(root.path()).unwrap();
        assert_eq!(progress(root.path()).unwrap().unwrap().stopped, !finished);
        assert!(
            stop_request_at(root.path()).is_none(),
            "the request is spent"
        );
        assert_eq!(
            holds_at(root.path()),
            before_fold,
            "folding keeps the same Stop"
        );
        fold_stop_request(root.path()).unwrap();
        assert_eq!(
            holds_at(root.path()),
            before_fold,
            "folding again keeps it too"
        );
        // A choice after it runs as a run of its own.
        let choice = system_state_choice(root.path(), &leaving(&[]));
        assert!(!stopped(root.path()).unwrap());
        assert_eq!(revision_at(root.path()).unwrap(), choice);
        assert!(holds_at(root.path()).stopped_revision.is_none());
        request_stop_at(root.path(), choice).unwrap();
        let later = holds_at(root.path());
        assert_eq!(later.stopped_revision, Some(choice));
        for setting in [Setting::Messages, Setting::Repositories] {
            assert!(
                !before_fold.later(&before_fold, setting),
                "the old Stop is unchanged"
            );
            assert!(
                later.later(&before_fold, setting),
                "another revision was stopped"
            );
        }
        fold_stop_request(root.path()).unwrap();
        assert_eq!(
            holds_at(root.path()),
            later,
            "the later Stop keeps its identity"
        );
    }
}

/// A page choice made while another holds the profile lock for a moment (the ALab watch
/// writing its upgrade) waits for it and is then admitted; it is never dropped. Only a holder
/// that outlasts the wait refuses it, in the person's words.
#[test]
fn a_choice_waits_for_the_profile_lock_another_holds_briefly() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut sent = Plan::default();
    sent.selection.set(Setting::Spotlight, false);
    let choose = |root: &Path| {
        InitialSetupRestart::chosen().persist_with(
            root,
            &sent,
            false,
            || Ok(None),
            save_progress,
            observe_restart,
        )
    };
    let mut watch = Some(HeldLock::take(root.path()).unwrap());
    let mut waits = 0;
    let admitted = observe_choice_at(
        root.path(),
        None,
        || {
            waits += 1;
            // The watch's write ends.
            watch = None;
            true
        },
        choose,
    );
    assert_eq!(waits, 1);
    let revision = admitted.result.unwrap().expect("the choice is admitted");
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(saved_plan(root.path()).unwrap(), Some(sent.clone()));
    let _holder = HeldLock::take(root.path()).unwrap();
    let refused = observe_choice_at(root.path(), None, || false, choose);
    assert_eq!(refused.result, Err(SETUP_BUSY.to_string()));
    assert_eq!(revision_at(root.path()).unwrap(), revision);
}

/// A press that finds the profile lock another setup holds says so the moment it starts
/// waiting, before its first wait (System State design §3.3), so the page shows it waiting
/// from then, never a run going for the ten seconds the wait can last; a press that finds
/// the lock free says nothing.
#[test]
fn a_press_says_it_waits_the_moment_it_finds_the_lock_held() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::default()).unwrap();
    let mut sent = Plan::default();
    sent.selection.set(Setting::Dock, false);
    let said = Arc::new(AtomicUsize::new(0));
    let press = || {
        let said = Arc::clone(&said);
        InitialSetupRestart::chosen().noting_wait(move || {
            said.fetch_add(1, Ordering::AcqRel);
        })
    };
    let mut holder = Some(HeldLock::take(root.path()).unwrap());
    let mut waits = 0;
    let admitted = press().persist_at(
        root.path(),
        None,
        || {
            waits += 1;
            assert_eq!(
                said.load(Ordering::Acquire),
                1,
                "said before the first wait"
            );
            holder = None;
            true
        },
        &sent,
        false,
        || Ok(None),
    );
    assert!(admitted.result.unwrap().is_some(), "the choice is admitted");
    assert_eq!((said.load(Ordering::Acquire), waits), (1, 1));
    sent.selection.set(Setting::Spotlight, false);
    let again = press().persist_at(root.path(), None, || true, &sent, false, || Ok(None));
    assert!(
        again.result.unwrap().is_some(),
        "the next choice is admitted"
    );
    assert_eq!(said.load(Ordering::Acquire), 1, "a free lock says nothing");
}

/// The launch's mend of the vault's sync process runs only while the plan in force has the
/// Secrets vault at Set up (every group, before any plan is saved) and no person's Stop, on
/// record or asked for, nor an older aterm's Cancel holds its run. A Restore holds nothing:
/// it leaves credentials as they are.
#[test]
fn the_vault_sync_mend_runs_only_while_the_plan_in_force_sets_up_the_vault() {
    let root = aterm_tempfile::tempdir().unwrap();
    assert!(
        keeps_vault_sync_at(root.path()).unwrap(),
        "no plan saved yet"
    );
    authorize_at(root.path(), &Plan::default()).unwrap();
    assert!(keeps_vault_sync_at(root.path()).unwrap());
    let mut without = Plan::default();
    without.selection.set(Setting::Secrets, false);
    system_state_choice(root.path(), &without);
    assert!(
        !keeps_vault_sync_at(root.path()).unwrap(),
        "Secrets left as is"
    );
    let revision = system_state_choice(root.path(), &Plan::default());
    assert!(keeps_vault_sync_at(root.path()).unwrap());
    request_stop_at(root.path(), revision).unwrap();
    assert!(
        !keeps_vault_sync_at(root.path()).unwrap(),
        "a Stop asked for"
    );
    fold_stop_request(root.path()).unwrap();
    assert!(
        !keeps_vault_sync_at(root.path()).unwrap(),
        "a Stop on record"
    );
    system_state_choice(root.path(), &Plan::default());
    assert!(keeps_vault_sync_at(root.path()).unwrap());
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    assert!(
        keeps_vault_sync_at(root.path()).unwrap(),
        "a Restore holds nothing"
    );
    super::super::write_legacy_cancel_at(root.path());
    assert!(
        !keeps_vault_sync_at(root.path()).unwrap(),
        "an older aterm's Cancel"
    );
}

/// A Restore's put-back that waits for Full Disk Access outlives a run that leaves its group
/// at Leave as is (design §3.4): a choice of another group, or the run the Restore of an older
/// aterm's pending setup brings forward for the offer, which sets aterm's permissions and so
/// gets the grant itself. A Restore of an older aterm's verified setup keeps its permissions
/// with no run at all. Once granted the widget list is put back, once.
#[test]
fn a_waiting_restore_puts_back_after_a_run_that_leaves_its_group_as_is() {
    use super::super::tests::Withholding;
    use aterm_types::macos_setup::Group;
    for case in [
        "a choice",
        "an older verified setup",
        "an older pending setup",
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let mut plan = Plan::from(group_selection(Group::Desktop));
        plan.selection.set(Setting::Permissions, true);
        let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
        let mut stages = WithholdingStages::over(Withholding::widgets());
        stages.doctor.grant();
        execute_initial_for_revision(
            &mut stages,
            root.path(),
            revision,
            &mut None,
            SetupHooks::NONE,
        )
        .unwrap()
        .expect("the run executes");
        assert!(stages.doctor.notification_center_cleared());
        if case != "a choice" {
            // What an older aterm left: its plan and first-run record, and no run.
            std::fs::remove_file(root.path().join("initial-setup.json")).unwrap();
            save_automatic_plan(root.path(), Selection::all().workstation()).unwrap();
            save_workstation_decision(root.path(), case == "an older verified setup").unwrap();
        }
        stages.doctor.revoke();
        stages.doctor.restore(root.path()).unwrap();
        assert!(stages.doctor.notification_center_cleared(), "it waits");
        assert_eq!(super::super::restore_waiting(root.path()).unwrap().len(), 1);
        assert_eq!(
            bring_forward_restored_at(root.path()).unwrap().is_some(),
            case == "an older pending setup",
            "{case}: only a run the Restore staged waits for the offer"
        );
        let revision = match case {
            "a choice" => {
                let spotlight = selected(&[Setting::Spotlight]);
                Some(page_choice(root.path(), &spotlight, spotlight.selection))
            }
            "an older verified setup" => {
                assert!(permissions_allowed(root.path()), "{case}");
                assert_eq!(launch_startup(root.path()), None, "{case}: no run");
                None
            }
            _ => {
                stages.grant_in_permissions = true;
                Some(launch_startup(root.path()).expect("the offer admits the run"))
            }
        };
        let desktop = stages.doctor.window_manager_writes();
        if let Some(revision) = revision {
            let calls = stages.rest.calls.len();
            execute_initial_for_revision(
                &mut stages,
                root.path(),
                revision,
                &mut None,
                SetupHooks::NONE,
            )
            .unwrap()
            .expect("the run executes");
            let ran = &stages.rest.calls[calls..];
            assert_eq!(
                ran,
                if case == "a choice" {
                    [Stage::Spotlight]
                } else {
                    [Stage::Permissions]
                },
                "{case}"
            );
        }
        assert_eq!(
            stages.doctor.window_manager_writes(),
            desktop,
            "{case}: no preference set"
        );
        stages.doctor.grant();
        let report = stages.doctor.follow_up(root.path()).unwrap();
        let put_back = !stages.doctor.notification_center_cleared();
        if case == "an older pending setup" {
            assert!(report.is_none(), "the run's own follow-up put it back");
        } else {
            assert!(
                report.is_some_and(|report| report.restore.is_some()),
                "{case}: the follow-up puts it back"
            );
        }
        assert!(put_back, "{case}");
        assert!(
            super::super::restore_waiting(root.path())
                .unwrap()
                .is_empty()
        );
        let writes = stages.doctor.notification_center_writes();
        assert!(stages.doctor.follow_up(root.path()).unwrap().is_none());
        assert_eq!(stages.doctor.notification_center_writes(), writes, "once");
        assert!(finished_at(root.path()).unwrap());
    }
}

/// Whether aterm's own permission steps may run now, as the window reads the records.
fn permissions_allowed(root: &Path) -> bool {
    snapshot_at(root, Plan::default())
        .unwrap()
        .permissions_allowed
}

/// An admitted run of the default plan that waits on the vault: its preferences, Spotlight and
/// aterm's permissions are set, the rest still owed. Its revision.
fn admitted_run_waiting_on_the_vault(root: &Path) -> u64 {
    let revision = authorize_at(root, &Plan::default()).unwrap().unwrap();
    let mut first = Fake {
        waiting: Some(Stage::Secrets),
        ..Fake::default()
    };
    execute_initial_for_revision(&mut first, root, revision, &mut None, SetupHooks::NONE).unwrap();
    assert!(permissions_allowed(root));
    revision
}

/// A Restore of an admitted run, cut short before any of its writes, never withdraws aterm's
/// own permissions (design §3.4): the next launch, or Restore pressed again, finishes its
/// writes. The run keeps its admission and the Permissions approval it continues, the groups
/// the Restore put back read Leave as is, and nothing runs: no pass, no offer.
#[test]
fn a_restore_cut_short_at_any_write_keeps_aterms_permissions_and_runs_nothing_else() {
    for cut in [
        RestoreWrite::Record,
        RestoreWrite::Plan,
        RestoreWrite::Progress,
        RestoreWrite::Run,
    ] {
        for again in [false, true] {
            let case = format!(
                "cut before {cut:?}, then {}",
                if again {
                    "Restore again"
                } else {
                    "the next launch"
                }
            );
            let root = aterm_tempfile::tempdir().unwrap();
            let revision = admitted_run_waiting_on_the_vault(root.path());
            save_restore(root.path()).unwrap();
            let ended = leave_restored_as_is_with(root.path(), |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            assert!(ended.is_err(), "{case}");
            if again {
                save_restore(root.path()).unwrap();
                leave_restored_as_is(root.path()).unwrap();
            } else {
                bring_forward_restored_at(root.path()).unwrap();
                assert_eq!(launch_startup(root.path()), None, "{case}: nothing offered");
            }
            let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
            assert!(snapshot.permissions_allowed, "{case}");
            assert!(snapshot.finished, "{case}");
            assert_eq!(
                snapshot.access_revision, revision,
                "{case}: the same approval"
            );
            assert!(
                preferences(snapshot.plan.selection).is_empty(),
                "{case}: the restored groups are left as is"
            );
            assert!(!saved_plan_provisional(root.path()).unwrap(), "{case}");
            assert!(
                admission_at(root.path()).unwrap().is_none(),
                "{case}: no run resumes"
            );
            let mut host = Fake::default();
            assert!(
                execute_initial(&mut host, root.path(), &mut None)
                    .unwrap()
                    .is_none(),
                "{case}"
            );
            assert!(host.calls.is_empty(), "{case}");
            let whole = Records::read(root.path()).unwrap();
            assert_eq!(
                whole.restored.and_then(|restored| restored.run),
                Some(snapshot.revision),
                "{case}: the record names the run in force"
            );
            assert_eq!(
                bring_forward_restored_at(root.path()).unwrap(),
                None,
                "{case}"
            );
            assert_eq!(Records::read(root.path()).unwrap(), whole, "{case}: once");
        }
    }
}

/// A run number reserved past the run a Restore ends (a progress write that failed after its
/// reservation, say) is never used again: the Restore carries that run on as the number after
/// the reserved one, and its record names the run its progress holds, whether or not a write
/// was cut short, so the next launch finds nothing left to finish.
#[test]
fn a_restore_carries_its_run_past_a_reserved_run_number() {
    for cut in [
        None,
        Some(RestoreWrite::Record),
        Some(RestoreWrite::Plan),
        Some(RestoreWrite::Progress),
        Some(RestoreWrite::Run),
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = admitted_run_waiting_on_the_vault(root.path());
        revision::reserve(root.path(), revision + 5).unwrap();
        save_restore(root.path()).unwrap();
        if let Some(cut) = cut {
            let ended = leave_restored_as_is_with(root.path(), |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            });
            assert!(ended.is_err(), "{cut:?}");
        }
        leave_restored_as_is(root.path()).unwrap();
        let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
        assert_eq!(
            snapshot.revision,
            revision + 6,
            "{cut:?}: past the reserved number"
        );
        assert!(snapshot.permissions_allowed, "{cut:?}");
        assert_eq!(
            snapshot.access_revision, revision,
            "{cut:?}: the same approval"
        );
        let records = Records::read(root.path()).unwrap();
        assert_eq!(
            records.restored.and_then(|restored| restored.run),
            Some(snapshot.revision),
            "{cut:?}: the record names the run in force"
        );
        assert_eq!(
            bring_forward_restored_at(root.path()).unwrap(),
            None,
            "{cut:?}"
        );
        assert_eq!(
            Records::read(root.path()).unwrap(),
            records,
            "{cut:?}: once"
        );
    }
}

/// `aterm doctor --apply` after a Restore, cut short before any of its writes, never
/// withdraws aterm's own permissions: the run the Restore ended keeps its admission and the
/// approval it continues while the decision still reads restored, and where the explicit
/// plan landed ahead of its progress the next launch's offer admits it.
#[test]
fn aterm_doctor_apply_after_a_restore_keeps_aterms_permissions_wherever_it_is_cut_short() {
    for cut in [
        ExplicitWrite::Revision,
        ExplicitWrite::Decision,
        ExplicitWrite::Plan,
        ExplicitWrite::Progress,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        let revision = admitted_run_waiting_on_the_vault(root.path());
        save_restore(root.path()).unwrap();
        leave_restored_as_is(root.path()).unwrap();
        assert!(permissions_allowed(root.path()), "{cut:?}");
        let ended = save_explicit_workstation_plan_with(
            root.path(),
            Selection::all().workstation(),
            |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(ended.is_err(), "{cut:?}");
        if cut != ExplicitWrite::Progress {
            // The run in force is admitted and bound to the plan saved.
            assert!(permissions_allowed(root.path()), "{cut:?}");
            assert_eq!(
                snapshot_at(root.path(), Plan::default())
                    .unwrap()
                    .access_revision,
                revision,
                "{cut:?}: the same approval"
            );
        }
        bring_forward_restored_at(root.path()).unwrap();
        launch_startup(root.path());
        assert!(
            permissions_allowed(root.path()),
            "{cut:?}: after the next launch"
        );
    }
}

/// A Restore pressed while the launch's offer still waits for its answer, over a run whose
/// admission never landed, leaves no admitted run to carry aterm's own permissions. The
/// Restore brings it forward under its own lock, so the offer made again in this launch
/// admits a run that sets aterm's permissions alone: the restored groups stay at Leave as
/// is, and no sign-out, Spotlight or Dock step runs. So does a Restore that left that plan
/// as it was (aterm's own plan of Permissions alone). A Restore cut short before it brought
/// the setup forward is brought forward at the next launch, and so are the bring-forward's
/// writes cut short anywhere.
#[test]
fn a_restore_over_an_unanswered_offer_gets_aterms_permissions_from_the_offer() {
    let unadmitted = |root: &Path, plan: &Plan| {
        authorize_at(root, plan).unwrap();
        let mut staged = progress(root).unwrap().unwrap();
        staged.admitted = false;
        save_progress(root, staged).unwrap();
        assert!(!finished_at(root).unwrap(), "the launch offers the plan");
        save_restore(root).unwrap();
    };
    // The Restore's writes, cut short before it brought the setup forward.
    let restored_short = |root: &Path, plan: &Plan| {
        unadmitted(root, plan);
        leave_restored_as_is_with(root, |_| Ok(())).unwrap();
        assert!(finished_at(root).unwrap());
        assert!(!permissions_allowed(root), "until the next launch");
    };
    let mut permissions_only = Plan::from(Selection::none());
    permissions_only.selection.set(Setting::Permissions, true);
    for plan in [Plan::default(), permissions_only] {
        let root = aterm_tempfile::tempdir().unwrap();
        unadmitted(root.path(), &plan);
        leave_restored_as_is(root.path()).unwrap();
        assert!(!finished_at(root.path()).unwrap(), "the offer comes again");
        let before = Records::read(root.path()).unwrap();
        let staged = bring_forward_restored_at(root.path())
            .unwrap()
            .expect("the run waits for the offer");
        assert_eq!(
            Records::read(root.path()).unwrap(),
            before,
            "the Restore brought it forward itself"
        );
        assert!(
            !permissions_allowed(root.path()),
            "until the offer admits it"
        );
        let revision = launch_startup(root.path()).expect("the offer admits the run");
        assert!(revision > staged);
        let admitted = snapshot_at(root.path(), Plan::default()).unwrap();
        assert!(admitted.permissions_allowed);
        assert!(preferences(admitted.plan.selection).is_empty());
        let host = run_pass(root.path(), revision);
        assert_eq!(host.calls, [Stage::Permissions]);
        assert!(finished_at(root.path()).unwrap());
        assert!(permissions_allowed(root.path()));
        assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    }

    // The Restore saved the plan this stages already: the run it stages, its progress, then
    // the record.
    let reference = aterm_tempfile::tempdir().unwrap();
    restored_short(reference.path(), &Plan::default());
    let mut writes = Vec::new();
    bring_forward_restored_with(reference.path(), |write| {
        writes.push(write);
        Ok(())
    })
    .unwrap()
    .unwrap();
    assert_eq!(
        writes,
        [
            RestoreWrite::Run,
            RestoreWrite::Progress,
            RestoreWrite::Record
        ]
    );
    let whole = Records::read(reference.path()).unwrap();
    let brought = aterm_tempfile::tempdir().unwrap();
    unadmitted(brought.path(), &Plan::default());
    leave_restored_as_is(brought.path()).unwrap();
    assert_eq!(
        Records::read(brought.path()).unwrap(),
        whole,
        "the Restore's own bring-forward writes the same records"
    );
    for cut in [
        RestoreWrite::Run,
        RestoreWrite::Progress,
        RestoreWrite::Record,
    ] {
        let root = aterm_tempfile::tempdir().unwrap();
        restored_short(root.path(), &Plan::default());
        let _lock = HeldLock::take(root.path()).unwrap();
        let ended = bring_forward_restored_with(root.path(), |write| {
            if write == cut {
                Err("the process ended here".into())
            } else {
                Ok(())
            }
        });
        drop(_lock);
        assert!(ended.is_err(), "{cut:?}");
        assert!(finished_at(root.path()).unwrap(), "{cut:?}");
        assert_eq!(
            launch_startup(root.path()),
            None,
            "{cut:?}: nothing offered yet"
        );
        bring_forward_restored_at(root.path()).unwrap().unwrap();
        assert_eq!(Records::read(root.path()).unwrap(), whole, "{cut:?}");
    }
}

/// A person's Stop of the run a Restore left unadmitted holds aterm's permissions, and so does
/// a person's plan that leaves the Permissions group as is: neither is brought forward.
#[test]
fn a_stop_or_a_plan_without_permissions_is_never_brought_forward() {
    let stopped = aterm_tempfile::tempdir().unwrap();
    authorize_at(stopped.path(), &Plan::default()).unwrap();
    let mut staged = progress(stopped.path()).unwrap().unwrap();
    staged.admitted = false;
    staged.stopped = true;
    save_progress(stopped.path(), staged).unwrap();
    save_restore(stopped.path()).unwrap();
    leave_restored_as_is(stopped.path()).unwrap();
    let before = Records::read(stopped.path()).unwrap();
    assert_eq!(bring_forward_restored_at(stopped.path()).unwrap(), None);
    assert_eq!(Records::read(stopped.path()).unwrap(), before);
    assert_eq!(launch_startup(stopped.path()), None);
    assert!(!permissions_allowed(stopped.path()));

    let declined = aterm_tempfile::tempdir().unwrap();
    let mut plan = Plan::default();
    plan.selection.set(Setting::Permissions, false);
    authorize_at(declined.path(), &plan).unwrap();
    let mut staged = progress(declined.path()).unwrap().unwrap();
    staged.admitted = false;
    save_progress(declined.path(), staged).unwrap();
    save_restore(declined.path()).unwrap();
    leave_restored_as_is(declined.path()).unwrap();
    let before = Records::read(declined.path()).unwrap();
    assert_eq!(bring_forward_restored_at(declined.path()).unwrap(), None);
    assert_eq!(Records::read(declined.path()).unwrap(), before);
}

/// A Restore over a choice cut short after it saved its plan, before its progress, keeps the
/// admission of the run the progress records: aterm's permissions go on at once, and the
/// choice that never ran does not run.
#[test]
fn a_restore_over_a_choice_cut_short_keeps_aterms_permissions() {
    let root = aterm_tempfile::tempdir().unwrap();
    let revision = admitted_run_waiting_on_the_vault(root.path());
    let mut chosen = Plan::default();
    chosen.selection.set(Setting::Spotlight, false);
    let cut = stage_restart_with(
        root.path(),
        &chosen,
        Restart {
            chosen: true,
            joins: false,
            automatic: false,
        },
        AdmittedVault::Unknown,
        save_progress,
        |write| {
            if write == RestartWrite::Progress {
                Err("the process ended here".into())
            } else {
                Ok(())
            }
        },
    );
    assert!(cut.is_err());
    save_restore(root.path()).unwrap();
    leave_restored_as_is(root.path()).unwrap();
    let snapshot = snapshot_at(root.path(), Plan::default()).unwrap();
    assert!(snapshot.permissions_allowed);
    assert_eq!(snapshot.access_revision, revision);
    assert!(snapshot.finished);
    assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    assert_eq!(launch_startup(root.path()), None);
}

/// An older aterm's setup keeps no progress record: its plan (with no vault field) and its
/// decision say what its one pass did. The launch admits nothing over a finished one, and a
/// page choice over it, or the launch's offer over one whose sign-outs were never verified,
/// sets only what the plan admitted turns on: the Dock reset and the Messages and FaceTime
/// sign-outs that pass made never run again. aterm's own permissions, which that plan
/// predates, stay approved through the choice, since nobody turned them off.
#[test]
fn a_choice_after_an_older_aterms_setup_runs_only_what_it_turned_on() {
    let older = |root: &Path, verified: bool| {
        save_automatic_plan(root, Selection::all().workstation()).unwrap();
        write_json(
            &root.join("first-run.json"),
            &json!({"profile":PROFILE,"state":"applied","accounts_verified":verified}),
        )
        .unwrap();
        bring_forward_restored_at(root).unwrap();
    };
    // Finished: a page choice of one more group.
    let root = aterm_tempfile::tempdir().unwrap();
    older(root.path(), true);
    assert_eq!(
        launch_startup(root.path()),
        None,
        "a finished setup offers nothing"
    );
    assert!(permissions_allowed(root.path()));
    let mut plan = snapshot_at(root.path(), Plan::default()).unwrap().plan;
    assert!(
        plan.selection.contains(Setting::Permissions),
        "nobody turned them off"
    );
    plan.selection.set(Setting::Spotlight, true);
    let revision = system_state_choice(root.path(), &plan);
    assert!(permissions_allowed(root.path()), "the choice keeps them");
    let host = run_pass(root.path(), revision);
    assert_eq!(host.calls, [Stage::Spotlight, Stage::Permissions]);
    assert!(finished_at(root.path()).unwrap());

    // Applied with its sign-outs unverified: the launch's offer runs those, not the Dock.
    let root = aterm_tempfile::tempdir().unwrap();
    older(root.path(), false);
    let revision = launch_startup(root.path()).expect("the offer admits the plan");
    assert!(
        permissions_allowed(root.path()),
        "an automatic setup starts its permission steps"
    );
    let host = run_pass(root.path(), revision);
    assert_eq!(host.calls, [Stage::Permissions, Stage::Accounts]);
}

/// An older aterm's Cancel over its pending run, then `aterm doctor --apply`: the apply's
/// pass resets the Dock and signs out, and records those stages finished though the Cancel
/// holds the run's admission, so no launch runs them again.
#[test]
fn doctor_apply_over_a_cancel_leaves_the_next_launch_nothing_it_ran() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    crate::macos_doctor::save_automatic_plan(root, Selection::all().workstation()).unwrap();
    write_json(
        &root.join("initial-setup.json"),
        &json!({"profile":PROFILE,"state":"pending","done":0}),
    )
    .unwrap();
    crate::macos_doctor::write_legacy_cancel_at(root);
    doctor_apply(root);
    let calls = launch_startup(root).map(|revision| run_pass(root, revision).calls);
    assert!(
        calls.as_ref().is_none_or(|calls| {
            !calls.contains(&Stage::Workstation) && !calls.contains(&Stage::Accounts)
        }),
        "the next launch reset the Dock or signed out again: {calls:?}"
    );
}

/// `aterm doctor --apply`'s record writes around its pass ([`super::super::run_mutation`]):
/// the explicit plan, the pass's own completion record, then the explicit completion.
fn doctor_apply(root: &Path) {
    let explicit = save_explicit_workstation_plan(root, Selection::all().workstation()).unwrap();
    // The explicit pass set every Mac preference and signed out (`execute(Apply)`).
    record_applied(root, Selection::all().workstation()).unwrap();
    complete_explicit_workstation_plan_for_test(root, explicit).unwrap();
}

/// `aterm init`'s pass of the whole profile holds the records the first launch holds and
/// records what it verified, as `aterm doctor --apply` does: the next launch neither resets
/// the Dock nor signs out again. Without that record the launch's offer runs both.
#[test]
fn a_whole_init_pass_leaves_the_next_launch_nothing_it_ran() {
    use aterm_machines::init::Held as _;
    let ran_by_the_launch = |init: bool| {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        if init {
            let hold = super::super::init_hold_at(
                root,
                aterm_machines::init::Pass::Whole { clones: true },
                false,
                Some(&|line| panic!("nothing else holds the records: {line}")),
                complete_explicit_workstation_plan_for_test,
            )
            .unwrap();
            Box::new(hold).verified().unwrap();
        }
        launch_startup(root).map(|revision| run_pass(root, revision).calls)
    };
    let calls = ran_by_the_launch(true);
    assert!(
        calls.as_ref().is_none_or(|calls| {
            !calls.contains(&Stage::Workstation) && !calls.contains(&Stage::Accounts)
        }),
        "the next launch reset the Dock or signed out again: {calls:?}"
    );
    let calls = ran_by_the_launch(false).expect("a fresh Mac's launch offers its setup");
    assert!(
        calls.contains(&Stage::Workstation) && calls.contains(&Stage::Accounts),
        "{calls:?}"
    );
}

fn automatic_completion_account_retry(block_after_signout: bool) {
    use crate::macos_doctor::accounts_visibility_tests::{Account, system, system_after_signout};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let root = aterm_tempfile::tempdir().unwrap();
    // Secrets keeps the same admitted run unfinished after Accounts verifies.
    let plan = selected(&[Setting::Messages, Setting::Secrets]);
    let revision = authorize_at(root.path(), &plan).unwrap().unwrap();
    let _profile = HeldLock::take(root.path()).unwrap();
    let held = root.path().join(HELD_RECORD);
    let account = Arc::new(Account {
        signed_in: AtomicBool::new(true),
        unreadable: false,
        signouts: AtomicUsize::new(0),
    });
    if !block_after_signout {
        std::fs::create_dir(&held).unwrap();
    }
    let blocker = held.clone();
    let machine = system_after_signout(None, &account, move || {
        assert!(
            block_after_signout,
            "an unreadable cache must stop before the effect"
        );
        std::fs::create_dir(&blocker).unwrap();
    });
    let mut first = Fake {
        pending: Some(Stage::Secrets),
        native_accounts: Some((
            system(None, &account),
            NativeHost::over(
                Box::new(machine),
                Box::new(aterm_machines::init::prepare_workstation_selected),
                complete_explicit_workstation_plan_for_test,
            ),
        )),
        native_accounts_now: Some(std::time::SystemTime::now()),
        ..Fake::default()
    };
    let (report, _) =
        execute_initial_for_revision(&mut first, root.path(), revision, &mut None, AT_THE_PAGE)
            .unwrap()
            .unwrap();
    assert!(held.is_dir());
    assert!(
        report.rows.iter().any(|row| {
            row.name == Stage::Accounts.name()
                && row.status == "error"
                && row.detail.contains(HELD_RECORD)
        }),
        "{}",
        report.to_text()
    );
    assert_eq!(
        account.signouts.load(Ordering::SeqCst),
        usize::from(block_after_signout)
    );
    assert_eq!(
        first_run_complete(root.path()).unwrap(),
        block_after_signout
    );
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert!(
        !finished_at(root.path()).unwrap(),
        "Secrets is still pending"
    );

    std::fs::remove_dir(&held).unwrap();
    if block_after_signout {
        // The first pass verified the sign-out and published Applied. A person's
        // later login is not work for the cache write's retry to undo.
        assert!(!account.signed_in.load(Ordering::SeqCst));
        account.signed_in.store(true, Ordering::SeqCst);
    }
    let retry_at = super::super::automatic_gate_opens(root.path());
    let mut retry = Fake {
        pending: Some(Stage::Secrets),
        native_accounts: Some((
            system(None, &account),
            NativeHost::over(
                Box::new(system(None, &account)),
                Box::new(aterm_machines::init::prepare_workstation_selected),
                complete_explicit_workstation_plan_for_test,
            ),
        )),
        native_accounts_now: Some(retry_at),
        ..Fake::default()
    };
    execute_initial_for_revision(&mut retry, root.path(), revision, &mut None, AT_THE_PAGE)
        .unwrap()
        .unwrap();
    assert_eq!(revision_at(root.path()).unwrap(), revision);
    assert_eq!(
        account.signouts.load(Ordering::SeqCst),
        1,
        "retry the record, never repeat a verified sign-out"
    );
    assert_eq!(
        account.signed_in.load(Ordering::SeqCst),
        block_after_signout,
        "an earlier cache read error still leaves the account to be signed out"
    );
}

#[test]
fn automatic_completion_held_save_failure_preserves_a_later_login() {
    automatic_completion_account_retry(true);
}

#[test]
fn automatic_completion_held_read_failure_still_retries_the_unperformed_signout() {
    automatic_completion_account_retry(false);
}

#[test]
fn automatic_completion_partial_account_pass_does_not_complete_the_other_account() {
    let root = aterm_tempfile::tempdir().unwrap();
    let plan = selected(&[Setting::Messages, Setting::FaceTime, Setting::Secrets]);
    authorize_at(root.path(), &plan).unwrap().unwrap();
    let _profile = HeldLock::take(root.path()).unwrap();
    let before = progress(root.path()).unwrap().unwrap();
    record_applied(root.path(), selected(&[Setting::Messages]).selection).unwrap();
    assert_eq!(
        progress(root.path()).unwrap(),
        Some(before),
        "one completed account cannot credit the other account or an independent stage"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InitCompletion {
    BeforeVerification,
    AfterApplied,
    SummaryError,
    Complete,
}

fn init_completion_account_pass(cut: InitCompletion) -> (usize, bool) {
    use crate::macos_doctor::accounts_visibility_tests::{Account, system};
    use aterm_machines::init::Held as _;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let root = aterm_tempfile::tempdir().unwrap();
    let mut held = Some(
        super::super::init_hold_at(
            root.path(),
            aterm_machines::init::Pass::Whole { clones: true },
            false,
            Some(&|line| panic!("{line}")),
            complete_explicit_workstation_plan_for_test,
        )
        .unwrap(),
    );
    let account = Arc::new(Account {
        signed_in: AtomicBool::new(true),
        unreadable: false,
        signouts: AtomicUsize::new(0),
    });
    let native = || {
        NativeHost::over(
            Box::new(system(None, &account)),
            Box::new(aterm_machines::init::prepare_workstation_selected),
            complete_explicit_workstation_plan_for_test,
        )
    };
    if cut != InitCompletion::BeforeVerification {
        // The Desktop lane's real account items, with all subprocesses and
        // account state supplied by the same inert machine as the resumed pass.
        let mut desktop = native();
        desktop.select(Selection::all().workstation());
        let rows = desktop
            .workstation(WorkstationGroup::Accounts, true)
            .unwrap();
        assert!(rows.iter().all(|(_, row)| row_ready(row)), "{rows:?}");
        assert_eq!(account.signouts.load(Ordering::SeqCst), 1);
        assert!(!account.signed_in.load(Ordering::SeqCst));
        match cut {
            InitCompletion::AfterApplied => {
                // The actual first completion write of InitHold::verified;
                // the process ends before its final Progress write.
                record_applied(root.path(), Selection::all().workstation()).unwrap();
            }
            InitCompletion::SummaryError => {
                let before = progress(root.path()).unwrap().unwrap();
                let written =
                    record_applied_with(root.path(), Selection::all().workstation(), |root| {
                        let read = decision_record(root)?;
                        std::fs::rename(root.join("first-run.json"), root.join("first-run.saved"))
                            .unwrap();
                        std::fs::create_dir(root.join("first-run.json")).unwrap();
                        Ok(read)
                    });
                assert!(written.is_err(), "the actual summary publication must fail");
                std::fs::remove_dir(root.path().join("first-run.json")).unwrap();
                std::fs::rename(
                    root.path().join("first-run.saved"),
                    root.path().join("first-run.json"),
                )
                .unwrap();
                let mut expected = before;
                expected.done |= Stage::Workstation.bit() | Stage::Accounts.bit();
                assert_eq!(
                    progress(root.path()).unwrap(),
                    Some(expected),
                    "verified work survives a failed summary without changing other run fields"
                );
            }
            InitCompletion::Complete => Box::new(held.take().unwrap()).verified().unwrap(),
            InitCompletion::BeforeVerification => unreachable!(),
        }
        // A later login is the person's change, after the verified sign-out.
        account.signed_in.store(true, Ordering::SeqCst);
    }
    drop(held);
    assert!(
        !root.path().join(HELD_RECORD).exists(),
        "raw init keeps no automatic item cache"
    );
    let before = account.signouts.load(Ordering::SeqCst);
    let mut resumed = Fake {
        native_accounts: Some((system(None, &account), native())),
        ..Fake::default()
    };
    if let Some(revision) = launch_startup(root.path()) {
        let _profile = HeldLock::take(root.path()).unwrap();
        execute_initial_for_revision(&mut resumed, root.path(), revision, &mut None, AT_THE_PAGE)
            .unwrap()
            .expect("the actual admitted startup pass resumes");
    }
    (
        account.signouts.load(Ordering::SeqCst) - before,
        account.signed_in.load(Ordering::SeqCst),
    )
}

#[test]
fn init_completion_interrupted_after_applied_keeps_verified_signouts() {
    assert_eq!(
        init_completion_account_pass(InitCompletion::AfterApplied),
        (0, true),
        "the next launch must preserve the person's later login"
    );
}

#[test]
fn init_completion_summary_error_keeps_verified_signouts() {
    assert_eq!(
        init_completion_account_pass(InitCompletion::SummaryError),
        (0, true)
    );
}

#[test]
fn init_completion_success_keeps_verified_signouts() {
    assert_eq!(
        init_completion_account_pass(InitCompletion::Complete),
        (0, true)
    );
}

#[test]
fn init_completion_before_verification_still_runs_accounts() {
    assert_eq!(
        init_completion_account_pass(InitCompletion::BeforeVerification),
        (1, false)
    );
}

#[test]
fn init_completion_partial_pass_does_not_credit_the_whole_profile() {
    let root = aterm_tempfile::tempdir().unwrap();
    let _held = super::super::init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        false,
        Some(&|line| panic!("{line}")),
        complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    let mut expected = progress(root.path()).unwrap().unwrap();
    record_applied(root.path(), preferences(Selection::all())).unwrap();
    expected.done |= Stage::Workstation.bit();
    assert_eq!(
        progress(root.path()).unwrap(),
        Some(expected),
        "verified preferences cannot complete Accounts or change other run fields"
    );
}

/// `aterm init` behind a first-launch pass that holds the profile lock says one line and
/// waits; once the pass lets the lock go, init holds it and runs.
#[test]
fn init_waits_with_one_line_while_a_first_launch_pass_holds_the_records() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    let pass = super::super::HeldLock::take(root).unwrap();
    let (heard, said) = std::sync::mpsc::channel::<String>();
    let waiting = std::thread::scope(|threads| {
        let init = threads.spawn(|| {
            super::super::init_hold_at(
                root,
                aterm_machines::init::Pass::Part,
                false,
                Some(&|line| {
                    heard.send(line.to_owned()).unwrap();
                }),
                complete_explicit_workstation_plan_for_test,
            )
            .map(|_| ())
        });
        // A hang detector: init says it waits while the pass holds the records.
        let line = said
            .recv_timeout(std::time::Duration::from_secs(60))
            .expect("init said it waits");
        assert!(
            !init.is_finished(),
            "init ran while the pass held the records"
        );
        drop(pass);
        init.join().unwrap().unwrap();
        line
    });
    assert_eq!(waiting, super::super::INIT_WAITS);
    assert!(said.try_recv().is_err(), "one line, not one per look");
    assert!(super::super::HeldLock::try_take(root).unwrap().is_some());
}

/// `aterm init`'s grant rows name the app a grant must go to: Full Disk Access and
/// Accessibility were read for the app macOS holds responsible (Terminal, under Terminal),
/// Developer Tools from aterm's own entry; a grant read as unknown is unread, not missing.
#[test]
fn init_grant_rows_name_the_app_the_reading_measured() {
    use aterm_machines::init::Grant;
    use aterm_types::macos_setup::{Access, Standing};
    let how = |access, app| match init_grant(access, Standing::NeedsYou, None, app) {
        Grant::NeedsYou(how) => how,
        other => panic!("{access:?}: {other:?}"),
    };
    assert!(how(Access::FullDiskAccess, "Terminal").ends_with("switch on Terminal"));
    assert!(how(Access::Accessibility, "Terminal").ends_with("switch on Terminal"));
    assert!(how(Access::FullDiskAccess, "aterm").ends_with("switch on aterm"));
    assert!(how(Access::DeveloperTools, "Terminal").ends_with("switch on aterm"));
    assert_eq!(
        init_grant(Access::FullDiskAccess, Standing::InPlace, None, "Terminal"),
        Grant::On
    );
    assert!(matches!(
        init_grant(Access::Debugger, Standing::Unknown, None, "Terminal"),
        Grant::Unread(_)
    ));
}

/// `aterm init` over developer mode off whose password is not coming by itself (a person
/// declined, or the record cannot be read) says what the row says, aterm does not ask again
/// and the person's routes, never that aterm asks by itself.
/// NEGATIVE CONTROL: the same row while the ask comes is the elevation's wait, no step of the
/// person's.
#[test]
fn init_grant_says_developer_mode_is_not_asked_for_where_no_ask_comes() {
    use aterm_machines::init::Grant;
    use aterm_types::macos_setup::{Access, DEBUGGER_NOT_ASKED, Selection, Setting};
    let grant = |coming: bool| {
        let grants = access::Grants {
            debugger: access::off(coming),
            acquiring: true,
            ..access::Grants::fixture()
        };
        // As `init_grants` reads them: the permissions in the plan.
        let mut selection = Selection::none();
        selection.set(Setting::Permissions, true);
        let row = access::observation(Access::Debugger, &grants, selection);
        let why = if row.note.as_deref() == Some(DEBUGGER_NOT_ASKED) {
            row.note
        } else {
            row.detail.or(row.note)
        };
        init_grant(Access::Debugger, row.standing, why, "Terminal")
    };
    assert_eq!(grant(false), Grant::NeedsYou(DEBUGGER_NOT_ASKED.to_owned()));
    assert!(
        !matches!(grant(true), Grant::NeedsYou(_)),
        "{:?}",
        grant(true)
    );
}

/// `aterm doctor --apply` while a staged run waits for the launch's offer (here the run a
/// restored setup was brought forward to) resets the Dock and signs out in its own pass, and
/// admits that run at its last write with those stages finished: the next launch runs
/// aterm's permissions, never the Dock or the sign-outs again (design §3.4). A Cancel that
/// waits holds it: the apply admits nothing, and nothing runs at a launch.
#[test]
fn doctor_apply_over_a_staged_run_leaves_the_next_launch_nothing_it_did() {
    let root = aterm_tempfile::tempdir().unwrap();
    older_restored_setup(root.path(), Some(Selection::all().workstation()));
    bring_forward_restored_at(root.path())
        .unwrap()
        .expect("staged");
    // The offer waits for a focused window; the person runs `aterm doctor --apply` meanwhile.
    doctor_apply(root.path());
    assert!(
        permissions_allowed(root.path()),
        "`doctor --apply` starts selected permission steps"
    );
    let revision = launch_startup(root.path()).expect("the admitted run goes on");
    let host = run_pass(root.path(), revision);
    assert_eq!(host.calls, [Stage::Permissions]);
    assert!(finished_at(root.path()).unwrap());

    let cancelled = aterm_tempfile::tempdir().unwrap();
    authorize_at(
        cancelled.path(),
        &Plan::from(Selection::all().workstation()),
    )
    .unwrap();
    super::super::write_legacy_cancel_at(cancelled.path());
    let mut staged = progress(cancelled.path()).unwrap().unwrap();
    staged.admitted = false;
    save_progress(cancelled.path(), staged).unwrap();
    let explicit =
        save_explicit_workstation_plan(cancelled.path(), Selection::all().workstation()).unwrap();
    complete_explicit_workstation_plan_for_test(cancelled.path(), explicit).unwrap();
    assert!(!progress(cancelled.path()).unwrap().unwrap().admitted);
    assert!(!permissions_allowed(cancelled.path()));
}

/// After a whole Restore a person turns Desktop & Dock back to Set up, and the choice is cut
/// short at one of its writes before its decision ([`RestartWrite`]): the plan landed, the
/// decision still says restored. The next launch hands that choice to its offer, which
/// admits the plan the person chose and sets Desktop & Dock alone; it never puts the plan
/// back to the Restore's. A person's Stop of the staged choice holds it: nothing is offered
/// or run, and the plan stays theirs.
#[test]
fn a_choice_after_a_restore_cut_short_is_offered_at_the_next_launch() {
    use aterm_types::macos_setup::Group;
    let desktop = group_selection(Group::Desktop);
    let cut_short = |cut: RestartWrite| {
        let root = restored_after(&Plan::default());
        leave_restored_as_is(root.path()).unwrap();
        let mut chosen = snapshot_at(root.path(), Plan::default()).unwrap().plan;
        for setting in Group::Desktop.settings() {
            chosen.selection.set(*setting, true);
        }
        let ended = stage_restart_with(
            root.path(),
            &chosen,
            Restart {
                chosen: true,
                joins: false,
                automatic: false,
            },
            AdmittedVault::None,
            save_progress,
            |write| {
                if write == cut {
                    Err("the process ended here".into())
                } else {
                    Ok(())
                }
            },
        );
        assert!(ended.is_err(), "{cut:?}");
        (root, chosen)
    };
    for cut in [
        RestartWrite::Progress,
        RestartWrite::Reset,
        RestartWrite::Decision,
    ] {
        let (root, chosen) = cut_short(cut);
        bring_forward_restored_at(root.path())
            .unwrap()
            .expect("handed to the offer");
        assert_eq!(
            saved_plan(root.path()).unwrap(),
            Some(chosen.clone()),
            "{cut:?}"
        );
        let revision = launch_startup(root.path()).expect("the offer admits the choice");
        assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen), "{cut:?}");
        let host = run_pass(root.path(), revision);
        assert_eq!(host.only, [Some(desktop)], "{cut:?}");
        assert!(
            !host.calls.contains(&Stage::Accounts),
            "{cut:?}: {:?}",
            host.calls
        );
        assert!(finished_at(root.path()).unwrap(), "{cut:?}");
    }

    let (root, chosen) = cut_short(RestartWrite::Decision);
    request_stop_at(root.path(), revision_at(root.path()).unwrap()).unwrap();
    assert_eq!(bring_forward_restored_at(root.path()).unwrap(), None);
    assert_eq!(launch_startup(root.path()), None, "the Stop holds it");
    assert_eq!(saved_plan(root.path()).unwrap(), Some(chosen));
}

/// A launch whose bring-forward meets profile.lock held by another pass (the follow-up with
/// Full Disk Access a grant started, say) hears that it is busy, never that there is nothing
/// to bring forward, and brings the setup forward once the holder lets go: aterm's
/// permissions get their run in this launch.
#[test]
fn a_busy_profile_lock_holds_the_bring_forward_until_it_is_let_go() {
    let root = aterm_tempfile::tempdir().unwrap();
    older_restored_setup(root.path(), Some(Selection::all().workstation()));
    let busy = HeldLock::take(root.path()).unwrap();
    assert_eq!(
        bring_forward_waiting(root.path(), || false).unwrap(),
        BroughtForward::Busy
    );
    assert_eq!(launch_startup(root.path()), None, "nothing to offer yet");
    drop(busy);
    assert!(matches!(
        bring_forward_waiting(root.path(), || false).unwrap(),
        BroughtForward::Run(_)
    ));
    let revision = launch_startup(root.path()).expect("the offer admits the run");
    assert_eq!(run_pass(root.path(), revision).calls, [Stage::Permissions]);
    assert_eq!(
        bring_forward_waiting(root.path(), || false).unwrap(),
        BroughtForward::Nothing
    );
}

/// A newer aterm (a build beside this one, in the same state folder) saved a plan with a
/// group this build does not know, or progress with a stage it does not know: that record is
/// sound, so nothing is set aside and the person's plan stays as it is. This build reads it
/// as the newer aterm's ([`NEWER_SETUP`]) and changes none of it.
#[test]
fn a_newer_aterms_records_are_never_set_aside() {
    for case in [
        "plan with a new group",
        "progress with a new stage",
        "newer format",
    ] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let plan = plan_of(
            &[Setting::Permissions],
            &[aterm_types::macos_setup::Group::Desktop],
        );
        let revision = system_state_choice(root, &plan);
        run_pass(root, revision);
        assert!(finished_at(root).unwrap());
        if case == "plan with a new group" {
            write_json(
                &root.join("automatic-plan.json"),
                &json!({"profile":PROFILE,"selection":plan.selection.bits() | 1 << 12,
                        "vault_url":plan.vault_url}),
            )
            .unwrap();
        } else {
            let mut record: Value =
                aterm_json::from_slice(&std::fs::read(root.join("initial-setup.json")).unwrap())
                    .unwrap();
            let fields = record.as_object_mut().unwrap();
            if case == "newer format" {
                fields.insert("format".into(), Value::from(FORMAT + 1));
            } else {
                let done = fields["done"].as_u64().unwrap() | 1 << 6;
                fields.insert("done".into(), Value::from(done));
                fields.insert("stages".into(), Value::from(7));
            }
            write_json(&root.join("initial-setup.json"), &record).unwrap();
        }
        let before = [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json",
        ]
        .map(|name| std::fs::read(root.join(name)).unwrap());
        assert_eq!(
            set_aside_unreadable_setup_at(root, false).unwrap(),
            None,
            "{case}"
        );
        assert_eq!(
            [
                "automatic-plan.json",
                "initial-setup.json",
                "first-run.json"
            ]
            .map(|name| std::fs::read(root.join(name)).unwrap()),
            before,
            "{case}: nothing rewritten"
        );
        assert!(
            matches!(
                snapshot_at(root, Plan::default()),
                Err(InitialSetupSnapshotError::Newer)
            ),
            "{case}"
        );
        let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &plan,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result;
        assert!(chosen.is_err_and(|why| setup_is_newer(&why)), "{case}");
    }
}

/// A damaged record beside a newer aterm's: that setup is the newer aterm's, which no
/// set-aside touches, so every reader says so (the page's calm line, no read again by
/// itself), never that it reads the damaged record again, which would try a set-aside that
/// is refused, again and again.
#[test]
fn a_damaged_record_beside_a_newer_aterms_reads_as_the_newer_aterms() {
    for damaged in ["first-run.json", "automatic-plan.json"] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let plan = plan_of(&[Setting::Permissions], &[]);
        let revision = system_state_choice(root, &plan);
        run_pass(root, revision);
        let mut record: Value =
            aterm_json::from_slice(&std::fs::read(root.join("initial-setup.json")).unwrap())
                .unwrap();
        record
            .as_object_mut()
            .unwrap()
            .insert("format".into(), Value::from(FORMAT + 1));
        write_json(&root.join("initial-setup.json"), &record).unwrap();
        std::fs::write(root.join(damaged), CORRUPT).unwrap();
        assert!(
            matches!(
                snapshot_at(root, Plan::default()),
                Err(InitialSetupSnapshotError::Newer)
            ),
            "{damaged}"
        );
        assert!(
            matches!(
                snapshot_in_vault(root, Ok(None)),
                Err(InitialSetupSnapshotError::Newer)
            ),
            "{damaged}"
        );
        assert_eq!(
            set_aside_unreadable_setup_at(root, false).unwrap(),
            None,
            "{damaged}: nothing is set aside"
        );
        assert_eq!(std::fs::read(root.join(damaged)).unwrap(), CORRUPT);
    }
}

/// The run counter that every progress save reserves in first cannot be read: it is rebuilt,
/// so a later choice and `aterm doctor --apply` go on as before.
#[test]
fn a_damaged_run_counter_never_stops_a_later_choice() {
    for set_aside in [true, false] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        let revision = system_state_choice(root, &selected(&[Setting::Permissions]));
        run_pass(root, revision);
        std::fs::write(root.join("run-revision.json"), CORRUPT).unwrap();
        if set_aside {
            assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
            assert!(!revision::damaged(root).unwrap(), "rebuilt");
        }
        let more = selected(&[Setting::Permissions, Setting::Spotlight]);
        let chosen = observe_mutation_at(root, None, Err("busy".into()), |root| {
            InitialSetupRestart::chosen().persist_with(
                root,
                &more,
                false,
                || Ok(None),
                save_progress,
                observe_restart,
            )
        })
        .result;
        assert!(
            matches!(chosen, Ok(Some(later)) if later > revision),
            "{chosen:?}"
        );
        assert!(save_explicit_workstation_plan(root, Selection::all().workstation()).is_ok());
        assert!(root.join("run-revision.json.unreadable").exists());
    }
}

/// A finished set-aside's marker that cannot be read says nothing of a hold: with every
/// record sound, it is written again, finished, and aterm's permissions nobody stopped stay
/// set up.
#[test]
fn a_damaged_set_aside_marker_never_holds_what_nobody_stopped() {
    let scratch = aterm_tempfile::tempdir().unwrap();
    let root = scratch.path();
    system_state_choice(root, &selected(&[Setting::Permissions]));
    note_person_asked_at(root).unwrap();
    std::fs::write(root.join("initial-setup.json"), CORRUPT).unwrap();
    set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
    let revision = launch_startup(root).expect("the offer admits the recovered plan");
    run_pass(root, revision);
    assert!(finished_at(root).unwrap());
    let before = [
        "automatic-plan.json",
        "initial-setup.json",
        "first-run.json",
    ]
    .map(|name| std::fs::read(root.join(name)).unwrap());
    std::fs::write(root.join("set-aside.json"), CORRUPT).unwrap();
    assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
    assert_eq!(
        [
            "automatic-plan.json",
            "initial-setup.json",
            "first-run.json"
        ]
        .map(|name| std::fs::read(root.join(name)).unwrap()),
        before,
        "nothing set aside again"
    );
    assert!(
        saved_plan(root)
            .unwrap()
            .unwrap()
            .selection
            .contains(Setting::Permissions)
    );
    assert!(permissions_allowed(root));
    assert_eq!(
        std::fs::read(root.join("set-aside.json.unreadable")).unwrap(),
        CORRUPT
    );
    assert_eq!(set_aside_unreadable_setup_at(root, false).unwrap(), None);
}

/// Design §3.5: the set-aside's calm note goes once a person's choice is saved. A person's
/// answer to the startup's offer is one; the window's own answer of the plan it offered,
/// with no press, is not.
#[test]
fn a_persons_answer_to_the_offer_takes_the_set_aside_note_away() {
    for person in [true, false] {
        let scratch = aterm_tempfile::tempdir().unwrap();
        let root = scratch.path();
        system_state_choice(root, &selected(&[Setting::Permissions]));
        std::fs::write(root.join("initial-setup.json"), CORRUPT).unwrap();
        set_aside_unreadable_setup_at(root, false).unwrap().unwrap();
        assert!(set_aside::shown(root));
        let mut startup = InitialSetupAuthorization::capture_at(root).unwrap();
        let offered = startup.offer_reading(root, || Ok(None)).unwrap();
        let admitted = if person {
            let chosen = selected(&[Setting::Permissions, Setting::Spotlight]);
            assert_ne!(offered, chosen);
            startup.persist_at(root, &chosen)
        } else {
            startup.persist_offered_at(root, &offered)
        };
        admitted.result.unwrap().expect("the answer is admitted");
        assert_eq!(set_aside::shown(root), !person, "person: {person}");
    }
}

/// The concurrent Secrets lane waits for the root LAN broker, never for a person, and joins
/// the moment it answers while the Workstation pass is still running; a sequential host
/// joins after Workstation, and master-Off holds both paths before any vault read or join.
#[test]
fn secrets_join_starts_once_the_lan_broker_answers_in_a_lane_beside_the_workstation_pass() {
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[derive(Default)]
    struct LaneTimings {
        events: Vec<&'static str>,
        secrets_finished: bool,
        lane_checked: bool,
        post_off_checked: bool,
        swarm_off: bool,
        broker: bool,
        joined: bool,
        repositories_saw_joined: bool,
    }

    struct LaneFixture {
        parallel: bool,
        swarm: bool,
        turn_off_on_grant: bool,
        /// Workstation ends the pass with an error before the lane hears its grant.
        abort_before_grant: bool,
        state: Arc<(Mutex<LaneTimings>, Condvar)>,
    }

    struct ConcurrentSecrets {
        swarm: bool,
        state: Arc<(Mutex<LaneTimings>, Condvar)>,
    }

    impl SecretsLane for ConcurrentSecrets {
        fn swarm_enabled(&mut self) -> Result<bool, String> {
            let mut state = self.state.0.lock().unwrap();
            if state.swarm_off {
                state.post_off_checked = true;
                self.state.1.notify_all();
            }
            Ok(self.swarm && !state.swarm_off)
        }
        fn lan_unavailable(&mut self) -> Option<String> {
            let broker = self.state.0.lock().unwrap().broker;
            (!broker).then(|| "no root LAN broker answers".to_owned())
        }
        fn secrets(
            &mut self,
            _plan: &Plan,
            _replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            assert!(
                self.swarm && !self.state.0.lock().unwrap().swarm_off,
                "master-Off must never reach a join"
            );
            // The join spends the password it was given.
            password.take();
            let (lock, cvar) = &*self.state;
            {
                let mut st = lock.lock().unwrap();
                st.events.push("secrets-started");
                st.joined = true;
                st.events.push("secrets-finished");
                st.secrets_finished = true;
            }
            cvar.notify_all();
            Ok(vec![readiness_row(
                Stage::Secrets.name(),
                true,
                "joined in parallel lane".into(),
            )])
        }

        fn secrets_pace(&mut self) -> Option<Pace> {
            None
        }

        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            assert!(
                self.swarm && !self.state.0.lock().unwrap().swarm_off,
                "master-Off must not read the vault"
            );
            let joined = {
                let mut state = self.state.0.lock().unwrap();
                state.lane_checked = true;
                self.state.1.notify_all();
                state.joined
            };
            Ok(joined.then(|| ConfiguredVault {
                config: aterm_secrets::client::Config {
                    source: aterm_secrets::transport::Source::Url(
                        aterm_types::macos_setup::DEFAULT_VAULT_URL.into(),
                    ),
                    mirrors: Vec::new(),
                    name: "alab".into(),
                    signer: Some([7; 32]),
                    followed: Vec::new(),
                    host: "m99".into(),
                },
                home: PathBuf::from("/tmp/c3-lane-home"),
            }))
        }

        fn current_vault(&mut self, _plan: &Plan) -> Result<Vec<Row>, String> {
            Ok(vec![readiness_row(
                Stage::Secrets.name(),
                true,
                "vault verified".into(),
            )])
        }
    }

    impl InitialHost for LaneFixture {
        fn swarm_enabled(&mut self) -> Result<bool, String> {
            Ok(self.swarm && !self.state.0.lock().unwrap().swarm_off)
        }
        fn legacy(&mut self, _root: &Path) -> Result<Option<Report>, String> {
            unreachable!("modern plan")
        }

        fn workstation(
            &mut self,
            root: &Path,
            _only: Option<Selection>,
        ) -> Result<Option<Report>, String> {
            if self.abort_before_grant {
                // The pass then reads no progress after the step, an error it ends on.
                std::fs::remove_file(root.join("initial-setup.json")).unwrap();
                return Ok(Some(Report {
                    action: Action::Apply,
                    rows: vec![readiness_row(
                        Stage::Workstation.name(),
                        true,
                        "workstation preferences applied".into(),
                    )],
                    manual_steps: Vec::new(),
                    backup: root.join("original-preferences.json"),
                    restore: None,
                    page: None,
                }));
            }
            if self.turn_off_on_grant {
                let (lock, cvar) = &*self.state;
                let mut state = lock.lock().unwrap();
                let deadline = Instant::now() + Duration::from_secs(60);
                while !state.lane_checked {
                    let left = deadline.saturating_duration_since(Instant::now());
                    assert!(!left.is_zero(), "lane never checked the master switch");
                    state = cvar.wait_timeout(state, left).unwrap().0;
                }
                state.swarm_off = true;
            }
            // The broker coming up releases the lane while Workstation is still active.
            {
                let mut state = self.state.0.lock().unwrap();
                state.events.push("grant");
                state.broker = true;
            }
            access::wake_lane_waiters();
            if self.turn_off_on_grant {
                let (lock, cvar) = &*self.state;
                let mut state = lock.lock().unwrap();
                let deadline = Instant::now() + Duration::from_secs(60);
                while !state.post_off_checked {
                    let left = deadline.saturating_duration_since(Instant::now());
                    assert!(
                        !left.is_zero(),
                        "lane never rechecked master-Off after the grant"
                    );
                    state = cvar.wait_timeout(state, left).unwrap().0;
                }
            }

            if self.parallel && self.swarm && !self.turn_off_on_grant {
                let (lock, cvar) = &*self.state;
                let mut guard = lock.lock().unwrap();
                let deadline = Instant::now() + Duration::from_secs(60);
                while !guard.secrets_finished {
                    let wait = deadline.saturating_duration_since(Instant::now());
                    assert!(!wait.is_zero(), "parallel Secrets lane did not finish");
                    let (next, _) = cvar.wait_timeout(guard, wait).unwrap();
                    guard = next;
                }
            }

            // Checkpoint Workstation's progress mid-stage just as `apply_preferences_at`
            // does, verifying zero clobber with the concurrent Secrets lane.
            let mut saved = progress(root)?.expect("progress");
            saved.done |= Stage::Workstation.bit();
            save_progress(root, saved)?;

            self.state
                .0
                .lock()
                .unwrap()
                .events
                .push("workstation-finished");
            Ok(Some(Report {
                action: Action::Apply,
                rows: vec![readiness_row(
                    Stage::Workstation.name(),
                    true,
                    "workstation preferences applied".into(),
                )],
                manual_steps: Vec::new(),
                backup: root.join("original-preferences.json"),
                restore: None,
                page: None,
            }))
        }

        fn spotlight(&mut self) -> Result<Vec<Row>, String> {
            Ok(vec![readiness_row(
                Stage::Spotlight.name(),
                true,
                "spotlight verified".into(),
            )])
        }

        fn permissions(&mut self, _plan: &Plan, _revision: u64) -> Result<Vec<Row>, String> {
            Ok(vec![readiness_row(
                Stage::Permissions.name(),
                true,
                "permissions verified".into(),
            )])
        }

        fn secrets(
            &mut self,
            plan: &Plan,
            replaces: Replaces,
            password: &mut Option<SecretBytes>,
        ) -> Result<Vec<Row>, String> {
            ConcurrentSecrets {
                swarm: self.swarm,
                state: Arc::clone(&self.state),
            }
            .secrets(plan, replaces, password)
        }

        fn secrets_pace(&mut self) -> Option<Pace> {
            None
        }

        fn configured_vault(&mut self) -> Result<Option<ConfiguredVault>, String> {
            ConcurrentSecrets {
                swarm: self.swarm,
                state: Arc::clone(&self.state),
            }
            .configured_vault()
        }

        fn current_vault(&mut self, plan: &Plan) -> Result<Vec<Row>, String> {
            ConcurrentSecrets {
                swarm: self.swarm,
                state: Arc::clone(&self.state),
            }
            .current_vault(plan)
        }

        fn current_repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            self.repositories(vault)
        }

        fn repositories(&mut self, vault: Option<&Plan>) -> Result<Vec<Row>, String> {
            let joined = self.state.0.lock().unwrap().joined && vault.is_some();
            let master_on = self.swarm && !self.state.0.lock().unwrap().swarm_off;
            self.state.0.lock().unwrap().repositories_saw_joined = joined;
            Ok(vec![readiness_row(
                Stage::Repositories.name(),
                joined || !master_on,
                "repositories checked".into(),
            )])
        }

        fn accounts(&mut self, _root: &Path) -> Result<Option<Report>, String> {
            unreachable!("accounts unselected")
        }

        fn withheld(&mut self, _root: &Path) -> Result<Option<Report>, String> {
            Ok(None)
        }

        fn secrets_lane(&mut self) -> Option<Box<dyn SecretsLane>> {
            self.parallel.then(|| {
                Box::new(ConcurrentSecrets {
                    swarm: self.swarm,
                    state: Arc::clone(&self.state),
                }) as Box<dyn SecretsLane>
            })
        }
    }

    let chosen = selected(&[
        Setting::Dock,
        Setting::Permissions,
        Setting::Secrets,
        Setting::Repositories,
    ]);

    // Workstation's condition variable waits for Secrets completion, proving ordering
    // without assigning a latency budget to the machine running the test.
    let parallel_root = aterm_tempfile::tempdir().unwrap();
    authorize_at(parallel_root.path(), &chosen)
        .unwrap()
        .unwrap();
    let parallel_state = Arc::new((Mutex::new(LaneTimings::default()), Condvar::new()));
    let mut parallel_host = LaneFixture {
        parallel: true,
        swarm: true,
        turn_off_on_grant: false,
        abort_before_grant: false,
        state: Arc::clone(&parallel_state),
    };
    let (_, next) = execute_pass(
        &mut parallel_host,
        parallel_root.path(),
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("pass ran");
    assert_eq!(next, SetupNext::Done);
    let saved = progress(parallel_root.path()).unwrap().unwrap();
    assert_ne!(saved.done & Stage::Workstation.bit(), 0);
    assert_ne!(saved.done & Stage::Secrets.bit(), 0);
    assert_ne!(saved.done & Stage::Repositories.bit(), 0);
    let parallel_timings = parallel_state.0.lock().unwrap();
    assert_eq!(
        parallel_timings.events,
        [
            "grant",
            "secrets-started",
            "secrets-finished",
            "workstation-finished"
        ]
    );
    assert!(parallel_timings.repositories_saw_joined);
    drop(parallel_timings);

    // Negative control: a sequential host finishes Workstation before starting Secrets.
    let queued_root = aterm_tempfile::tempdir().unwrap();
    authorize_at(queued_root.path(), &chosen).unwrap().unwrap();
    let queued_state = Arc::new((Mutex::new(LaneTimings::default()), Condvar::new()));
    let mut queued_host = LaneFixture {
        parallel: false,
        swarm: true,
        turn_off_on_grant: false,
        abort_before_grant: false,
        state: Arc::clone(&queued_state),
    };
    execute_pass(
        &mut queued_host,
        queued_root.path(),
        &mut None,
        SetupHooks::NONE,
    )
    .unwrap()
    .expect("queued control ran");
    let queued_timings = queued_state.0.lock().unwrap();
    assert_eq!(
        queued_timings.events,
        [
            "grant",
            "workstation-finished",
            "secrets-started",
            "secrets-finished"
        ]
    );
    drop(queued_timings);

    // Both automatic paths respect master-Off before any vault read or join. Repositories
    // can still use local credentials; explicit manual join APIs were not changed.
    for (parallel, swarm, turn_off_on_grant) in [
        (false, false, false),
        (true, false, false),
        (true, true, true),
    ] {
        let off_root = aterm_tempfile::tempdir().unwrap();
        authorize_at(off_root.path(), &chosen).unwrap().unwrap();
        let state = Arc::new((Mutex::new(LaneTimings::default()), Condvar::new()));
        let mut host = LaneFixture {
            parallel,
            swarm,
            turn_off_on_grant,
            abort_before_grant: false,
            state: Arc::clone(&state),
        };
        let (report, next) = execute_pass(&mut host, off_root.path(), &mut None, SetupHooks::NONE)
            .unwrap()
            .expect("Off pass ran");
        assert_eq!(next, SetupNext::Done);
        assert!(
            report
                .rows
                .iter()
                .any(|row| row.detail == aterm_secrets::swarm::SWARM_OFF)
        );
        let recorded = state.0.lock().unwrap();
        assert!(!recorded.joined);
        assert_eq!(recorded.events, ["grant", "workstation-finished"]);
    }

    // An earlier stage's error, while the lane still waits for its grant, ends the pass:
    // that stage's fence still closes, and the join password the lane did not spend comes
    // back to the person's run. NEGATIVE CONTROL: a pass whose lane joined spent the password,
    // and closed each fence it opened too.
    static FENCES: Mutex<Vec<StageReading>> = Mutex::new(Vec::new());
    fn record_fence(event: StageEvent<'_>) {
        FENCES.lock().unwrap().push((
            event.running,
            event.settings.to_vec(),
            event.subjects.to_vec(),
        ));
    }
    let fenced = SetupHooks {
        stage: record_fence,
        ..SetupHooks::NONE
    };
    let typed = || Some(SecretBytes::from(b"typed-join-pass".as_slice()));
    let spent_root = aterm_tempfile::tempdir().unwrap();
    authorize_at(spent_root.path(), &chosen).unwrap().unwrap();
    let mut spent_host = LaneFixture {
        parallel: true,
        swarm: true,
        turn_off_on_grant: false,
        abort_before_grant: false,
        state: Arc::new((Mutex::new(LaneTimings::default()), Condvar::new())),
    };
    let mut spent = typed();
    execute_pass(&mut spent_host, spent_root.path(), &mut spent, fenced)
        .unwrap()
        .expect("the control pass ran");
    assert!(spent.is_none(), "the lane's join spent the password");
    let events = std::mem::take(&mut *FENCES.lock().unwrap());
    let opened = events.iter().filter(|event| event.0).count();
    assert!(opened > 1 && events.len() == 2 * opened, "{events:?}");
    let abort_root = aterm_tempfile::tempdir().unwrap();
    authorize_at(abort_root.path(), &chosen).unwrap().unwrap();
    let mut abort_host = LaneFixture {
        parallel: true,
        swarm: true,
        turn_off_on_grant: false,
        abort_before_grant: true,
        state: Arc::new((Mutex::new(LaneTimings::default()), Condvar::new())),
    };
    let mut unspent = typed();
    let abort = execute_pass(&mut abort_host, abort_root.path(), &mut unspent, fenced)
        .expect_err("a pass that loses its progress ends on an error");
    assert!(abort.contains("setup progress changed"), "{abort}");
    assert_eq!(
        unspent.as_ref().map(SecretBytes::as_slice),
        Some(b"typed-join-pass".as_slice()),
        "an unspent join password outlives an earlier stage's error"
    );
    let events = std::mem::take(&mut *FENCES.lock().unwrap());
    assert_eq!(
        events.iter().map(|event| event.0).collect::<Vec<_>>(),
        [true, false],
        "{events:?}"
    );
    assert_eq!((&events[0].1, &events[0].2), (&events[1].1, &events[1].2));
    assert!(events[0].1.contains(&Setting::Dock), "{events:?}");
}

/// The explicit plan a whole-profile `aterm init` and `aterm doctor --apply` save
/// ([`save_explicit_workstation_plan`]) keeps the Permissions group the person turned to
/// Leave as is: the run's own line says it was left as they set it, and so it stays.
#[test]
fn an_explicit_pass_keeps_permissions_the_person_turned_off() {
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::from(Selection::all())).unwrap();
    let mut theirs = Selection::all();
    theirs.set(Setting::Permissions, false);
    restart_at(root.path(), &Plan::from(theirs)).unwrap();
    assert!(holds_at(root.path()).off.contains(Setting::Permissions));
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let plan = saved_plan(root.path()).unwrap().unwrap();
    assert!(
        !plan.selection.contains(Setting::Permissions),
        "an explicit pass turned the person's Permissions switch back on: {:?}",
        plan.selection
    );
}

/// Records a newer aterm saved hold every group for `aterm init`, whose line then says why
/// in plain words: a newer aterm set this Mac up, and this one leaves its settings alone; not
/// that the records cannot be read, and not that Set up now goes on.
#[test]
fn a_newer_records_line_says_so_plainly() {
    let root = aterm_tempfile::tempdir().unwrap();
    write_json(
        &root.path().join("automatic-plan.json"),
        &json!({
            "profile": PROFILE,
            "selection": Selection::all().bits(),
            "vault_url": aterm_types::macos_setup::DEFAULT_VAULT_URL,
            "format": 2,
        }),
    )
    .unwrap();
    let holds = holds_at(root.path());
    assert!(holds.newer && holds.holds(Setting::Dock), "{holds:?}");
    let line = holds.line(&[Setting::Dock], true).expect("a held line");
    assert!(
        line.contains("a newer aterm set this Mac up")
            && !line.contains("cannot be read")
            && !line.contains("Set up now"),
        "{line}"
    );
}

/// On a Mac with no setup records (aterm.app never ran: a CLI-only Mac), the plan a whole
/// `aterm init` or `aterm doctor --apply` saves is aterm's own, the base plan with what it
/// ran: the vault stays in sync and nothing reads it as the person's Leave as is.
#[test]
fn an_explicit_pass_on_a_mac_with_no_records_keeps_the_vault_in_sync() {
    let root = aterm_tempfile::tempdir().unwrap();
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let plan = saved_plan(root.path()).unwrap().unwrap();
    assert!(
        keeps_vault_sync_at(root.path()).unwrap(),
        "plan {:?}, held {:?}",
        plan.selection,
        holds_at(root.path())
    );
    assert!(
        plan.selection.contains(Setting::Secrets),
        "{:?}",
        plan.selection
    );
    assert!(saved_plan_provisional(root.path()).unwrap());
}

/// A whole `aterm init` never turns on, in System State's plan, a group the person left as
/// is there: the plan it saves keeps that group off (a flag naming it sets it up this once),
/// and the run it admits stands. A group they leave as is while init's other lanes run
/// holds the run from then: it records no whole pass over it. NEGATIVE CONTROL: with no new
/// choice, the run held again stands.
#[test]
fn a_whole_init_keeps_the_groups_the_person_left_as_is() {
    use aterm_machines::init::Held as _;

    let hold = |root: &Path| {
        let mut chosen = Plan::from(Selection::all());
        chosen.selection.set(Setting::Dock, false);
        authorize_at(root, &chosen).unwrap();
        let held = super::super::init_hold_at(
            root,
            aterm_machines::init::Pass::Whole { clones: true },
            true,
            Some(&|line| panic!("{line}")),
            complete_explicit_workstation_plan_for_test,
        )
        .unwrap();
        let plan = saved_plan(root).unwrap().unwrap();
        assert!(
            !plan.selection.contains(Setting::Dock),
            "{:?}",
            plan.selection
        );
        assert!(holds_at(root).off.contains(Setting::Dock));
        assert!(
            held.run.is_some(),
            "the Dock left as is holds no other group"
        );
        held
    };
    let root = aterm_tempfile::tempdir().unwrap();
    let mut held = hold(root.path());
    held.release();
    // The person leaves the widgets as they are while init's clones run.
    let mut now = saved_plan(root.path()).unwrap().unwrap();
    now.selection.set(Setting::Widgets, false);
    save_plan(root.path(), &now).unwrap();
    assert!(holds_at(root.path()).off.contains(Setting::Widgets));
    held.again(Some(&|line| panic!("{line}"))).unwrap();
    assert!(
        held.run.is_none(),
        "a group left as is mid-run holds the run"
    );

    let control = aterm_tempfile::tempdir().unwrap();
    let mut held = hold(control.path());
    held.release();
    held.again(Some(&|line| panic!("{line}"))).unwrap();
    assert!(held.run.is_some(), "nothing new holds the run");
}

/// `aterm init` that builds no preference (no terminal on an ALab Mac, a person at the
/// screen) holds the records as a part pass ([`aterm_machines::init::Pass::Part`]): over a
/// run the window finished, its sign-outs' checkpoint leaves the finished Mac settings stage
/// as it is, so the window's next pass does not run the Dock again.
#[test]
fn a_sign_out_only_part_hold_keeps_the_finished_mac_settings_stage() {
    use aterm_machines::init::Held as _;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::from(Selection::all())).unwrap();
    let mut finished = progress(root.path()).unwrap().unwrap();
    finished.done = Stage::ALL.iter().fold(0, |done, stage| done | stage.bit());
    finished.completed = true;
    save_progress(root.path(), finished).unwrap();
    let held = super::super::init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Part,
        true,
        Some(&|line| panic!("{line}")),
        complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    let mut sign_outs = Selection::none();
    sign_outs.set(Setting::Messages, true);
    sign_outs.set(Setting::FaceTime, true);
    held.checkpoint(sign_outs).unwrap();
    drop(held);
    let after = progress(root.path()).unwrap().unwrap();
    assert!(
        after.done & Stage::Workstation.bit() != 0 && after.completed,
        "a sign-out-only init undid the window's finished Mac settings stage: {after:?}"
    );
}

/// init sets the preferences under its first hold and lets the records go while its clones
/// run; the person presses Set up now (a new run of the plan in force, which sets every key
/// again). init's second hold reads no Stop and no group turned off, yet the new run's Mac
/// settings stage does not read done from init's pass, which ran before that run began.
#[test]
fn a_run_restarted_while_init_let_the_records_go_is_not_marked_done_by_init() {
    use aterm_machines::init::Held as _;
    let root = aterm_tempfile::tempdir().unwrap();
    authorize_at(root.path(), &Plan::from(Selection::all())).unwrap();
    let mut held = super::super::init_hold_at(
        root.path(),
        aterm_machines::init::Pass::Whole { clones: true },
        true,
        Some(&|line| panic!("{line}")),
        complete_explicit_workstation_plan_for_test,
    )
    .unwrap();
    held.checkpoint(preferences(Selection::all())).unwrap();
    held.release();
    let plan = saved_plan(root.path()).unwrap().unwrap();
    let before = progress(root.path()).unwrap().unwrap().revision;
    let restarted = restart_at(root.path(), &plan).unwrap();
    assert_ne!(restarted, before);
    assert_eq!(
        progress(root.path()).unwrap().unwrap().done & Stage::Workstation.bit(),
        0,
        "precondition: Set up now's run sets the preferences again"
    );
    held.again(Some(&|line| panic!("{line}"))).unwrap();
    held.checkpoint(Selection::all().workstation()).unwrap();
    Box::new(held).verified().unwrap();
    let after = progress(root.path()).unwrap().unwrap();
    assert_eq!(after.revision, restarted);
    assert_eq!(
        after.done & Stage::Workstation.bit(),
        0,
        "init marked the run Set up now started as having set the preferences: {after:?}"
    );
}

/// An older aterm's plan with a run on record (it predates the Permissions group, so it
/// presents aterm's own permissions on): a whole `aterm init` or `aterm doctor --apply`
/// starts its explicit plan from that presented plan, never from the bits on disk, so it
/// keeps aterm's permissions on, which nobody turned off.
#[test]
fn an_explicit_pass_over_an_older_run_keeps_aterms_permissions() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut legacy = Selection::all();
    legacy.set(Setting::Permissions, false);
    authorize_at(root.path(), &Plan::from(legacy)).unwrap();
    write_json(
        &root.path().join("automatic-plan.json"),
        &stamped(json!({ "profile": PROFILE, "selection": legacy.bits() })),
    )
    .unwrap();
    assert!(
        progress(root.path()).unwrap().is_some(),
        "a run is on record"
    );
    assert!(
        !holds_at(root.path()).off.contains(Setting::Permissions),
        "the older plan presents aterm's permissions on"
    );
    save_explicit_workstation_plan(root.path(), Selection::all().workstation()).unwrap();
    let holds = holds_at(root.path());
    assert!(
        !holds.off.contains(Setting::Permissions),
        "the explicit pass turned aterm's permissions to Leave as is: {holds:?}"
    );
}
