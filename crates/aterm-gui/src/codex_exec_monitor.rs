// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Drives only the closing handle. This module cannot access the gate's private
//! state, its execution handle, or any launch binding, command or checkpoint.

use super::settlement::{Observation, Settlement};
use super::{ExecSource, now_ms, remaining};
use crate::budget::Slots;
use std::sync::{Arc, Weak, mpsc};
use std::time::{Duration, Instant};

static MONITORS: std::sync::LazyLock<Arc<Slots>> =
    std::sync::LazyLock::new(|| Arc::new(Slots::new(64)));

struct Job {
    owner: Weak<crate::SessionCtx>,
    source: aterm_agent::harness::native_act::Process,
    settlement: Settlement,
}

/// Reserve capacity and start its receiver before publishing a root. Dropping
/// an unused reservation stops the receiver and returns its capacity.
pub(super) struct Monitor(mpsc::SyncSender<Job>);
impl Monitor {
    pub(super) fn reserve() -> Option<Self> {
        Self::reserve_using(&MONITORS, |work| {
            std::thread::Builder::new()
                .name("codex-exec-custody".into())
                .spawn(work)
                .map(|_| ())
        })
    }

    fn reserve_using(
        budget: &Arc<Slots>,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
    ) -> Option<Self> {
        let slot = budget.try_take()?;
        let (send, receive) = mpsc::sync_channel(1);
        spawn(Box::new(move || {
            let _slot = slot;
            if let Ok(job) = receive.recv() {
                run(job);
            }
        }))
        .ok()?;
        Some(Self(send))
    }

    pub(super) fn start(
        self,
        ctx: &Arc<crate::SessionCtx>,
        source: &ExecSource,
        settlement: Settlement,
    ) {
        // There is no receiver exit before recv except this sender's EOF.
        self.0
            .send(Job {
                owner: Arc::downgrade(ctx),
                source: source.process.clone(),
                settlement,
            })
            .unwrap_or_else(|_| unreachable!("reserved settlement receiver exited"));
    }
}

fn run(mut job: Job) {
    let mut gone = false;
    loop {
        let ctx = job.owner.upgrade();
        let observation = ctx.as_ref().map(|ctx| Observation {
            authority: ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .supervisor_authority(crate::metrics::now_us()),
            human_seq: ctx.human_input.seq(),
            held: ctx.fabric.hold().is_some(),
            policy: ctx.native_policy.quota_rotation_enabled(),
        });
        let pid = i32::try_from(job.source.pid).unwrap_or(-1);
        if !gone {
            gone = aterm_uds::process::read_birth(pid)
                .is_some_and(|birth| birth.0 != job.source.birth)
                || matches!(
                    aterm_uds::process::process_presence(pid),
                    aterm_uds::process::ProcessPresence::Absent
                        | aterm_uds::process::ProcessPresence::Exited
                );
        }
        let expired = remaining(job.settlement.deadline()).is_none();
        let poll = job.settlement.poll(
            observation,
            gone,
            expired,
            Instant::now(),
            i64::try_from(now_ms() / 1000).unwrap_or(i64::MAX),
        );
        if let Some(ctx) = ctx
            && let Some(connection) = poll.release
        {
            crate::session_timeline::publish_release_supervisor(
                &ctx,
                Some(connection),
                crate::metrics::now_us(),
                aterm_slog::Actor::Aterm,
            )
            .published();
            crate::session_timeline::close_claim_connection(
                &ctx,
                connection,
                crate::metrics::now_us(),
            );
        }
        if poll.done {
            return;
        }
        if poll.terminate
            && aterm_agent::harness::native_act::Process::observe(job.source.pid).as_ref()
                == Some(&job.source)
            && let Some(identity) = aterm_uds::process::read_identity(pid)
        {
            let _ = aterm_uds::process::end_proven(
                identity,
                Duration::from_secs(1),
                &mut aterm_uds::process::EndControlLive,
                |_| {
                    aterm_agent::harness::native_act::Process::observe(job.source.pid).as_ref()
                        == Some(&job.source)
                },
            );
        }
        std::thread::park_timeout(poll.delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_and_failed_start_never_admit_unowned_work() {
        let budget = Arc::new(Slots::new(2));
        let first = budget.try_take().unwrap();
        let second = budget.try_take().unwrap();
        assert!(
            Monitor::reserve_using(&budget, |_| panic!("full budget must not start work"))
                .is_none()
        );
        drop(first);
        assert!(
            Monitor::reserve_using(&budget, |_| Err(std::io::Error::other("start refused")))
                .is_none()
        );
        assert_eq!(budget.used(), 1, "failed spawn returns its reservation");
        drop(second);
        assert_eq!(budget.used(), 0);
    }
}
