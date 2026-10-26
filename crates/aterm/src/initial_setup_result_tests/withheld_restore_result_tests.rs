// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_cli::macos_doctor::{
    Action, InitialSetupAdmissionStopTest, InitialSetupObserved, Report, RestoreRows,
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
