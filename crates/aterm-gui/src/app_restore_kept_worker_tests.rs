// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use aterm_uds::process::ProcessPresence;

/// A JOB IN FRONT OF THE SHELL KEEPS THE WORKER WORKING. The leaf names one pid: with a job in
/// front of its shell, as a newer agent the image never saw is, that pid's death says nothing
/// of what runs there. CONTROL: with the shell leading its terminal, the same death ends it.
#[test]
fn a_dead_agents_pid_ends_nothing_with_a_job_in_front_of_its_shell() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let app = App::headless_for_test();
        let id = app.focused_session_id(WindowId(0)).unwrap();
        let ctx = app.pool.get(id).unwrap().ctx.clone();
        crate::task_record::mark_session_task(
            &ctx,
            id,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
        )
        .unwrap();
        let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
        let check = || RestoredWorkerCheck {
            session: id,
            nonce: ctx.nonce,
            record: record.clone(),
        };
        for presence in [ProcessPresence::Absent, ProcessPresence::Exited] {
            assert!(
                check()
                    .observed(123, "original", presence, None, false)
                    .is_none()
            );
            assert!(
                check()
                    .observed(123, "original", presence, None, true)
                    .is_some(),
                "CONTROL: the shell leads"
            );
        }
        assert!(
            check()
                .observed(
                    123,
                    "original",
                    ProcessPresence::Present,
                    Some("replacement"),
                    false,
                )
                .is_none()
        );
    });
}

/// The read the worker's end rests on says what the kernel does: yes at a prompt, no with a job
/// in front, no for a terminal that answers nothing.
#[cfg(unix)]
#[test]
fn a_shell_leads_its_terminal_only_with_no_job_in_front_of_it() {
    use crate::app_restore::crash_journal_program_tests::{
        kill_all, spawn_leader, type_line, wait_for,
    };
    assert!(!shell_leads_its_terminal(-1, 1), "no terminal answers");
    let home = aterm_tempfile::tempdir().unwrap();
    let (master, shell) = spawn_leader(&["/bin/sh", "-i"], home.path());
    wait_for("the shell leads its terminal", || {
        shell_leads_its_terminal(master, shell)
    });
    assert!(!shell_leads_its_terminal(master, 0), "pid 0 is no shell");
    type_line(master, "/bin/sleep 600\n");
    let mut job = -1;
    wait_for("the job in front of the shell", || {
        job = crate::quit_safety::foreground_pgrp(master);
        job > 0 && job != shell
    });
    let leads = shell_leads_its_terminal(master, shell);
    kill_all(&[shell, job], &[master]);
    assert!(!leads, "a job stands in front of it");
}

/// A start is compared only where the kernel stores it. Elsewhere a difference is no proof
/// that another process holds the pid.
#[test]
fn a_start_is_read_for_the_comparison_only_where_the_kernel_stores_it() {
    assert_eq!(
        stored_start(std::process::id()).is_some(),
        cfg!(target_vendor = "apple")
    );
}

#[test]
fn restored_worker_closure_requires_positive_identity_evidence_and_the_same_record() {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        for (presence, start, ended) in [
            (ProcessPresence::Present, Some("original"), false),
            (ProcessPresence::Unknown, None, false),
            (ProcessPresence::Present, None, false),
            (ProcessPresence::Absent, None, true),
            (ProcessPresence::Exited, None, true),
            (ProcessPresence::Present, Some("replacement"), true),
        ] {
            let mut app = App::headless_for_test();
            let id = app.focused_session_id(WindowId(0)).unwrap();
            let ctx = app.pool.get(id).unwrap().ctx.clone();
            crate::task_record::mark_session_task(
                &ctx,
                id,
                &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
            )
            .unwrap();
            let row = crate::task_record::restore_row(&ctx).unwrap();
            crate::task_record::seed_restored(&ctx, &row, true, true);
            let record = ctx.timeline.lock().unwrap().task().cloned().unwrap();
            assert_eq!(record.why, Some(crate::task_record::WHY_RESTORED));
            app.finish_restored_worker_check(RestoredWorkerCheck {
                session: id,
                nonce: aterm_session::LaunchNonce::generate(),
                record: record.clone(),
            });
            assert_eq!(ctx.timeline.lock().unwrap().task(), Some(&record));
            let observed = RestoredWorkerCheck {
                session: id,
                nonce: ctx.nonce,
                record: record.clone(),
            }
            .observed(123, "original", presence, start, true);
            assert_eq!(observed.is_some(), ended);
            if let Some(check) = observed {
                app.finish_restored_worker_check(check);
                assert_eq!(
                    ctx.timeline.lock().unwrap().task().unwrap().state,
                    crate::task_record::TaskState::Exited
                );
                // An old result cannot overwrite a later working task.
                crate::task_record::mark_session_task(
                    &ctx,
                    id,
                    &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
                )
                .unwrap();
                let changed = ctx.timeline.lock().unwrap().task().cloned().unwrap();
                app.finish_restored_worker_check(RestoredWorkerCheck {
                    session: id,
                    nonce: ctx.nonce,
                    record,
                });
                assert_eq!(ctx.timeline.lock().unwrap().task(), Some(&changed));
            } else {
                assert_eq!(ctx.timeline.lock().unwrap().task(), Some(&record));
            }
        }
    });
}
