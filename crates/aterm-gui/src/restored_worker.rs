// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Kernel exit observations for tasks kept across a cold restore.

use crate::{App, SessionCtx, Wake, restore};
use aterm_agent::harness::relaunch::{ProcessLifetime, ProcessLiveness};
use aterm_uds::exitwatch::ExitMoment;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// A proven exit may still wait for the shell's foreground or the task mark's fence.
const SETTLEMENT_RETRY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RestoredWorkerCheck {
    pub(crate) session: u64,
    nonce: aterm_session::LaunchNonce,
    record: crate::task_record::TaskRecord,
}

pub(crate) struct Worker {
    check: RestoredWorkerCheck,
    pid: u32,
    lifetime: Option<ProcessLifetime>,
    state: State,
    local_observed: bool,
    settle_after: Instant,
}

enum State {
    Unarmed(Instant),
    Watching(ExitMoment<()>),
    Ended,
}

pub(crate) type Workers = BTreeMap<u64, Worker>;

fn observe(pid: u32, lifetime: Option<&ProcessLifetime>) -> ProcessLiveness {
    if let Some(lifetime) = lifetime {
        return if lifetime.pid() == pid {
            lifetime.observe()
        } else {
            ProcessLiveness::Unknown
        };
    }
    // A local PID's absence says nothing about an old leaf whose machine/boot scope is missing.
    // Restoring that leaf never manufactures the missing lifetime from this machine's kernel.
    ProcessLiveness::Unknown
}

impl Worker {
    fn due(&self, now: Instant) -> bool {
        match &self.state {
            State::Ended => false,
            State::Watching(watch) => watch.get().is_some(),
            State::Unarmed(next) => now >= *next,
        }
    }

    fn refresh(
        &mut self,
        now: Instant,
        mut read: impl FnMut() -> ProcessLiveness,
        mut arm: impl FnMut(RestoredWorkerCheck) -> Option<ExitMoment<()>>,
    ) {
        if !self.due(now) {
            return;
        }
        self.state = State::Unarmed(now + Duration::from_secs(1));
        match read() {
            ProcessLiveness::Ended => self.state = State::Ended,
            ProcessLiveness::Alive => {
                self.local_observed = true;
                let Some(watch) = arm(self.check.clone()) else {
                    return;
                };
                // Acquisition is bracketed by the saved lifetime. A readiness notification is
                // only a wake: it never substitutes for a positive fresh death observation.
                match read() {
                    ProcessLiveness::Alive => self.state = State::Watching(watch),
                    ProcessLiveness::Ended => self.state = State::Ended,
                    ProcessLiveness::Unknown => {}
                }
            }
            ProcessLiveness::Unknown => {}
        }
    }
}

impl App {
    pub(crate) fn check_kept_worker(
        &mut self,
        session: u64,
        ctx: &SessionCtx,
        agent: &restore::AgentRestore,
    ) {
        let proxy = self.proxy.clone();
        let pid = agent.pid;
        self.check_kept_worker_with(session, ctx, pid, agent.lifetime.clone(), move |check| {
            let proxy = proxy.clone()?;
            ExitMoment::watch_then(
                pid,
                || (),
                move || {
                    let _ = proxy.send_event(Wake::RestoredWorkerEnded(check));
                },
            )
        });
    }

    fn check_kept_worker_with(
        &mut self,
        session: u64,
        ctx: &SessionCtx,
        pid: u32,
        lifetime: Option<ProcessLifetime>,
        arm: impl FnMut(RestoredWorkerCheck) -> Option<ExitMoment<()>>,
    ) {
        let record = ctx
            .timeline
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .task()
            .filter(|record| {
                record.state == crate::task_record::TaskState::Working
                    && record.why == Some(crate::task_record::WHY_RESTORED)
            })
            .cloned();
        let Some(record) = record else { return };
        let check = RestoredWorkerCheck {
            session,
            nonce: ctx.nonce,
            record,
        };
        let now = Instant::now();
        let mut worker = Worker {
            check,
            pid,
            lifetime,
            state: State::Unarmed(now),
            local_observed: false,
            settle_after: now,
        };
        let saved = worker.lifetime.clone();
        worker.refresh(now, || observe(pid, saved.as_ref()), arm);
        self.restored_workers.insert(session, worker);
    }

    pub(crate) fn retain_restored_worker(&mut self, session: u64) {
        let keep = self.restored_workers.get(&session).is_some_and(|worker| {
            self.pool.get(session).is_some_and(|live| {
                live.ctx.nonce == worker.check.nonce
                    && live
                        .ctx
                        .timeline
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .task()
                        == Some(&worker.check.record)
            })
        });
        if !keep {
            self.restored_workers.remove(&session);
        }
    }

    pub(crate) fn finish_restored_worker_check(&mut self, check: RestoredWorkerCheck) {
        if self
            .restored_workers
            .get(&check.session)
            .is_some_and(|worker| worker.check == check)
        {
            self.service_restored_workers();
        }
    }

    /// Failed registration of a proven local lifetime and a proven exit still waiting to settle
    /// owe a retry clock. Live kernel watches and missing or foreign scope add no periodic wake.
    pub(crate) fn restored_worker_wake(&self) -> Option<Instant> {
        if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
            return None;
        }
        self.restored_workers
            .values()
            .filter_map(|worker| match &worker.state {
                State::Unarmed(next) if worker.local_observed => Some(*next),
                State::Ended => Some(worker.settle_after),
                _ => None,
            })
            .min()
    }

    pub(crate) fn service_restored_workers(&mut self) {
        self.service_restored_workers_at(Instant::now());
    }

    fn service_restored_workers_at(&mut self, now: Instant) {
        let ids: Vec<_> = self.restored_workers.keys().copied().collect();
        for id in ids {
            let current = self.restored_workers.get(&id).is_some_and(|worker| {
                self.pool
                    .get(id)
                    .is_some_and(|live| live.ctx.nonce == worker.check.nonce)
            });
            if !current {
                self.restored_workers.remove(&id);
                continue;
            }
            let Some(worker) = self.restored_workers.get_mut(&id) else {
                continue;
            };
            if worker.due(now) {
                let saved = worker.lifetime.clone();
                let pid = worker.pid;
                let proxy = self.proxy.clone();
                worker.refresh(
                    now,
                    || observe(pid, saved.as_ref()),
                    move |check| {
                        let proxy = proxy.clone()?;
                        ExitMoment::watch_then(
                            pid,
                            || (),
                            move || {
                                let _ = proxy.send_event(Wake::RestoredWorkerEnded(check));
                            },
                        )
                    },
                );
            }
            if !matches!(worker.state, State::Ended) {
                continue;
            }
            // Keep the oldest owed turn while unrelated events arrive. A fresh event may
            // settle sooner, but a failed early attempt must not postpone the quiet retry.
            if now >= worker.settle_after {
                worker.settle_after = now + SETTLEMENT_RETRY;
            }
            self.retain_restored_worker(id);
            let Some(worker) = self.restored_workers.get(&id) else {
                continue;
            };
            let Some(live) = self.pool.get(id) else {
                continue;
            };
            // Foreground is read at settlement, never carried from before the background look.
            // This probe is the session factory's injected terminal reader, including headless.
            if live.pid <= 1 || (self.session_factory.fg_probe)(live.master) != live.pid {
                continue;
            }
            let mut mark = crate::task_record::TaskMark::new(crate::task_record::TaskState::Exited);
            mark.why = Some(crate::task_record::WHY_EXIT);
            let expected = &worker.check.record;
            let marked = crate::task_record::mark_session_task_with_context(
                &live.ctx,
                id,
                Some(&mark),
                crate::task_record::AttentionEdit::Keep,
                Duration::ZERO,
                |_, meta, timeline, _| {
                    timeline.task() == Some(expected)
                        && !meta.task_exit_owner.current(live.ctx.nonce, None)
                },
            );
            if marked.is_ok_and(|marked| {
                marked.is_some_and(|marked| {
                    marked
                        .record
                        .is_some_and(|record| record.state == crate::task_record::TaskState::Exited)
                })
            }) {
                self.restored_workers.remove(&id);
            }
        }
    }
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "app_restore_kept_worker_tests.rs"]
mod tests;
