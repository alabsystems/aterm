// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::sync::{Arc, mpsc};

struct Sleeper(aterm_uds::spawnfd::Child);
impl Sleeper {
    fn new() -> Self {
        Self(
            aterm_uds::spawnfd::command("/bin/sleep")
                .arg("600")
                .spawn()
                .unwrap(),
        )
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
    fn lifetime(&self) -> ProcessLifetime {
        ProcessLifetime::capture(self.pid()).expect("live child's precise lifetime")
    }
    fn end(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
impl Drop for Sleeper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn with_app(f: impl FnOnce(&mut App, u64, &Arc<SessionCtx>)) {
    if !crate::test_alone::alone() {
        return;
    }
    let state = aterm_tempfile::tempdir().unwrap();
    crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
        let mut app = App::headless_for_test();
        let id = app.focused_session_id(crate::WindowId(0)).unwrap();
        // The foreground probe is injected; the headless session never owns PID 42.
        app.pool.sessions.get_mut(&id).unwrap().session.pid = 42;
        app.session_factory.fg_probe = |_| 42;
        let ctx = app.pool.get(id).unwrap().ctx.clone();
        crate::task_record::mark_session_task(
            &ctx,
            id,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
        )
        .unwrap();
        let row = crate::task_record::restore_row(&ctx).unwrap();
        crate::task_record::seed_restored(&ctx, &row, true, true);
        assert_eq!(record(&ctx).why, Some(crate::task_record::WHY_RESTORED));
        f(&mut app, id, &ctx);
    });
}

fn record(ctx: &SessionCtx) -> crate::task_record::TaskRecord {
    ctx.timeline.lock().unwrap().task().cloned().unwrap()
}

fn attach(
    app: &mut App,
    id: u64,
    ctx: &SessionCtx,
    child: &Sleeper,
) -> mpsc::Receiver<RestoredWorkerCheck> {
    let (tx, rx) = mpsc::sync_channel(1);
    let pid = child.pid();
    app.check_kept_worker_with(id, ctx, pid, Some(child.lifetime()), move |check| {
        let tx = tx.clone();
        ExitMoment::watch_then(
            pid,
            || (),
            move || {
                let _ = tx.send(check);
            },
        )
    });
    assert!(matches!(
        app.restored_workers[&id].state,
        State::Watching(_)
    ));
    rx
}

fn shell_is_idle(ctx: &SessionCtx) {
    let mut timeline = ctx.timeline.lock().unwrap();
    timeline.note_foreground_group(42);
    timeline.set_program(42, Some("zsh".to_string()));
    timeline.publish_agent(
        "idle",
        None,
        None,
        None,
        crate::session_timeline::AgentStamp {
            generation: crate::control::ScreenGen { epoch: 1, seq: 1 },
            fp: 1,
        },
    );
    assert_eq!(timeline.agent().word, "idle");
    assert!(crate::harness_host::identified_agent(timeline.agent()).is_none());
}

fn take_host_hand(ctx: &SessionCtx) {
    let request = aterm_agent::harness::upgrade_drive::hand_take_request();
    let reply = crate::control::cmd_lease(ctx, request.strip_prefix("lease ").unwrap());
    assert!(reply.starts_with("OK lease acquired"), "{reply}");
}

#[test]
fn a_real_runner_carries_the_restored_task_across_the_old_agents_idle_shell_gap() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        let _runner = crate::harness_host::task_exit::tests::AdmittedRunner::new(ctx, id);
        take_host_hand(ctx);
        shell_is_idle(ctx);
        let before = record(ctx);
        let store = Arc::clone(&app.store);
        let watching = ctx.watchers.count();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                crate::control::await_task(
                    &store,
                    &[crate::control::TaskTarget {
                        local_id: id,
                        ctx,
                        since: Some(before.rev),
                        facts: false,
                        fact_since: None,
                    }],
                    crate::task_record::Combine::Any,
                    60_000,
                )
            });
            let deadline = Instant::now() + Duration::from_secs(60);
            while ctx.watchers.count() == watching {
                assert!(
                    Instant::now() < deadline,
                    "await task did not register its waiter"
                );
                aterm_uds::deadline::poll_yield();
            }
            child.end();
            let check = rx.recv_timeout(Duration::from_secs(60)).unwrap();
            app.finish_restored_worker_check(check.clone());
            app.service_restored_workers();
            assert_eq!(
                record(ctx),
                before,
                "the admitted runner owns this task's handoff"
            );
            assert!(matches!(app.restored_workers[&id].state, State::Ended));
            crate::task_record::mark_session_task(
                ctx,
                id,
                &crate::task_record::TaskMark::new(crate::task_record::TaskState::Done),
            )
            .unwrap();
            let reply = waiter.join().unwrap();
            assert!(
                reply.contains("state=done") && reply.contains("conclusion=finished"),
                "{reply}"
            );
            assert!(
                !reply.contains("conclusion=failed"),
                "no failed exit may be latched: {reply}"
            );
            let done = record(ctx);
            app.finish_restored_worker_check(check);
            app.service_restored_workers();
            assert_eq!(record(ctx), done);
            assert!(app.restored_workers.is_empty());
        });
    });
}

#[test]
fn a_runner_that_finishes_without_a_successor_releases_the_retained_exit_once() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        let mut runner = crate::harness_host::task_exit::tests::AdmittedRunner::new(ctx, id);
        take_host_hand(ctx);
        shell_is_idle(ctx);
        let before = record(ctx);
        child.end();
        let check = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        app.finish_restored_worker_check(check.clone());
        assert_eq!(record(ctx), before);
        assert!(matches!(app.restored_workers[&id].state, State::Ended));
        runner.finish();
        app.service_restored_workers();
        let closed = record(ctx);
        assert_eq!(closed.state, crate::task_record::TaskState::Exited);
        assert_eq!(closed.rev, before.rev + 1);
        assert!(app.restored_workers.is_empty());
        app.finish_restored_worker_check(check);
        app.service_restored_workers();
        assert_eq!(record(ctx), closed);
    });
}

#[test]
fn a_claim_and_host_hand_without_an_actual_runner_do_not_suppress_a_real_exit() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        assert_eq!(
            crate::session_timeline::claim_supervisor(
                ctx,
                &crate::harness_host::holder(),
                None,
                None,
                crate::metrics::now_us(),
            ),
            Ok(true)
        );
        take_host_hand(ctx);
        shell_is_idle(ctx);
        let before = record(ctx);
        assert!(
            !ctx.meta
                .lock()
                .unwrap()
                .task_exit_owner
                .current(ctx.nonce, None)
        );
        child.end();
        app.finish_restored_worker_check(rx.recv_timeout(Duration::from_secs(60)).unwrap());
        assert_eq!(record(ctx).state, crate::task_record::TaskState::Exited);
        assert_eq!(record(ctx).rev, before.rev + 1);
        assert!(app.restored_workers.is_empty());
    });
}

#[test]
fn a_fenced_exit_and_a_deferred_agent_exit_keep_their_watch_until_the_mark_commits() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        shell_is_idle(ctx);
        let before = record(ctx);
        ctx.fabric.fence_hold(false);
        child.end();
        let check = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        app.finish_restored_worker_check(check.clone());
        assert_eq!(record(ctx), before);
        assert!(matches!(app.restored_workers[&id].state, State::Ended));
        let retry = app
            .restored_worker_wake()
            .expect("the fenced exit owes a turn");
        ctx.fabric.lift_hold_fence();
        {
            let mut timeline = ctx.timeline.lock().unwrap();
            // The old agent's busy screen can outlast its process until a fresh shell read.
            // Publish that real verdict gate; a reader hint alone cannot identify it over zsh.
            timeline.publish_agent(
                "busy",
                None,
                None,
                None,
                crate::session_timeline::AgentStamp {
                    generation: crate::control::ScreenGen { epoch: 1, seq: 2 },
                    fp: 2,
                },
            );
            assert_eq!(timeline.agent().word, "busy");
            assert!(!timeline.task().unwrap().runs_a_program());
        }
        app.service_restored_workers_at(retry - Duration::from_millis(1));
        assert_eq!(
            record(ctx),
            before,
            "an unchanged deferred mark is not a committed exit"
        );
        assert!(matches!(app.restored_workers[&id].state, State::Ended));
        assert_eq!(app.restored_worker_wake(), Some(retry));
        app.service_restored_workers_at(retry);
        assert_eq!(record(ctx), before);
        assert_eq!(app.restored_worker_wake(), Some(retry + SETTLEMENT_RETRY));
        shell_is_idle(ctx);
        app.service_restored_workers_at(retry + SETTLEMENT_RETRY);
        let closed = record(ctx);
        assert_eq!(closed.state, crate::task_record::TaskState::Exited);
        assert_eq!(closed.rev, before.rev + 1);
        assert!(app.restored_workers.is_empty());
        assert!(app.restored_worker_wake().is_none());
        app.finish_restored_worker_check(check);
        assert_eq!(record(ctx), closed);
    });
}

#[cfg(unix)]
#[test]
fn a_kept_agent_that_dies_after_its_first_live_look_closes_its_task_once() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        let before = record(ctx);
        app.service_restored_workers();
        assert_eq!(record(ctx), before, "the first live look is not an exit");
        child.end();
        let check = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        app.finish_restored_worker_check(check.clone());
        let closed = record(ctx);
        assert_eq!(closed.state, crate::task_record::TaskState::Exited);
        assert_eq!(closed.why, Some(crate::task_record::WHY_EXIT));
        assert!(app.restored_workers.is_empty());
        app.finish_restored_worker_check(check);
        app.service_restored_workers();
        assert_eq!(
            record(ctx),
            closed,
            "a duplicate exit cannot mint another conclusion"
        );
    });
}

#[cfg(unix)]
#[test]
fn an_exit_keeps_its_evidence_until_the_current_foreground_job_returns_the_shell() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        let before = record(ctx);
        app.session_factory.fg_probe = |_| 43;
        child.end();
        let check = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        app.finish_restored_worker_check(check);
        assert_eq!(record(ctx), before, "a newer foreground job keeps custody");
        assert!(matches!(app.restored_workers[&id].state, State::Ended));
        let retry = app
            .restored_worker_wake()
            .expect("the exit waits for its shell");
        app.session_factory.fg_probe = |_| -1;
        app.service_restored_workers_at(retry - Duration::from_millis(1));
        assert_eq!(record(ctx), before, "an unread foreground is not the shell");
        assert_eq!(app.restored_worker_wake(), Some(retry));
        app.service_restored_workers_at(retry);
        assert_eq!(record(ctx), before);
        assert_eq!(app.restored_worker_wake(), Some(retry + SETTLEMENT_RETRY));
        app.session_factory.fg_probe = |_| 42;
        app.service_restored_workers_at(retry + SETTLEMENT_RETRY);
        assert_eq!(record(ctx).state, crate::task_record::TaskState::Exited);
        assert!(app.restored_workers.is_empty());
        assert!(app.restored_worker_wake().is_none());
    });
}

#[cfg(unix)]
#[test]
fn a_changed_task_cancels_its_watch_and_a_late_callback_cannot_close_the_successor() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let _rx = attach(app, id, ctx, &child);
        let old = app.restored_workers[&id].check.clone();
        crate::task_record::mark_session_task(
            ctx,
            id,
            &crate::task_record::TaskMark::new(crate::task_record::TaskState::Working),
        )
        .unwrap();
        let successor = record(ctx);
        assert_ne!(successor, old.record);
        app.on_task_changed(id);
        assert!(app.restored_workers.is_empty());
        assert_eq!(child.lifetime().observe(), ProcessLiveness::Alive);
        child.end();
        app.finish_restored_worker_check(old);
        assert_eq!(record(ctx), successor);
    });
}

#[cfg(unix)]
#[test]
fn a_replaced_session_nonce_or_retired_session_drops_its_watch() {
    with_app(|app, id, ctx| {
        let child = Sleeper::new();
        let _rx = attach(app, id, ctx, &child);
        app.restored_workers.get_mut(&id).unwrap().check.nonce =
            aterm_session::LaunchNonce::generate();
        app.service_restored_workers();
        assert!(app.restored_workers.is_empty());
        let _rx = attach(app, id, ctx, &child);
        app.retire_session_registration(id);
        assert!(app.restored_workers.is_empty());
        assert_eq!(child.lifetime().observe(), ProcessLiveness::Alive);
    });
}

#[cfg(unix)]
#[test]
fn an_unregistered_restore_leaf_keeps_its_watch_until_its_session_is_inserted() {
    with_app(|app, id, ctx| {
        let child = Sleeper::new();
        let held = app.pool.sessions.remove(&id).unwrap();
        let _rx = attach(app, id, ctx, &child);
        assert_eq!(
            app.restored_workers.len(),
            1,
            "capture precedes pool insertion"
        );
        app.pool.sessions.insert(id, held);
        app.service_restored_workers();
        assert_eq!(
            app.restored_workers.len(),
            1,
            "the same registered task owns the watch"
        );
    });
}

#[cfg(unix)]
#[test]
fn a_failed_watch_can_be_rearmed_but_uncertain_registration_never_proves_death() {
    with_app(|app, id, ctx| {
        let child = Sleeper::new();
        let _rx = attach(app, id, ctx, &child);
        let mut worker = app.restored_workers.remove(&id).unwrap();
        let now = Instant::now();
        worker.state = State::Unarmed(now);
        worker.refresh(now, || ProcessLiveness::Alive, |_| None);
        assert!(matches!(worker.state, State::Unarmed(_)));
        let later = now + Duration::from_secs(2);
        let mut reads = 0;
        worker.refresh(
            later,
            || {
                reads += 1;
                if reads == 1 {
                    ProcessLiveness::Alive
                } else {
                    ProcessLiveness::Unknown
                }
            },
            |_| ExitMoment::watch(child.pid(), || ()),
        );
        assert_eq!(reads, 2, "registration is bracketed");
        assert!(
            matches!(worker.state, State::Unarmed(_)),
            "unknown evidence drops the watch"
        );
        worker.refresh(
            later + Duration::from_secs(2),
            || ProcessLiveness::Alive,
            |_| ExitMoment::watch(child.pid(), || ()),
        );
        assert!(
            matches!(worker.state, State::Watching(_)),
            "failure retained retryable work"
        );
        assert_eq!(record(ctx).state, crate::task_record::TaskState::Working);
    });
}

#[cfg(unix)]
#[test]
fn a_kernel_notification_is_only_a_wake_until_a_fresh_read_proves_the_saved_process_ended() {
    with_app(|app, id, ctx| {
        let mut child = Sleeper::new();
        let rx = attach(app, id, ctx, &child);
        child.end();
        let _ = rx.recv_timeout(Duration::from_secs(60)).unwrap();
        let worker = app.restored_workers.get_mut(&id).unwrap();
        worker.refresh(Instant::now(), || ProcessLiveness::Unknown, |_| None);
        assert!(matches!(worker.state, State::Unarmed(_)));
        assert_eq!(record(ctx).state, crate::task_record::TaskState::Working);
    });
}

#[test]
fn a_live_pid_cannot_reconstruct_a_missing_saved_lifetime() {
    assert_eq!(observe(std::process::id(), None), ProcessLiveness::Unknown);
    assert_eq!(observe(0, None), ProcessLiveness::Unknown);
    assert_eq!(observe(1, None), ProcessLiveness::Unknown);
    let mut child = Sleeper::new();
    let pid = child.pid();
    child.end();
    assert_eq!(
        observe(pid, None),
        ProcessLiveness::Unknown,
        "local absence cannot prove a leaf with no saved machine/boot scope ended"
    );
}

#[test]
fn a_failed_local_watch_arms_the_park_deadline_but_unknown_or_active_watches_do_not() {
    with_app(|app, id, ctx| {
        let child = Sleeper::new();
        app.check_kept_worker_with(id, ctx, child.pid(), Some(child.lifetime()), |_| None);
        let expected = match app.restored_workers[&id].state {
            State::Unarmed(next) => next,
            _ => panic!("the unavailable watch is retained"),
        };
        assert_eq!(app.restored_worker_wake(), Some(expected));
        let mut deadline = None;
        let mut owner = crate::deadline_heal::DeadlineFold::default();
        crate::fold_owned_deadline(
            &mut deadline,
            &mut owner,
            app.restored_worker_wake().unwrap(),
            crate::metrics::DeadlineOwner::Presence,
        );
        assert_eq!(
            deadline,
            Some(expected),
            "a quiet loop must use WaitUntil, not Wait"
        );
        assert_eq!(owner.owner(), crate::metrics::DeadlineOwner::Presence);

        let _rx = attach(app, id, ctx, &child);
        assert!(
            app.restored_worker_wake().is_none(),
            "a live kernel watch adds no timer"
        );
        app.check_kept_worker_with(id, ctx, child.pid(), None, |_| panic!("no saved lifetime"));
        assert!(app.restored_worker_wake().is_none());
        let foreign: ProcessLifetime = aterm_json::from_str(&format!(
            r#"{{"host":"foreign:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","boot":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","pid":{},"birth":456}}"#, child.pid())).unwrap();
        app.check_kept_worker_with(id, ctx, child.pid(), Some(foreign), |_| {
            panic!("foreign scope")
        });
        assert!(
            app.restored_worker_wake().is_none(),
            "foreign evidence must not create a polling clock"
        );
        assert_eq!(record(ctx).state, crate::task_record::TaskState::Working);
    });
}
