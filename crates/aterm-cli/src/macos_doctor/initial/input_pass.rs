// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Resumption of an already-admitted input task, without admitting another setup plan.

use super::*;

/// The exact modern plan and revision whose unheld Caps setting is still owed. Construction
/// reads canonical records; callers cannot create authority by supplying a selection.
#[derive(Clone, PartialEq, Eq)]
pub struct PendingInputAdmission {
    plan: Plan,
    revision: u64,
    selected: Selection,
}

impl PendingInputAdmission {
    /// The existing run, unchanged by this scoped execution target.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

impl std::fmt::Debug for PendingInputAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingInputAdmission")
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// The input task's result. `applied` describes this task alone; the original Workstation
/// stage and whole plan can still owe unrelated settings or stages.
pub struct InitialInputPass {
    /// Native rows for this scoped pass alone.
    pub report: Report,
    /// The canonical coordinator's next action for the scoped task.
    pub next: SetupNext,
    /// Caps has actual accepted per-setting evidence in the original revision.
    pub applied: bool,
}

fn capture(root: &Path) -> Result<Option<PendingInputAdmission>, String> {
    let records = observation_at(root).map_err(|why| why.to_string())?.records;
    let Some(revision) = pending_inputs::caps_revision(root, &records) else {
        return Ok(None);
    };
    let Some((plan, true)) = records.plan else {
        return Ok(None);
    };
    let mut selected = Selection::none();
    selected.set(Setting::CapsLock, true);
    Ok(Some(PendingInputAdmission {
        plan,
        revision,
        selected,
    }))
}

/// Read only this account's current admitted input task. Completed, held, stopped, unqueued
/// Restore debt, unreadable and legacy selections grant no pending-task authority.
pub fn initial_setup_pending_inputs() -> Result<Option<PendingInputAdmission>, String> {
    ensure_person_scope()?;
    capture(&state_root()?)
}

fn run_with(
    host: &mut dyn InitialHost,
    root: &Path,
    expected: &PendingInputAdmission,
    hooks: SetupHooks,
) -> Result<Option<InitialInputPass>, String> {
    if capture(root)?.as_ref() != Some(expected) || (hooks.stop)(expected.revision) {
        return Ok(None);
    }
    let Some((report, next)) = execute_initial_for_revision_target(
        host,
        root,
        expected.revision,
        &mut None,
        hooks,
        PassTarget::Inputs(expected.selected),
    )?
    else {
        return Ok(None);
    };
    let applied = input_activation::revision_at(root)? == Some(expected.revision);
    Ok(Some(InitialInputPass {
        report,
        next,
        applied,
    }))
}

/// Run only the currently admitted owed input settings through the canonical coordinator.
/// The profile lock and a fresh exact ticket check fence every pass; no plan is published or
/// widened, and no Accounts, Secrets, permission or deferred-container effect runs here.
pub fn run_initial_input_pass(
    expected: &PendingInputAdmission,
    hooks: SetupHooks,
) -> InitialSetupObserved<Option<InitialInputPass>> {
    if aterm_types::dirs::runs_under_private_state_root() {
        return InitialSetupObserved::new(Ok(None), None);
    }
    observe_person_mutation(Ok(None), |root| {
        run_with(&mut LiveInitial::live(), root, expected, hooks)
    })
}

fn retry_deadline(root: &Path, expected: &PendingInputAdmission) -> Option<u64> {
    let build = aterm_stamp::build_id();
    let record = Automatic::load(root, expected.revision).ok()?;
    record
        .failed
        .iter()
        .filter(|failed| {
            failed.build == build
                && failed.tries == 1
                && !record.settled.contains(&failed.item)
                && aterm_machines::init_preferences::setting_of(&failed.item)
                    .is_some_and(|setting| expected.selected.contains(setting))
        })
        .map(|failed| failed.at.saturating_add(SECOND_TRY_AFTER.as_secs()))
        .min()
}

pub(super) fn park_mark(
    root: Option<&Path>,
    expected: &PendingInputAdmission,
    now: u64,
) -> InitialSetupParkMark {
    let current = root
        .and_then(|root| capture(root).ok().flatten())
        .filter(|current| current == expected);
    let run = current.as_ref().map(|current| current.revision);
    let deadline = root.and_then(|root| {
        current
            .as_ref()
            .and_then(|current| retry_deadline(root, current))
    });
    InitialSetupParkMark {
        vault: None,
        network_run: None,
        materialization_run: None,
        materialization: None,
        subscriber_run: None,
        subscriber: None,
        join_run: None,
        network: None,
        git_run: None,
        git: None,
        repository_inputs: None,
        workstation_run: run,
        console: None,
        keyboard_run: run,
        keyboards: None,
        readable_keyboards: InputServiceSet::default(),
        retry_due: None,
        mac_settings_retry_due: deadline.filter(|deadline| *deadline <= now),
        retry_ahead: deadline.filter(|deadline| *deadline > now),
        broker_unready: None,
        in_settings: false,
    }
}

/// The scoped worker's own facts and existing Caps retry deadline. No vault, network,
/// account, private application or unrelated preference input is observed.
pub fn initial_setup_input_park_mark(expected: &PendingInputAdmission) -> InitialSetupParkMark {
    let root = ensure_person_scope().and_then(|_| state_root()).ok();
    let sys = aterm_machines::port::Live::new();
    park_mark(root.as_deref(), expected, ledger::now_secs())
        .with_console(|| Some(!aterm_machines::console::remote(&sys)))
        .with_keyboard_services(&sys)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn capture_at(root: &Path) -> Result<Option<PendingInputAdmission>, String> {
    capture(root)
}

#[cfg(any(test, debug_assertions))]
pub(super) fn run_at(
    host: &mut dyn InitialHost,
    root: &Path,
    expected: &PendingInputAdmission,
    hooks: SetupHooks,
) -> Result<Option<InitialInputPass>, String> {
    run_with(host, root, expected, hooks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admit<'a>(root: &'a Path, extra: &[Setting]) -> (InitialSetupAdmissionStopTest<'a>, u64) {
        let mut selection = Selection::none();
        for setting in std::iter::once(&Setting::CapsLock).chain(extra) {
            selection.set(*setting, true);
        }
        let mut setup = InitialSetupAdmissionStopTest::startup(root)
            .unwrap()
            .with_plan(Plan::from(selection));
        let revision = setup
            .persist(InitialSetupAdmissionFault::None)
            .unwrap()
            .unwrap();
        (setup, revision)
    }

    #[test]
    fn scoped_caps_checkpoints_only_its_setting_and_leaves_other_work_owed() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, revision) = admit(
            dir.path(),
            &[Setting::Dock, Setting::Permissions, Setting::Secrets],
        );
        let before = observation_at(dir.path()).unwrap();
        let ticket = setup.pending_inputs().unwrap().unwrap();
        let pass = setup
            .native_input_pass(
                &ticket,
                || Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[])),
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
        assert!(pass.applied, "{}", pass.report.to_text());
        assert!(
            pass.report.rows.iter().all(|row| row.name.contains("Caps")),
            "{}",
            pass.report.to_text()
        );
        let after = observation_at(dir.path()).unwrap();
        assert_eq!(after.records.plan, before.records.plan);
        let progress = after.records.progress.unwrap();
        assert_eq!(progress.revision, revision);
        assert_eq!(
            progress.done, 0,
            "a Caps-only read never completes all Workstation settings"
        );
        assert!(!progress.completed);
        let only = progress.only.unwrap();
        assert!(only.contains(Setting::Dock));
        assert!(!only.contains(Setting::CapsLock));
        assert_eq!(setup.input_activation_revision().unwrap(), Some(revision));
        assert!(setup.pending_inputs().unwrap().is_none());
    }

    #[test]
    fn scoped_caps_unavailable_does_not_borrow_completion_or_other_effects() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let (setup, _) = admit(dir.path(), &[Setting::Dock, Setting::Permissions]);
        let ticket = setup.pending_inputs().unwrap().unwrap();
        let pass = setup
            .native_input_pass(
                &ticket,
                || Box::new(InitialSetupAdmissionStopTest::keyboard_system(false, &[])),
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
        assert!(!pass.applied);
        assert_eq!(pass.next, SetupNext::Retry);
        assert_eq!(setup.pending_inputs().unwrap().as_ref(), Some(&ticket));
        assert_eq!(setup.input_activation_revision().unwrap(), None);
        assert_eq!(progress(dir.path()).unwrap().unwrap().done, 0);
    }

    #[test]
    fn scoped_caps_preserves_an_unrelated_interrupted_stage() {
        let dir = aterm_tempfile::tempdir().unwrap();
        let root = dir.path();
        let (setup, revision) = admit(root, &[Setting::Secrets]);
        let mut saved = progress(root).unwrap().unwrap();
        saved.running = Some(Stage::Secrets);
        save_progress(root, saved).unwrap();
        ledger::update(root, revision, |ledger| {
            assert!(matches!(
                ledger.begin(
                    Stage::Secrets,
                    0,
                    ledger::Stamp {
                        boot: None,
                        at: 1,
                        awake: None,
                        run: None,
                        software: None,
                    },
                    root
                ),
                ledger::Admit::Run
            ));
        })
        .unwrap();
        let before = ledger::load(root, revision).ledger;
        let ticket = setup.pending_inputs().unwrap().unwrap();
        let pass = setup
            .native_input_pass(
                &ticket,
                || Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[])),
                SetupHooks::NONE,
            )
            .unwrap()
            .unwrap();
        assert!(pass.applied);
        let after = ledger::load(root, revision).ledger;
        assert_eq!(
            after.running & Stage::Secrets.bit(),
            before.running & Stage::Secrets.bit()
        );
        assert_eq!(
            after.attempt(Stage::Secrets),
            before.attempt(Stage::Secrets)
        );
        assert_eq!(
            progress(root).unwrap().unwrap().running,
            Some(Stage::Secrets)
        );
        assert!(!progress(root).unwrap().unwrap().completed);
    }

    #[test]
    fn scoped_caps_never_migrates_missing_progress_or_uses_a_stale_ticket() {
        for stopped in [false, true] {
            let dir = aterm_tempfile::tempdir().unwrap();
            let (setup, revision) = admit(dir.path(), &[]);
            let ticket = setup.pending_inputs().unwrap().unwrap();
            if stopped {
                setup.stop_revision(revision).unwrap();
            } else {
                std::fs::remove_file(dir.path().join("initial-setup.json")).unwrap();
            }
            let before = observation_at(dir.path()).unwrap();
            assert!(
                setup
                    .native_input_pass(
                        &ticket,
                        || Box::new(InitialSetupAdmissionStopTest::keyboard_system(true, &[])),
                        SetupHooks::NONE
                    )
                    .unwrap()
                    .is_none()
            );
            assert_eq!(observation_at(dir.path()).unwrap(), before);
        }
    }
}
