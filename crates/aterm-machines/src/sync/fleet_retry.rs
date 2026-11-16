// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retry fleet, node or fabric faults beside repository work in the same admitted pass. The
//! main thread remains their only owner; a scoped repository helper shares its existing lease.

use super::{FileJournal, ItemReport, Items, Kind, Mode, Ports, Scope, Synced, Trigger};
use aterm_secrets::converge::{Last, RunnerRetry};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

pub(super) enum Event {
    Row(Box<ItemReport>),
    Finished,
}

pub(super) enum Woke {
    Event(Event),
    Retry,
    Closed,
}

/// The existing retry's monotonic clock and a wait interrupted by repository progress.
pub(super) trait Wait {
    fn now_ms(&self) -> u64;
    fn next(&mut self, events: &Receiver<Event>, pause: Option<Duration>) -> Woke;
}

pub(super) struct LiveWait(Instant);

impl Default for LiveWait {
    fn default() -> Self {
        Self(Instant::now())
    }
}

impl Wait for LiveWait {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn next(&mut self, events: &Receiver<Event>, pause: Option<Duration>) -> Woke {
        match pause {
            Some(pause) => match events.recv_timeout(pause) {
                Ok(event) => Woke::Event(event),
                Err(RecvTimeoutError::Timeout) => Woke::Retry,
                Err(RecvTimeoutError::Disconnected) => Woke::Closed,
            },
            None => events.recv().map_or(Woke::Closed, Woke::Event),
        }
    }
}

/// Complete even when the helper unwinds, so its owner always joins it rather than retrying
/// beside work that ended. The flag closes the gap between a timed wait and a completed helper.
struct Finished<'a> {
    done: &'a AtomicBool,
    events: Sender<Event>,
}

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
        let _ = self.events.send(Event::Finished);
    }
}

pub(super) struct Retry<'a> {
    pub ports: &'a dyn Ports,
    pub trigger: Trigger,
    pub scope: Scope,
    pub failed_at: u64,
    pub journal: FileJournal,
}

impl Retry<'_> {
    pub fn during(
        mut self,
        repositories: &Items<'_>,
        rows: &mut Vec<ItemReport>,
        on_row: &mut dyn FnMut(&ItemReport),
        wait: &mut dyn Wait,
    ) -> Result<Vec<ItemReport>, String> {
        let done = AtomicBool::new(false);
        std::thread::scope(|threads| {
            let (events, received) = mpsc::channel();
            let mut journal = self.journal.clone();
            let scope = self.scope;
            let completed = &done;
            let helper = std::thread::Builder::new()
                .name("aterm-sync-repositories".into())
                .spawn_scoped(threads, move || {
                    let _finished = Finished {
                        done: completed,
                        events: events.clone(),
                    };
                    aterm_converge::run_scoped(
                        repositories,
                        Mode::Apply,
                        scope,
                        &mut journal,
                        &mut |row| {
                            let _ = events.send(Event::Row(Box::new(row.clone())));
                        },
                    )
                    .map_err(|why| why.to_string())
                })
                .map_err(|why| format!("the repository lane could not start: {why}"))?;
            let mut retry = RunnerRetry::default();
            retry.reaped(Last::Failed, self.failed_at);
            loop {
                // Drain progress before considering a deadline. Row traffic never resets that
                // deadline, and a completed repository segment owes no further in-pass retry.
                match received.try_recv() {
                    Ok(Event::Row(row)) => on_row(&row),
                    Ok(Event::Finished) | Err(TryRecvError::Disconnected) => break,
                    Err(TryRecvError::Empty) => {}
                }
                if done.load(Ordering::Acquire) {
                    while let Ok(event) = received.recv() {
                        match event {
                            Event::Row(row) => on_row(&row),
                            Event::Finished => break,
                        }
                    }
                    break;
                }
                let pause = retry.remaining(wait.now_ms());
                if pause.is_some_and(|pause| pause.is_zero()) {
                    self.attempt(rows, on_row, &mut retry, wait)?;
                    continue;
                }
                match wait.next(&received, pause) {
                    Woke::Event(Event::Row(row)) => on_row(&row),
                    Woke::Event(Event::Finished) | Woke::Closed => break,
                    Woke::Retry => {}
                }
            }
            helper
                .join()
                .map_err(|_| "the repository lane stopped unexpectedly".to_owned())?
        })
    }

    fn attempt(
        &mut self,
        rows: &mut Vec<ItemReport>,
        on_row: &mut dyn FnMut(&ItemReport),
        retry: &mut RunnerRetry,
        wait: &dyn Wait,
    ) -> Result<(), String> {
        // New items re-read the current vault hold, swarm choice, signed fleet code and native
        // membership. Neither the first attempt's cached probe nor a scheduling receipt admits it.
        let kinds = if self.ports.fabric_needs_node() {
            [Kind::Fleet, Kind::Node, Kind::Fabric]
        } else {
            [Kind::Fabric, Kind::Fleet, Kind::Node]
        };
        let items: Items<'_> = kinds
            .into_iter()
            .map(|kind| Box::new(Synced::new(kind, self.ports, self.trigger, Mode::Apply)) as _)
            .collect();
        retry.started();
        // The initial local rows have finished before the repository helper starts. A joined
        // fleet can therefore repair its node and fabric immediately, without racing their
        // first execution. Their own current probes retain every explicit off/refusal.
        let latest =
            aterm_converge::run_scoped(&items, Mode::Apply, self.scope, &mut self.journal, on_row)
                .map_err(|why| why.to_string())?;
        retry.reaped(
            if latest.iter().any(|row| row.outcome.is_failure()) {
                Last::Failed
            } else {
                Last::Ended
            },
            wait.now_ms(),
        );
        for mut row in latest {
            if let Some(before) = rows.iter_mut().find(|before| before.id == row.id) {
                row.elapsed_ms = before
                    .elapsed_ms
                    .zip(row.elapsed_ms)
                    .map(|(before, after)| before.saturating_add(after));
                *before = row;
            }
        }
        Ok(())
    }
}
