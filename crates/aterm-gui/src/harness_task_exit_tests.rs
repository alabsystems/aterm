// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::harness_host::{Acts, BodyEnd, HostHandle};
use std::sync::{Condvar, Mutex, mpsc};
use std::time::Duration;

#[derive(Default)]
struct Gate {
    released: Mutex<bool>,
    ready: Condvar,
}

impl Gate {
    fn wait(&self) {
        let released = self.released.lock().unwrap();
        let (released, _) = self
            .ready
            .wait_timeout_while(released, Duration::from_secs(60), |released| !*released)
            .unwrap();
        assert!(*released, "runner gate was not released");
    }

    fn release(&self) {
        *self.released.lock().unwrap() = true;
        self.ready.notify_all();
    }
}

/// A real admitted host thread, held inside its loop after the production entry published
/// its unique guard. A supervisor claim or lease cannot construct this fixture's admission.
pub(crate) struct AdmittedRunner {
    host: HostHandle,
    gate: Arc<Gate>,
    revoked: Arc<AtomicBool>,
    stopped: bool,
}

impl AdmittedRunner {
    pub(crate) fn new(ctx: &Arc<crate::SessionCtx>, local: u64) -> Self {
        Self::with_wake(ctx, local, Arc::new(|_| {}))
    }

    pub(crate) fn for_agent(ctx: &Arc<crate::SessionCtx>, local: u64, agent: Program) -> Self {
        Self::with_agent_wake(ctx, local, agent, Arc::new(|_| {}))
    }

    fn with_wake(
        ctx: &Arc<crate::SessionCtx>,
        local: u64,
        wake: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Self {
        Self::with_agent_wake(ctx, local, Program::Claude, wake)
    }

    fn with_agent_wake(
        ctx: &Arc<crate::SessionCtx>,
        local: u64,
        agent: Program,
        wake: Arc<dyn Fn(u64) + Send + Sync>,
    ) -> Self {
        let gate = Arc::new(Gate::default());
        let body_gate = Arc::clone(&gate);
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let entered_tx = Mutex::new(Some(entered_tx));
        let body = Arc::new(move |job: &WorkerJob| {
            if let Some(tx) = entered_tx.lock().unwrap().take() {
                tx.send(Arc::clone(&job.revoked)).unwrap();
            }
            body_gate.wait();
            BodyEnd::Stopped
        });
        let mut acts = Acts::inert();
        let published = Arc::clone(ctx);
        acts.task_exit_owner = Arc::new(move |_, admission| {
            Some(publish(&published, local, admission, Arc::clone(&wake)))
        });
        let host = HostHandle::for_supervised_program_test_with_acts(
            vec![ctx.self_id.as_str().to_string()],
            agent,
            body,
            acts,
        );
        let revoked = entered_rx.recv_timeout(Duration::from_secs(60)).unwrap();
        assert!(
            ctx.meta
                .lock()
                .unwrap()
                .task_exit_owner
                .current(ctx.nonce, Some(agent))
        );
        Self {
            host,
            gate,
            revoked,
            stopped: false,
        }
    }

    pub(crate) fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }

    pub(crate) fn disable(&self) {
        self.host
            .set_config(aterm_agent::supervise::SupervisorConfig {
                enabled: false,
                ..Default::default()
            });
    }

    pub(crate) fn finish(&mut self) {
        if !self.stopped {
            self.gate.release();
            self.host.shutdown_and_join();
            self.stopped = true;
        }
    }
}

impl Drop for AdmittedRunner {
    fn drop(&mut self) {
        self.finish();
    }
}

#[test]
fn only_the_matching_live_admitted_runner_carries_task_exit_responsibility() {
    let ctx = crate::session_store::test_handle_on(7621, -1).ctx;
    let mut runner = AdmittedRunner::new(&ctx, 7621);
    let slot = ctx.meta.lock().unwrap().task_exit_owner.clone();
    assert!(slot.current(ctx.nonce, None));
    assert!(!slot.current(LaunchNonce::generate(), None));
    assert!(!slot.current(ctx.nonce, Some(Program::Codex)));
    runner.revoke();
    assert!(
        !slot.current(ctx.nonce, None),
        "revocation is live admission"
    );
    runner.finish();

    let runner = AdmittedRunner::new(&ctx, 7621);
    let slot = ctx.meta.lock().unwrap().task_exit_owner.clone();
    runner.disable();
    assert!(
        !slot.current(ctx.nonce, None),
        "policy withdrawal is live admission"
    );
    drop(runner);

    let _runner = AdmittedRunner::for_agent(&ctx, 7621, Program::Codex);
    let slot = ctx.meta.lock().unwrap().task_exit_owner.clone();
    assert!(slot.current(ctx.nonce, Some(Program::Codex)));
    assert!(!slot.current(ctx.nonce, Some(Program::Claude)));
}

#[test]
fn a_temporary_read_pin_cannot_extend_a_finished_runner_and_its_wake_sees_the_end() {
    let ctx = crate::session_store::test_handle_on(7622, -1).ctx;
    let observed = Arc::new(Mutex::new(Weak::<State>::new()));
    let at_wake = Arc::clone(&observed);
    let (tx, rx) = mpsc::channel();
    let wake = Arc::new(move |local| {
        let live = at_wake
            .lock()
            .unwrap()
            .upgrade()
            .map(|state| state.live.load(Ordering::SeqCst));
        tx.send((local, live)).unwrap();
    });
    let mut runner = AdmittedRunner::with_wake(&ctx, 7622, wake);
    let slot = ctx.meta.lock().unwrap().task_exit_owner.clone();
    let pin = slot
        .0
        .upgrade()
        .expect("a temporary read of the actual runner");
    *observed.lock().unwrap() = Arc::downgrade(&pin);
    assert!(pin.live.load(Ordering::SeqCst));
    runner.finish();
    assert!(!pin.live.load(Ordering::SeqCst));
    assert!(!slot.current(ctx.nonce, None));
    let wakes: Vec<_> = rx.try_iter().collect();
    assert!(!wakes.is_empty(), "runner drop wakes the passive watcher");
    assert!(
        wakes.iter().all(|wake| *wake == (7622, Some(false))),
        "{wakes:?}"
    );
}

#[test]
fn unwinding_the_actual_entry_guard_releases_it_before_notifying() {
    let ctx = crate::session_store::test_handle_on(7623, -1).ctx;
    let published = Arc::clone(&ctx);
    let (tx, rx) = mpsc::channel();
    let mut acts = Acts::inert();
    acts.task_exit_owner = Arc::new(move |_, admission| {
        let observed = Arc::clone(&published);
        let tx = tx.clone();
        let runner = publish(
            &published,
            7623,
            admission,
            Arc::new(move |local| {
                let state = observed.meta.lock().unwrap().task_exit_owner.0.upgrade();
                tx.send((local, state.map(|state| state.live.load(Ordering::SeqCst))))
                    .unwrap();
            }),
        );
        let pin = published
            .meta
            .lock()
            .unwrap()
            .task_exit_owner
            .0
            .upgrade()
            .unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _runner = runner;
            panic!("the entry unwinds after publishing its actual admission");
        }));
        assert!(result.is_err());
        assert!(!pin.live.load(Ordering::SeqCst));
        None
    });
    let host = HostHandle::for_supervised_test_with_acts(
        vec![ctx.self_id.as_str().to_string()],
        Arc::new(|_| BodyEnd::Stopped),
        acts,
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(60)).unwrap(),
        (7623, Some(false))
    );
    host.shutdown_and_join();
}
