// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    Action, InitialSetupAdmissionStopTest, InitialSetupObserved, InitialSetupWithheldStopTest,
    PassPage, Report, RestoreRows, Row,
};
use aterm_types::macos_setup::{PutBack, RestoreOutcome};
use std::cell::{Cell, RefCell};

fn restored_report(root: &Path) -> Report {
    Report {
        action: Action::Restore,
        rows: Vec::new(),
        manual_steps: Vec::new(),
        backup: root.join("original-preferences.json"),
        restore: Some(RestoreRows {
            outcome: RestoreOutcome::of([(Some(Setting::Widgets), PutBack::Back)]),
        }),
        page: None,
    }
}

/// A follow-up's under-lock observation is the only context its result may carry. Backup
/// refusal gets the existing passive Restore explanation; a later real choice filters it out.
#[test]
fn withheld_restore_reports_use_the_operation_context_and_classify_only_backup_errors() {
    for failed in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        let setup = InitialSetupAdmissionStopTest::restored_before_run(root.path()).unwrap();
        assert!(setup.bring_forward().unwrap().is_some());
        let result = if failed {
            Err(format!(
                "{}: the owed original is missing",
                aterm_cli::macos_doctor::BACKUP_UNREAD
            ))
        } else {
            Ok(Some(restored_report(root.path())))
        };
        let before = setup.snapshot().unwrap();
        let published = RefCell::new(None);
        report_withheld_preferences(
            (
                InitialSetupObserved {
                    result,
                    observation: Some(before.observation.clone()),
                },
                None::<()>,
            ),
            |report, observation| {
                *published.borrow_mut() = Some(InitialSetupRows {
                    observation,
                    rows: initial_setup_report_rows(report),
                    check: None,
                });
            },
            |why, observation| {
                *published.borrow_mut() = Some(InitialSetupRows {
                    observation,
                    rows: vec![restore_failed_row(&why)],
                    check: None,
                });
            },
        );
        let published = published
            .into_inner()
            .expect("the Restore result is published");
        assert_eq!(published.observation.as_ref(), Some(&before.observation));
        let baseline = initial_setup_status_with(Ok(before), false, &InitialSetupRows::default());
        let status = initial_setup_status_with(Ok(setup.snapshot().unwrap()), false, &published);
        assert_eq!(status.plan, baseline.plan);
        assert_eq!(status.permissions, baseline.permissions);
        assert_eq!(status.run, baseline.run);
        let row = status
            .rows
            .iter()
            .find(|row| row.name == "Restore previous settings")
            .expect("the current operation reaches the page");
        if failed {
            assert_eq!(
                row,
                &restore_failed_row(aterm_cli::macos_doctor::BACKUP_UNREAD)
            );
        } else {
            assert_eq!(row.status, "restored");
        }
        assert!(setup.choose_group(&[Setting::Dock]).unwrap().is_some());
        let newer = setup.snapshot().unwrap();
        let baseline = initial_setup_status_with(
            Ok(setup.snapshot().unwrap()),
            false,
            &InitialSetupRows::default(),
        );
        let status = initial_setup_status_with(Ok(newer), false, &published);
        assert_eq!(
            status.rows, baseline.rows,
            "the old operation cannot follow a newer choice"
        );
        assert_eq!(status.plan, baseline.plan);
        assert_eq!(status.permissions, baseline.permissions);
        assert_eq!(status.run, baseline.run);
    }

    for result in [Err("an unrelated setup save failed".into()), Ok(None)] {
        report_withheld_preferences(
            (
                InitialSetupObserved {
                    result,
                    observation: None,
                },
                None::<()>,
            ),
            |_, _| panic!("no successful Restore"),
            |_, _| panic!("an unclassified failure is not a Restore result"),
        );
    }
}

/// Releasing the operation slot lets a newer worker publish. The old result must have been
/// published first, on both the successful Restore and classified failure paths.
#[test]
fn withheld_restore_keeps_its_reservation_through_result_publication() {
    struct Reservation<'a> {
        released: &'a Cell<bool>,
        published: &'a RefCell<InitialSetupRows>,
        next: Option<InitialSetupRows>,
    }
    impl Drop for Reservation<'_> {
        fn drop(&mut self) {
            self.released.set(true);
            *self.published.borrow_mut() = self.next.take().unwrap();
        }
    }

    for failed in [false, true] {
        let root = aterm_tempfile::tempdir().unwrap();
        publish_decision(root.path(), "restored");
        let old = snapshot(root.path()).observation;
        publish_decision(root.path(), "pending");
        let current = snapshot(root.path());
        let new_row = aterm_gui::SetupStatusRow {
            name: Setting::Spotlight.name().into(),
            status: "error".into(),
            detail: "a newer stage failed".into(),
        };
        let published = RefCell::new(InitialSetupRows::default());
        let released = Cell::new(false);
        let reservation = Reservation {
            released: &released,
            published: &published,
            next: Some(InitialSetupRows {
                observation: Some(current.observation.clone()),
                rows: vec![new_row.clone()],
                check: None,
            }),
        };
        report_withheld_preferences(
            (
                InitialSetupObserved {
                    result: if failed {
                        Err(aterm_cli::macos_doctor::BACKUP_UNREAD.into())
                    } else {
                        Ok(Some(restored_report(root.path())))
                    },
                    observation: Some(old),
                },
                Some(reservation),
            ),
            |report, observation| {
                assert!(!released.get(), "the newer worker is still waiting");
                *published.borrow_mut() = InitialSetupRows {
                    observation,
                    rows: initial_setup_report_rows(report),
                    check: None,
                };
            },
            |why, observation| {
                assert!(!released.get(), "the newer worker is still waiting");
                *published.borrow_mut() = InitialSetupRows {
                    observation,
                    rows: vec![restore_failed_row(&why)],
                    check: None,
                };
            },
        );
        assert!(released.get());
        let published = published.into_inner();
        assert_eq!(published.observation.as_ref(), Some(&current.observation));
        let status = initial_setup_status_with(Ok(current), false, &published);
        assert!(status.rows.contains(&new_row));
        assert!(
            !status
                .rows
                .iter()
                .any(|row| row.name == "Restore previous settings")
        );
    }
}

/// The put-back and the later setup-debt read are distinct outcomes. Once the
/// original is back, an unrelated read error cannot leave the old Waits result
/// on the page or turn the completed Restore into a failure.
#[test]
fn withheld_restore_publishes_its_completed_put_back_when_setup_debt_is_unreadable() {
    for unreadable in [false, true] {
        let _globals = globals();
        let _io = TestIo::install();
        let root = aterm_tempfile::tempdir().unwrap();
        let mut setup = InitialSetupWithheldStopTest::waiting_restore(root.path()).unwrap();
        let writes = setup.notification_center_writes();
        let before = snapshot(root.path());
        let mut waiting = restored_report(root.path());
        waiting.restore.as_mut().unwrap().outcome =
            RestoreOutcome::of([(Some(Setting::Widgets), PutBack::Waits)]);
        let waiting_rows = initial_setup_report_rows(&waiting);
        let published = RefCell::new(InitialSetupRows {
            observation: Some(before.observation.clone()),
            rows: waiting_rows.clone(),
            check: None,
        });
        assert!(
            initial_setup_status_with(Ok(snapshot(root.path())), false, &published.borrow(),)
                .rows
                .ends_with(&published.borrow().rows),
            "the earlier waiting result belongs to this same operation context"
        );
        let debt = root.path().join("withheld-preferences.json");
        assert!(!debt.exists(), "Restore already retired the setup debt");
        if unreadable {
            std::fs::create_dir(&debt).unwrap();
        }

        let finished =
            finish_withheld_preferences_with(|reserve, stopped| setup.follow_up(reserve, stopped));
        assert_eq!(setup.notification_center_writes(), writes + 1);
        assert!(
            finished.1.is_some(),
            "the actual put-back reserved the worker"
        );
        assert!(INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        assert_eq!(finished.0.observation.as_ref(), Some(&before.observation));
        let calls = Cell::new(0);
        report_withheld_preferences(
            finished,
            |report, observation| {
                assert!(INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
                assert_eq!(observation.as_ref(), Some(&before.observation));
                assert_eq!(
                    report.restore.as_ref().unwrap().outcome.rows,
                    [(Some(Setting::Widgets), PutBack::Back)]
                );
                if unreadable {
                    let path = debt.to_string_lossy();
                    assert!(report.needs_attention());
                    assert!(report.to_text().contains(path.as_ref()));
                    assert!(report.to_json().contains(path.as_ref()));
                }
                calls.set(calls.get() + 1);
                *published.borrow_mut() = InitialSetupRows {
                    observation,
                    rows: initial_setup_report_rows(report),
                    check: None,
                };
            },
            |_, _| panic!("a later setup read refusal is no failure of the completed Restore"),
        );
        assert_eq!(
            calls.get(),
            1,
            "the completed Restore must replace the old Waits result"
        );
        assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
        let published = published.into_inner();
        assert_eq!(published.observation.as_ref(), Some(&before.observation));
        let restored = initial_setup_report_rows(&restored_report(root.path()));
        for row in &restored {
            assert!(published.rows.contains(row));
        }
        assert!(published.rows.iter().all(|row| {
            row.status != "failed" && !row.detail.contains(root.path().to_string_lossy().as_ref())
        }));
        let uncertainty = aterm_gui::SetupStatusRow {
            name: aterm_gui::SETUP_ROW.into(),
            status: "unverified".into(),
            detail: String::new(),
        };
        assert_eq!(published.rows.contains(&uncertainty), unreadable);
        let current = initial_setup_status_with(Ok(snapshot(root.path())), false, &published);
        assert!(current.rows.iter().all(|row| !waiting_rows.contains(row)));
        for row in &restored {
            assert!(current.rows.contains(row));
        }
        assert_eq!(current.rows.contains(&uncertainty), unreadable);

        if unreadable {
            std::fs::remove_dir(&debt).unwrap();
        }
        assert!(!setup.owed().unwrap());
        let (observed, reservation) =
            finish_withheld_preferences_with(|reserve, stopped| setup.follow_up(reserve, stopped));
        assert!(observed.result.unwrap().is_none());
        assert!(
            reservation.is_none(),
            "the completed put-back cannot replay"
        );
        assert_eq!(setup.notification_center_writes(), writes + 1);

        setup.choose().unwrap();
        let baseline = initial_setup_status_with(
            Ok(snapshot(root.path())),
            false,
            &InitialSetupRows::default(),
        );
        let newer = initial_setup_status_with(Ok(snapshot(root.path())), false, &published);
        assert_eq!(
            newer.rows, baseline.rows,
            "a later choice retires this operation result"
        );
        assert_eq!(newer.permissions, baseline.permissions);
        assert_eq!(newer.run, baseline.run);
    }
}

#[test]
fn withheld_restore_does_not_turn_an_unrelated_setup_debt_read_error_into_success() {
    let _globals = globals();
    let _io = TestIo::install();
    let root = aterm_tempfile::tempdir().unwrap();
    let mut setup = InitialSetupWithheldStopTest::unfinished(root.path()).unwrap();
    let debt = root.path().join("withheld-preferences.json");
    let saved = root.path().join("withheld-preferences.saved");
    let before = std::fs::read(&debt).unwrap();
    std::fs::rename(&debt, &saved).unwrap();
    std::fs::create_dir(&debt).unwrap();
    let finished =
        finish_withheld_preferences_with(|reserve, stopped| setup.follow_up(reserve, stopped));
    assert!(finished.0.result.is_err());
    assert!(finished.0.observation.is_none());
    assert!(
        finished.1.is_none(),
        "the unreadable preflight takes no slot"
    );
    assert_eq!(setup.notification_center_writes(), 0);
    report_withheld_preferences(
        finished,
        |_, _| panic!("no Restore took place"),
        |_, _| panic!("the setup debt refusal is no Restore result"),
    );
    assert!(!INITIAL_SETUP_BUSY.load(std::sync::atomic::Ordering::Acquire));
    assert_eq!(std::fs::read(&saved).unwrap(), before);
    std::fs::remove_dir(&debt).unwrap();
    std::fs::rename(saved, debt).unwrap();
    assert!(
        setup.owed().unwrap(),
        "the unperformed setup work stays owed"
    );
}

/// Only the page's explicit projection may accompany the retained Restore
/// outcome. The CLI keeps its raw diagnostic without placing it on the page.
#[test]
fn withheld_restore_projection_keeps_explicit_uncertainty_without_raw_diagnostics() {
    let root = aterm_tempfile::tempdir().unwrap();
    let mut report = restored_report(root.path());
    let path = root.path().join("withheld-preferences.json");
    let diagnostic = format!("{}: injected read refusal", path.display());
    report.rows.push(Row {
        name: "com.apple.notificationcenterui widgets".into(),
        status: "error".into(),
        detail: diagnostic.clone(),
        elapsed_ms: None,
    });
    report.page = Some(PassPage {
        rows: vec![Row {
            name: aterm_gui::SETUP_ROW.into(),
            status: "unverified".into(),
            detail: String::new(),
            elapsed_ms: None,
        }],
        ..PassPage::default()
    });
    assert!(report.needs_attention());
    assert!(report.to_text().contains(&diagnostic));
    assert!(report.to_json().contains(&diagnostic));
    let rows = initial_setup_report_rows(&report);
    for restored in initial_setup_report_rows(&restored_report(root.path())) {
        assert!(rows.contains(&restored));
    }
    assert!(rows.iter().all(|row| {
        !row.detail.contains(root.path().to_string_lossy().as_ref())
            && !row.name.contains("com.apple.notificationcenterui")
            && !row.detail.contains("injected read refusal")
    }));
    assert!(rows.contains(&aterm_gui::SetupStatusRow {
        name: aterm_gui::SETUP_ROW.into(),
        status: "unverified".into(),
        detail: String::new(),
    }));
}
