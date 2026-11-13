// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

#[path = "withheld_restore_result_tests.rs"]
mod withheld_restore_result_tests;

#[path = "withheld_stop_tests.rs"]
mod withheld_stop_tests;

#[path = "startup_retry_wake_tests.rs"]
mod startup_retry_wake_tests;

#[path = "admission_stop_handoff_tests.rs"]
mod admission_stop_handoff_tests;

#[path = "watch_stop_handoff_tests.rs"]
mod watch_stop_handoff_tests;

#[path = "startup_recovery_tests.rs"]
mod startup_recovery_tests;

#[path = "startup_cancel_watch_tests.rs"]
mod startup_cancel_watch_tests;

#[path = "startup_repair_debt_tests.rs"]
mod startup_repair_debt_tests;

#[path = "queued_press_tests.rs"]
mod queued_press_tests;

#[path = "progress_publication_tests.rs"]
mod progress_publication_tests;

#[path = "restore_retry_tests.rs"]
mod restore_retry_tests;

#[path = "bind_stop_tests.rs"]
mod bind_stop_tests;

#[path = "startup_stop_retry_tests.rs"]
mod startup_stop_retry_tests;

#[path = "startup_newer_tests.rs"]
mod startup_newer_tests;

use super::*;
use aterm_cli::macos_doctor::{
    InitialSetupObservation, InitialSetupSnapshot, InitialSetupSnapshotError,
    initial_setup_snapshot_at,
};
use aterm_types::macos_setup::Plan;
use std::path::Path;

fn publish_decision(root: &Path, state: &str) {
    let next = root.join("decision.next");
    std::fs::write(
        &next,
        format!(r#"{{"profile":"macos-workstation-v1","state":"{state}"}}"#),
    )
    .unwrap();
    std::fs::rename(next, root.join("first-run.json")).unwrap();
}

fn snapshot(root: &Path) -> InitialSetupSnapshot {
    initial_setup_snapshot_at(root, Plan::default()).unwrap()
}

/// A Stop pressed in this launch for the run in force holds every widen in it, even one the
/// Stop came too late for (it came during the run's last stage, which finished, so the
/// records keep no Stop and still widen): a person who pressed Stop gets no more automatic
/// setup this launch (design §3.5). A later run, which the Stop did not stop, may be
/// widened.
#[test]
fn a_stop_in_this_launch_widens_nothing_of_the_run_it_stopped() {
    let _globals = globals();
    reset_worker_state();
    assert_eq!(
        widens_here_with(|| Ok(true), || Ok(7)),
        Ok(true),
        "control: no Stop"
    );
    // What `stop_setup_run(7)` leaves in memory; the durable request it saves is spent.
    *SETUP_STOP.lock().unwrap() = Some(7);
    let held = widens_here_with(|| Ok(true), || Ok(7));
    let later = widens_here_with(|| Ok(true), || Ok(8));
    let narrow = widens_here_with(|| Ok(false), || panic!("not read"));
    reset_worker_state();
    assert_eq!(held, Ok(false), "no widen of the run the person stopped");
    assert_eq!(later, Ok(true), "a later run is not the one stopped");
    assert_eq!(narrow, Ok(false), "records that no longer widen");
}

/// A stand-in for the watch's upgrade: each persist answers as scripted, with whether the
/// attempt is still held after it.
struct ScriptedUpgrade {
    pending: bool,
    answers: VecDeque<(
        Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>,
        bool,
    )>,
    persisted: u32,
    forgot: u32,
}

impl ScriptedUpgrade {
    fn new(
        answers: impl IntoIterator<
            Item = (
                Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String>,
                bool,
            ),
        >,
    ) -> Self {
        Self {
            pending: false,
            answers: answers.into_iter().collect(),
            persisted: 0,
            forgot: 0,
        }
    }
}

impl WatchUpgrade for ScriptedUpgrade {
    fn pending(&self) -> bool {
        self.pending
    }
    fn widens_from(&self) -> Option<u64> {
        self.pending.then_some(7)
    }
    fn revision(&self) -> Result<u64, String> {
        Ok(7)
    }
    fn persist(&mut self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        self.persisted += 1;
        let (answer, pending) = self.answers.pop_front().expect("a scripted answer");
        self.pending = pending;
        answer
    }
    fn published(&self) -> Result<Option<aterm_cli::macos_doctor::SetupUpgrade>, String> {
        Ok(None)
    }
    fn forget(&mut self) {
        self.forgot += 1;
        self.pending = false;
    }
}

const WIDENED: aterm_cli::macos_doctor::SetupUpgrade =
    aterm_cli::macos_doctor::SetupUpgrade { from: 7, to: 8 };

/// One look of a watch that heard the ALab signal. The watch keeps `widened` from look to
/// look.
fn look_once(
    widened: &mut Widened,
    upgrade: &mut ScriptedUpgrade,
    queue: impl FnMut(&Widened) -> bool,
) -> Result<Option<u64>, String> {
    look_settling_with(widened, upgrade, queue)
}

/// Every look of a heard widen ends its upgrade but one still pending: saved, the widened
/// plan's revision; nothing written (a Stop retired it, the records changed, a write failed
/// for good), the upgrade let go so no later look retries it; a write that failed with its
/// attempt held, finished by the next look.
#[test]
fn every_look_of_a_heard_widen_saves_keeps_or_lets_go_its_upgrade() {
    let _globals = globals();
    reset_worker_state();
    let no_wait = |_: &Widened| -> bool { panic!("nothing waits for another setup") };

    let mut upgrade = ScriptedUpgrade::new([(Ok(Some(WIDENED)), false)]);
    let looked = look_once(&mut Widened::default(), &mut upgrade, no_wait);
    assert_eq!((looked, upgrade.pending), (Ok(Some(8)), false));

    for (why, answer_now, looked_now) in [
        ("nothing changed", Ok(None), Ok(None)),
        (
            "a write that failed for good",
            Err("disk".to_string()),
            Err("disk".to_string()),
        ),
    ] {
        let mut upgrade = ScriptedUpgrade::new([(answer_now, false)]);
        let looked = look_once(&mut Widened::default(), &mut upgrade, no_wait);
        assert_eq!((looked, upgrade.pending), (looked_now, false), "{why}");
    }

    // A write that failed, its attempt held: the next look finishes it.
    let mut upgrade =
        ScriptedUpgrade::new([(Err("disk".to_string()), true), (Ok(Some(WIDENED)), false)]);
    let mut widened = Widened::default();
    let looked = look_once(&mut widened, &mut upgrade, no_wait);
    assert_eq!(looked, Err("disk".to_string()));
    assert!(upgrade.pending, "kept while pending");
    let looked = look_once(&mut widened, &mut upgrade, no_wait);
    assert_eq!((looked, upgrade.pending), (Ok(Some(8)), false));

    // This launch's Stop of the run the widen would replace (one spent on a finished run
    // included): the look retires the upgrade unwritten.
    *SETUP_STOP.lock().unwrap() = Some(7);
    let mut upgrade = ScriptedUpgrade::new([(Ok(Some(WIDENED)), false)]);
    let looked = look_once(&mut Widened::default(), &mut upgrade, no_wait);
    reset_worker_state();
    assert_eq!(looked, Ok(None));
    assert_eq!(upgrade.persisted, 0, "nothing reached the records");
}

/// The upgrade finds another setup holding the records past its wait: it waits on, shown
/// waiting, as a choice does, and is admitted once that setup lets go; one a Stop or a later
/// choice lets go meanwhile is retired unwritten.
#[test]
fn a_widen_another_setup_holds_waits_until_admitted_or_let_go() {
    let _globals = globals();
    reset_worker_state();
    let busy = || (Err(aterm_cli::macos_doctor::SETUP_BUSY.to_string()), true);
    let mut upgrade = ScriptedUpgrade::new([busy(), busy(), (Ok(Some(WIDENED)), false)]);
    let mut waits = 0;
    let looked = look_once(&mut Widened::default(), &mut upgrade, |_| {
        waits += 1;
        true
    });
    assert_eq!((looked, waits), (Ok(Some(8)), 2));

    let mut upgrade = ScriptedUpgrade::new([busy()]);
    let looked = look_once(&mut Widened::default(), &mut upgrade, |_| false);
    reset_worker_state();
    assert_eq!(looked, Ok(None));
    assert_eq!(
        (upgrade.forgot, upgrade.pending),
        (1, false),
        "retired unwritten"
    );
}

/// A widen whose upgrade meets a newer aterm's records (written by the setup it waited for)
/// ends with that one look, even where the upgrade would keep its attempt, so the page shows
/// the newer aterm's setup with no run going and no later look retries it
/// (`a_held_widen_apply_meeting_a_newer_aterms_records_ends` in aterm-cli is the real
/// upgrade's side).
#[test]
fn a_widen_meeting_a_newer_aterms_records_is_let_go() {
    let _globals = globals();
    reset_worker_state();
    // As the real upgrade's look of those records says it.
    let newer = (Err(InitialSetupSnapshotError::Newer.to_string()), true);
    let mut upgrade = ScriptedUpgrade::new([newer]);
    let looked = look_once(&mut Widened::default(), &mut upgrade, |_| {
        panic!("nothing waits for another setup")
    });
    assert!(looked.is_err_and(|why| setup_is_newer(&why)));
    assert_eq!(
        (upgrade.pending, upgrade.forgot),
        (false, 1),
        "the attempt ends"
    );
}

/// An older aterm's Cancel of the review that offered what a Restore brought forward, read
/// from its records: the status the page reads says so as for any Cancel, every group at
/// Leave as is, aterm's own permissions held; Set up now then applies the plan the review
/// showed (design §3.3).
#[test]
fn an_older_cancel_of_a_restore_brought_forward_reads_as_any_cancel() {
    use aterm_cli::macos_doctor::InitialSetupAdmissionStopTest;
    let root = aterm_tempfile::tempdir().unwrap();
    let restored = InitialSetupAdmissionStopTest::restored_before_run(root.path()).unwrap();
    assert!(
        restored.bring_forward().unwrap().is_some(),
        "brought forward"
    );
    let review = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    let reviewed = review.plan();
    review.older_cancel().unwrap();
    let status = initial_setup_status_with(
        Ok(review.snapshot().unwrap()),
        false,
        &InitialSetupRows::default(),
    );
    assert!(
        status.cancelled,
        "the page's Cancel line, beside Set up now"
    );
    assert!(status.finished && !status.provisional);
    assert!(
        status.plan.unwrap().selection.is_empty(),
        "every group at Leave as is"
    );
    assert!(
        !status.permissions.unwrap().allowed,
        "aterm's permission steps held"
    );

    assert!(
        review.set_up_now().unwrap().is_some(),
        "Set up now is admitted"
    );
    assert_eq!(review.saved_plan().unwrap(), Some(reviewed.clone()));
    let status = initial_setup_status_with(
        Ok(review.snapshot().unwrap()),
        false,
        &InitialSetupRows::default(),
    );
    assert!(!status.cancelled);
    assert_eq!(status.plan, Some(reviewed));
    // Resuming the selected Permissions stage starts its automatic steps.
    assert!(status.permissions.unwrap().allowed);
}

#[test]
fn clt_install_status_distinguishes_the_run_from_carried_permission_approval_and_stop() {
    use aterm_cli::macos_doctor::ProvisionalSetupWatchTest;

    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = ProvisionalSetupWatchTest::new(root.path()).unwrap();
    setup.person_asks_for_access().unwrap();
    let before = setup.snapshot().unwrap();
    let approval = before.access_revision;
    let run = setup.upgrade(None).unwrap().unwrap();
    let current = setup.snapshot().unwrap();
    assert_eq!(current.revision, run);
    assert!(run > before.revision);
    assert_eq!(current.access_revision, approval);
    assert!(
        current
            .plan
            .selection
            .contains(aterm_types::macos_setup::Setting::Repositories)
    );
    assert!(current.permissions_allowed);
    assert!(current.run_allowed);
    let status = initial_setup_status_with(Ok(current), false, &InitialSetupRows::default());
    assert_eq!(status.permissions.unwrap().revision, approval);
    assert_eq!(
        status.run,
        Some(aterm_gui::InitialSetupRunPolicy {
            revision: run,
            allowed: true
        })
    );
    assert!(!status.finished);

    setup.stop().unwrap();
    let stopped = setup.snapshot().unwrap();
    assert_eq!((stopped.revision, stopped.access_revision), (run, approval));
    assert!(!stopped.permissions_allowed);
    assert!(!stopped.run_allowed);
    let status = initial_setup_status_with(Ok(stopped), false, &InitialSetupRows::default());
    assert_eq!(status.permissions.unwrap().revision, approval);
    assert!(!status.permissions.unwrap().allowed);
    assert_eq!(
        status.run,
        Some(aterm_gui::InitialSetupRunPolicy {
            revision: run,
            allowed: false
        })
    );
    assert!(status.finished);
}

fn cached(observation: Option<InitialSetupObservation>, checked: bool) -> InitialSetupRows {
    let row = aterm_gui::SetupStatusRow {
        name: "Earlier result".into(),
        status: "blocked".into(),
        detail: "approval code OLD42".into(),
    };
    if checked {
        InitialSetupRows {
            observation,
            check: Some(aterm_gui::SetupCheck {
                plan: Plan::default(),
                rows: vec![row],
            }),
            ..InitialSetupRows::default()
        }
    } else {
        InitialSetupRows {
            observation,
            rows: vec![row],
            ..InitialSetupRows::default()
        }
    }
}

fn has_old_result(status: &aterm_gui::SetupStatus) -> bool {
    status
        .rows
        .iter()
        .chain(status.check.iter().flat_map(|check| check.rows.iter()))
        .any(|row| row.detail.contains("OLD42"))
}

#[test]
fn cached_setup_results_follow_the_exact_record_publication() {
    for checked in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        publish_decision(root.path(), "pending");
        let origin = snapshot(root.path());
        let last = cached(Some(origin.observation.clone()), checked);
        let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
        assert!(
            has_old_result(&status),
            "an unchanged poll keeps its result"
        );

        publish_decision(root.path(), "restored");
        let stopped = snapshot(root.path());
        assert_eq!(stopped.revision, origin.revision);
        let status = initial_setup_status_with(Ok(stopped), false, &last);
        assert!(!has_old_result(&status));
        assert!(status.finished);
        assert!(!status.permissions.unwrap().allowed);

        // A delayed publisher must retain its original context, even if a
        // newer state is already visible when it finishes publishing.
        let delayed = cached(Some(origin.observation), checked);
        assert!(!has_old_result(&initial_setup_status_with(
            Ok(snapshot(root.path())),
            false,
            &delayed,
        )));
    }
}

#[test]
fn a_stale_snapshot_cannot_evict_a_newer_setup_result() {
    let root = aterm_tempfile::tempdir().unwrap();
    publish_decision(root.path(), "pending");
    let old = snapshot(root.path());
    publish_decision(root.path(), "restored");
    let current = snapshot(root.path());
    let last = cached(Some(current.observation), false);

    // The slow reader collected the old state before the worker published
    // this result. It can filter that result, but must not delete it.
    assert!(!has_old_result(&initial_setup_status_with(
        Ok(old),
        false,
        &last,
    )));
    assert!(has_old_result(&initial_setup_status_with(
        Ok(snapshot(root.path())),
        false,
        &last,
    )));
}

#[test]
fn unknown_setup_state_never_borrows_an_old_results_context() {
    let root = aterm_tempfile::tempdir().unwrap();
    publish_decision(root.path(), "pending");
    let last = cached(Some(snapshot(root.path()).observation), true);
    for error in [
        InitialSetupSnapshotError::Updating,
        InitialSetupSnapshotError::Read("current records cannot be read".into()),
    ] {
        let changing = matches!(error, InitialSetupSnapshotError::Updating);
        let status = initial_setup_status_with(Err(error), true, &last);
        assert_eq!(status.changing, changing, "only a read that met a write");
        assert!(!has_old_result(&status));
        assert!(status.permissions.is_none());
        assert!(status.busy);
        assert_eq!(status.rows.len(), 1);
    }
    let unbound = cached(None, false);
    let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &unbound);
    assert!(!has_old_result(&status));
    assert!(!status.changing, "a read that met no write");
}

/// A Restore reaches the page as what it did to each System State row: one calm line in
/// the page's own row names, never a preference key, a process or a row per login reload.
/// A reader that did not relaunch leaves the rows it reads to the next login, which the
/// login sentence says (`aterm doctor --restore` keeps its `Reload` line); a setting it
/// could not put back stays a failure, named as Restore's and its row's.
#[test]
fn a_restore_reaches_the_page_in_row_names_not_preference_keys() {
    use aterm_cli::macos_doctor::{Action, LOGIN, Report, RestoreRows, Row};
    use aterm_types::macos_setup::{PutBack, RestoreOutcome};
    const KEY: &str = "-g com.apple.keyboard.modifiermapping.1452-834-0";
    let report = |outcome: RestoreOutcome| Report {
        action: Action::Restore,
        rows: vec![
            Row {
                name: KEY.into(),
                status: "restored".into(),
                detail: "original value restored".into(),
                elapsed_ms: None,
            },
            Row {
                name: "Caps Lock key".into(),
                status: LOGIN.into(),
                detail: "log out and back in to load the saved mappings".into(),
                elapsed_ms: None,
            },
            Row {
                name: "Reload Dock".into(),
                status: "error".into(),
                detail: "Dock did not come back".into(),
                elapsed_ms: None,
            },
        ],
        manual_steps: Vec::new(),
        backup: std::path::PathBuf::from("original-preferences.json"),
        restore: Some(RestoreRows { outcome }),
        page: None,
    };
    let back = initial_setup_report_rows(&report(RestoreOutcome::of([
        (Some(Setting::CapsLock), PutBack::AtLogin),
        (Some(Setting::Dock), PutBack::AtLogin),
    ])));
    assert_eq!(
        back.iter()
            .map(|row| (row.name.as_str(), row.status.as_str()))
            .collect::<Vec<_>>(),
        [("Restore previous settings", "restored")]
    );
    assert_eq!(
        back[0].detail,
        "Restore previous settings finished. Caps Lock key and Dock come back at your next \
         login."
    );
    let failed = initial_setup_report_rows(&report(RestoreOutcome::of([(
        Some(Setting::CapsLock),
        PutBack::Failed,
    )])));
    assert_eq!(
        failed
            .iter()
            .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
            .collect::<Vec<_>>(),
        [(
            "Restore: Caps Lock key",
            "failed",
            "Caps Lock key could not be put back as it was."
        )]
    );
    for row in back.iter().chain(&failed) {
        for engine in ["com.apple", "Reload", "did not come back"] {
            assert!(
                !row.name.contains(engine) && !row.detail.contains(engine),
                "{row:?}"
            );
        }
    }
}

/// A Check reaches the page through its page as a pass does (design §3.6): a stage it could
/// not confirm under its rows, with none of the stage's words, which `aterm doctor` keeps;
/// and a change the journal could not log is no row of the page's at all.
#[test]
fn a_check_reaches_the_page_in_row_names_and_the_journal_row_never_does() {
    use aterm_cli::macos_doctor::{Action, JOURNAL_ROW, PassPage, Report, Row};
    let row = |name: &str, status: &str, detail: &str| Row {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
        elapsed_ms: None,
    };
    let report = Report {
        action: Action::Check,
        rows: vec![row(
            "Spotlight and Rust documentation",
            "unverified",
            "rustup: /usr/bin/env exited 1",
        )],
        manual_steps: Vec::new(),
        backup: std::path::PathBuf::from("original-preferences.json"),
        restore: None,
        page: Some(PassPage {
            readback: aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
            outcome: aterm_types::macos_setup::PassOutcome::default(),
            rows: vec![row("Rust in Spotlight", "unverified", "")],
        }),
    };
    let said: Vec<(String, String, String)> = initial_setup_report_rows(&report)
        .into_iter()
        .map(|row| (row.name, row.status, row.detail))
        .collect();
    assert_eq!(
        said,
        [(
            "Check: Rust in Spotlight".to_owned(),
            "unverified".to_owned(),
            String::new()
        )]
    );
    let journal = Report {
        action: Action::Apply,
        rows: vec![
            row("Dock", "ok", "set"),
            row(JOURNAL_ROW, "error", "doctor.dock: disk full"),
        ],
        page: None,
        ..report
    };
    assert_eq!(journal.journal_refused(), Some("doctor.dock: disk full"));
    assert!(
        initial_setup_report_rows(&journal)
            .iter()
            .all(|row| row.name != JOURNAL_ROW),
        "the exceptions digest says it"
    );
}

/// A setup pass reaches the page in row names too (design §3.6): a change that did not take
/// is a failure of its row, and what its rollback put back is one calm line in a Restore's
/// words. `aterm doctor`'s rows, which name each key, stay in the terminal; the pass's
/// other rows reach the page as they are.
#[test]
fn a_pass_reaches_the_page_in_row_names_not_preference_keys() {
    use aterm_cli::macos_doctor::{Action, PassPage, Report, Row};
    use aterm_types::macos_setup::{PassOutcome, PutBack, RestoreOutcome, Unfinished};
    let row = |name: &str, status: &str, detail: &str| Row {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
        elapsed_ms: None,
    };
    let repositories = row("GitHub repositories", "ok", "all 3 cloned");
    let report = Report {
        action: Action::Apply,
        rows: vec![
            row(
                "Dock: com.apple.dock show-recents",
                "error",
                "absent → false",
            ),
            row(
                "Preference rollback",
                "restored",
                "rolled back com.apple.dock show-recents",
            ),
            repositories.clone(),
        ],
        manual_steps: Vec::new(),
        backup: std::path::PathBuf::from("original-preferences.json"),
        restore: None,
        page: Some(PassPage {
            readback: aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
            outcome: PassOutcome {
                rolled_back: Some(RestoreOutcome::of([(Some(Setting::Dock), PutBack::Back)])),
                unfinished: vec![(Setting::Dock, Unfinished::Failed)],
            },
            rows: vec![repositories],
        }),
    };
    let rows = initial_setup_report_rows(&report);
    assert_eq!(
        rows.iter()
            .map(|row| (row.name.as_str(), row.status.as_str(), row.detail.as_str()))
            .collect::<Vec<_>>(),
        [
            ("Dock", "error", "The change did not take."),
            (
                "Undo",
                "restored",
                "aterm put back what it could not finish. Dock is as it was."
            ),
            ("GitHub repositories", "ok", "all 3 cloned"),
        ]
    );
}

#[test]
fn a_blocked_restore_remains_visible_as_a_bound_operation_failure() {
    use aterm_cli::macos_doctor::{Action, Report, Row};
    const WHY: &str = "The original Widgets value could not be read; it was left unchanged.";
    for action in [Action::Apply, Action::Check, Action::Restore] {
        let root = aterm_tempfile::tempdir().unwrap();
        publish_decision(root.path(), "restored");
        let current = snapshot(root.path());
        let report = Report {
            action,
            rows: vec![Row {
                name: "Widgets".into(),
                status: "blocked".into(),
                detail: WHY.into(),
                elapsed_ms: None,
            }],
            manual_steps: Vec::new(),
            backup: root.path().join("original-preferences.json"),
            restore: None,
            page: None,
        };
        let rows = initial_setup_report_rows(&report);
        let last = if action == Action::Check {
            InitialSetupRows {
                observation: Some(current.observation),
                check: Some(aterm_gui::SetupCheck {
                    plan: current.plan,
                    rows,
                }),
                ..InitialSetupRows::default()
            }
        } else {
            InitialSetupRows {
                observation: Some(current.observation),
                rows,
                ..InitialSetupRows::default()
            }
        };
        let status = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
        let row = status
            .rows
            .iter()
            .chain(status.check.iter().flat_map(|check| check.rows.iter()))
            .find(|row| row.detail == WHY)
            .expect("the result belongs to the unchanged publication");
        let expected = match action {
            Action::Apply => ("Widgets", "blocked"),
            Action::Check => ("Check: Widgets", "blocked"),
            Action::Restore => ("Restore: Widgets", "failed"),
        };
        assert_eq!((row.name.as_str(), row.status.as_str()), expected);
        assert_eq!(
            report.rows[0].status, "blocked",
            "CLI semantics stay intact"
        );
        assert!(report.needs_attention());

        publish_decision(root.path(), "cancelled");
        let next = initial_setup_status_with(Ok(snapshot(root.path())), false, &last);
        assert!(
            next.rows.iter().all(|row| row.detail != WHY)
                && next
                    .check
                    .as_ref()
                    .is_none_or(|check| check.rows.iter().all(|row| row.detail != WHY)),
            "an older Restore result cannot follow a newer publication"
        );
    }
}

use aterm_types::macos_setup::{Group, Observation, Setting, Subject};
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The tests below share the worker's process-wide state: its registration,
/// its readings, the setup slot and [`SETUP_IO`].
static SETUP_GLOBALS: Mutex<()> = Mutex::new(());

fn globals() -> MutexGuard<'static, ()> {
    SETUP_GLOBALS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A wait for something that must happen: a hang detector, not a budget.
const HANG: Duration = Duration::from_secs(60);

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + HANG;
    while !done() {
        assert!(Instant::now() < deadline, "{what} never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The Unix second [`TestIo`]'s readings are stamped with, unless a test moves it.
const READ_AT: u64 = 1_700_000_000;

/// `row` as [`observe_rows`] keeps it once [`TestIo`]'s reader returned it: stamped
/// [`READ_AT`].
fn as_read(row: &Observation) -> Observation {
    row.clone().stamped(READ_AT)
}

/// What the worker asked of the outside under [`TestIo`], and how it answers.
struct Script {
    /// Readings started: their plan, their rows, and whether they used the
    /// network.
    reads: Vec<(Plan, Vec<Subject>, bool)>,
    /// Whether each read carries a person's explicit Check intent.
    read_checks: Vec<bool>,
    /// Replies bound to each reading when it starts, before a held read
    /// can be overtaken by a choice.
    readings: VecDeque<Vec<Observation>>,
    /// The wall clock each reading is stamped with ([`SetupIo::now_secs`]).
    now_secs: u64,
    /// The rows the next reading's private reader is not admitted to: they come back withheld.
    not_admitted: Vec<Subject>,
    /// Readings wait while this is set.
    hold: bool,
    /// The next held reading unwinds after releasing the script lock.
    panic_read: bool,
    /// Stop saves begun: the run each names and the thread it ran on.
    stop_attempts: Vec<(u64, std::thread::ThreadId)>,
    /// Stop saves that landed.
    stops: Vec<u64>,
    /// Stop saves wait while this is set, as a loaded disk holds them.
    stop_hold: bool,
    /// Stop saves that fail before one lands.
    stop_failures: usize,
    /// An optional actual scratch Stop writer for admission handoff tests.
    stop_root: Option<std::path::PathBuf>,
    /// A person's choice asks for the claim only once this is clear.
    claim_hold: bool,
    /// Admissions of a person's choice begun.
    persists: usize,
    /// Admissions wait while this is set.
    persist_hold: bool,
    /// Each admission first finds this Mac's setup records held by another setup, as the
    /// profile lock's wait does ([`aterm_cli::macos_doctor::InitialSetupRestart::noting_wait`]).
    persist_finds_lock_held: bool,
    /// What each admission does in turn: admits its choice as this
    /// revision, or refuses it (`None`, and when none is left).
    admits: VecDeque<Option<u64>>,
    /// A run that registers while a choice is being refused.
    registers: Option<u64>,
    /// The runs admitted choices started.
    runs: Vec<u64>,
    /// The revision in force: the last one admitted.
    revision: u64,
    /// The startup's read of the plan in force waits while this is set.
    initial_hold: bool,
    /// The startup is reading the plan in force, holding its claim.
    startup_reading: bool,
    /// Offers the startup showed.
    offers: usize,
    /// The real request sent its answer, before the startup reads it.
    startup_answer_delivered: bool,
    /// Hold the offer after delivery, before it returns to the coordinator.
    startup_answer_hold: bool,
    /// What each write of the page's answer does in turn: admits it as
    /// this revision (`Ok(Some)`), finds it retired (`Ok(None)`), or
    /// fails while the startup keeps its claim (`Err`).
    authorizations: VecDeque<Result<Option<u64>, String>>,
    /// Writes of the page's answer begun.
    authorizing: usize,
    /// Those writes wait while this is set.
    authorize_hold: bool,
    /// The startup's waits before it writes its answer again, holding
    /// its claim, begun.
    retry_waits: usize,
    /// The startup's waits holding no claim last while this is set, as
    /// a minute does.
    startup_backoff_hold: bool,
    /// What the startup reported failing.
    startup_failures: Vec<String>,
    /// Follow-ups with Full Disk Access a recorded grant started.
    follow_ups: usize,
    /// Starts of the vault subscriber's mend asked for.
    mends: usize,
    /// Starts of the converge pass's window lane asked for.
    converge_lanes: usize,
    /// Converge passes the Secrets stage's end started.
    setup_converges: usize,
    /// What bringing a restored setup forward found.
    brought_forward: aterm_cli::macos_doctor::BroughtForward,
    /// Check again's looks at the LAN.
    vault_looks: usize,
    /// Lifts of the run's parked stages a press of Check again asked for.
    lifts: usize,
    /// Startups run again in this process.
    startups: usize,
    /// Whether each requested startup worker was accepted by the spawner.
    startup_spawns: VecDeque<bool>,
    /// Startup continuations on a released Restore worker.
    startups_inline: usize,
    /// The answers, one an ask, to whether another setup holds the records (none: no).
    records_held: VecDeque<bool>,
    /// The widened mark ([`SETUP_WIDENED`]) as each run started.
    widened: Vec<u64>,
    /// The vault's materialization mark ([`SetupIo::vault_mark`]).
    vault_mark: Option<aterm_secrets::client::MaterializationMark>,
    pending_repositories:
        VecDeque<Result<Option<aterm_cli::macos_doctor::InitialSetupAdmission>, String>>,
    retire_repository_choice_after_guard: bool,
    /// What a previous launch kept of the private rows ([`SetupIo::remembered`]).
    remembered: Vec<(Observation, aterm_cli::macos_doctor::Fence)>,
    /// Looks for it.
    remembered_looks: usize,
    /// Each snapshot kept for the next launch ([`SetupIo::remember`]).
    remembered_writes: Vec<Vec<(Observation, aterm_cli::macos_doctor::Fence)>>,
    /// The journal as each read starts, one an ask ([`SetupIo::fence`]; none: no file).
    fences: VecDeque<aterm_cli::macos_doctor::Fence>,
}

impl Script {
    const NEW: Self = Self {
        reads: Vec::new(),
        read_checks: Vec::new(),
        readings: VecDeque::new(),
        now_secs: READ_AT,
        not_admitted: Vec::new(),
        hold: false,
        panic_read: false,
        stop_attempts: Vec::new(),
        stops: Vec::new(),
        stop_hold: false,
        stop_failures: 0,
        stop_root: None,
        claim_hold: false,
        persists: 0,
        persist_hold: false,
        persist_finds_lock_held: false,
        admits: VecDeque::new(),
        registers: None,
        runs: Vec::new(),
        revision: 1,
        initial_hold: false,
        startup_reading: false,
        offers: 0,
        startup_answer_delivered: false,
        startup_answer_hold: false,
        authorizations: VecDeque::new(),
        authorizing: 0,
        authorize_hold: false,
        retry_waits: 0,
        startup_backoff_hold: false,
        startup_failures: Vec::new(),
        follow_ups: 0,
        mends: 0,
        converge_lanes: 0,
        setup_converges: 0,
        brought_forward: aterm_cli::macos_doctor::BroughtForward::Nothing,
        vault_looks: 0,
        lifts: 0,
        startups: 0,
        startup_spawns: VecDeque::new(),
        startups_inline: 0,
        records_held: VecDeque::new(),
        widened: Vec::new(),
        vault_mark: None,
        pending_repositories: VecDeque::new(),
        retire_repository_choice_after_guard: false,
        remembered: Vec::new(),
        remembered_looks: 0,
        remembered_writes: Vec::new(),
        fences: VecDeque::new(),
    };
}

static SCRIPT: Mutex<Script> = Mutex::new(Script::NEW);
static SCRIPT_MOVED: Condvar = Condvar::new();

fn script() -> MutexGuard<'static, Script> {
    SCRIPT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Change the script and wake whatever waits on it.
fn change(edit: impl FnOnce(&mut Script)) {
    edit(&mut script());
    SCRIPT_MOVED.notify_all();
}

/// Wait while `held` says so (a hang detector bounds it).
fn wait_while(
    mut script: MutexGuard<'static, Script>,
    held: impl Fn(&Script) -> bool,
) -> MutexGuard<'static, Script> {
    let deadline = Instant::now() + HANG;
    while held(&script) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        script = SCRIPT_MOVED
            .wait_timeout(script, left)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
    script
}

fn release_readings() {
    change(|script| script.hold = false);
}

fn observe_idle() -> bool {
    OBSERVE_QUEUE.lock().is_ok_and(|queue| !queue.running)
}

/// No Stop waits for the Stop's worker, nor is being saved.
fn stop_worker_idle() -> bool {
    let saves = stop_saves();
    saves.saving.is_none() && saves.wanted.is_none()
}

fn reset_worker_state() {
    set_setup_run(None);
    *SETUP_STOP.lock().unwrap_or_else(PoisonError::into_inner) = None;
    // The worker, once started, lives on for the next test.
    let mut saves = stop_saves();
    *saves = StopSaves {
        worker: saves.worker,
        ..StopSaves::default()
    };
    drop(saves);
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap_or_else(PoisonError::into_inner);
        rows.plan = None;
        rows.remembered_looked = false;
        rows.retained_reads.clear();
        rows.history_restored.clear();
        rows.history_epoch = 0;
        rows.last_known.clear();
        rows.unconfirmed.clear();
    }
    {
        let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        queue.active.clear();
        queue.current = None;
    }
    *GITHUB_TOKEN_MARK
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = None;
}

/// The folder [`TestIo`]'s claims are taken in.
static CLAIM_ROOT: Mutex<Option<std::path::PathBuf>> = Mutex::new(None);

/// The worker's outside, scripted for a test that holds [`globals`]: it
/// reads no row of the Mac and writes nothing to the person's state root.
/// Its automatic claim is the real lock, in a scratch folder. Dropped, it
/// waits for the readings, the choice and the Stop the test started to end
/// before the live [`SetupIo`] is back. It holds [`TEST_IO`] for its whole
/// life, so no two run at once and one's drop puts back no [`SETUP_IO`]
/// another test still scripts.
struct TestIo {
    _claims: aterm_tempfile::TempDir,
    _serial: MutexGuard<'static, ()>,
}

/// Serialises every [`TestIo`]. Taken inside [`globals`], which every
/// test that installs one holds first.
static TEST_IO: Mutex<()> = Mutex::new(());

impl TestIo {
    fn install() -> Self {
        let serial = TEST_IO.lock().unwrap_or_else(PoisonError::into_inner);
        let claims = aterm_tempfile::tempdir().unwrap();
        *CLAIM_ROOT.lock().unwrap_or_else(PoisonError::into_inner) = Some(claims.path().to_owned());
        *script() = Script::NEW;
        *SETUP_IO.write().unwrap_or_else(PoisonError::into_inner) = SetupIo {
            scope_refused: || false,
            history_store: || None,
            read: Self::read,
            now_secs: || script().now_secs,
            save_stop: Self::save_stop,
            claim_automatic: Self::claim,
            claim_rerun: Self::claim_rerun,
            persist: Self::persist,
            run: Self::run,
            mend_subscriber: || change(|script| script.mends += 1),
            converge_lane: || change(|script| script.converge_lanes += 1),
            converge_setup: || change(|script| script.setup_converges += 1),
            put_back: |_| Ok(false),
            start_follow_up: || change(|script| script.follow_ups += 1),
            bring_forward: || Ok(script().brought_forward),
            look_again: || change(|script| script.vault_looks += 1),
            lift_parks: || {
                change(|script| script.lifts += 1);
                Ok(())
            },
            start_startup: || {
                let mut script = script();
                script.startups += 1;
                script.startup_spawns.pop_front().unwrap_or(true)
            },
            start_startup_inline: || change(|script| script.startups_inline += 1),
            records_held: || script().records_held.pop_front().unwrap_or(false),
            vault_mark: || script().vault_mark,
            pending_repositories: || {
                let (answer, retire) = {
                    let mut script = script();
                    let answer = script.pending_repositories.pop_front().unwrap_or(Ok(None));
                    let retire = script.pending_repositories.is_empty()
                        && std::mem::take(&mut script.retire_repository_choice_after_guard);
                    (answer, retire)
                };
                if retire {
                    OBSERVED_ROWS.lock().unwrap().choices += 1;
                }
                answer
            },
            remember: |rows| {
                change(|script| script.remembered_writes.push(rows.to_vec()));
                Ok(())
            },
            remembered: || {
                let mut script = script();
                script.remembered_looks += 1;
                script.remembered.clone()
            },
            fence: || Some(script().fences.pop_front().unwrap_or_default()),
        };
        reset_worker_state();
        Self {
            _claims: claims,
            _serial: serial,
        }
    }

    fn read(
        plan: &Plan,
        subjects: &[Subject],
        network: bool,
        admission: aterm_cli::macos_doctor::ObservationRead,
    ) -> Vec<Observation> {
        let mut script = script();
        script
            .reads
            .push((plan.clone(), subjects.to_vec(), network));
        script.read_checks.push(matches!(
            admission,
            aterm_cli::macos_doctor::ObservationRead::ExplicitCheck
        ));
        let reading = script.readings.pop_front().unwrap_or_default();
        let not_admitted = std::mem::take(&mut script.not_admitted);
        let panics = std::mem::take(&mut script.panic_read);
        SCRIPT_MOVED.notify_all();
        drop(wait_while(script, |script| script.hold));
        assert!(!panics, "injected reader unwind");
        // A private row its reader was not admitted to comes back withheld, as the live
        // reader returns it.
        reading
            .into_iter()
            .map(|seen| {
                if not_admitted.contains(&seen.subject) {
                    Observation::withheld(seen.subject)
                } else {
                    seen
                }
            })
            .collect()
    }

    fn save_stop(revision: u64) -> Result<(), String> {
        let mut script = script();
        script
            .stop_attempts
            .push((revision, std::thread::current().id()));
        SCRIPT_MOVED.notify_all();
        let mut script = wait_while(script, |script| script.stop_hold);
        if script.stop_failures > 0 {
            script.stop_failures -= 1;
            return Err("the disk is full".into());
        }
        let root = script.stop_root.clone();
        drop(script);
        if let Some(root) = root {
            aterm_cli::macos_doctor::ProvisionalSetupWatchTest::at(&root)
                .stop_revision(revision)?;
        }
        let mut script = self::script();
        script.stops.push(revision);
        SCRIPT_MOVED.notify_all();
        Ok(())
    }

    fn claim() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String> {
        let root = CLAIM_ROOT
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
            .expect("TestIo's claim folder");
        aterm_cli::macos_doctor::claim_initial_setup_rerun_at(&root)
    }

    fn claim_rerun() -> Result<Option<aterm_cli::macos_doctor::AutomaticSetupClaim>, String> {
        drop(wait_while(script(), |script| script.claim_hold));
        Self::claim()
    }

    fn persist(
        restart: &mut aterm_cli::macos_doctor::InitialSetupRestart,
        _: &Plan,
        _: bool,
    ) -> aterm_cli::macos_doctor::InitialSetupObserved<Option<u64>> {
        if script().persist_finds_lock_held {
            restart.finds_lock_held_for_test();
        }
        let mut script = script();
        script.persists += 1;
        SCRIPT_MOVED.notify_all();
        let mut script = wait_while(script, |script| script.persist_hold);
        let admitted = script.admits.pop_front().flatten();
        let registers = script.registers;
        if let Some(revision) = admitted {
            script.revision = revision;
        }
        drop(script);
        let result = match admitted {
            Some(revision) => Ok(Some(revision)),
            None => {
                if let Some(revision) = registers {
                    set_setup_run(Some(revision));
                }
                Err("the plan could not be saved".into())
            }
        };
        aterm_cli::macos_doctor::InitialSetupObserved {
            result,
            observation: None,
        }
    }

    fn run(
        claim: Option<aterm_cli::macos_doctor::AutomaticSetupClaim>,
        revision: u64,
        _: Option<aterm_secrets::SecretBytes>,
        busy: InitialSetupBusy,
    ) {
        change(|script| {
            script.runs.push(revision);
            script
                .widened
                .push(SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire));
        });
        drop(claim);
        drop(busy);
    }
}

impl Drop for TestIo {
    fn drop(&mut self) {
        change(|script| {
            script.hold = false;
            script.stop_hold = false;
            script.claim_hold = false;
            script.persist_hold = false;
            script.initial_hold = false;
            script.authorize_hold = false;
            script.startup_backoff_hold = false;
            script.startup_answer_hold = false;
        });
        let deadline = Instant::now() + HANG;
        while !(observe_idle()
            && stop_worker_idle()
            && !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        *SETUP_IO.write().unwrap_or_else(PoisonError::into_inner) = SetupIo::LIVE;
        reset_worker_state();
    }
}

/// Every group set up but Workspace and Messages & FaceTime, the two whose
/// readings use the network and the person's own account state.
fn left_as_is() -> Plan {
    let mut plan = Plan::default();
    for setting in [
        Setting::Spotlight,
        Setting::Secrets,
        Setting::Repositories,
        Setting::Messages,
        Setting::FaceTime,
    ] {
        plan.selection.set(setting, false);
    }
    plan
}

/// A [`TestIo`] holds [`TEST_IO`] for its whole life, so a second one waits
/// for the first before replacing [`SETUP_IO`].
#[test]
fn a_test_io_holds_its_tests_lock_while_it_lives() {
    let _globals = globals();
    let _io = TestIo::install();
    assert!(
        TEST_IO.try_lock().is_err(),
        "a TestIo another test can replace while this one lives"
    );
}

/// The window recording Full Disk Access granted starts the follow-up that puts back what
/// waited for it, once per grant; no other step the window records (handed, declined, nor
/// any other access granted) starts one.
#[test]
fn recording_full_disk_access_granted_starts_the_follow_up_once() {
    use aterm_types::macos_setup::{Access, AccessOutcome};
    let _globals = globals();
    let _io = TestIo::install();
    // Keep a known plan whose personal/network groups are left as is: the
    // recovery fact requests Access rows alone and grants no broader read.
    OBSERVED_ROWS.lock().unwrap().plan = Some(left_as_is());
    for access in Access::ALL {
        for outcome in [
            AccessOutcome::Handed,
            AccessOutcome::Declined,
            AccessOutcome::Granted,
        ] {
            if (access, outcome) != (Access::FullDiskAccess, AccessOutcome::Granted) {
                access_step_recorded(1, access, outcome);
            }
        }
    }
    assert_eq!(script().follow_ups, 0);
    access_step_recorded(1, Access::FullDiskAccess, AccessOutcome::Granted);
    assert_eq!(script().follow_ups, 1);
    access_step_recorded(1, Access::FullDiskAccess, AccessOutcome::Declined);
    assert_eq!(script().follow_ups, 1);
    // The same verified fact without an access revision has the same
    // read/wake/recovery effects, without inventing a recorded revision.
    let before = SETUP_WAKES.count();
    verified_full_disk_access_granted();
    assert_eq!(script().follow_ups, 2);
    assert!(SETUP_WAKES.count() > before);
    wait_until("the Access rows' readings", observe_idle);
    let reads = script().reads.clone();
    assert!(!reads.is_empty());
    assert!(reads.iter().all(|(_, subjects, network)| {
        *subjects == Access::ALL.map(Subject::Access) && !network
    }));
}

/// A Restore that left aterm's own permissions with no admitted run to carry them (one that
/// let the launch's offer go unanswered) is offered again in this launch; so is one whose
/// bring-forward met the profile lock busy, which the startup waits for. A Restore whose run
/// carries them starts nothing.
#[test]
fn a_restore_that_left_no_run_for_the_permissions_is_offered_again_in_this_launch() {
    use aterm_cli::macos_doctor::BroughtForward;
    let _globals = globals();
    let _io = TestIo::install();
    offer_again_after_restore();
    assert_eq!(script().startups, 0, "nothing brought forward");
    change(|script| script.brought_forward = BroughtForward::Run(3));
    offer_again_after_restore();
    assert_eq!(script().startups, 1);
    change(|script| script.brought_forward = BroughtForward::Busy);
    offer_again_after_restore();
    assert_eq!(script().startups, 2, "the startup waits for the lock");
}

/// A launch whose setup has finished starts the follow-up with Full Disk Access on a thread
/// of its own: its wait for a profile lock another pass holds never holds up what the
/// startup does next (the watch for the ALab vault).
#[test]
fn a_finished_launch_starts_its_follow_up_beside_the_startup() {
    let _globals = globals();
    let _io = TestIo::install();
    run_prepared_setup(PreparedSetup::Finished, None, StartupSent::default());
    assert_eq!(script().follow_ups, 1);
    assert_eq!(script().mends, 1, "the vault subscriber's mend");
    assert_eq!(
        script().converge_lanes,
        1,
        "the converge pass's window lane"
    );
}

/// A hidden launch and a page refresh, forced or not, both skip private readers without
/// publishing failure-shaped Unknown rows: the skipped read comes back withheld, so an
/// earlier reading stands with its own time and a row nothing read says unread. A
/// subsequent admitted read can still replace the row, and so can an attempted one.
#[test]
fn skipped_private_checks_preserve_evidence_until_an_admitted_read() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let plan = Plan::default();
    let dock = Subject::Setting(Setting::Dock);
    let ready = Observation {
        standing: Standing::InPlace,
        now: Some("verified".into()),
        ..Observation::unknown(dock)
    };
    for forced in [false, true] {
        for previous in [None, Some(ready.clone().stamped(READ_AT - 300))] {
            {
                let mut rows = OBSERVED_ROWS.lock().unwrap();
                rows.plan = Some(plan.clone());
                rows.vault_url = Some(plan.vault_url.trim().to_owned());
                rows.rows = previous.clone().into_iter().collect();
            }
            change(|script| {
                script.readings.push_back(vec![Observation::unknown(dock)]);
                script.not_admitted = vec![dock];
            });
            let choices = OBSERVED_ROWS.lock().unwrap().choices;
            let request = || ObserveRequest {
                plan: Some(plan.clone()),
                choices,
                subjects: vec![dock],
                forced,
                pending_repositories: false,
            };
            observe_rows(request());
            assert_eq!(
                OBSERVED_ROWS.lock().unwrap().rows,
                vec![previous.unwrap_or_else(|| Observation::withheld(dock))],
                "a skipped read keeps the earlier reading, or reads unread ({forced})"
            );
            change(|script| script.readings.push_back(vec![ready.clone()]));
            observe_rows(request());
            assert_eq!(
                OBSERVED_ROWS.lock().unwrap().rows,
                vec![ready.clone().stamped(READ_AT)]
            );
            change(|script| {
                script.readings.push_back(vec![Observation::unknown(dock)]);
            });
            observe_rows(request());
            assert_eq!(
                OBSERVED_ROWS.lock().unwrap().rows,
                vec![Observation::unknown(dock).stamped(READ_AT)],
                "an attempted read still replaces previous evidence"
            );
        }
    }
}

/// A completed run starts no apply pass to refresh its readings. Its launch still reads
/// the current checklist without opening the page, replaying settings, or widening the
/// scope of an ordinary refresh. An unread answer remains unread after publication.
#[test]
fn a_finished_launch_reads_its_current_checklist_without_replaying_setup() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    for (plan, unread) in [
        (Plan::default(), false),
        (Plan::default(), true),
        (
            Plan::from(aterm_types::macos_setup::Selection::none()),
            false,
        ),
    ] {
        let _io = TestIo::install();
        let subjects: Vec<_> = Subject::all()
            .into_iter()
            .filter(|subject| {
                *subject != Subject::Setting(Setting::Repositories)
                    || plan.selection.contains(Setting::Repositories)
            })
            .filter(|subject| {
                subject.setting().group() != Group::Accounts
                    || plan.selection.contains(subject.setting())
            })
            .collect();
        let readings: Vec<_> = subjects
            .iter()
            .map(|subject| {
                if unread && *subject == Subject::Setting(Setting::Dock) {
                    Observation::unknown(*subject)
                } else {
                    Observation {
                        standing: Standing::InPlace,
                        now: Some("verified current state".into()),
                        ..Observation::unknown(*subject)
                    }
                }
            })
            .collect();
        {
            let mut observed = OBSERVED_ROWS.lock().unwrap();
            observed.plan = Some(plan.clone());
            observed.rows.clear();
            observed.vault_url = None;
        }
        change(|script| {
            script.hold = true;
            script.readings.push_back(readings.clone());
        });

        run_prepared_setup(PreparedSetup::Finished, None, StartupSent::default());

        assert!(
            OBSERVE_QUEUE.lock().unwrap().running,
            "a finished launch must refresh its checklist without a page or apply pass"
        );
        wait_until("the completed launch's reading starts", || {
            script().reads.len() == 1
        });
        {
            let script = script();
            assert_eq!(
                script.reads,
                [(
                    plan.clone(),
                    subjects,
                    plan.selection.contains(Setting::Repositories)
                )]
            );
            assert!(
                script.runs.is_empty(),
                "completed settings are not replayed"
            );
            assert_eq!(script.persists, 0, "no new plan is admitted");
            assert_eq!(script.offers, 0);
            assert!(script.stops.is_empty());
            assert_eq!(
                script.read_checks,
                [false],
                "a completed launch is no explicit Check"
            );
        }
        release_readings();
        wait_until("the completed launch's reading is published", observe_idle);
        let mut status = aterm_gui::SetupStatus::default();
        add_setup_readings(&mut status);
        let stamped: Vec<_> = readings.iter().map(as_read).collect();
        assert_eq!(
            status.observed, stamped,
            "each reading is dated as its read started"
        );
    }
}

/// A launch that resumes an admitted run starts the vault subscriber's mend and the converge
/// pass's window lane before the run: the run may wait on a person past its done Secrets
/// stage for as long as they take, which would leave a subscriber whose program is gone dead
/// meanwhile, and the Mac with no window pass for the process's life.
/// NEGATIVE CONTROL: a resumed run that starts no lane fails here.
#[test]
fn a_launch_resuming_a_run_starts_the_vault_mend_and_the_converge_lane() {
    let _globals = globals();
    let _io = TestIo::install();
    let claim = StartupClaim::claim().unwrap().unwrap();
    run_prepared_setup(
        PreparedSetup::Authorized { claim, revision: 8 },
        None,
        StartupSent::default(),
    );
    assert_eq!(script().runs, [8]);
    assert_eq!(script().mends, 1);
    assert_eq!(
        script().converge_lanes,
        1,
        "the converge pass's window lane"
    );
    clear_setup_run(8);
}

/// A launch whose setup runs nothing still starts the converge pass's window lane, since no
/// run's end starts it later in this process: one unavailable (an update's successor that
/// withholds its offer, a Stop).
/// NEGATIVE CONTROL: an unavailable launch that starts no lane fails here.
#[test]
fn a_launch_with_no_run_still_starts_the_converge_lane() {
    let _globals = globals();
    let _io = TestIo::install();
    run_prepared_setup(PreparedSetup::Unavailable, None, StartupSent::default());
    assert_eq!(script().converge_lanes, 1, "unavailable");
    assert!(script().runs.is_empty());
}

/// The converge pass runs as the Secrets stage ends: not as it starts, and not at another
/// stage's end.
/// NEGATIVE CONTROL: a check that ignores `running` starts a pass as the stage starts too.
#[test]
fn the_secrets_stage_starts_a_converge_pass_as_it_ends() {
    use aterm_cli::macos_doctor::StageEvent;
    use aterm_types::macos_setup::Setting;
    let event = |settings: &[Setting], running| {
        ends_secrets_stage(&StageEvent {
            revision: 1,
            settings,
            subjects: &[],
            running,
            readback: &aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
        })
    };
    assert!(event(&[Setting::Secrets], false), "the Secrets stage ended");
    assert!(!event(&[Setting::Secrets], true), "it started");
    assert!(
        !event(&[Setting::Repositories], false),
        "another stage ended"
    );
    assert!(!event(&[], false), "a stage with nothing selected");
}

#[test]
fn converge_completion_reads_only_current_pending_repositories_and_requires_fresh_readiness() {
    use aterm_cli::macos_doctor::InitialSetupAdmission;
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let mut plan = left_as_is();
    plan.selection.set(Setting::Repositories, true);
    let admitted = InitialSetupAdmission {
        plan: plan.clone(),
        revision: 9,
        completed: false,
    };
    let current = || Ok(Some(admitted.clone()));
    for (name, before, after, standing, previous, stopped, reads, wakes) in [
        (
            "first-ready-row",
            current(),
            current(),
            Standing::InPlace,
            false,
            false,
            1,
            true,
        ),
        (
            "already-ready-row",
            current(),
            current(),
            Standing::InPlace,
            true,
            false,
            1,
            true,
        ),
        (
            "still-waiting",
            current(),
            current(),
            Standing::Waiting,
            false,
            false,
            1,
            false,
        ),
        (
            "no-pending-run",
            Ok(None),
            current(),
            Standing::InPlace,
            false,
            false,
            0,
            false,
        ),
        (
            "completed-before-read",
            Ok(Some(InitialSetupAdmission {
                completed: true,
                ..admitted.clone()
            })),
            current(),
            Standing::InPlace,
            false,
            false,
            0,
            false,
        ),
        (
            "unread-scope",
            Err("records held".into()),
            current(),
            Standing::InPlace,
            false,
            false,
            0,
            false,
        ),
        (
            "immediate-stop",
            current(),
            current(),
            Standing::InPlace,
            false,
            true,
            0,
            false,
        ),
        (
            "retired-during-read",
            current(),
            Ok(None),
            Standing::InPlace,
            false,
            false,
            1,
            false,
        ),
        (
            "new-revision",
            current(),
            Ok(Some(InitialSetupAdmission {
                revision: 10,
                ..admitted.clone()
            })),
            Standing::InPlace,
            false,
            false,
            1,
            false,
        ),
        (
            "new-source",
            current(),
            Ok(Some(InitialSetupAdmission {
                plan: left_as_is(),
                ..admitted.clone()
            })),
            Standing::InPlace,
            false,
            false,
            1,
            false,
        ),
    ] {
        let _io = TestIo::install();
        let row = Observation {
            standing,
            ..Observation::unknown(Subject::Setting(Setting::Repositories))
        };
        {
            let mut observed = OBSERVED_ROWS.lock().unwrap();
            observed.plan = Some(plan.clone());
            observed.vault_url = Some(plan.vault_url.trim().to_owned());
            observed.rows = if previous {
                vec![row.clone()]
            } else {
                Vec::new()
            };
        }
        change(|script| {
            script.pending_repositories = [before, after].into();
            script.readings = [vec![row]].into();
        });
        if stopped {
            *SETUP_STOP.lock().unwrap() = Some(admitted.revision);
        }
        let seen = SETUP_WAKES.marks();
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        observe_rows(ObserveRequest {
            plan: None,
            choices,
            subjects: vec![Subject::Setting(Setting::Repositories)],
            forced: false,
            pending_repositories: true,
        });
        assert_eq!(script().reads.len(), reads, "{name}");
        assert_eq!(SETUP_WAKES.marks().all != seen.all, wakes, "{name}");
        assert!(!SETUP_WAKES.marks().checked_since(seen), "{name}");
        assert!(!SETUP_WAKES.marks().refreshed_since(seen), "{name}");
        assert!(script().read_checks.iter().all(|check| !check), "{name}");
        assert!(
            script().runs.is_empty(),
            "an observation owns no effects: {name}"
        );
        if reads > 0 && !wakes && standing == Standing::InPlace {
            assert!(
                OBSERVED_ROWS.lock().unwrap().rows.is_empty(),
                "retired reading: {name}"
            );
        }
    }
    let _io = TestIo::install();
    change(|script| script.pending_repositories = [current()].into());
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.rows.clear();
        rows.plan = Some(left_as_is());
        rows.choices += 1;
        rows.choices
    };
    let seen = SETUP_WAKES.marks();
    observe_rows(ObserveRequest {
        plan: None,
        choices: choices - 1,
        subjects: vec![Subject::Setting(Setting::Repositories)],
        forced: false,
        pending_repositories: true,
    });
    assert!(
        script().reads.is_empty(),
        "a newer choice retires the queued pending scope"
    );
    assert_eq!(SETUP_WAKES.marks(), seen);
}

#[test]
fn a_choice_retired_after_the_completion_guard_keeps_the_pending_only_requeue() {
    use aterm_cli::macos_doctor::InitialSetupAdmission;
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let mut plan = left_as_is();
    plan.selection.set(Setting::Repositories, true);
    let admitted = InitialSetupAdmission {
        plan: plan.clone(),
        revision: 9,
        completed: false,
    };
    change(|script| {
        script.pending_repositories = [Ok(Some(admitted.clone())), Ok(Some(admitted))].into();
        script.retire_repository_choice_after_guard = true;
        script.readings = [vec![Observation {
            standing: Standing::InPlace,
            ..Observation::unknown(Subject::Setting(Setting::Repositories))
        }]]
        .into();
    });
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan);
        rows.rows.clear();
        rows.choices
    };
    let seen = SETUP_WAKES.marks();
    observe_rows(ObserveRequest {
        plan: None,
        choices,
        subjects: vec![Subject::Setting(Setting::Repositories)],
        forced: false,
        pending_repositories: true,
    });
    wait_until("the retired completion requeue ends", observe_idle);
    assert_eq!(
        script().reads.len(),
        1,
        "the next request must still require a current pending scope"
    );
    assert!(OBSERVED_ROWS.lock().unwrap().rows.is_empty());
    assert_eq!(SETUP_WAKES.marks(), seen);
    assert_eq!(script().read_checks, [false]);
}

#[test]
fn pending_repository_refresh_cannot_widen_or_retire_an_ordinary_observation() {
    let mut queue = ObserveQueue {
        running: false,
        active: Vec::new(),
        current: None,
        next: None,
        check: None,
        misses: 0,
        missed_at: 0,
    };
    let pending = || ObserveRequest {
        plan: None,
        choices: 7,
        subjects: vec![Subject::Setting(Setting::Repositories)],
        forced: false,
        pending_repositories: true,
    };
    queue.enqueue(pending());
    queue.enqueue(ObserveRequest {
        subjects: vec![Subject::Setting(Setting::Dock)],
        pending_repositories: false,
        ..pending()
    });
    let ordinary = queue.next.take().unwrap();
    assert!(!ordinary.pending_repositories && !ordinary.forced);
    assert!(ordinary.subjects.contains(&Subject::Setting(Setting::Dock)));
    queue.enqueue(ordinary);
    queue.enqueue(pending());
    assert!(!queue.next.as_ref().unwrap().pending_repositories);
    queue.enqueue(ObserveRequest {
        forced: true,
        pending_repositories: false,
        ..pending()
    });
    let check = queue.check.take().unwrap();
    assert!(check.forced && !check.pending_repositories);
    assert!(check.subjects.contains(&Subject::Setting(Setting::Dock)));
}

/// Check again looks for the ALab vault before it reads the Secrets vault row, so the row
/// reads what the vault answers now; a reading nobody forced, or a Check of other rows, never
/// looks.
#[test]
fn check_again_looks_for_the_vault_before_it_reads_the_secrets_row() {
    let _globals = globals();
    let _io = TestIo::install();
    let secrets = vec![Subject::Setting(Setting::Secrets)];
    for (forced, subjects, looks) in [
        (false, secrets.clone(), 0),
        (true, vec![Subject::Setting(Setting::Dock)], 0),
        (true, secrets, 1),
    ] {
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        observe_rows(ObserveRequest {
            plan: Some(left_as_is()),
            choices,
            subjects,
            forced,
            pending_repositories: false,
        });
        assert_eq!(std::mem::take(&mut script().vault_looks), looks, "{forced}");
        assert_eq!(std::mem::take(&mut script().read_checks), [forced]);
    }
}

#[test]
fn vault_sync_guard_preserves_a_local_late_stop_and_refuses_unreadable_state() {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionStopTest, with_initial_setup_vault_sync_at,
    };
    use std::cell::Cell;

    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupAdmissionStopTest::startup(root.path()).unwrap();
    let revision = setup.complete_vault_setup().unwrap();
    change(|script| script.stop_root = Some(root.path().to_owned()));
    register_admitted_run(revision, send_admission(), false, None);
    with_setup_run(revision, || {});
    let effects = Cell::new(0);
    let mend = || {
        while_vault_sync_kept_with(&mut || effects.set(effects.get() + 1), |work| {
            with_initial_setup_vault_sync_at(root.path(), work)
        })
    };
    assert!(mend());
    assert_eq!(effects.get(), 1);

    assert!(stop_initial_setup());
    wait_until("the completed run's late Stop is saved", stop_worker_idle);
    assert!(setup.snapshot().unwrap().finished);
    assert_eq!(
        with_initial_setup_vault_sync_at(root.path(), |_| true).unwrap(),
        Some(true),
        "the completed backend run spends the Stop; this process still holds it locally"
    );
    assert!(!mend());
    assert_eq!(effects.get(), 1, "no mend effect crosses the local Stop");

    let sent = send_admission();
    let next = setup.choose().unwrap();
    assert!(next > revision);
    register_admitted_run(next, sent, false, None);
    with_setup_run(next, || {});
    assert!(mend(), "a new accepted choice admits its own effects");
    assert_eq!(effects.get(), 2);

    let path = root.path().join("initial-setup.json");
    let saved = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{\"profile\":").unwrap();
    assert!(!mend(), "an unreadable run certifies no mend authority");
    assert_eq!(effects.get(), 2);
    std::fs::write(&path, saved).unwrap();
    assert!(mend(), "a repaired record is read again on the next turn");
    assert_eq!(effects.get(), 3);
    assert!(script().runs.is_empty(), "the guard starts no setup stage");
}

#[test]
fn stop_hands_its_save_to_a_worker_and_a_quit_waits_for_it() {
    let _globals = globals();
    let _io = TestIo::install();
    // No run is registered here: nothing stops, and nothing is saved.
    assert!(!stop_initial_setup());
    assert!(settle_setup_stop(Duration::ZERO));
    assert!(script().stop_attempts.is_empty());
    // Run 41 is going, and the disk is slow. Stop returns while its save
    // waits: it never runs on the thread that asked, the UI's.
    change(|script| script.stop_hold = true);
    set_setup_run(Some(41));
    assert!(stop_initial_setup());
    // The pass in this process stops at its next boundary from memory.
    assert!(setup_stop_requested(41));
    wait_until("the Stop's worker begins its save", || {
        !script().stop_attempts.is_empty()
    });
    let (revision, thread) = script().stop_attempts[0];
    assert_eq!(revision, 41);
    assert_ne!(thread, std::thread::current().id());
    // Stop pressed again: the save in flight is not begun again.
    assert!(stop_initial_setup());
    // A quit waits for the save.
    assert!(
        !settle_setup_stop(Duration::ZERO),
        "the save is still going"
    );
    let quit = std::thread::spawn(|| settle_setup_stop(HANG));
    change(|script| script.stop_hold = false);
    assert!(quit.join().unwrap());
    assert_eq!(script().stops, [41]);
    // A saved Stop is saved once per run.
    assert!(stop_initial_setup());
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stop_attempts.len(), 1);
}

#[test]
fn a_stop_whose_save_failed_is_saved_on_the_next_ask_or_at_quit() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| script.stop_failures = 1);
    set_setup_run(Some(41));
    assert!(stop_initial_setup());
    wait_until("the failed save ends", stop_worker_idle);
    assert_eq!(script().stop_attempts.len(), 1);
    assert!(script().stops.is_empty());
    // Stop pressed again: the save is made again.
    assert!(stop_initial_setup());
    wait_until("the second save ends", stop_worker_idle);
    assert_eq!(script().stops, [41]);
    assert_eq!(script().stop_attempts.len(), 2);
    // A save that fails just before a quit is made again by the quit.
    change(|script| script.stop_failures = 1);
    set_setup_run(Some(42));
    assert!(stop_initial_setup());
    wait_until("the failed save ends", stop_worker_idle);
    assert_eq!(script().stops, [41]);
    assert!(settle_setup_stop(HANG));
    assert_eq!(script().stops, [41, 42]);
}

#[test]
fn a_choice_made_while_a_replacing_one_is_admitted_stops_the_run_given_back() {
    let _globals = globals();
    let _io = TestIo::install();
    // Run 7 is parked. The person's first choice takes the free slot and
    // drops run 7's registration; its admission is slow, and refuses it.
    set_setup_run(Some(7));
    change(|script| {
        script.persist_hold = true;
        script.admits = [None, Some(9)].into();
    });
    let (receipt, first) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    wait_until("the first choice's admission begins", || {
        script().persists == 1
    });
    // The person chooses again meanwhile: no run is registered, so the
    // second choice stops nothing yet, and waits for the slot.
    let (receipt, second) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(Plan::default(), false, None, receipt));
    assert!(!setup_stop_requested(7));
    // The first is refused: run 7 is given back, stopped before the slot
    // is free for it to take a step, and the second choice runs next.
    change(|script| script.persist_hold = false);
    assert_eq!(first.recv_timeout(HANG), Ok(false));
    assert_eq!(second.recv_timeout(HANG), Ok(true));
    assert!(setup_stop_requested(7));
    assert_eq!(setup_run(), Some(9));
    // The admitted choice runs once its receipt is accepted.
    wait_until("run 9 starts", || !script().runs.is_empty());
    assert_eq!(script().runs, [9]);
    wait_until("run 7's Stop is saved", stop_worker_idle);
    assert_eq!(script().stops, [7]);
}

/// This launch's startup as `prepare_setup` drives it, on the live claim
/// ([`StartupClaim`]), the live count of choices waiting for it
/// ([`choice_awaits_claim`]) and the live numbering of what it sends
/// ([`StartupSent`]); its reads and writes are [`TestIo`]'s script.
struct Startup {
    /// Its offer waits for the page's answer until it is withdrawn
    /// ([`startup_offer_withdrawn`]); otherwise nobody answers it.
    awaits_answer: bool,
    /// The page's answer, when the page answers at once.
    answer: Option<Plan>,
    sent: StartupSent,
}

impl SetupStartup for Startup {
    type Claim = StartupClaim;

    fn send(&mut self) {
        self.sent.send();
    }
    fn answered(&mut self) {
        self.sent.answered();
    }
    fn bind_authorization(&mut self) -> Result<u64, String> {
        Ok(1)
    }
    fn revision(&mut self) -> Result<u64, String> {
        Ok(script().revision)
    }
    fn claim(&mut self) -> Result<Option<StartupClaim>, String> {
        StartupClaim::claim()
    }
    fn finished(&mut self) -> Result<bool, String> {
        Ok(false)
    }
    fn choice_awaits_claim(&mut self) -> bool {
        choice_awaits_claim()
    }
    fn admission(&mut self) -> Result<Option<SetupAdmission>, String> {
        Ok(None)
    }
    fn initial(&mut self) -> Result<Plan, String> {
        change(|script| script.startup_reading = true);
        let mut script = wait_while(script(), |script| script.initial_hold);
        script.startup_reading = false;
        Ok(Plan::default())
    }
    fn offer(&mut self, initial: Plan) -> aterm_gui::WorkstationSetupDecision {
        change(|script| script.offers += 1);
        if let Some(answer) = self.answer.take() {
            if script().startup_answer_hold {
                return aterm_gui::drive_initial_setup_answer_for_test(
                    initial,
                    self.sent.handoff(),
                    |deliver| {
                        assert!(deliver(
                            aterm_gui::WorkstationSetupDecision::Begin(answer).into()
                        ));
                        change(|script| script.startup_answer_delivered = true);
                        drop(wait_while(script(), |script| script.startup_answer_hold));
                    },
                )
                .decision;
            }
            return aterm_gui::WorkstationSetupDecision::Begin(answer);
        }
        if self.awaits_answer {
            wait_until("the offer is withdrawn for the choice", || {
                startup_offer_withdrawn(|| false)
            });
        }
        aterm_gui::WorkstationSetupDecision::Unavailable
    }
    fn authorize(&mut self, _: &Plan) -> Result<Option<u64>, String> {
        change(|script| script.authorizing += 1);
        let mut script = wait_while(script(), |script| script.authorize_hold);
        let written = script
            .authorizations
            .pop_front()
            .expect("a write of the answer was scripted");
        if let Ok(Some(revision)) = written {
            script.revision = revision;
        }
        written
    }
    fn authorization_pending(&self) -> bool {
        true
    }
    fn stopped(&self) -> bool {
        stop_covers(self.sent.sent)
    }
    fn wait_holding_claim(&mut self) -> bool {
        change(|script| script.retry_waits += 1);
        // Twice the hang detector: a choice that had to wait it out
        // fails the test before the wait ends.
        let mut seen = SETUP_WAKES.count();
        wait_before_admission_retry(self.sent.sent, &mut seen, HANG * 2, |seen, limit| {
            if !choice_awaits_claim() {
                SETUP_WAKES.back_off(seen, limit);
            }
        });
        choice_awaits_claim()
    }
    fn backoff(&mut self) {
        drop(wait_while(script(), |script| script.startup_backoff_hold));
        std::thread::sleep(Duration::from_millis(20));
    }
    fn report_failure(&mut self, why: &str) {
        change(|script| script.startup_failures.push(why.to_owned()));
    }
}

/// The startup, on its own thread: what it prepared, and, as the window's
/// worker does next, that run, started ([`run_prepared_setup`]).
fn startup_with(
    awaits_answer: bool,
    answer: Option<Plan>,
) -> std::thread::JoinHandle<&'static str> {
    std::thread::spawn(move || {
        let mut host = Startup {
            awaits_answer,
            answer,
            sent: StartupSent::default(),
        };
        let prepared = prepare_setup(&mut host);
        let outcome = match &prepared {
            Ok(PreparedSetup::Authorized { .. }) => "authorized",
            Ok(PreparedSetup::Finished) => "finished",
            Ok(PreparedSetup::Unavailable) => "unavailable",
            Err(_) => "failed",
        };
        if let Ok(prepared) = prepared {
            run_prepared_setup(prepared, None, host.sent);
        }
        outcome
    })
}

fn startup(awaits_answer: bool) -> std::thread::JoinHandle<&'static str> {
    startup_with(awaits_answer, None)
}

#[test]
fn a_choice_made_while_the_startup_reads_the_plan_waits_for_its_claim_and_no_offer_shows() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| {
        script.initial_hold = true;
        script.claim_hold = true;
        script.admits = [Some(9)].into();
    });
    let startup = startup(false);
    wait_until("the startup reads the plan in force", || {
        script().startup_reading
    });
    assert!(startup_holds_claim());
    // The person chooses meanwhile. The choice waits for the startup's
    // claim, counted from the moment it is made, before its worker asks.
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert!(choice_awaits_claim());
    // The startup lets its claim go rather than offer over the choice;
    // the choice is admitted, and the startup's run is superseded.
    change(|script| {
        script.initial_hold = false;
        script.claim_hold = false;
    });
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    assert_eq!(startup.join().unwrap(), "unavailable");
    assert_eq!(script().offers, 0, "no offer is shown over the choice");
    // The admitted choice runs once its receipt is accepted.
    wait_until("run 9 starts", || !script().runs.is_empty());
    assert_eq!(script().runs, [9]);
    assert!(!choice_awaits_claim() && !startup_holds_claim());
}

#[test]
fn an_offer_waiting_for_its_answer_is_withdrawn_for_a_choice_made_meanwhile() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| script.admits = [Some(9)].into());
    let startup = startup(true);
    wait_until("the startup offers its plan", || script().offers == 1);
    // The offer has not reached the page yet when the person chooses.
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    assert_eq!(startup.join().unwrap(), "unavailable");
    assert_eq!(script().offers, 1, "the offer is not shown again");
    // The admitted choice runs once its receipt is accepted.
    wait_until("run 9 starts", || !script().runs.is_empty());
    assert_eq!(script().runs, [9]);
    assert!(script().startup_failures.is_empty());
}

/// A choice waits for at most one read or one write of the startup's (design §3.3): one made
/// while the startup waits to write its failed answer again gets the claim at once.
#[test]
fn a_choice_made_while_the_startup_waits_to_write_its_answer_again_gets_the_claim_at_once() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| {
        script.authorizations = [
            Err("another doctor profile pass is running".into()),
            Ok(None),
        ]
        .into();
        script.startup_backoff_hold = true;
        script.admits = [Some(9)].into();
    });
    let startup = startup_with(false, Some(Plan::default()));
    wait_until("the startup waits to write its answer again", || {
        script().retry_waits == 1
    });
    assert!(startup_holds_claim());
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    // Admitted while the startup's wait (a minute) has not run out.
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    assert_eq!(script().authorizing, 1, "the answer was not written first");
    wait_until("run 9 starts", || !script().runs.is_empty());
    // The startup's wait ends: it takes the claim again, and finds its
    // answer retired by the choice's admission.
    change(|script| script.startup_backoff_hold = false);
    assert_eq!(startup.join().unwrap(), "unavailable");
    assert_eq!(script().authorizing, 2);
    assert_eq!(script().runs, [9]);
    assert_eq!(
        script().startup_failures,
        ["another doctor profile pass is running"]
    );
}

/// A Stop pressed while a choice that replaces a parked run is being admitted holds for the
/// run the choice admits, or for the run a refusal gives back; a quit meanwhile waits until
/// that Stop is saved.
#[test]
fn a_stop_pressed_while_a_choice_is_admitted_holds_for_its_run_and_a_quit_waits_for_it() {
    let _globals = globals();
    let _io = TestIo::install();
    for admitted in [Some(9), None] {
        wait_until("the slot is free", || {
            !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
        });
        reset_worker_state();
        set_setup_run(Some(7));
        change(|script| {
            script.persist_hold = true;
            script.admits = [admitted].into();
            script.persists = 0;
            script.stop_attempts.clear();
            script.stops.clear();
            script.runs.clear();
        });
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left_as_is(), false, None, receipt));
        wait_until("the choice's admission begins", || script().persists == 1);
        // Stop: the choice took run 7's registration, so nothing stops now.
        assert!(!stop_initial_setup());
        assert!(script().stop_attempts.is_empty());
        // A quit now waits for what the Stop owes.
        assert!(
            !settle_setup_stop(Duration::ZERO),
            "a quit waits for the admission the Stop covers"
        );
        let quit = std::thread::spawn(|| settle_setup_stop(HANG));
        change(|script| script.persist_hold = false);
        assert_eq!(answer.recv_timeout(HANG), Ok(admitted.is_some()));
        assert!(quit.join().unwrap(), "the quit saw the Stop saved");
        let stopped = admitted.unwrap_or(7);
        assert_eq!(setup_run(), Some(stopped));
        assert!(setup_stop_requested(stopped));
        assert_eq!(script().stops, [stopped]);
        if admitted.is_some() {
            wait_until("run 9 starts, to stop at its first boundary", || {
                !script().runs.is_empty()
            });
            assert_eq!(script().runs, [9]);
        }
    }
}

/// The same while a pass runs: the choice stops run 7 and waits for the slot; its own run
/// starts stopped as it registers, and a quit before then waits for its Stop.
#[test]
fn a_stop_pressed_while_a_choice_waits_for_the_pass_ahead_holds_for_its_run() {
    let _globals = globals();
    let _io = TestIo::install();
    set_setup_run(Some(7));
    assert!(take_initial_setup_slot_now(), "run 7's pass holds the slot");
    change(|script| script.admits = [Some(9)].into());
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert!(setup_stop_requested(7), "the choice stops the pass ahead");
    assert!(stop_initial_setup(), "Stop: run 7 is the run going");
    // The Stop's worker saves the newest Stop handed to it: run 9's,
    // handed before it takes run 7's, would stand for both.
    wait_until("run 7's Stop is saved", || script().stops == [7]);
    assert!(!settle_setup_stop(Duration::ZERO));
    let quit = std::thread::spawn(|| settle_setup_stop(HANG));
    // Run 7's stage ends: it lets the slot go.
    clear_setup_run(7);
    INITIAL_SETUP_BUSY.store(false, std::sync::atomic::Ordering::Release);
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    assert!(quit.join().unwrap());
    assert!(setup_stop_requested(9), "run 9 starts stopped");
    assert_eq!(script().stops, [7, 9]);
}

/// Stop pressed while the startup writes the page's answer: its run starts stopped as it
/// registers, and a quit made meanwhile waits for that.
#[test]
fn a_stop_pressed_while_the_startups_answer_is_admitted_holds_for_its_run() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| {
        script.authorizations = [Ok(Some(8))].into();
        script.authorize_hold = true;
    });
    let startup = startup_with(false, Some(Plan::default()));
    wait_until("the startup writes the page's answer", || {
        script().authorizing == 1
    });
    assert!(!stop_initial_setup(), "no run is registered yet");
    assert!(!settle_setup_stop(Duration::ZERO));
    let quit = std::thread::spawn(|| settle_setup_stop(HANG));
    change(|script| script.authorize_hold = false);
    assert!(quit.join().unwrap());
    assert_eq!(startup.join().unwrap(), "authorized");
    assert!(setup_stop_requested(8), "run 8 starts stopped");
    assert_eq!(script().stops, [8]);
    assert_eq!(script().runs, [8]);
}

/// A Stop that holds the run a watch widened, asked for the plan it widened or pressed since
/// the watch looked, marks that run for its pass to put back the plan it widened (System
/// State design §3.5). A run a later choice overtook, a choice's own run, and a widened run
/// nobody stopped are not marked.
#[test]
fn only_a_widened_run_a_stop_holds_puts_back_its_plan() {
    let _globals = globals();
    let _io = TestIo::install();
    let held = |stop_before: bool, stop_after: bool, overtaken: bool, widened: bool| {
        wait_until("no Stop is owed", stop_worker_idle);
        reset_worker_state();
        set_setup_run(Some(7));
        if stop_before {
            assert!(stop_initial_setup());
        }
        let look = send_admission();
        if stop_after {
            assert!(stop_initial_setup());
        }
        let held = register_admitted_run(8, look, overtaken, widened.then_some(7));
        assert_eq!(
            setup_stop_requested(8),
            stop_before && widened || stop_after || overtaken,
            "the run is stopped as it registers"
        );
        held
    };
    assert!(
        held(true, false, false, true),
        "a Stop of the plan it widened"
    );
    assert!(
        held(false, true, false, true),
        "a Stop since the watch looked"
    );
    assert!(
        !held(false, true, true, true),
        "a later choice replaces the plan"
    );
    assert!(!held(false, true, false, false), "a choice's own run");
    assert!(!held(false, false, false, true), "nobody stopped it");
    wait_until("no Stop is owed", stop_worker_idle);
    reset_worker_state();
}

/// A run aterm widened says so on the page while it goes (System State design §3.5), and no
/// longer once its worker has ended: a later worker the slot keeps busy, which the page
/// counts as a run (a put-back once Full Disk Access is granted), never shows the note
/// again.
#[test]
fn the_widened_note_goes_when_its_run_ends() {
    let _globals = globals();
    let _io = TestIo::install();
    let claim = StartupClaim::claim().unwrap().unwrap();
    let sent = StartupSent {
        widened_from: Some(7),
        ..StartupSent::default()
    };
    run_prepared_setup(PreparedSetup::Authorized { claim, revision: 8 }, None, sent);
    assert_eq!(script().runs, [8]);
    assert_eq!(script().widened, [8], "said while the run goes");
    assert_eq!(
        SETUP_WIDENED.load(std::sync::atomic::Ordering::Acquire),
        0,
        "the run ended"
    );
    assert!(!initial_setup_status().widened);
    clear_setup_run(8);
}

/// A saved setup aterm could not read just now is one calm line, no failure (System State
/// design §3.6): aterm reads it again by itself.
#[test]
fn a_saved_setup_unread_just_now_is_one_calm_line() {
    let status = initial_setup_status_with(
        Err(InitialSetupSnapshotError::Read(
            "automatic-plan.json: Permission denied".into(),
        )),
        false,
        &InitialSetupRows::default(),
    );
    assert!(status.plan.is_none());
    assert_eq!(
        status.rows,
        [aterm_gui::calm_setup_row(aterm_gui::STATE_UNREAD_NOTE)]
    );
    assert!(status.rows.iter().all(|row| row.status != "failed"));
}

/// A newer aterm's saved setup is one calm line that says so, no failure, and no read again
/// by itself: this aterm leaves that setup as it is.
#[test]
fn a_newer_aterms_saved_setup_is_one_calm_line_read_no_more() {
    let status = initial_setup_status_with(
        Err(InitialSetupSnapshotError::Newer),
        false,
        &InitialSetupRows::default(),
    );
    assert!(status.plan.is_none() && status.permissions.is_none());
    assert_eq!(
        status.rows,
        [aterm_gui::calm_setup_row(aterm_gui::NEWER_SETUP_NOTE)]
    );
    assert!(aterm_cli::macos_doctor::setup_is_newer(
        &InitialSetupSnapshotError::Newer.to_string()
    ));
}

/// A Join refused because aterm cannot read this Mac's vault settings is one calm line that
/// says so, never a failure with Set up now: pressing it again meets the same settings. Any
/// other setup error, and a Check's, is still the failure it was.
#[test]
fn a_join_refused_for_unread_vault_settings_is_one_calm_line() {
    let refused = "cannot read which vault this Mac is in: source.toml: Permission denied";
    assert_eq!(
        setup_error_row(refused, false),
        aterm_gui::calm_setup_row(aterm_gui::VAULT_UNREAD_NOTE)
    );
    assert_eq!(setup_error_row(refused, true).status, "failed");
    let other = setup_error_row("the plan could not be saved", false);
    assert_eq!(
        (other.name.as_str(), other.status.as_str()),
        (aterm_gui::SETUP_ROW, "failed")
    );
}

/// The launch's Join that aterm admitted without the Join is said in one calm line while
/// the Secrets row still reads that aterm cannot read this Mac's vault settings, and only
/// then: once they read, the row says how the vault stands, and the typed vault is the
/// person's to Join.
#[test]
fn a_startup_join_left_unjoined_is_said_while_the_vault_settings_stay_unread() {
    use aterm_types::macos_setup::{Observation, Setting, Subject};
    let secrets = |note: Option<&str>| Observation {
        note: note.map(str::to_owned),
        ..Observation::unknown(Subject::Setting(Setting::Secrets))
    };
    let status = |observed: Vec<Observation>| aterm_gui::SetupStatus {
        plan: Some(Plan::default()),
        observed,
        ..aterm_gui::SetupStatus::default()
    };
    let unread = aterm_cli::macos_doctor::VAULT_SETTINGS_UNREAD;
    let mut said = status(vec![secrets(Some(unread))]);
    add_vault_not_joined(&mut said, true);
    assert_eq!(
        said.rows,
        [aterm_gui::calm_setup_row(aterm_gui::VAULT_NOT_JOINED_NOTE)]
    );
    for (not_joined, observed) in [
        (false, vec![secrets(Some(unread))]),
        (true, vec![secrets(None)]),
        (true, Vec::new()),
    ] {
        let mut quiet = status(observed);
        add_vault_not_joined(&mut quiet, not_joined);
        assert!(quiet.rows.is_empty());
    }
}

/// A step of the launch's setup that failed is tried again by itself: the page says so in
/// one calm line with every failure kept off the menu line and the tab (System State design
/// §3.5, §3.6); ten minutes on, once, the exceptions digest says setup has not started, and
/// the entry goes once the startup goes on.
#[test]
fn a_startup_that_tries_again_is_calm_and_told_to_the_digest_ten_minutes_on() {
    let start = Instant::now();
    let mut retry = StartupRetry::NONE;
    assert!(!retry.failed(start));
    assert!(retry.retrying());
    assert!(!retry.failed(start + Duration::from_secs(9 * 60)));
    assert!(retry.failed(start + STARTUP_STUCK_AFTER));
    assert!(!retry.failed(start + STARTUP_STUCK_AFTER * 2), "once");
    assert!(retry.went_on(), "its entry goes");
    assert_eq!(retry, StartupRetry::NONE);
    assert!(!retry.went_on());
    assert!(
        !retry.failed(start + STARTUP_STUCK_AFTER * 3),
        "counted anew"
    );

    let mut status = aterm_gui::SetupStatus {
        plan: Some(Plan::default()),
        ..aterm_gui::SetupStatus::default()
    };
    add_startup_retry(&mut status, false);
    assert!(!status.retrying && status.rows.is_empty());
    add_startup_retry(&mut status, true);
    assert!(status.retrying);
    assert_eq!(
        status.rows,
        [aterm_gui::calm_setup_row(aterm_gui::STARTUP_RETRY_NOTE)]
    );
    // A saved setup it cannot read says so in its own line alone.
    let mut unread = initial_setup_status_with(
        Err(InitialSetupSnapshotError::Read("unread".into())),
        false,
        &InitialSetupRows::default(),
    );
    add_startup_retry(&mut unread, true);
    assert_eq!(
        unread.rows,
        [aterm_gui::calm_setup_row(aterm_gui::STATE_UNREAD_NOTE)]
    );
}

/// A Restore that failed as a whole says why in the page's words: one that cannot read the
/// settings aterm saved before setup changed them names where to learn why, since pressing
/// it again reads the same record.
#[test]
fn a_whole_restore_failure_says_why_in_the_pages_words() {
    let unread = restore_failed_row(&format!(
        "{}: backup: expected value",
        aterm_cli::macos_doctor::BACKUP_UNREAD
    ));
    assert_eq!(unread.detail, aterm_gui::RESTORE_BACKUP_UNREAD_NOTE);
    assert_eq!(unread.status, "failed");
    let other = restore_failed_row("state.json: No space left on device");
    assert_eq!(other.detail, aterm_gui::RESTORE_FAILED_NOTE);
    assert_eq!(unread.name, other.name);
}

#[test]
fn a_startup_answer_handoff_keeps_its_stop_owed_before_the_worker_reads_the_answer() {
    let _globals = globals();
    let _io = TestIo::install();
    change(|script| {
        script.authorizations = [Ok(Some(8))].into();
        script.startup_answer_hold = true;
    });
    let startup = startup_with(false, Some(Plan::default()));
    wait_until("the request delivers the page's answer", || {
        script().startup_answer_delivered
    });
    assert!(!stop_initial_setup(), "no run is registered yet");
    let settled_before_return = settle_setup_stop(Duration::ZERO);
    let quit = std::thread::spawn(|| settle_setup_stop(HANG));
    change(|script| script.startup_answer_hold = false);
    assert_eq!(startup.join().unwrap(), "authorized");
    assert!(quit.join().unwrap());
    wait_until("run 8's Stop is saved", || script().stops == [8]);
    assert!(setup_stop_requested(8));
    assert_eq!(script().runs, [8]);
    assert!(
        !settled_before_return,
        "the quit must wait for a delivered answer before the worker reads it"
    );
}

#[test]
fn an_unanswered_startup_answer_handoff_leaves_nothing_for_a_quit_to_wait_for() {
    let _globals = globals();
    let _io = TestIo::install();
    let mut sent = StartupSent::default();
    sent.send();
    let answer = aterm_gui::drive_initial_setup_answer_for_test(
        Plan::default(),
        sent.handoff(),
        |deliver| {
            assert!(!stop_initial_setup());
            assert!(settle_setup_stop(Duration::ZERO));
            assert!(deliver(
                aterm_gui::WorkstationSetupDecision::Unavailable.into()
            ));
        },
    );
    assert_eq!(
        answer.decision,
        aterm_gui::WorkstationSetupDecision::Unavailable
    );
    assert!(settle_setup_stop(Duration::ZERO));
    assert!(script().stop_attempts.is_empty());
}

/// A Stop covers only what was sent before it: pressed before the startup offers or before a
/// choice is made, it stops neither's run, and a quit owes nothing for them.
#[test]
fn a_stop_pressed_before_a_plan_is_sent_never_stops_its_run() {
    let _globals = globals();
    let _io = TestIo::install();
    assert!(!stop_initial_setup());
    change(|script| script.authorizations = [Ok(Some(8))].into());
    assert_eq!(
        startup_with(false, Some(Plan::default())).join().unwrap(),
        "authorized"
    );
    assert_eq!(script().runs, [8]);
    assert!(!setup_stop_requested(8));
    clear_setup_run(8);
    assert!(stop_initial_setup());
    wait_until("completed run 8's Stop is saved", stop_worker_idle);
    assert_eq!(script().stops, [8]);
    change(|script| script.admits = [Some(9)].into());
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    wait_until("run 9 starts", || script().runs.len() == 2);
    assert!(!setup_stop_requested(9));
    assert!(settle_setup_stop(Duration::ZERO), "nothing is owed");
    assert_eq!(script().stops, [8], "the earlier Stop never names run 9");
}

/// While the GitHub repositories need a person, System State's look for a renewed token
/// reads that row alone, and only once the vault's mark moved since the row was last read:
/// an unchanged mark, or one that cannot be read, reads nothing and asks nothing of GitHub
/// (design §3.6).
#[test]
fn a_look_for_a_renewed_token_reads_github_alone_once_its_mark_moved() {
    use aterm_secrets::client::MaterializationMark;
    let _globals = globals();
    let _io = TestIo::install();
    let mark = |byte: u8| MaterializationMark::from_cache_key(&format!("{byte:02x}").repeat(32));
    script().vault_mark = mark(1);
    let plan = Plan::default();
    // The page's reading of every row reads GitHub too, and keeps the vault's mark.
    assert!(observe_initial_setup(plan.clone()));
    wait_until("the page's reading ends", || {
        script().reads.len() == 1 && observe_idle()
    });
    assert!(
        !read_github_again_if_its_token_moved(plan.clone()),
        "an unchanged mark"
    );
    script().vault_mark = None;
    assert!(
        !read_github_again_if_its_token_moved(plan.clone()),
        "a mark that cannot be read"
    );
    // A token renewed in the vault reached this Mac.
    script().vault_mark = mark(2);
    assert!(read_github_again_if_its_token_moved(plan.clone()));
    wait_until("the row's reading ends", || {
        script().reads.len() == 2 && observe_idle()
    });
    assert!(
        !read_github_again_if_its_token_moved(plan.clone()),
        "once per move"
    );
    let reads = std::mem::take(&mut script().reads);
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(
        reads[1],
        (plan, vec![Subject::Setting(Setting::Repositories)], true),
        "GitHub repositories alone, over the network"
    );
}

#[test]
fn a_token_look_retries_a_refused_observation_start() {
    use aterm_secrets::client::MaterializationMark;
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let mark = |byte: u8| MaterializationMark::from_cache_key(&format!("{byte:02x}").repeat(32));
    script().vault_mark = mark(1);
    let plan = Plan::default();
    assert!(observe_initial_setup(plan.clone()));
    wait_until("the original token's reading ends", || {
        script().reads.len() == 1 && observe_idle()
    });

    script().vault_mark = mark(2);
    let choices = OBSERVED_ROWS.lock().unwrap().choices;
    let refused =
        read_github_again_if_its_token_moved_with(plan.clone(), |plan, subjects, forced| {
            dispatch_observation_with(
                Some(ObserveRequest {
                    plan,
                    choices,
                    subjects,
                    forced,
                    pending_repositories: false,
                }),
                || 0,
                |_| {
                    Err(std::io::Error::other(
                        "injected token observation spawn refusal",
                    ))
                },
            )
        });
    assert!(!refused);
    assert!(observe_idle());
    assert_eq!(script().reads.len(), 1, "no worker read the renewed token");

    let accepted =
        read_github_again_if_its_token_moved_with(plan.clone(), |plan, subjects, forced| {
            dispatch_observation_with(
                Some(ObserveRequest {
                    plan,
                    choices,
                    subjects,
                    forced,
                    pending_repositories: false,
                }),
                || 0,
                |_| panic!("another request must not bypass the failed-start backoff"),
            )
        });
    let mut worker = None;
    let (release, held) = std::sync::mpsc::sync_channel(1);
    assert!(dispatch_observation_with(
        None,
        first_observe_retry_ms,
        |job| {
            worker = Some(std::thread::spawn(move || {
                held.recv_timeout(HANG).unwrap();
                job();
            }));
            Ok(())
        },
    ));
    // An accepted reading still waiting to run owns this mark. Another
    // look must not enqueue a duplicate before the worker samples it.
    let repeated = read_github_again_if_its_token_moved(plan.clone());
    if let Some(worker) = worker {
        release.send(()).unwrap();
        worker.join().unwrap();
    }
    assert!(
        accepted,
        "the same renewed token still needs its refused reading"
    );
    assert!(
        !repeated,
        "the accepted observation already owns this token move"
    );
    assert!(observe_idle());
    assert_eq!(
        script().reads,
        [
            (plan.clone(), Subject::all(), true),
            (
                plan.clone(),
                vec![Subject::Setting(Setting::Repositories)],
                true
            ),
        ],
        "the recovered observation reads only the requested source and row"
    );
    assert!(!read_github_again_if_its_token_moved(plan));
    assert_eq!(
        script().reads.len(),
        2,
        "one accepted reading per token move"
    );
}

#[test]
fn a_choice_outranks_every_reading_queued_or_in_flight_when_it_is_made() {
    let _globals = globals();
    for refresh in 0..3 {
        let _io = TestIo::install();
        // The page is shown with every group set up: its reading reads GitHub
        // over the network, and Messages and FaceTime. Check again queues its
        // broader read behind that one before the person's newer choice.
        let shown = Plan::default();
        script().hold = true;
        assert!(observe_initial_setup(shown.clone()));
        wait_until("the page's reading starts", || script().reads.len() == 1);
        assert!(request_observation(
            Some(shown.clone()),
            Subject::all(),
            true
        ));
        // Meanwhile the person leaves Workspace and Messages & FaceTime as is.
        let left = left_as_is();
        script().admits = [Some(9)].into();
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
        assert!(restart_initial_setup(left.clone(), false, None, receipt));
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        assert_eq!(setup_run(), Some(9));
        match refresh {
            0 => {}
            1 => assert!(observe_initial_setup(left.clone())),
            _ => assert!(request_observation(None, Subject::all(), false)),
        }
        // The readings end, and then a stage's end asks for one naming no plan.
        release_readings();
        wait_until("the queued reading ends", || {
            script().reads.len() == 2 && observe_idle()
        });
        assert!(request_observation(None, Subject::all(), false));
        wait_until("the stage's reading ends", || {
            script().reads.len() == 3 && observe_idle()
        });
        let reads = std::mem::take(&mut script().reads);
        // Only the reading in flight when the person chose was taken for the
        // page's plan. Neither it nor the page's queued one put that plan back.
        assert_eq!((&reads[0].0, reads[0].2), (&shown, true));
        for (plan, subjects, network) in &reads[1..] {
            assert_eq!(plan, &left);
            assert!(!network, "GitHub is never read for a group left as is");
            assert!(
                !subjects.iter().any(|subject| matches!(
                    subject,
                    Subject::Setting(setting) if setting.group() == Group::Accounts
                )),
                "nor is the person's account state"
            );
        }
        assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(left));
    }
}

/// Restore the queue even when an original-behavior assertion fails, so
/// TestIo's teardown never waits for a worker that failed to start or died.
struct ObserverDispatchReset;

impl Drop for ObserverDispatchReset {
    fn drop(&mut self) {
        let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
        queue.running = false;
        queue.active.clear();
        queue.current = None;
        queue.next = None;
        queue.check = None;
        queue.misses = 0;
        queue.missed_at = 0;
    }
}

/// A refresh that withheld a private row's read (System State not in front with a person)
/// read nothing, so the row's admitted reading stands, with its own time (design §3.5),
/// until a stage that could change the row runs: after it the old reading says nothing,
/// and the row is unread. An admitted reading replaces either, and a withheld read of a row
/// with no reading still says the row is unread.
#[test]
fn a_hidden_refresh_keeps_an_admitted_private_reading() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let dock = Subject::Setting(Setting::Dock);
    let plan = left_as_is();
    let admitted = Observation {
        standing: Standing::InPlace,
        now: Some("Auto-hides".into()),
        ..Observation::unknown(dock)
    }
    .stamped(READ_AT - 300);
    let (choices, generation) = {
        let epoch = SETUP_ACTIVITY.lock().unwrap().epoch;
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows = vec![admitted.clone()];
        // The page's presentation history already holds this reading, as a read before
        // this one would have left it, so the history copy at the next read's start
        // changes nothing either.
        rows.last_known = vec![admitted.clone()];
        rows.read_epochs = vec![(dock, epoch)];
        (rows.choices, rows.generation)
    };
    let read = |reading: Observation| {
        change(|script| script.readings = [vec![reading]].into());
        observe_rows(ObserveRequest {
            plan: Some(plan.clone()),
            choices,
            subjects: vec![dock],
            forced: false,
            pending_repositories: false,
        });
        let rows = OBSERVED_ROWS.lock().unwrap();
        (rows.rows.clone(), rows.generation)
    };
    assert_eq!(
        read(Observation::withheld(dock)),
        (vec![admitted.clone()], generation),
        "a read that never ran keeps the reading and its time, and changes nothing"
    );

    // A stage that can change the Dock starts and ends after that reading.
    {
        let mut activity = SETUP_ACTIVITY.lock().unwrap();
        activity.mark(&[Setting::Dock], &[dock], true);
        activity.mark(&[Setting::Dock], &[dock], false);
    }
    let (rows, after_stage) = read(Observation::withheld(dock));
    assert_eq!(
        rows,
        [Observation::withheld(dock)],
        "the reading from before the stage is not kept as the row now"
    );
    assert!(after_stage > generation);

    let differs = Observation {
        standing: Standing::Differs,
        now: Some("Shown".into()),
        ..Observation::unknown(dock)
    };
    let (rows, _) = read(differs.clone());
    assert_eq!(
        rows,
        [differs.stamped(READ_AT)],
        "an admitted reading replaces it, stamped as its read started"
    );

    OBSERVED_ROWS.lock().unwrap().rows.clear();
    let (rows, _) = read(Observation::withheld(dock));
    assert_eq!(
        rows,
        [Observation::withheld(dock)],
        "a row nothing read is unread"
    );
}

/// A relaunch nobody watches (design §3.5): what a previous launch kept of a private row it
/// read back as wanted stands for the row while this process has no reading of it, with its
/// own time, so a finished Mac reads set up as last read rather than unread. It is looked
/// for once, seeds no row this process read or a stage of this process touched, and this
/// process keeps what it reads in turn: a row read otherwise drops out of what the next
/// launch is handed. NEGATIVE CONTROL: before the look (no record kept), the row is unread.
#[test]
fn a_relaunch_reads_what_the_last_launch_read_back() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let dock = Subject::Setting(Setting::Dock);
    let desktop = Subject::Setting(Setting::Desktop);
    let plan = left_as_is();
    let kept = |subject: Subject| {
        Observation {
            standing: Standing::InPlace,
            now: Some("as wanted".into()),
            ..Observation::unknown(subject)
        }
        .stamped(READ_AT - 7200)
    };
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows.clear();
        rows.read_epochs.clear();
        rows.choices
    };
    // A stage of this process touched the Desktop, and none the Dock: what was kept of the
    // Desktop says nothing now.
    {
        let mut activity = SETUP_ACTIVITY.lock().unwrap();
        activity.settings.clear();
        activity.subjects.clear();
        activity.touched.clear();
        activity.mark(&[Setting::Desktop], &[desktop], true);
        activity.mark(&[Setting::Desktop], &[desktop], false);
    }
    let read = |readings: Vec<Observation>| {
        change(|script| script.readings = [readings].into());
        observe_rows(ObserveRequest {
            plan: Some(plan.clone()),
            choices,
            subjects: vec![dock, desktop],
            forced: false,
            pending_repositories: false,
        });
        OBSERVED_ROWS.lock().unwrap().rows.clone()
    };
    // A launch's first change that reads no private row writes nothing over the record
    // before its look.
    let secrets = Subject::Setting(Setting::Secrets);
    change(|script| {
        script.readings = [vec![Observation {
            standing: Standing::InPlace,
            ..Observation::unknown(secrets)
        }]]
        .into();
    });
    observe_rows(ObserveRequest {
        plan: Some(plan.clone()),
        choices,
        subjects: vec![secrets],
        forced: false,
        pending_repositories: false,
    });
    assert_eq!(OBSERVED_ROWS.lock().unwrap().rows.len(), 1);
    assert!(
        script().remembered_writes.is_empty(),
        "nothing written over the last launch's record before its look"
    );
    OBSERVED_ROWS.lock().unwrap().rows.clear();

    let fence = aterm_cli::macos_doctor::Fence::default();
    change(|script| script.remembered = vec![(kept(dock), fence), (kept(desktop), fence)]);
    let rows = read(vec![
        Observation::withheld(dock),
        Observation::withheld(desktop),
    ]);
    assert_eq!(
        rows,
        [kept(dock), Observation::withheld(desktop)],
        "the Dock reads as last read, its time kept; the Desktop a stage touched is unread"
    );
    assert_eq!(script().remembered_looks, 1);
    assert_eq!(
        script().remembered_writes.last(),
        Some(&vec![(kept(dock), fence)]),
        "what stands is kept again for the next launch, under its own fence"
    );
    let rows = read(vec![
        Observation::withheld(dock),
        Observation::withheld(desktop),
    ]);
    assert_eq!(rows, [kept(dock), Observation::withheld(desktop)]);
    assert_eq!(script().remembered_looks, 1, "looked for once");

    // A person at the page reads the Dock otherwise: it drops out of what is kept.
    let differs = Observation {
        standing: Standing::Differs,
        now: Some("Shown".into()),
        ..Observation::unknown(dock)
    };
    let rows = read(vec![differs.clone(), Observation::withheld(desktop)]);
    assert_eq!(rows[0], differs.stamped(READ_AT));
    assert_eq!(script().remembered_writes.last(), Some(&Vec::new()));

    // CONTROL: a launch with nothing kept leaves the row unread.
    OBSERVED_ROWS.lock().unwrap().rows.clear();
    OBSERVED_ROWS.lock().unwrap().remembered_looked = false;
    change(|script| script.remembered = Vec::new());
    let rows = read(vec![
        Observation::withheld(dock),
        Observation::withheld(desktop),
    ]);
    assert_eq!(
        rows,
        [Observation::withheld(dock), Observation::withheld(desktop)]
    );
}

/// A finished setup's first look at a launch is a refresh, not a request: nothing is known
/// to have changed, so it retires no reading and touches no row, and a private row it may
/// not read stands as the last launch read it back. NEGATIVE CONTROL: an ordinary request of
/// the same rows retires them first (something may have changed them), so what the last
/// launch kept says nothing and the row is unread.
#[test]
fn a_finished_launchs_first_look_keeps_what_the_last_launch_read_back() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    // The look queues behind this "running" worker; the test drains it.
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let dock = Subject::Setting(Setting::Dock);
    let plan = left_as_is();
    let kept = Observation {
        standing: Standing::InPlace,
        now: Some("as wanted".into()),
        ..Observation::unknown(dock)
    }
    .stamped(READ_AT - 7200);
    let fence = aterm_cli::macos_doctor::Fence::default();
    let looks: [(bool, fn() -> bool); 2] = [
        (true, look_at_finished_setup),
        (false, || request_observation(None, Subject::all(), false)),
    ];
    for (finished, look) in looks {
        {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(plan.clone());
            rows.vault_url = Some(plan.vault_url.trim().to_owned());
            rows.rows.clear();
            rows.read_epochs.clear();
            rows.remembered_looked = false;
        }
        {
            let mut activity = SETUP_ACTIVITY.lock().unwrap();
            activity.settings.clear();
            activity.subjects.clear();
            activity.touched.clear();
        }
        change(|script| {
            script.remembered = vec![(kept.clone(), fence)];
            script.readings = [vec![Observation::withheld(dock)]].into();
        });
        assert!(look());
        let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
        for request in pending.into_iter().flatten() {
            observe_rows(request);
        }
        let read: Vec<Observation> = OBSERVED_ROWS
            .lock()
            .unwrap()
            .rows
            .iter()
            .filter(|row| row.subject == dock)
            .cloned()
            .collect();
        if finished {
            assert_eq!(
                read,
                std::slice::from_ref(&kept),
                "the Dock reads as last read"
            );
        } else {
            assert_eq!(
                read,
                [Observation::withheld(dock)],
                "a request leaves it unread"
            );
        }
    }
}

/// Review finding 3: each reading is kept for the next launch under the journal as its own
/// read started, never as the snapshot is written: a later read that withholds the row (the
/// page out of sight) keeps the earlier reading under its earlier fence, so a change any
/// aterm process journaled between that read and the write keeps it out of the record
/// (`aterm_cli::macos_doctor::remember_private_read_back`). Before, the write took the
/// journal as it stood at the write, after such a change.
/// NEGATIVE CONTROL: a fresh reading of the row is kept under its own, later fence.
#[test]
fn a_reading_is_kept_under_the_journal_as_its_read_started() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let dock = Subject::Setting(Setting::Dock);
    let plan = left_as_is();
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows.clear();
        rows.read_epochs.clear();
        rows.fences.clear();
        rows.remembered_looked = true;
        rows.choices
    };
    {
        let mut activity = SETUP_ACTIVITY.lock().unwrap();
        activity.settings.clear();
        activity.subjects.clear();
        activity.touched.clear();
    }
    let in_place = Observation {
        standing: Standing::InPlace,
        now: Some("as wanted".into()),
        ..Observation::unknown(dock)
    };
    let read = |reading: Observation, fence: aterm_cli::macos_doctor::Fence| {
        change(|script| {
            script.readings = [vec![reading]].into();
            script.fences = [fence].into();
        });
        observe_rows(ObserveRequest {
            plan: Some(plan.clone()),
            choices,
            subjects: vec![dock],
            forced: false,
            pending_repositories: false,
        });
        script().remembered_writes.last().cloned()
    };
    let first = aterm_cli::macos_doctor::Fence::at(9, 100);
    let later = aterm_cli::macos_doctor::Fence::at(9, 180);
    assert_eq!(
        read(in_place.clone(), first),
        Some(vec![(in_place.clone().stamped(READ_AT), first)])
    );
    // The page goes out of sight; the journal grew meanwhile.
    let fresher = Observation {
        now: Some("as wanted, again".into()),
        ..in_place.clone()
    };
    assert_eq!(
        read(fresher.clone(), later),
        Some(vec![(fresher.clone().stamped(READ_AT), later)]),
        "CONTROL: a fresh reading is kept under its own fence"
    );
    change(|script| script.remembered_writes.clear());
    OBSERVED_ROWS.lock().unwrap().rows[0] = in_place.clone().stamped(READ_AT);
    OBSERVED_ROWS.lock().unwrap().fence(dock, Some(first));
    let caps = Subject::Setting(Setting::CapsLock);
    change(|script| {
        script.readings = [vec![Observation {
            standing: Standing::InPlace,
            ..Observation::unknown(caps)
        }]]
        .into();
        script.fences = [later].into();
    });
    observe_rows(ObserveRequest {
        plan: Some(plan.clone()),
        choices,
        subjects: vec![caps],
        forced: false,
        pending_repositories: false,
    });
    let written = script().remembered_writes.last().cloned().unwrap();
    assert!(
        written.contains(&(in_place.stamped(READ_AT), first)),
        "the Dock's reading is kept under its own read's fence, not the write's: {written:?}"
    );
}

/// What a stage's own run read of its row is that row's reading as the stage ends (design
/// §3.5), dated from before that run read anything: it replaces the reading from before the
/// stage, and the end refresh, withheld with nobody at the page, leaves it standing, so an
/// unattended pass ends with an admitted, dated reading. NEGATIVE CONTROL: an end that read
/// nothing leaves the row unread.
#[test]
fn a_stage_end_keeps_what_the_stage_read_back() {
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionStopTest, StageEvent, WorkstationReadback, run_workstation_readback,
    };
    use aterm_machines::init::{WorkstationGroup, prepare_workstation_selected};
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    // The stage's end refresh queues behind this "running" worker; the test drains it.
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let caps = Subject::Setting(Setting::CapsLock);
    // The native producer the pass runs: no fabricated reading enters the stage hook.
    let native = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
    let read_at = aterm_machines::port::System::now(&native);
    let mut selection = aterm_types::macos_setup::Selection::none();
    selection.set(Setting::CapsLock, true);
    let prepared = prepare_workstation_selected(&native, WorkstationGroup::All, selection).unwrap();
    let (_, readback) = run_workstation_readback(
        prepared,
        &native,
        WorkstationGroup::All,
        aterm_machines::model::Mode::Apply,
    )
    .unwrap();
    let plan = left_as_is();
    let before = Observation {
        standing: Standing::Differs,
        now: Some("Caps Lock".into()),
        ..Observation::unknown(caps)
    }
    .stamped(read_at.saturating_sub(600));
    let generation = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(plan.clone());
        rows.vault_url = Some(plan.vault_url.trim().to_owned());
        rows.rows = vec![before];
        rows.generation
    };
    *SETUP_STOP.lock().unwrap() = None;
    set_setup_run(Some(1));
    let empty = WorkstationReadback::EMPTY;
    let stage = |readback: &WorkstationReadback| {
        with_setup_activity(|| {
            for running in [true, false] {
                setup_stage(StageEvent {
                    revision: 1,
                    settings: &[Setting::CapsLock],
                    subjects: &[caps],
                    running,
                    readback: if running { &empty } else { readback },
                });
            }
        });
        // The end refresh, with nobody at the page: its private read is withheld.
        change(|script| script.readings = [vec![Observation::withheld(caps)]].into());
        let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
        for request in pending.into_iter().flatten() {
            observe_rows(request);
        }
        let rows = OBSERVED_ROWS.lock().unwrap();
        let read: Vec<Observation> = rows
            .rows
            .iter()
            .filter(|row| row.subject == caps)
            .cloned()
            .collect();
        (read, rows.generation)
    };
    let (read, after) = stage(&readback);
    assert_eq!(read.len(), 1, "{read:?}");
    assert_eq!(
        (read[0].standing, read[0].now.as_deref(), read[0].read_at()),
        (Standing::InPlace, Some("Control"), Some(read_at)),
        "the stage's reading is the row's, dated from before its run, and the end refresh \
         keeps it"
    );
    assert!(after > generation, "the reading is published");

    let (read, _) = stage(&WorkstationReadback::EMPTY);
    assert_eq!(
        read,
        [Observation::withheld(caps)],
        "a stage that read nothing leaves its row unread"
    );
    set_setup_run(None);
}

/// A Full Disk Access grant that reads off has the Access rows read again, and only them,
/// as an ordinary refresh: their reads need no page in front (design §3.5).
#[test]
fn a_full_disk_access_regression_reads_the_access_rows_again() {
    use aterm_types::macos_setup::Access;

    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    OBSERVE_QUEUE.lock().unwrap().running = true;
    full_disk_access_reads_off();
    let pending: Vec<ObserveRequest> = OBSERVE_QUEUE
        .lock()
        .unwrap()
        .take_pending()
        .into_iter()
        .flatten()
        .collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (pending[0].subjects.as_slice(), pending[0].forced),
        (Access::ALL.map(Subject::Access).as_slice(), false)
    );
}

/// The first failed start's pause on [`OBSERVE_AGAIN`], in [`observation_millis`]'s units.
fn first_observe_retry_ms() -> u64 {
    u64::try_from(OBSERVE_AGAIN.after(1).as_millis()).unwrap()
}

/// The window keeper's round ([`maintain_observation_dispatch`]) publishes the readings only
/// where it owes them: none over an idle queue or a running reader while the window holds
/// them, one a round while the window's loop dropped the last publication, and one for each
/// round that finds a batch whose reader never started, held or started. A request still
/// publishes as it is queued.
/// NEGATIVE CONTROL: with the idle exit publishing whatever `undelivered` says, the eight
/// rounds over an idle queue publish eight times.
#[test]
fn a_keeper_round_that_changed_nothing_publishes_nothing() {
    const ROUNDS: u32 = 8;
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let published = std::cell::Cell::new(0_u32);
    let publish = || published.set(published.get() + 1);
    let request = || ObserveRequest {
        plan: Some(left_as_is()),
        choices: OBSERVED_ROWS.lock().unwrap().choices,
        subjects: vec![Subject::Setting(Setting::Dock)],
        forced: false,
        pending_repositories: false,
    };
    let refuse = |_: ObservationJob| -> std::io::Result<()> {
        Err(std::io::Error::other("injected worker spawn refusal"))
    };
    let idle_round = |undelivered: bool| {
        assert!(dispatch_observation_publishing(
            None,
            || undelivered,
            publish,
            || panic!("an idle or running queue reads no clock"),
            |_| panic!("an idle or running queue starts nothing"),
        ));
    };

    assert!(observe_idle());
    for _ in 0..ROUNDS {
        idle_round(false);
    }
    assert_eq!(published.get(), 0, "an idle queue the window has read");

    OBSERVE_QUEUE.lock().unwrap().running = true;
    for _ in 0..ROUNDS {
        idle_round(false);
    }
    assert_eq!(published.get(), 0, "a running reader publishes for itself");
    // A request queued behind that reader changes what the page shows as queued.
    assert!(dispatch_observation_publishing(
        Some(request()),
        || false,
        publish,
        || panic!("a running queue reads no clock"),
        |_| panic!("a running queue starts nothing"),
    ));
    assert_eq!(published.get(), 1, "a queued request");
    {
        let mut queue = OBSERVE_QUEUE.lock().unwrap();
        queue.running = false;
        queue.next = None;
    }

    published.set(0);
    for _ in 0..ROUNDS {
        idle_round(true);
    }
    assert_eq!(
        published.get(),
        ROUNDS,
        "the loop dropped the last publication"
    );

    // A batch whose reader never started: one publication a round, held or started.
    let first = first_observe_retry_ms();
    published.set(0);
    assert!(!dispatch_observation_publishing(
        Some(request()),
        || false,
        publish,
        || 0,
        refuse,
    ));
    assert_eq!(published.get(), 1, "the refused start");
    assert!(dispatch_observation_publishing(
        None,
        || panic!("a retained batch publishes whatever the window holds"),
        publish,
        || first - 1,
        |_| panic!("inside the first pause"),
    ));
    assert_eq!(published.get(), 2, "held inside its pause");
    let mut worker = None;
    assert!(dispatch_observation_publishing(
        None,
        || panic!("a retained batch publishes whatever the window holds"),
        publish,
        || first,
        |job| {
            worker = Some(std::thread::spawn(job));
            Ok(())
        },
    ));
    assert_eq!(published.get(), 3, "started at its pause's end");
    worker.expect("the reader started").join().unwrap();
    assert!(observe_idle());
    for _ in 0..ROUNDS {
        idle_round(false);
    }
    assert_eq!(published.get(), 3, "idle again once the reader ended");
}

/// The keeper's round as the window runs it ([`maintain_observation_dispatch`], with the
/// real publisher and the real record of what the window's loop took) over an idle queue:
/// one publication a round while no loop takes the readings, from the launch on; one to
/// the loop once it registered its proxy, and none in the rounds after; and one more after
/// a status the loop dropped. The publications are counted where they reach the loop's
/// sender, on a stand-in for the loop on this thread.
/// NEGATIVE CONTROLS: [`dispatch_observation_with`] passing `|| true` for `undelivered`
/// (the unconditional publish) publishes in every round after the loop took the readings;
/// passing `|| false` publishes in none before it did.
#[test]
fn the_keepers_own_round_publishes_only_readings_the_window_is_owed() {
    const ROUNDS: u32 = 8;
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let window = aterm_gui::SetupLoopForTest::before_its_proxy();
    {
        let queue = OBSERVE_QUEUE.lock().unwrap();
        assert!(
            !queue.running && queue.next.is_none() && queue.check.is_none(),
            "the rounds below meet an idle queue"
        );
    }
    let rounds = || {
        for _ in 0..ROUNDS {
            maintain_observation_dispatch();
        }
    };

    rounds();
    assert_eq!(
        window.readings_sent(),
        ROUNDS,
        "no loop has taken the readings since the launch"
    );
    window.register();
    rounds();
    assert_eq!(
        window.readings_sent(),
        ROUNDS + 1,
        "once to the loop that took them, then nothing"
    );
    window.close();
    aterm_gui::publish_initial_setup_status(aterm_gui::SetupStatus::default());
    window.register();
    rounds();
    assert_eq!(
        window.readings_sent(),
        ROUNDS + 2,
        "once more after a status the loop dropped"
    );
    assert_eq!(window.statuses_sent(), 1, "the rounds publish no status");
}

/// Failed starts of the setup reader in a row wait on [`OBSERVE_AGAIN`], the one retry
/// schedule: five seconds after the first, ten after the second, and a start that runs
/// clears the count, so the next failure waits five seconds again.
/// NEGATIVE CONTROL: a pulse inside a pause starts nothing; one at its end starts the reader.
#[test]
fn failed_reader_starts_in_a_row_wait_twice_as_long_each() {
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let request = || ObserveRequest {
        plan: Some(left_as_is()),
        choices: OBSERVED_ROWS.lock().unwrap().choices,
        subjects: vec![Subject::Setting(Setting::Dock)],
        forced: false,
        pending_repositories: false,
    };
    let refuse = |_: ObservationJob| -> std::io::Result<()> {
        Err(std::io::Error::other("injected worker spawn refusal"))
    };
    let first = first_observe_retry_ms();
    assert!(!dispatch_observation_with(Some(request()), || 0, refuse));
    assert!(dispatch_observation_with(
        None,
        || first - 1,
        |_| panic!("inside the first pause")
    ));
    assert!(!dispatch_observation_with(None, || first, refuse));
    assert!(dispatch_observation_with(
        None,
        || first * 3 - 1,
        |_| panic!("inside the second, twice as long")
    ));
    let mut worker = None;
    assert!(dispatch_observation_with(
        None,
        || first * 3,
        |job| {
            worker = Some(std::thread::spawn(job));
            Ok(())
        }
    ));
    worker.expect("the reader started").join().unwrap();
    assert!(observe_idle());
    let at = first * 4;
    assert!(!dispatch_observation_with(Some(request()), || at, refuse));
    assert!(dispatch_observation_with(
        None,
        || at + first - 1,
        |_| panic!("a start that ran cleared the count: the first pause again")
    ));
    let mut worker = None;
    assert!(dispatch_observation_with(
        None,
        || at + first,
        |job| {
            worker = Some(std::thread::spawn(job));
            Ok(())
        }
    ));
    worker.expect("the reader started").join().unwrap();
}

#[test]
fn observer_dispatch_does_not_acknowledge_work_to_a_failed_start() {
    let _globals = globals();
    let _io = TestIo::install();
    let _queue = ObserverDispatchReset;
    let mut plan = left_as_is();
    plan.vault_url = "https://example.invalid/forced-current".into();
    let expected = plan.clone();
    let request = ObserveRequest {
        plan: Some(plan),
        choices: OBSERVED_ROWS.lock().unwrap().choices,
        subjects: vec![Subject::Setting(Setting::Repositories)],
        forced: true,
        pending_repositories: false,
    };
    let mut second = None;
    let started = dispatch_observation_with(
        Some(ObserveRequest {
            plan: Some(left_as_is()),
            choices: request.choices,
            subjects: vec![Subject::Setting(Setting::Dock)],
            forced: false,
            pending_repositories: false,
        }),
        || 0,
        |_| {
            let (reached, wait) = std::sync::mpsc::sync_channel(1);
            second = Some(std::thread::spawn(move || {
                // Coordinate without a timing assertion: if admission is
                // unlocked, force the second call to finish before the
                // failed spawn returns. If locked, signal before waiting
                // for the first caller to release its failed admission.
                let unlocked = match OBSERVE_QUEUE.try_lock() {
                    Ok(queue) => {
                        drop(queue);
                        true
                    }
                    Err(std::sync::TryLockError::WouldBlock) => false,
                    Err(std::sync::TryLockError::Poisoned(_)) => panic!("queue poisoned"),
                };
                if !unlocked {
                    reached.send(()).unwrap();
                }
                let mut worker = None;
                let accepted = dispatch_observation_with(
                    Some(request),
                    || 0,
                    |job| {
                        worker = Some(std::thread::spawn(job));
                        Ok(())
                    },
                );
                if unlocked {
                    reached.send(()).unwrap();
                }
                (accepted, worker)
            }));
            wait.recv_timeout(HANG).unwrap();
            Err(std::io::Error::other("injected worker spawn refusal"))
        },
    );
    assert!(!started, "the failed caller reports refusal");
    let (accepted, mut worker) = second.take().unwrap().join().unwrap();
    assert!(accepted);
    assert!(
        worker.is_none(),
        "a new arrival retains the failed-start backoff"
    );
    assert!(script().reads.is_empty(), "neither request started");
    assert!(dispatch_observation_with(
        None,
        || 0,
        |_| panic!("the keeper pulse must preserve the failed-start backoff"),
    ));
    assert!(dispatch_observation_with(
        None,
        first_observe_retry_ms,
        |job| {
            worker = Some(std::thread::spawn(job));
            Ok(())
        },
    ));
    let ran = worker.is_some();
    if let Some(worker) = worker {
        worker.join().unwrap();
    }
    assert!(
        ran,
        "accepted work must have a worker after the failed start"
    );
    let script = script();
    assert_eq!(
        script.reads.len(),
        2,
        "the refused start must retain its original request"
    );
    assert_eq!(script.reads[0].0, left_as_is());
    assert_eq!(script.reads[0].1, [Subject::Setting(Setting::Dock)]);
    assert!(
        !script.reads[0].2,
        "the ordinary request gains no Check scope"
    );
    assert_eq!(script.reads[1].0, expected);
    assert_eq!(script.reads[1].1, [Subject::Setting(Setting::Repositories)]);
    assert!(
        script.reads[1].2,
        "the accepted Check retains its forced scope"
    );
    assert!(observe_idle());
    assert!(dispatch_observation_with(
        None,
        || panic!("an idle pulse reads no clock"),
        |_| panic!("completed observations are never replayed"),
    ));
}

#[test]
fn observer_dispatch_retires_an_unwound_read_and_drains_the_next_check_once() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    for panics in [false, true] {
        let _io = TestIo::install();
        let _queue = ObserverDispatchReset;
        let preference = Subject::Setting(Setting::Dock);
        let personal = Subject::Setting(Setting::Messages);
        let independent = Subject::Setting(Setting::Spotlight);
        let repositories = Subject::Setting(Setting::Repositories);
        let seen = |subject, standing| Observation {
            standing,
            ..Observation::unknown(subject)
        };
        let old = seen(preference, Standing::InPlace);
        let private = seen(personal, Standing::InPlace);
        let kept = seen(independent, Standing::NeedsYou);
        let fresh = seen(repositories, Standing::InPlace);
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.rows = vec![old.clone(), private.clone(), kept.clone()];
            rows.choices
        };
        change(|script| {
            script.hold = true;
            script.panic_read = panics;
            script.readings = [vec![old], vec![fresh.clone()]].into();
        });
        let mut worker = None;
        assert!(queue_observation_with(
            ObserveRequest {
                plan: Some(left_as_is()),
                choices,
                subjects: vec![preference, personal, repositories],
                forced: false,
                pending_repositories: false,
            },
            |job| {
                worker = Some(std::thread::spawn(job));
                Ok(())
            },
        ));
        wait_until("the observer reads its first batch", || {
            script().reads.len() == 1
        });
        let mut next = left_as_is();
        next.vault_url = "https://example.invalid/next-check".into();
        assert!(queue_observation_with(
            ObserveRequest {
                plan: Some(next.clone()),
                choices,
                subjects: vec![repositories],
                forced: true,
                pending_repositories: false,
            },
            |_| panic!("the current worker owns the queued Check"),
        ));
        release_readings();
        let finished = worker.take().unwrap().join();
        assert!(
            finished.is_ok(),
            "one read cannot strand the accepted Check"
        );
        assert!(observe_idle());
        let rows = OBSERVED_ROWS.lock().unwrap();
        let current = rows
            .rows
            .iter()
            .find(|row| row.subject == preference)
            .unwrap();
        assert_eq!(
            current.standing,
            if panics {
                Standing::Unknown
            } else {
                Standing::InPlace
            }
        );
        if panics {
            // The words the page shows under the row, which say to Check again.
            assert!(
                current
                    .note
                    .as_ref()
                    .is_some_and(|note| note.contains("Check again"))
            );
        }
        assert!(
            rows.rows.contains(&private),
            "filtered-out personal rows stay untouched"
        );
        assert!(rows.rows.contains(&kept));
        assert!(rows.rows.contains(&as_read(&fresh)));
        assert_eq!(rows.plan, Some(next.clone()));
        drop(rows);
        let script = script();
        assert_eq!(script.reads.len(), 2, "the unwound batch is never retried");
        assert_eq!(script.reads[0].1, [preference]);
        assert_eq!(script.reads[1].0, next);
        assert_eq!(script.reads[1].1, [repositories]);
        assert!(script.reads[1].2, "the queued Check keeps its own scope");
    }
}

/// The page's own Check reads every row and lifts nothing: no parked stage is lifted and no
/// Check again wake reaches a parked run, whose own pauses bring it back. Only a person's
/// press lifts the parks and wakes the run. NEGATIVE CONTROL: wake for every Check, whoever
/// made it, and the page's automatic reading lifts a park each time it looks.
#[test]
fn only_a_press_of_check_again_lifts_parks_and_wakes_the_run() {
    let _globals = globals();
    let _io = TestIo::install();
    // Another pass holds the slot: no check worker reaches this Mac.
    let _busy = take_initial_setup_slot().unwrap();
    let plan = left_as_is();
    let before = SETUP_WAKES.marks();
    let started = check_initial_setup(plan.clone(), aterm_gui::CheckBy::Page);
    assert!(started.reading);
    assert!(!started.check);
    assert_eq!(script().lifts, 0, "the page's own reading lifts no park");
    assert!(
        !SETUP_WAKES.marks().checked_since(before),
        "nor wakes a parked run"
    );
    let started = check_initial_setup(plan, aterm_gui::CheckBy::Press);
    assert!(started.reading);
    assert!(!started.check);
    assert_eq!(script().lifts, 1, "a press lifts the parks");
    assert!(
        SETUP_WAKES.marks().checked_since(before),
        "and wakes the parked run"
    );
    wait_until("the forced readings finish", observe_idle);
}

#[test]
fn queued_observation_intents_keep_a_draft_check_across_a_focus_refresh() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    SETUP_IO.write().unwrap().history_store = || {
        let root = CLAIM_ROOT.lock().unwrap().clone()?;
        Some(aterm_cli::macos_doctor::SetupHistoryStore::at(
            &root,
            std::path::Path::new("/fixture"),
        ))
    };
    let _busy = take_initial_setup_slot().unwrap();
    let mut shown = left_as_is();
    shown.vault_url = "https://example.invalid/accepted-a".into();
    let mut checked = shown.clone();
    checked.vault_url = "https://example.invalid/draft-b".into();
    let vault = Subject::Setting(Setting::Secrets);
    let accepted = Observation {
        standing: Standing::InPlace,
        now: Some("Synced".into()),
        ..Observation::unknown(vault)
    };
    let draft = Observation {
        standing: Standing::NeedsYou,
        ..Observation::unknown(vault)
    };
    change(|script| script.readings = [vec![accepted.clone()]].into());
    assert!(observe_initial_setup(shown.clone()));
    wait_until("the first accepted presentation is read", || {
        script().reads.len() == 1 && observe_idle()
    });
    let before = setup_readings();
    assert!(before.observed.contains(&as_read(&accepted)));
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.readings = [vec![accepted.clone()]].into();
    });
    assert!(observe_initial_setup(shown.clone()));
    wait_until("the returning page's fresh reading starts", || {
        script().reads.len() == 1
    });
    let while_reading = setup_readings();
    release_readings();
    wait_until("the returning page's reading finishes", observe_idle);
    assert_eq!(
        while_reading.observed, before.observed,
        "a same-plan focus refresh keeps the shown values until their replacement arrives"
    );
    assert_eq!(while_reading.observed_vault_url, before.observed_vault_url);
    assert!(
        while_reading
            .observing
            .contains(&(vault, Some(shown.vault_url.clone()), false))
    );
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.readings = [vec![accepted.clone()]].into();
    });
    assert!(check_initial_setup(shown.clone(), aterm_gui::CheckBy::Press).reading);
    wait_until("the same-source Check starts", || script().reads.len() == 1);
    let checking = setup_readings();
    release_readings();
    wait_until("the same-source Check finishes", observe_idle);
    assert!(
        !checking.observed.iter().any(|row| row.subject == vault),
        "Check still retires current authority"
    );
    assert!(
        checking.last_known.contains(&as_read(&accepted)),
        "the useful value remains presentation history, as it was read"
    );
    assert!(
        checking
            .observing
            .contains(&(vault, Some(shown.vault_url.clone()), false))
    );
    // An existing source/device/effect retirement in this process remains deliberate:
    // its next reader must not rehydrate this process's own just-saved old value.
    invalidate_setup_observations(&[vault]);
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.readings = [Vec::new()].into();
    });
    assert!(observe_initial_setup(shown.clone()));
    wait_until("the retired source's next reader starts", || {
        script().reads.len() == 1
    });
    let retired = setup_readings();
    release_readings();
    wait_until("the retired source's next reader finishes", observe_idle);
    assert!(
        !retired.last_known.iter().any(|row| row.subject == vault),
        "disk history cannot resurrect a deliberately retired current-process subject"
    );
    assert!(!retired.observed.iter().any(|row| row.subject == vault));

    // A new process has no current native facts. Load only the retained value before
    // its first actual reader, and keep the original read time through an empty batch.
    let store = (setup_io().history_store)().unwrap();
    let saved = store.begin().unwrap();
    let original_read = saved
        .rows
        .iter()
        .find(|row| row.subject == vault)
        .unwrap()
        .clone();
    assert!(original_read.read_at_ms > 0);
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.rows.clear();
        rows.last_known.clear();
        rows.retained_reads.clear();
        rows.history_restored.clear();
        rows.unconfirmed.clear();
        rows.caps_live = None;
        rows.generation += 1;
    }
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.readings = [Vec::new()].into();
    });
    assert!(observe_initial_setup(shown.clone()));
    wait_until("the relaunched page's first reader starts", || {
        script().reads.len() == 1
    });
    let relaunched = setup_readings();
    release_readings();
    wait_until("the relaunched empty reader finishes", observe_idle);
    assert!(
        relaunched.observed.is_empty(),
        "history supplies no current native evidence"
    );
    assert!(
        relaunched
            .last_known
            .iter()
            .any(|row| row.subject == vault && row.now.as_deref() == Some("Synced"))
    );
    assert!(relaunched.caps_live.is_none());
    let after_empty = store.begin().unwrap();
    let retained = after_empty
        .rows
        .iter()
        .find(|row| row.subject == vault)
        .unwrap();
    assert_eq!(
        retained, &original_read,
        "restoring/skipping never invents a new read time"
    );
    assert!(
        !store.save(saved.generation, &shown.vault_url, &saved.rows, || false),
        "a cache snapshot retired before the file lock cannot be saved"
    );
    change(|script| {
        script.reads.clear();
        script.hold = true;
        script.readings = [vec![accepted.clone()], vec![accepted], vec![draft.clone()]].into();
    });
    assert!(observe_initial_setup(shown.clone()));
    wait_until("the accepted source's first reading starts", || {
        script().reads.len() == 1
    });

    // The actual Check producer acknowledges its forced reading even
    // while another pass owns the separate check worker's slot.
    let started = check_initial_setup(checked.clone(), aterm_gui::CheckBy::Press);
    assert!(started.reading);
    assert!(!started.check);
    // The Host also asks for the reviewed plan's ordinary fallback, then
    // a genuine focus return asks for the accepted source once more.
    assert!(observe_initial_setup(checked.clone()));
    assert!(observe_initial_setup(shown.clone()));
    let queued = setup_readings();
    // Keep the second source's actual reader held after A finishes, so
    // the completed source cannot hide inside a whole-batch progress bit.
    SETUP_IO.write().unwrap().read = |plan, subjects, network, admission| {
        if plan.vault_url.ends_with("/draft-b") {
            change(|script| script.hold = true);
        }
        TestIo::read(plan, subjects, network, admission)
    };
    release_readings();
    wait_until("the explicit other-source Check starts", || {
        script().reads.len() == 3
    });
    let only_b = setup_readings();
    release_readings();
    wait_until("the accepted observation work finishes", observe_idle);
    assert!(
        queued
            .observing
            .contains(&(vault, Some(shown.vault_url.clone()), false))
    );
    assert!(
        queued
            .observing
            .contains(&(vault, Some(checked.vault_url.clone()), true))
    );
    assert_eq!(
        only_b
            .observing
            .iter()
            .filter(|(subject, _, _)| *subject == vault)
            .cloned()
            .collect::<Vec<_>>(),
        [(vault, Some(checked.vault_url.clone()), false)],
        "completed A is no longer checking while B remains held"
    );

    let reads = std::mem::take(&mut script().reads);
    assert_eq!(reads.len(), 3, "both pending intents must be read");
    assert_eq!((&reads[0].0, reads[0].2), (&shown, false));
    assert_eq!((&reads[1].0, reads[1].2), (&shown, false));
    assert_eq!((&reads[2].0, reads[2].2), (&checked, true));
    for personal in [Setting::Messages, Setting::FaceTime] {
        assert!(!reads[1].1.contains(&Subject::Setting(personal)));
        assert!(reads[2].1.contains(&Subject::Setting(personal)));
    }
    let rows = OBSERVED_ROWS.lock().unwrap();
    assert_eq!(rows.plan.as_ref(), Some(&checked));
    assert_eq!(rows.vault_url.as_deref(), Some(checked.vault_url.as_str()));
    assert!(rows.rows.contains(&as_read(&draft)));
    drop(rows);
    drop(_busy);

    // The page's automatic rerun carries no choice delta. While its actual admission
    // waits, current authority retires but the unchanged workstation value stays visible.
    let dock = Subject::Setting(Setting::Dock);
    let known = Observation {
        standing: Standing::InPlace,
        now: Some("Left".into()),
        ..Observation::unknown(dock)
    }
    .stamped(READ_AT - 300);
    let access = Observation {
        standing: Standing::InPlace,
        now: Some("Allowed".into()),
        ..Observation::unknown(Subject::Access(
            aterm_types::macos_setup::Access::FullDiskAccess,
        ))
    }
    .stamped(READ_AT - 300);
    let unstamped = Subject::Setting(Setting::NaturalScrolling);
    let held_history = Observation {
        standing: Standing::InPlace,
        now: Some("saved scroll history".into()),
        ..Observation::unknown(unstamped)
    };
    let unread = Observation {
        now: Some("not actually read".into()),
        ..held_history.clone()
    };
    let zero = Observation {
        standing: Standing::InPlace,
        now: Some("zero-date Desktop".into()),
        ..Observation::unknown(Subject::Setting(Setting::Desktop))
    }
    .stamped(0);
    let withheld = Observation {
        standing: Standing::InPlace,
        now: Some("withheld Widgets".into()),
        ..Observation::withheld(Subject::Setting(Setting::Widgets))
    };
    let saved = Plan::default();
    for case in [
        "same", "person", "chosen", "delta", "whole", "source", "plan", "typed", "password",
    ] {
        let mut cached = saved.clone();
        if case == "source" {
            cached.vault_url.push_str("/different");
        } else if case == "plan" {
            cached
                .selection
                .set(Setting::Dock, !saved.selection.contains(Setting::Dock));
        }
        assert_eq!(
            cached == saved,
            !matches!(case, "source" | "plan"),
            "{case}: exact plan premise"
        );
        {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.rows = vec![
                known.clone(),
                access.clone(),
                unread.clone(),
                zero.clone(),
                withheld.clone(),
            ];
            rows.last_known = vec![access.clone(), held_history.clone()];
            rows.unconfirmed.clear();
            rows.vault_url = Some(cached.vault_url.clone());
            rows.plan = Some(cached);
        }
        let changed = if case == "whole" {
            None
        } else {
            let mut changed = aterm_types::macos_setup::Selection::none();
            changed.set(Setting::Dock, case == "delta");
            Some(changed)
        };
        let admission = if case == "person" {
            aterm_gui::InitialSetupAdmission::Person
        } else {
            aterm_gui::InitialSetupAdmission::Automatic
        };
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached_with_change(
            admission,
            case == "chosen",
            changed,
        );
        let password =
            (case == "password").then(|| aterm_secrets::SecretBytes::from(b"fixture".as_slice()));
        let persisted = script().persists;
        change(|script| {
            script.persist_hold = true;
            script.admits.push_back(Some(90 + persisted as u64));
        });
        assert!(restart_initial_setup(
            saved.clone(),
            case == "typed",
            password,
            receipt
        ));
        wait_until("the unchanged plan's admission waits", || {
            script().persists > persisted
        });
        let before_effect = setup_readings();
        let no_effect = script().runs.len();
        change(|script| script.persist_hold = false);
        assert_eq!(answer.recv_timeout(HANG), Ok(true));
        wait_until("the admitted run releases its slot", || {
            !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
        });
        assert_eq!(script().runs.len(), no_effect + 1);
        assert!(
            !before_effect.observed.iter().any(|row| row.subject == dock),
            "{case}: old readings grant no current authority"
        );
        assert_eq!(
            before_effect.last_known.contains(&known),
            case == "same",
            "{case}: only an unchanged automatic rerun keeps workstation history"
        );
        assert!(
            !before_effect.last_known.contains(&access),
            "{case}: permission history is not retained"
        );
        assert_eq!(
            before_effect.last_known.contains(&held_history),
            case == "same",
            "{case}: undated current rows cannot replace saved display history"
        );
        assert!(
            !before_effect
                .last_known
                .iter()
                .any(|row| row.subject == zero.subject || row.subject == withheld.subject),
            "{case}: a zero-date or withheld row is no completed read"
        );
        assert_eq!(OBSERVED_ROWS.lock().unwrap().plan.as_ref(), Some(&saved));
    }

    // A refusal still made no effect. Keep only history that this exact admission still
    // owns; a device, Stop, source, or newer choice can retire it while persistence waits.
    for invalidation in ["none", "device", "stop", "source", "choice"] {
        *SETUP_STOP.lock().unwrap() = None;
        {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.rows = vec![known.clone()];
            rows.last_known.clear();
            rows.unconfirmed.clear();
            rows.plan = Some(saved.clone());
            rows.vault_url = Some(saved.vault_url.clone());
        }
        let persisted = script().persists;
        let ran = script().runs.len();
        change(|script| {
            script.persist_hold = true;
            script.admits.push_back(None);
        });
        let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached_with_change(
            aterm_gui::InitialSetupAdmission::Automatic,
            false,
            Some(aterm_types::macos_setup::Selection::none()),
        );
        assert!(restart_initial_setup(saved.clone(), false, None, receipt));
        wait_until("the automatic refusal waits in persistence", || {
            script().persists > persisted
        });
        let mut newer = saved.clone();
        newer
            .selection
            .set(Setting::Dock, !saved.selection.contains(Setting::Dock));
        let later = match invalidation {
            "device" => {
                invalidate_setup_observations(&[dock]);
                None
            }
            "stop" => {
                stop_setup_run(90);
                None
            }
            "source" => {
                let mut rows = OBSERVED_ROWS.lock().unwrap();
                let mut other = saved.clone();
                other.vault_url.push_str("/new-source");
                rows.vault_url = Some(other.vault_url.clone());
                rows.plan = Some(other);
                None
            }
            "choice" => {
                change(|script| script.admits.push_back(Some(500)));
                let (receipt, later) = aterm_gui::InitialSetupApplyReceipt::detached(true);
                assert!(restart_initial_setup(newer.clone(), false, None, receipt));
                Some(later)
            }
            _ => None,
        };
        change(|script| script.persist_hold = false);
        assert_eq!(answer.recv_timeout(HANG), Ok(false));
        if let Some(later) = later {
            assert_eq!(later.recv_timeout(HANG), Ok(true));
        }
        wait_until(
            "the refused admission and any newer choice release the slot",
            || !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire) && stop_worker_idle(),
        );
        let after = setup_readings();
        assert!(
            !after.observed.iter().any(|row| row.subject == dock),
            "{invalidation}: refused work grants no current authority"
        );
        assert_eq!(
            after.last_known.contains(&known),
            invalidation == "none",
            "{invalidation}: rollback cannot resurrect invalidated history"
        );
        assert_eq!(
            script().runs.len(),
            ran + usize::from(invalidation == "choice")
        );
        if invalidation == "choice" {
            assert_eq!(OBSERVED_ROWS.lock().unwrap().plan.as_ref(), Some(&newer));
        }
    }
}

#[test]
fn queued_observation_intents_keep_only_the_latest_check_until_a_new_choice() {
    let _globals = globals();
    for new_choice in [false, true] {
        let _io = TestIo::install();
        let busy = take_initial_setup_slot().unwrap();
        let mut shown = left_as_is();
        shown.vault_url = "https://example.invalid/accepted-a".into();
        let mut checked = shown.clone();
        checked.vault_url = "https://example.invalid/draft-b".into();
        change(|script| script.hold = true);
        assert!(observe_initial_setup(shown.clone()));
        wait_until("the old source reading starts", || {
            script().reads.len() == 1
        });
        for source in ["draft-b", "draft-c"] {
            checked.vault_url = format!("https://example.invalid/{source}");
            assert_eq!(
                check_initial_setup(checked.clone(), aterm_gui::CheckBy::Press),
                aterm_gui::CheckStarted {
                    reading: true,
                    check: false,
                }
            );
            assert!(observe_initial_setup(checked.clone()));
            assert!(observe_initial_setup(shown.clone()));
        }
        let mut chosen = shown.clone();
        chosen.vault_url = "https://example.invalid/new-choice".into();
        drop(busy);
        if new_choice {
            script().admits = [Some(9)].into();
            let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
            assert!(restart_initial_setup(chosen.clone(), false, None, receipt));
            assert_eq!(answer.recv_timeout(HANG), Ok(true));
            wait_until("the new choice's inert run finishes", || {
                !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
            });
            assert!(observe_initial_setup(chosen.clone()));
        }
        release_readings();
        wait_until("the latest pending reads finish", observe_idle);
        let reads = std::mem::take(&mut script().reads);
        if new_choice {
            assert_eq!(reads.len(), 2);
            assert_eq!((&reads[1].0, reads[1].2), (&chosen, false));
            assert!(!reads[1].1.contains(&Subject::Setting(Setting::Messages)));
            assert!(!reads[1].1.contains(&Subject::Setting(Setting::FaceTime)));
            assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(chosen));
        } else {
            assert_eq!(reads.len(), 3);
            assert_eq!((&reads[1].0, reads[1].2), (&shown, false));
            assert_eq!((&reads[2].0, reads[2].2), (&checked, true));
            assert!(reads[2].1.contains(&Subject::Setting(Setting::Messages)));
            assert!(reads[2].1.contains(&Subject::Setting(Setting::FaceTime)));
            assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(checked));
        }
    }
}

#[test]
fn a_queued_checks_scope_belongs_to_its_reviewed_plan() {
    let _globals = globals();
    let _io = TestIo::install();
    struct QueueLease;
    impl Drop for QueueLease {
        fn drop(&mut self) {
            let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running = false;
            queue.active.clear();
            queue.current = None;
            queue.next = None;
            queue.check = None;
        }
    }
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let _queue = QueueLease;
    for edited in [false, true] {
        let checked = left_as_is();
        let mut reviewed = checked.clone();
        if edited {
            reviewed.vault_url = "http://another-vault.local:7484/v1/team".into();
        }
        assert!(request_observation(
            Some(checked.clone()),
            Subject::all(),
            true
        ));
        assert!(observe_initial_setup(reviewed.clone()));
        let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
        for request in pending.into_iter().flatten() {
            observe_rows(request);
        }
        let reads = std::mem::take(&mut script().reads);
        assert_eq!(reads.len(), 1 + usize::from(edited));
        if edited {
            assert_eq!((&reads[0].0, reads[0].2), (&reviewed, false));
            assert!(
                reads[0].1.iter().all(|subject| !matches!(
                    subject,
                    Subject::Setting(setting) if setting.group() == Group::Accounts
                )),
                "account reads cannot transfer to the ordinary plan"
            );
        }
        let forced = reads.last().unwrap();
        assert_eq!((&forced.0, forced.2), (&checked, true));
        assert!(forced.1.contains(&Subject::Setting(Setting::Messages)));
        assert!(forced.1.contains(&Subject::Setting(Setting::FaceTime)));
        // A fresh Check of that exact edited plan still reads all rows,
        // even when an ordinary stage refresh coalesces behind it.
        assert!(request_observation(
            Some(reviewed.clone()),
            Subject::all(),
            true
        ));
        assert!(request_observation(None, Subject::all(), false));
        let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
        for request in pending.into_iter().flatten() {
            observe_rows(request);
        }
        let reads = std::mem::take(&mut script().reads);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].0, reviewed);
        assert!(reads[0].2);
        assert!(reads[0].1.contains(&Subject::Setting(Setting::Messages)));
        assert!(reads[0].1.contains(&Subject::Setting(Setting::FaceTime)));
    }
}

#[test]
fn a_late_old_check_cannot_relabel_a_newer_queued_read() {
    let _globals = globals();
    let _io = TestIo::install();
    struct QueueLease;
    impl Drop for QueueLease {
        fn drop(&mut self) {
            let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running = false;
            queue.active.clear();
            queue.current = None;
            queue.next = None;
            queue.check = None;
        }
    }
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let _queue = QueueLease;
    for check in [false, true] {
        // Capture Check A before a newer choice, then let it acquire the
        // queue only after the new choice's own read is already waiting.
        let old = ObserveRequest {
            plan: Some(left_as_is()),
            choices: OBSERVED_ROWS.lock().unwrap().choices,
            subjects: Subject::all(),
            forced: true,
            pending_repositories: false,
        };
        let mut reviewed = left_as_is();
        reviewed.vault_url = "http://newer-vault.local:7484/v1/team".into();
        let generation = SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire);
        let _choice = begin_setup_choice(&reviewed, false, generation, false);
        assert!(request_observation(
            Some(reviewed.clone()),
            Subject::all(),
            check
        ));
        assert!(queue_observation(old));
        let pending = OBSERVE_QUEUE.lock().unwrap().take_pending();
        for request in pending.into_iter().flatten() {
            observe_rows(request);
        }
        let reads = std::mem::take(&mut script().reads);
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].0, reviewed);
        assert_eq!(reads[0].2, check);
        assert_eq!(
            reads[0].1.contains(&Subject::Setting(Setting::Messages)),
            check
        );
        assert_eq!(
            reads[0].1.contains(&Subject::Setting(Setting::FaceTime)),
            check
        );
    }
}

#[test]
fn an_in_flight_observation_cannot_publish_after_a_new_choice() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    // Park only the queue's dispatcher: this reader still runs the real
    // observation entry, and a replacement read can be inspected before
    // it runs. Release the queue before TestIo waits for its workers.
    struct QueueLease;
    impl Drop for QueueLease {
        fn drop(&mut self) {
            let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running = false;
            queue.active.clear();
            queue.current = None;
            queue.next = None;
            queue.check = None;
        }
    }
    {
        let mut queue = OBSERVE_QUEUE.lock().unwrap();
        assert!(!queue.running);
        assert!(queue.next.is_none() && queue.check.is_none());
        queue.running = true;
    }
    let _queue = QueueLease;
    let secrets = Subject::Setting(Setting::Secrets);
    let repositories = Subject::Setting(Setting::Repositories);
    let reading = |subject, standing, note: &str| Observation {
        subject,
        standing,
        now: None,
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: Some(note.to_owned()),
        detail: None,
        read: aterm_types::macos_setup::Read::Unstamped,
    };
    let shown = Plan::default();
    let subjects = Subject::all();
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(shown.clone());
        rows.rows = vec![
            reading(secrets, Standing::Waiting, "earlier vault reading"),
            reading(
                repositories,
                Standing::Waiting,
                "earlier repository reading",
            ),
        ];
        rows.access_steps = 7;
        rows.choices
    };
    let current = reading(
        secrets,
        Standing::Waiting,
        "the newly selected vault did not answer",
    );
    change(|script| {
        script.hold = true;
        script.readings = [
            vec![
                reading(
                    secrets,
                    Standing::NeedsYou,
                    "approval for the old vault: OLD42",
                ),
                reading(
                    repositories,
                    Standing::InPlace,
                    "old vault credentials worked",
                ),
            ],
            vec![current.clone()],
        ]
        .into();
    });
    // On failure, release and join the reader before QueueLease and
    // TestIo drop; no delayed worker may outlive its inert I/O.
    struct Reader(Option<std::thread::JoinHandle<()>>);
    impl Drop for Reader {
        fn drop(&mut self) {
            release_readings();
            if let Some(reader) = self.0.take() {
                let _ = reader.join();
            }
        }
    }
    let requested = subjects.clone();
    let mut reader = Reader(Some(std::thread::spawn(move || {
        observe_rows(ObserveRequest {
            plan: Some(shown),
            choices,
            subjects: requested,
            forced: true,
            pending_repositories: false,
        });
    })));
    wait_until("the old Check's reading starts", || {
        script().reads.len() == 1
    });
    let mut chosen = Plan {
        vault_url: "http://new-vault.local:7484/v1/team".into(),
        ..Plan::default()
    };
    for setting in [Setting::Repositories, Setting::Messages, Setting::FaceTime] {
        chosen.selection.set(setting, false);
    }
    script().admits = [Some(9)].into();
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(chosen.clone(), true, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(true));
    wait_until("the new plan's inert run ends", || {
        !INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire)
    });
    let before = {
        let rows = OBSERVED_ROWS.lock().unwrap();
        (rows.rows.clone(), rows.generation, rows.access_steps)
    };
    let wakes = SETUP_WAKES.count();
    release_readings();
    reader.0.take().unwrap().join().unwrap();
    {
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert_eq!(rows.plan, Some(chosen.clone()));
        assert_eq!(
            rows.rows, before.0,
            "the old vault cannot publish under the new plan"
        );
        assert_eq!(rows.generation, before.1);
        assert_eq!(rows.access_steps, before.2);
    }
    assert_eq!(
        SETUP_WAKES.count(),
        wakes,
        "an old result cannot wake the new run"
    );
    let next = OBSERVE_QUEUE
        .lock()
        .unwrap()
        .next
        .take()
        .expect("a fresh reading is queued");
    assert_eq!(next.subjects, subjects);
    assert!(
        !next.forced,
        "a superseded Check cannot force old personal reads"
    );
    observe_rows(next);
    let reads = &script().reads;
    assert_eq!(reads.len(), 2);
    assert_eq!(reads[1].0, chosen);
    assert!(!reads[1].2, "the new plan leaves repositories unmanaged");
    assert!(!reads[1].1.iter().any(|subject| matches!(
        subject,
        Subject::Setting(Setting::Messages | Setting::FaceTime)
    )));
    assert_eq!(OBSERVED_ROWS.lock().unwrap().rows[0], as_read(&current));
}

#[test]
fn an_interrupted_pass_retires_held_readings_before_releasing_its_leases() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    for panics in [false, true] {
        let _io = TestIo::install();
        struct QueueLease;
        impl Drop for QueueLease {
            fn drop(&mut self) {
                let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
                queue.running = false;
                queue.active.clear();
                queue.current = None;
                queue.next = None;
                queue.check = None;
            }
        }
        OBSERVE_QUEUE.lock().unwrap().running = true;
        let _queue = QueueLease;
        struct Reader(Option<std::thread::JoinHandle<()>>);
        impl Drop for Reader {
            fn drop(&mut self) {
                release_readings();
                if let Some(reader) = self.0.take() {
                    let _ = reader.join();
                }
            }
        }
        // This drops after the pass helper, before the real claim and busy
        // slot. Record the boundary without panicking during the unwind.
        struct Boundary<'a>(&'a mut Option<(bool, bool, bool)>);
        impl Drop for Boundary<'_> {
            fn drop(&mut self) {
                *self.0 = Some((
                    SETUP_ACTIVITY.lock().unwrap().subjects.is_empty(),
                    OBSERVE_QUEUE.lock().unwrap().next.is_some(),
                    INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire),
                ));
            }
        }
        let preference = Subject::Setting(Setting::Dock);
        let independent = Subject::Setting(Setting::Spotlight);
        let seen = |subject, standing| Observation {
            standing,
            ..Observation::unknown(subject)
        };
        let failed = seen(preference, Standing::Failed);
        let unaffected = seen(independent, Standing::NeedsYou);
        let fresh = seen(preference, Standing::Differs);
        let plan = left_as_is();
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(plan.clone());
            rows.vault_url = Some(plan.vault_url.clone());
            rows.rows = vec![failed.clone(), Observation::unknown(independent)];
            rows.choices
        };
        observe_rows(ObserveRequest {
            plan: Some(plan.clone()),
            choices,
            subjects: Vec::new(),
            forced: false,
            pending_repositories: false,
        });
        change(|script| {
            script.reads.clear();
            script.hold = true;
            script.readings = [
                vec![seen(preference, Standing::InPlace), unaffected.clone()],
                vec![fresh.clone()],
            ]
            .into();
        });
        let mut reader = Reader(None);
        let mut boundary = None;
        let mut wakes = 0;
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert!(take_initial_setup_slot_now());
            let _busy = InitialSetupBusy { observation: None };
            let _claim = TestIo::claim().unwrap().unwrap();
            let _boundary = Boundary(&mut boundary);
            with_setup_activity(|| {
                setup_stage(aterm_cli::macos_doctor::StageEvent {
                    revision: 1,
                    settings: &[Setting::Dock],
                    subjects: &[preference],
                    running: true,
                    readback: &aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
                });
                reader.0 = Some(std::thread::spawn(move || {
                    observe_rows(ObserveRequest {
                        plan: Some(plan),
                        choices,
                        subjects: vec![preference, independent],
                        forced: false,
                        pending_repositories: false,
                    });
                }));
                wait_until("the mid-stage reading starts", || script().reads.len() == 1);
                wakes = SETUP_WAKES.count();
                assert!(!panics, "injected setup stage panic");
                Err::<(), _>("injected progress save error")
            })
        }));
        assert_eq!(unwound.is_err(), panics);
        if let Ok(result) = unwound {
            assert_eq!(result, Err("injected progress save error"));
        }
        assert_eq!(
            boundary,
            Some((true, true, true)),
            "cleanup and refresh precede the release of the setup slot"
        );
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert!(TestIo::claim().unwrap().is_some(), "the claim was released");
        release_readings();
        reader.0.take().unwrap().join().unwrap();
        {
            let rows = OBSERVED_ROWS.lock().unwrap();
            assert!(
                !rows.rows.iter().any(|row| row.subject == preference),
                "the old stage fact was retired"
            );
            assert!(rows.rows.contains(&as_read(&unaffected)));
        }
        assert_eq!(SETUP_WAKES.count(), wakes, "no false recovery wake");
        let refresh = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
        assert!(!refresh.forced);
        assert_eq!(refresh.subjects, Subject::all());
        observe_rows(refresh);
        assert!(
            OBSERVED_ROWS
                .lock()
                .unwrap()
                .rows
                .contains(&as_read(&fresh))
        );
        assert_eq!(script().reads.len(), 2);
    }
}

#[test]
fn an_unwound_worker_retires_only_its_own_registration() {
    let _globals = globals();
    let _io = TestIo::install();
    for panics in [false, true] {
        for replacement in [None, Some(42)] {
            set_setup_run(Some(41));
            let result = std::panic::catch_unwind(|| {
                with_setup_run(41, || {
                    if let Some(replacement) = replacement {
                        set_setup_run(Some(replacement));
                    }
                    assert!(!panics, "injected setup worker panic");
                });
            });
            assert_eq!(result.is_err(), panics);
            assert_eq!(setup_run(), replacement);
        }
    }
}

#[test]
fn preference_readings_retire_cached_and_held_reads() {
    use aterm_types::macos_setup::Standing;

    let _globals = globals();
    let _io = TestIo::install();
    // Keep the automatic refresh queued until the old read has returned.
    struct QueueLease;
    impl Drop for QueueLease {
        fn drop(&mut self) {
            let mut queue = OBSERVE_QUEUE.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running = false;
            queue.active.clear();
            queue.current = None;
            queue.next = None;
            queue.check = None;
        }
    }
    OBSERVE_QUEUE.lock().unwrap().running = true;
    let _queue = QueueLease;
    struct Reader(Option<std::thread::JoinHandle<()>>);
    impl Drop for Reader {
        fn drop(&mut self) {
            release_readings();
            if let Some(reader) = self.0.take() {
                let _ = reader.join();
            }
        }
    }
    let independent = Subject::Setting(Setting::Secrets);
    let seen = |subject, standing| Observation {
        standing,
        ..Observation::unknown(subject)
    };
    let plan = Plan::default();
    for (follow_up, failed) in [(false, false), (false, true), (true, false), (true, true)] {
        let preference = Subject::Setting(if follow_up {
            Setting::Widgets
        } else {
            Setting::CapsLock
        });
        let old = seen(
            preference,
            if follow_up && !failed {
                Standing::Differs
            } else {
                Standing::InPlace
            },
        );
        let vault = seen(independent, Standing::NeedsYou);
        let restored = seen(
            preference,
            if follow_up && !failed {
                Standing::InPlace
            } else {
                Standing::Differs
            },
        );
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(plan.clone());
            rows.vault_url = Some(plan.vault_url.clone());
            rows.rows = vec![old.clone(), vault.clone()];
            rows.choices
        };
        change(|script| {
            script.reads.clear();
            script.hold = true;
            script.readings = [vec![old, vault.clone()], vec![restored.clone()]].into();
        });
        let reading_plan = plan.clone();
        let mut reader = Reader(Some(std::thread::spawn(move || {
            observe_rows(ObserveRequest {
                plan: Some(reading_plan),
                choices,
                subjects: vec![preference, independent],
                forced: false,
                pending_repositories: false,
            });
        })));
        wait_until("the preference read starts", || script().reads.len() == 1);
        let mut during = Vec::new();
        let mut effect = || {
            during = OBSERVED_ROWS.lock().unwrap().rows.clone();
            if failed {
                Err("partial preference change")
            } else {
                Ok(())
            }
        };
        let result = if follow_up {
            finish_withheld_preferences_with(|reserve, _stopped| {
                let _reserved = reserve();
                effect()
            })
        } else {
            with_restore_readings(effect)
        };
        assert_eq!(result.is_err(), failed);
        release_readings();
        reader.0.take().unwrap().join().unwrap();
        let (retained, source) = {
            let rows = OBSERVED_ROWS.lock().unwrap();
            (rows.rows.clone(), rows.vault_url.clone())
        };
        assert!(
            !retained.iter().any(|row| row.subject == preference),
            "an old preference read must not publish after a change (follow-up: {follow_up})"
        );
        assert!(retained.contains(&as_read(&vault)));
        assert_eq!(source.as_ref(), Some(&plan.vault_url));
        assert!(
            !during.iter().any(|row| row.subject == preference),
            "the changing preference is unread while reserved (follow-up: {follow_up})"
        );
        assert!(during.contains(&vault));
        assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
        let fresh = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
        assert!(!fresh.forced);
        assert!(fresh.plan.is_none(), "refresh uses the plan in force");
        assert!(fresh.subjects.contains(&preference));
        if follow_up {
            assert_eq!(fresh.subjects, [Subject::Setting(Setting::Widgets)]);
        } else {
            assert!(
                fresh
                    .subjects
                    .iter()
                    .all(|subject| matches!(subject.group(), Group::Input | Group::Desktop))
            );
        }
        observe_rows(fresh);
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert!(rows.rows.contains(&as_read(&restored)));
        assert!(rows.rows.contains(&as_read(&vault)));
        assert_eq!(script().reads.len(), 2);
    }

    // The same canonical native producer used by the owned pass supplies
    // the sidecar. No fabricated observation can enter the stage hook.
    use aterm_cli::macos_doctor::{
        InitialSetupAdmissionStopTest, StageEvent, run_workstation_readback,
    };
    use aterm_machines::init::{WorkstationGroup, prepare_workstation_selected};
    let native = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
    let caps = Subject::Setting(Setting::CapsLock);
    let mut selection = aterm_types::macos_setup::Selection::none();
    selection.set(Setting::CapsLock, true);
    let prepared = prepare_workstation_selected(&native, WorkstationGroup::All, selection).unwrap();
    let (_, readback) = run_workstation_readback(
        prepared,
        &native,
        WorkstationGroup::All,
        aterm_machines::model::Mode::Apply,
    )
    .unwrap();
    assert!(readback.observations().iter().any(|row| {
        row.subject == caps
            && row.standing == Standing::InPlace
            && row.now.as_deref() == Some("Control")
    }));
    use aterm_machines::port::System as _;
    let aterm_machines::init::KeyboardAttachmentActivation::Verified(live_receipt) =
        aterm_machines::init::activate_keyboard_attachments(
            &native,
            &[41, 42],
            &|_| false,
            &|| true,
            &|| panic!("the actual completed mapping needs no native mutation"),
        )
        .unwrap()
    else {
        panic!("the canonical final probe must cover both native services");
    };
    let live_mapping = live_receipt
        .live_mapping(&native.keyboard_service_observations().unwrap())
        .unwrap();
    let paid_commands = native.calls();
    // An attempted old Unknown is no newer evidence than an old row.
    // Exercise both publication while the real stage is active and after
    // its canonical native readback has supplied the completed fact.
    for during_stage in [true, false] {
        set_setup_run(Some(1));
        let choices = {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(plan.clone());
            rows.rows = vec![Observation::unknown(caps)];
            rows.last_known.clear();
            rows.unconfirmed.clear();
            rows.choices
        };
        OBSERVE_QUEUE.lock().unwrap().next = None;
        change(|script| {
            script.reads.clear();
            script.hold = true;
            script.readings = [vec![Observation::unknown(caps)]].into();
        });
        let reading_plan = plan.clone();
        let mut old = Reader(Some(std::thread::spawn(move || {
            observe_rows(ObserveRequest {
                plan: Some(reading_plan),
                choices,
                subjects: vec![caps],
                forced: false,
                pending_repositories: false,
            })
        })));
        wait_until("the attempted old Unknown sample starts", || {
            script().reads.len() == 1
        });
        with_setup_activity(|| {
            setup_stage(StageEvent {
                revision: 1,
                settings: &[Setting::CapsLock],
                subjects: &[caps],
                running: true,
                readback: &aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
            });
            if during_stage {
                release_readings();
                old.0.take().unwrap().join().unwrap();
            }
            setup_stage(StageEvent {
                revision: 1,
                settings: &[Setting::CapsLock],
                subjects: &[caps],
                running: false,
                readback: &readback,
            });
        });
        release_readings();
        if let Some(reader) = old.0.take() {
            reader.join().unwrap();
        }
        let shown = setup_readings();
        assert!(
            shown
                .observed
                .iter()
                .any(|seen| seen.subject == caps && seen.standing == Standing::InPlace)
        );
        assert!(
            !shown.unconfirmed_observations.contains(&caps),
            "a stale Unknown cannot downgrade the paid result (during stage: {during_stage})"
        );
        assert_eq!(
            native.calls(),
            paid_commands,
            "transporting the fact adds no HID reads"
        );
    }
    for withdrawal in [
        "none",
        "input",
        "choice",
        "restore",
        "revision",
        "run",
        "scope",
        "refresh",
        "forced",
        "invalidated",
        "failed",
        "stop",
    ] {
        *SETUP_STOP.lock().unwrap() = None;
        set_setup_run(Some(1));
        let vault = seen(independent, Standing::NeedsYou);
        {
            let mut rows = OBSERVED_ROWS.lock().unwrap();
            rows.plan = Some(plan.clone());
            rows.rows = vec![Observation::unknown(caps), vault.clone()];
            rows.caps_live = None;
        }
        let live_scope = capture_caps_live_scope().unwrap();
        assert!(replace_caps_live(live_scope, Some((1, live_mapping))));
        assert_eq!(setup_readings().caps_live, Some(live_mapping));
        let generation = setup_readings().observed_generation;
        publish_caps_live(Some(live_scope), Some((1, live_mapping)));
        assert_eq!(
            setup_readings().observed_generation,
            generation,
            "an unchanged healthy diagnostic owes no publication"
        );
        assert!(
            setup_readings()
                .observed
                .iter()
                .any(|row| row.subject == caps && row.standing == Standing::Unknown)
        );
        OBSERVE_QUEUE.lock().unwrap().next = None;
        let stage_subject = if withdrawal == "scope" {
            Subject::Setting(Setting::Dock)
        } else {
            caps
        };
        let stage_setting = if withdrawal == "scope" {
            Setting::Dock
        } else {
            Setting::CapsLock
        };
        with_setup_activity(|| {
            setup_stage(StageEvent {
                revision: 1,
                settings: &[stage_setting],
                subjects: &[stage_subject],
                running: true,
                readback: &aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
            });
            assert_eq!(setup_readings().caps_live.is_some(), withdrawal == "scope");
            assert!(
                !OBSERVED_ROWS
                    .lock()
                    .unwrap()
                    .rows
                    .iter()
                    .any(|row| row.subject == stage_subject)
            );
            match withdrawal {
                "input" => invalidate_input_observations(),
                "choice" => {
                    let _ = begin_setup_choice(&plan, false, 0, false);
                }
                "stop" => stop_setup_run(1),
                "restore" => {
                    drop(PreferenceReadings::begin(vec![caps]));
                }
                "run" => set_setup_run(Some(2)),
                _ => {}
            }
            setup_stage(StageEvent {
                revision: if withdrawal == "revision" { 2 } else { 1 },
                settings: &[stage_setting],
                subjects: &[stage_subject],
                running: false,
                readback: &readback,
            });
        });
        let ready = OBSERVED_ROWS.lock().unwrap().rows.iter().any(|row| {
            row.subject == caps
                && row.standing == Standing::InPlace
                && row.now.as_deref() == Some("Control")
        });
        assert!(
            !replace_caps_live(live_scope, Some((1, live_mapping))),
            "an old live sample cannot cross its setting's stage/choice/Stop fence"
        );
        let admitted = matches!(
            withdrawal,
            "none" | "refresh" | "forced" | "invalidated" | "failed"
        );
        assert_eq!(ready, admitted, "{withdrawal}");
        assert!(OBSERVED_ROWS.lock().unwrap().rows.contains(&vault));
        let refresh = OBSERVE_QUEUE.lock().unwrap().next.take().unwrap();
        assert_eq!(refresh.subjects.contains(&caps), !admitted, "{withdrawal}");
        assert_eq!(
            native.calls(),
            paid_commands,
            "transporting readback starts no HID reads"
        );
        if matches!(withdrawal, "refresh" | "forced" | "invalidated" | "failed") {
            // A hidden redundant refresh is not an attempted read. A real
            // invalidation, failed read or forced Check still retires it.
            let forced = withdrawal == "forced";
            let attempted = matches!(withdrawal, "forced" | "failed");
            if withdrawal == "invalidated" {
                invalidate_input_observations();
            }
            change(|script| {
                script.readings.push_back(vec![Observation::unknown(caps)]);
                script.not_admitted = if attempted { Vec::new() } else { vec![caps] };
            });
            let choices = OBSERVED_ROWS.lock().unwrap().choices;
            observe_rows(ObserveRequest {
                plan: Some(plan.clone()),
                choices,
                subjects: vec![caps],
                forced,
                pending_repositories: false,
            });
            let rows = OBSERVED_ROWS.lock().unwrap();
            assert_eq!(
                rows.rows
                    .iter()
                    .any(|row| row.subject == caps && row.standing == Standing::InPlace),
                withdrawal == "refresh"
            );
        }
    }
    set_setup_run(None);
    wait_until("the injected Stop is saved", stop_worker_idle);
    reset_worker_state();

    // A Caps Lock row a stage's own readback left in place, as the window's pass leaves it:
    // the activation lease keeps a read begun mid-write from republishing it.
    *SETUP_STOP.lock().unwrap() = None;
    set_setup_run(Some(1));
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.plan = Some(Plan::from(selection));
        rows.rows = vec![Observation::unknown(caps)];
    }
    with_setup_activity(|| {
        setup_stage(StageEvent {
            revision: 1,
            settings: &[Setting::CapsLock],
            subjects: &[caps],
            running: true,
            readback: &aterm_cli::macos_doctor::WorkstationReadback::EMPTY,
        });
        setup_stage(StageEvent {
            revision: 1,
            settings: &[Setting::CapsLock],
            subjects: &[caps],
            running: false,
            readback: &readback,
        });
    });
    OBSERVE_QUEUE.lock().unwrap().next = None;
    change(|script| script.reads.clear());
    {
        let actor = InitialSetupAdmissionStopTest::keyboard_system(true, &[]);
        let (activation, retired) = super::input_attachments::activate_observing(|invalidate| {
            aterm_machines::init::activate_keyboard_attachments(
                &actor,
                &[41, 42],
                &|_| false,
                &|| true,
                invalidate,
            )
        });
        assert!(matches!(
            activation.unwrap(),
            aterm_machines::init::KeyboardAttachmentActivation::Verified(_)
        ));
        assert!(!retired, "a real no-op does not queue a success refresh");
        assert!(
            setup_readings()
                .observed
                .iter()
                .any(|row| row.subject == caps && row.standing == Standing::InPlace)
        );
        assert!(actor.keyboard_changes().is_empty());
        use aterm_machines::model::PValue;
        use aterm_machines::port::{Cmd, Ran, System};
        let census = actor
            .run(&aterm_machines::facts::hidutil_keyboards())
            .unwrap()
            .out();
        let original = actor
            .run(&Cmd::new("/usr/bin/hidutil").arg("dump"))
            .unwrap()
            .out();
        let defaults = actor
            .run(&Cmd::new("/usr/bin/defaults").arg("-g"))
            .unwrap()
            .out();
        let mut unmapped = aterm_machines::plist::parse(original.as_bytes()).unwrap();
        fn without_user_mapping(value: &mut PValue) {
            match value {
                PValue::Dict(fields) => {
                    for (name, value) in fields {
                        if name == "UserKeyMapping" {
                            *value = PValue::Array(Vec::new());
                        } else {
                            without_user_mapping(value);
                        }
                    }
                }
                PValue::Array(values) => values.iter_mut().for_each(without_user_mapping),
                _ => {}
            }
        }
        without_user_mapping(&mut unmapped);
        let unmapped = aterm_machines::plist::document(&unmapped);
        actor.simulate(move |_, command| {
            let first = command.args.first().and_then(|arg| arg.to_str());
            match command.program.file_name().and_then(|name| name.to_str()) {
                Some("hidutil") if first == Some("list") => Some(Ran::ok(&census)),
                Some("hidutil") if first == Some("property") => Some(Ran::ok(
                    "RegistryID Key Value\n29 UserKeyMapping (null)\n2a UserKeyMapping (null)\n",
                )),
                Some("hidutil") if first == Some("dump") => Some(Ran::ok(&unmapped)),
                Some("defaults") => Some(Ran::ok(&defaults)),
                _ => None,
            }
        });
        let current = setup_readings()
            .observed
            .into_iter()
            .find(|row| row.subject == caps)
            .unwrap();
        let choices = OBSERVED_ROWS.lock().unwrap().choices;
        let (entered, entering) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        type ActivationReply = (
            Result<aterm_machines::init::KeyboardAttachmentActivation, String>,
            bool,
            usize,
        );
        struct Effect {
            release: Option<std::sync::mpsc::SyncSender<()>>,
            worker: Option<std::thread::JoinHandle<ActivationReply>>,
        }
        impl Drop for Effect {
            fn drop(&mut self) {
                if let Some(release) = self.release.take() {
                    let _ = release.send(());
                }
                if let Some(worker) = self.worker.take() {
                    let _ = worker.join();
                }
            }
        }
        let mut effect = Effect {
            release: Some(release),
            worker: Some(std::thread::spawn(move || {
                let (result, retired) =
                    super::input_attachments::activate_observing(|invalidate| {
                        aterm_machines::init::activate_keyboard_attachments(
                            &actor,
                            &[41, 42],
                            &|_| false,
                            &|| true,
                            &|| {
                                invalidate();
                                entered.send(()).unwrap();
                                released
                                    .recv_timeout(HANG)
                                    .expect("effect was not released");
                            },
                        )
                    });
                (result, retired, actor.keyboard_changes().len())
            })),
        };
        entering.recv_timeout(HANG).unwrap();
        change(|script| script.readings.push_back(vec![current.clone()]));
        observe_rows(ObserveRequest {
            plan: Some(Plan::from(selection)),
            choices,
            subjects: vec![caps],
            forced: false,
            pending_repositories: false,
        });
        let mid_write = setup_readings();
        change(|script| {
            script.hold = true;
            script.readings.push_back(vec![current]);
        });
        let mut late_reader = Reader(Some(std::thread::spawn(move || {
            observe_rows(ObserveRequest {
                plan: Some(Plan::from(selection)),
                choices,
                subjects: vec![caps],
                forced: false,
                pending_repositories: false,
            });
        })));
        wait_until("the reading begins inside the activation lease", || {
            script().reads.len() == 2
        });
        effect.release.take().unwrap().send(()).unwrap();
        let (failed, retired, writes) = effect.worker.take().unwrap().join().unwrap();
        release_readings();
        late_reader.0.take().unwrap().join().unwrap();
        assert!(
            matches!(
                failed.unwrap(),
                aterm_machines::init::KeyboardAttachmentActivation::Unfinished
            ) && retired
        );
        assert_eq!(
            writes, 2,
            "the actual setter wrote both services before failed readback"
        );
        assert!(mid_write.running.contains(&Setting::CapsLock));
        assert!(!mid_write.observed.iter().any(|row| row.subject == caps));
        assert!(
            !setup_readings()
                .observed
                .iter()
                .any(|row| row.subject == caps),
            "a reader begun mid-write cannot republish after the lease ends"
        );
        assert!(setup_readings().running.is_empty());
    }
    set_setup_run(None);
    assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
}

#[test]
fn preference_readings_unwind_without_leaving_activity_or_busy() {
    let _globals = globals();
    let _io = TestIo::install();
    for follow_up in [false, true] {
        OBSERVE_QUEUE.lock().unwrap().running = true;
        let result = std::panic::catch_unwind(|| {
            if follow_up {
                finish_withheld_preferences_with(|reserve, _stopped| {
                    let _reserved = reserve();
                    panic!("injected follow-up panic");
                });
            } else {
                assert!(take_initial_setup_slot_now());
                let _busy = InitialSetupBusy { observation: None };
                with_restore_readings(|| panic!("injected restore panic"));
            }
        });
        assert!(result.is_err());
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert!(SETUP_ACTIVITY.lock().unwrap().subjects.is_empty());
        let fresh = {
            let mut queue = OBSERVE_QUEUE.lock().unwrap();
            queue.running = false;
            queue.next.take()
        };
        assert!(
            fresh.is_some(),
            "an unwound preference change is read again"
        );
    }
}

#[test]
fn a_follow_up_that_never_reserves_leaves_its_readings_alone() {
    let _globals = globals();
    let _io = TestIo::install();
    let widgets = Observation::unknown(Subject::Setting(Setting::Widgets));
    let source = "https://example.invalid/current-vault".to_owned();
    {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.rows = vec![widgets.clone()];
        rows.vault_url = Some(source.clone());
    }
    let epoch = SETUP_ACTIVITY.lock().unwrap().epoch;
    let generation = OBSERVED_ROWS.lock().unwrap().generation;
    // The CLI calls reserve only once it has found owed work and Full
    // Disk Access. Both earlier exits take this same unreserved path.
    assert_eq!(finish_withheld_preferences_with(|_, _| None::<()>), None);
    assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(SETUP_ACTIVITY.lock().unwrap().epoch, epoch);
    let rows = OBSERVED_ROWS.lock().unwrap();
    assert_eq!(rows.rows, [widgets]);
    assert_eq!(rows.vault_url, Some(source));
    assert_eq!(rows.generation, generation);
    assert!(script().reads.is_empty());
    let queue = OBSERVE_QUEUE.lock().unwrap();
    assert!(!queue.running && queue.next.is_none() && queue.check.is_none());
}

#[test]
fn a_partial_observation_of_another_vault_retires_both_source_rows() {
    use aterm_types::macos_setup::{Access, Standing};

    let _globals = globals();
    let _io = TestIo::install();
    let caps = Subject::Setting(Setting::CapsLock);
    let secrets = Subject::Setting(Setting::Secrets);
    let repositories = Subject::Setting(Setting::Repositories);
    let access = Subject::Access(Access::FullDiskAccess);
    let reading = |subject, standing, note: &str| Observation {
        subject,
        standing,
        now: None,
        after_login: None,
        wanted: subject.wanted().to_owned(),
        note: Some(note.to_owned()),
        detail: None,
        read: aterm_types::macos_setup::Read::Unstamped,
    };
    let old = Plan::default();
    let kept = reading(caps, Standing::InPlace, "independent keyboard reading");
    let choices = {
        let mut rows = OBSERVED_ROWS.lock().unwrap();
        rows.rows = vec![
            kept.clone(),
            reading(secrets, Standing::NeedsYou, "old source approval OLD42"),
            reading(
                repositories,
                Standing::Failed,
                "old source authentication failure",
            ),
        ];
        rows.plan = Some(old.clone());
        rows.vault_url = Some(old.vault_url);
        rows.choices
    };
    let current = Plan {
        vault_url: "  http://current-vault.local:7484/v1/team  ".into(),
        ..Plan::default()
    };
    change(|script| {
        script.readings = [
            vec![reading(
                access,
                Standing::Differs,
                "access reading for current plan",
            )],
            vec![reading(
                repositories,
                Standing::Waiting,
                "current repository reading",
            )],
        ]
        .into();
    });
    observe_rows(ObserveRequest {
        plan: Some(current.clone()),
        choices,
        subjects: vec![access],
        forced: false,
        pending_repositories: false,
    });
    {
        let rows = OBSERVED_ROWS.lock().unwrap();
        assert_eq!(rows.vault_url.as_deref(), Some(current.vault_url.trim()));
        assert!(rows.rows.contains(&kept));
        assert!(rows.rows.iter().any(|row| row.subject == access));
        assert!(!rows.rows.iter().any(|row| row.subject.uses_vault_source()));
    }
    // A later partial repository read cannot relabel the old Secrets
    // code as belonging to the current source.
    observe_rows(ObserveRequest {
        plan: None,
        choices,
        subjects: vec![repositories],
        forced: false,
        pending_repositories: false,
    });
    let rows = OBSERVED_ROWS.lock().unwrap();
    assert!(rows.rows.contains(&kept));
    assert!(rows.rows.iter().any(|row| row.subject == repositories));
    assert!(!rows.rows.iter().any(|row| row.subject == secrets));
    assert!(!rows.rows.iter().any(|row| {
        row.note
            .as_deref()
            .is_some_and(|note| note.contains("OLD42"))
    }));
}

#[test]
fn a_refused_choice_gives_stop_back_the_run_it_unregistered() {
    let _globals = globals();
    let _io = TestIo::install();
    // Run 7 is parked, so the person's choice takes the free slot and
    // drops run 7's registration; then it is refused (another aterm or
    // `aterm doctor` holds the claim, or the plan could not be saved).
    set_setup_run(Some(7));
    let choices = OBSERVED_ROWS.lock().unwrap().choices;
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(false));
    wait_until("the refused choice is let go", || {
        OBSERVED_ROWS.lock().unwrap().choices == choices + 2
    });
    // Run 7 is the run a Stop stops again: pressed now, it stops run 7.
    assert_eq!(setup_run(), Some(7));
    assert!(stop_initial_setup());
    assert!(setup_stop_requested(7));
    wait_until("run 7's Stop is saved", stop_worker_idle);
    assert_eq!(script().stops, [7]);
    // Readings go back to the plan in force.
    assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, None);

    // A run that registered while the choice was refused keeps its place.
    reset_worker_state();
    set_setup_run(Some(7));
    script().registers = Some(8);
    let (receipt, answer) = aterm_gui::InitialSetupApplyReceipt::detached(true);
    let choices = OBSERVED_ROWS.lock().unwrap().choices;
    assert!(restart_initial_setup(left_as_is(), false, None, receipt));
    assert_eq!(answer.recv_timeout(HANG), Ok(false));
    wait_until("the refused choice is let go", || {
        OBSERVED_ROWS.lock().unwrap().choices == choices + 2
    });
    assert_eq!(setup_run(), Some(8));
}

/// The window's wiring (`wire_window`) registers the access-record watcher (System State
/// design §6): a line written to the record wakes a run parked on the Permissions stage and
/// queues a reading of every Access row. A reading already running stands in, so this test
/// reads no grant of this Mac; the record is a scratch folder's.
#[test]
fn an_access_step_on_record_wakes_the_run_and_reads_the_access_rows() {
    use aterm_types::macos_setup::{Access, AccessOutcome, Subject, access_record};
    let _globals = SETUP_GLOBALS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    {
        let mut queue = OBSERVE_QUEUE.lock().unwrap();
        assert!(!queue.running, "no reading runs in this test binary");
        queue.running = true;
        queue.next = None;
        queue.check = None;
    }
    // The wiring `run_window` does before the GUI starts, as a headless
    // window's (no LAN agent retirement): the watcher must be part of it.
    let _ = wire_window(&[OsString::from("--headless")]);
    let woken = SETUP_WAKES.count();
    let state = aterm_tempfile::tempdir().unwrap();
    access_record::append(
        state.path(),
        7,
        Access::FullDiskAccess,
        AccessOutcome::Granted,
    )
    .unwrap();
    let queued = {
        let mut queue = OBSERVE_QUEUE.lock().unwrap();
        queue.running = false;
        queue.next.take()
    };
    assert!(SETUP_WAKES.count() > woken, "a parked run is woken");
    let queued = queued.expect("a reading of the Access rows is asked for");
    assert_eq!(queued.plan, None, "the page's plan is kept");
    assert_eq!(queued.subjects, Access::ALL.map(Subject::Access).to_vec());
    assert!(!queued.forced);
}

#[test]
fn a_timed_out_dialog_waiter_wakes_setup_after_the_last_guard_ends() {
    use aterm_types::macos_setup::MacosDialog;
    use std::cell::Cell;
    use std::time::Duration;

    let _globals = globals();
    let _ = wire_window(&[OsString::from("--headless")]);
    for released_before_park in [true, false] {
        let first = MacosDialog::up();
        let last = MacosDialog::up();
        let seen = SETUP_WAKES.count();
        assert!(MacosDialog::when_none_up(Duration::from_nanos(1)).is_none());
        drop(first);
        assert_eq!(SETUP_WAKES.count(), seen, "one dialog remains up");

        let mut last = Some(last);
        if released_before_park {
            drop(last.take());
        }
        let waits = Cell::new(0);
        wait_for_setup_park_change(
            || (),
            |_| false,
            || SETUP_WAKES.count() == seen,
            || {
                assert!(
                    !released_before_park && waits.get() == 0,
                    "a released dialog must wake this wait without a focus or grant change"
                );
                waits.set(waits.get() + 1);
                drop(last.take());
            },
        );
        assert_eq!(waits.get(), usize::from(!released_before_park));
        assert_eq!(SETUP_WAKES.count(), seen + 1);
    }
}

#[test]
fn dialog_ownership_without_a_blocking_waiter_leaves_setup_parked() {
    use aterm_types::macos_setup::MacosDialog;
    use std::cell::Cell;
    use std::time::Duration;

    let _globals = globals();
    let _ = wire_window(&[OsString::from("--headless")]);
    let seen = SETUP_WAKES.count();
    let own = MacosDialog::when_none_up(Duration::from_nanos(1)).unwrap();
    assert!(MacosDialog::when_none_up(Duration::ZERO).is_none());
    drop(own);
    assert_eq!(SETUP_WAKES.count(), seen);

    // Neither a successful stage's own dialog nor an immediate UI slot
    // probe moves an unanswered Accessibility row. It still needs a person.
    let waits = Cell::new(0);
    wait_for_setup_park_change(
        || (),
        |_| false,
        || waits.get() < 2,
        || {
            assert_eq!(SETUP_WAKES.count(), seen);
            waits.set(waits.get() + 1);
        },
    );
    assert_eq!(waits.get(), 2);
}

#[test]
fn a_choice_that_holds_the_slot_leaves_stop_to_the_run_it_admits() {
    let _globals = globals();
    let _io = TestIo::install();
    // Run 7 is parked. A choice made while a pass runs stops that pass.
    set_setup_run(Some(7));
    // No later choice overtakes these two.
    let generation = SETUP_APPLY_GENERATION.load(std::sync::atomic::Ordering::Acquire);
    let running = begin_setup_choice(&Plan::default(), false, generation, false);
    assert_eq!((setup_run(), running.replaced), (Some(7), None));
    // A choice that took the slot replaces run 7 once admitted. Until its own run registers
    // there is nothing for a Stop to take; a Stop pressed meanwhile stops that run as it
    // registers, or run 7 as a refusal gives it back.
    let holding = begin_setup_choice(&Plan::default(), true, generation, false);
    assert_eq!((setup_run(), holding.replaced), (None, Some(7)));
    // Refusing the earlier choice keeps the later one's plan; refusing the choice that holds
    // the slot gives run 7 back.
    forget_setup_choice(&running);
    assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, Some(Plan::default()));
    forget_setup_choice(&holding);
    assert_eq!(setup_run(), Some(7));
    assert_eq!(OBSERVED_ROWS.lock().unwrap().plan, None);
}
