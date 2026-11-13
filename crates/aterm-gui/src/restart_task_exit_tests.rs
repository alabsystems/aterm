// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness_host::task_exit::tests::AdmittedRunner;

fn restored_record(agent: &RealAgent, local: u64) -> crate::task_record::TaskRecord {
    crate::task_record::mark_session_task(
        &agent.ctx,
        local,
        &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
    )
    .unwrap();
    {
        let mut timeline = agent.ctx.timeline.lock().unwrap();
        let mut legacy = timeline.task().cloned().unwrap();
        // This persisted label can outlive its original agent. The observed Claude snapshot,
        // rather than this label, must choose the runner required before signalling.
        legacy.agent = Some(Program::Codex);
        timeline.seed_task(legacy);
    }
    let row = crate::task_record::restore_row(&agent.ctx).unwrap();
    crate::task_record::seed_restored(&agent.ctx, &row, true, true);
    let record = agent.ctx.timeline.lock().unwrap().task().cloned().unwrap();
    assert_eq!(record.why, Some(crate::task_record::WHY_RESTORED));
    record
}

#[test]
fn a_real_restored_agent_cannot_be_ended_until_its_matching_runner_acknowledges_custody() {
    let local = 7630;
    let agent = RealAgent::spawn(false, local);
    let (acting, host) = agent.world();
    agent.claim();
    let before = restored_record(&agent, local);
    let refused = || {
        let reply = agent.restart(&acting, &format!("pid={}", agent.pid));
        assert_eq!(
            reply,
            format!(
                "ERR restart unavailable: {}\n",
                crate::restart_act::EXIT_OWNER_NOT_READY
            )
        );
        assert!(restart_stamp::read(acting.stamp_dir(), &agent.sid).is_none());
        assert_eq!(agent.acts("restart"), 0, "refusal sends no signal");
        assert!(agent.ended().is_none() && agent.record_left());
        assert_eq!(agent.ctx.timeline.lock().unwrap().task(), Some(&before));
    };
    refused();

    // A real runner from another launch must not be inherited through copied metadata.
    let other = crate::session_store::test_handle_on(7631, -1).ctx;
    let mut foreign = AdmittedRunner::new(&other, 7631);
    agent.ctx.meta.lock().unwrap().task_exit_owner =
        other.meta.lock().unwrap().task_exit_owner.clone();
    refused();
    foreign.finish();

    let mut wrong_agent = AdmittedRunner::for_agent(&agent.ctx, local, Program::Codex);
    agent.published();
    refused();
    wrong_agent.finish();

    let mut revoked = AdmittedRunner::new(&agent.ctx, local);
    agent.published();
    revoked.revoke();
    refused();
    revoked.finish();

    let mut disabled = AdmittedRunner::new(&agent.ctx, local);
    agent.published();
    disabled.disable();
    refused();
    disabled.finish();

    // The second half must recheck: its own stamp is taken back if the admitted runner
    // withdraws between stamping and signalling the old process.
    let mut withdrawn = AdmittedRunner::new(&agent.ctx, local);
    agent.published();
    let stamped = crate::restart_act::begin(
        agent.master,
        &format!("pid={}", agent.pid),
        &agent.ctx,
        Some(acting.stamp_dir()),
        aterm_slog::Actor::Ctl,
    )
    .unwrap_or_else(|reply| panic!("the actual runner is ready: {reply}"));
    assert!(restart_stamp::read(acting.stamp_dir(), &agent.sid).is_some());
    withdrawn.revoke();
    assert_eq!(
        stamped.signal(),
        format!(
            "ERR restart unavailable: {}\n",
            crate::restart_act::EXIT_OWNER_NOT_READY
        )
    );
    refused();
    withdrawn.finish();

    let _owner = AdmittedRunner::new(&agent.ctx, local);
    agent.published();
    let reply = agent.restart(&acting, &format!("pid={}", agent.pid));
    assert!(reply.starts_with("OK restarting"), "{reply}");
    until("the admitted restart really ends its old process", || {
        agent.ended().is_some()
    });
    assert_eq!(agent.acts("restart"), 1);
    assert_eq!(agent.ctx.timeline.lock().unwrap().task(), Some(&before));
    host.shutdown_and_join();
}

#[test]
fn a_real_restart_without_a_restored_task_keeps_its_existing_admission() {
    let agent = RealAgent::spawn(false, 7632);
    let (acting, host) = agent.world();
    agent.claim();
    assert!(agent.ctx.timeline.lock().unwrap().task().is_none());
    assert!(
        !agent
            .ctx
            .meta
            .lock()
            .unwrap()
            .task_exit_owner
            .current(agent.ctx.nonce, None)
    );
    let reply = agent.restart(&acting, &format!("pid={}", agent.pid));
    assert!(reply.starts_with("OK restarting"), "{reply}");
    until("the ordinary restart really ends its old process", || {
        agent.ended().is_some()
    });
    assert_eq!(agent.acts("restart"), 1);
    host.shutdown_and_join();
}
