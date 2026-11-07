// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_agent::harness::native_receipts::testing::Journal;

#[test]
fn terminal_reply_preserves_the_hosts_phase_in_reopened_storage() {
    for (local, phase) in [(8340, ImagePhase::Forward), (8341, ImagePhase::Rollback)] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (handle, mut facts, prepare, actor) = closed_restart::fixture(local);
        facts.native_journal = Some(Journal::new(home.path()));
        let binding = admitted_binding(&run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor.clone(),
        ));
        let operation = lookup(&handle.ctx, &binding).unwrap();
        operation.lock().unwrap().resume_phase = Some(phase);
        facts.processes.lock().unwrap().remove(&4242);
        assert_eq!(
            run(
                &handle.ctx,
                &facts,
                Request::Reconcile(binding.clone()),
                actor
            ),
            NOEXEC
        );
        let observation = Journal::new(home.path())
            .observe_phase(&prepare, &binding)
            .unwrap()
            .unwrap();
        assert_eq!(observation.phase, phase);
        assert_eq!(
            operation
                .lock()
                .unwrap()
                .closed_restart
                .as_ref()
                .unwrap()
                .closed_phase(),
            phase
        );
    }
}

#[test]
fn retired_cleanup_preserves_phase_without_a_control_reply() {
    for (local, phase) in [(8342, ImagePhase::Forward), (8343, ImagePhase::Rollback)] {
        let home = aterm_tempfile::tempdir().unwrap();
        let (handle, mut facts, prepare, actor) = closed_restart::fixture(local);
        facts.native_journal = Some(Journal::new(home.path()));
        let binding = admitted_binding(&run(
            &handle.ctx,
            &facts,
            Request::prepare(prepare.clone()),
            actor,
        ));
        let operation = lookup(&handle.ctx, &binding).unwrap();
        facts.processes.lock().unwrap().remove(&4242);
        let completion = {
            let mut op = operation.lock().unwrap();
            op.resume_phase = Some(phase);
            op.seal_retirement(Retirement::NotGranted, &facts)
        };
        assert_eq!(finish(&handle.ctx, &operation, completion), NOEXEC);
        assert!(
            Journal::new(home.path())
                .observe_phase(&prepare, &binding)
                .unwrap()
                .is_none()
        );
        assert!(super::super::preparation::settle_retired(
            &handle.ctx,
            operation,
            &facts
        ));
        assert_eq!(
            Journal::new(home.path())
                .observe_phase(&prepare, &binding)
                .unwrap()
                .unwrap()
                .phase,
            phase
        );
    }
}

#[test]
fn a_phase_inconsistent_with_the_retained_closure_is_not_published() {
    let (handle, facts, prepare, actor) = closed_restart::fixture(8348);
    let binding = admitted_binding(&run(&handle.ctx, &facts, Request::prepare(prepare), actor));
    let operation = lookup(&handle.ctx, &binding).unwrap();
    facts.processes.lock().unwrap().remove(&4242);
    let completion = {
        let mut op = operation.lock().unwrap();
        op.resume_phase = Some(ImagePhase::Rollback);
        op.seal_retirement(Retirement::NotGranted, &facts)
    };
    assert_eq!(finish(&handle.ctx, &operation, completion), NOEXEC);
    let mut op = operation.lock().unwrap();
    assert_eq!(
        op.closed_restart.as_ref().unwrap().closed_phase(),
        ImagePhase::Rollback
    );
    op.resume_phase = Some(ImagePhase::Forward);
    assert_eq!(terminal_record(&op, NOEXEC), None);
    assert_eq!(terminal_record(&op, UNKNOWN), None);
}
