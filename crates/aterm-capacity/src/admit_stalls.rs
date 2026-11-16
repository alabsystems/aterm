// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Progress observations belong to one claim, not every claim a long-lived process makes.
//! A later waiter shares the same claim's history; another claim, or an observed change in its
//! admission stamp or credited work set, starts a new observation. Current ledger fields cannot
//! reveal a credit set changing away and back between successful scans.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_toolchain::stall::{BUSY_OVER, Stall};
use aterm_toolchain::waits::Waiting;

use super::Holder;
use crate::ledger::Record;
use crate::reading::Procs;

type Order = (u64, u32, u64);

/// Forget a ledger not observed for an hour. Within an observed ledger, its complete held
/// snapshot bounds the cache; a live claim keeps its history while it blocks another resource.
const KEPT: Duration = Duration::from_secs(3600);

#[derive(PartialEq, Eq)]
struct Epoch {
    admitted_ms: u64,
    credit: Vec<u32>,
}

impl Epoch {
    fn of(record: &Record) -> Option<Self> {
        let held = record.held?;
        let mut credit = record.credit.clone();
        credit.sort_unstable();
        credit.dedup();
        Some(Self {
            admitted_ms: held.admitted_ms,
            credit,
        })
    }
}

struct Observed {
    epoch: Epoch,
    /// Created at the first actual work read, with that observer's busy window.
    stall: Option<Stall>,
}

struct Ledger {
    observed: Instant,
    claims: BTreeMap<Order, Observed>,
}

/// This process's observations, shared across waiters but separated by immutable claim order.
#[derive(Default)]
pub(super) struct Cache {
    ledgers: BTreeMap<PathBuf, Ledger>,
}

impl Cache {
    pub(super) const fn new() -> Self {
        Self {
            ledgers: BTreeMap::new(),
        }
    }

    /// Refresh from a complete successful ledger scan, before any blocker/ancestor filtering.
    /// Missing claims and observed epoch changes discard only their own histories. Heartbeats,
    /// writes and memory steps of the same claim do not change its observation epoch.
    pub(super) fn refresh(&mut self, dir: &Path, records: &[Record], now: Instant) {
        let phases: BTreeMap<_, _> = records
            .iter()
            .filter_map(|r| Epoch::of(r).map(|epoch| (r.order(), epoch)))
            .collect();
        let ledger = self
            .ledgers
            .entry(dir.to_path_buf())
            .or_insert_with(|| Ledger {
                observed: now,
                claims: BTreeMap::new(),
            });
        ledger.observed = now;
        ledger
            .claims
            .retain(|order, claim| phases.get(order).is_some_and(|phase| *phase == claim.epoch));
        for (order, epoch) in phases {
            ledger
                .claims
                .entry(order)
                .or_insert(Observed { epoch, stall: None });
        }
        self.ledgers.retain(|_, ledger| {
            !ledger.claims.is_empty() && now.saturating_duration_since(ledger.observed) < KEPT
        });
    }

    /// Book the same work reads as admission used before; absence of a claim or a process
    /// sweep proves no stall. Nested waits retain their independent `Waits::Stuck` evidence.
    pub(super) fn read(
        &mut self,
        dir: &Path,
        holders: &mut [Holder],
        procs: Option<&Procs>,
        waits: &[Waiting],
        now: Instant,
        after: Duration,
    ) {
        for holder in holders.iter_mut().filter(|h| h.held) {
            holder.stalled = self
                .ledgers
                .get_mut(dir)
                .and_then(|ledger| ledger.claims.get_mut(&holder.order))
                .and_then(|claim| {
                    let work = procs.and_then(|p| p.work(holder.pid, &holder.credit, waits));
                    claim
                        .stall
                        .get_or_insert_with(|| Stall::over(BUSY_OVER.min(after)))
                        .stuck(holder.pid, work, now, after)
                });
        }
    }
}

#[cfg(test)]
#[path = "admit_stalls_tests.rs"]
mod tests;
