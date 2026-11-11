// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! GPU time per process: the host's reads of the GPU's user clients (cumulative GPU time per
//! client, macOS's `AppUsage`), turned into each process's share of the device over the window
//! between two reads, and the factor κ that attributes the device's busy time to them in
//! proportion where their queues overlap (`docs/DESIGN-stats-deck-flow-2026-11-02.md` §3).

use std::collections::{HashMap, HashSet};

use crate::explain::WINDOW_MAX_NS;

/// One GPU user client as the host read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuClientRow {
    /// The client's registry entry id, unique for the boot.
    pub entry: u64,
    /// The process that opened it.
    pub pid: u32,
    /// That process's short name as the client names it (`p_comm`, 16 bytes at most).
    pub name: String,
    /// GPU time the client's queues ran, summed, ns; cumulative while the client lives.
    pub gpu_ns: u64,
}

/// Every GPU user client, read together.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuClientsSample {
    /// When it was read (ns on [`crate::Sample::at_ns`]'s clock).
    pub at_ns: u64,
    /// The clients.
    pub rows: Vec<GpuClientRow>,
    /// Every enumerated accelerator child's entry id, including children whose counter
    /// properties were absent or unreadable.
    pub entries: Vec<u64>,
    /// Whether every child's entry id was read. A client missing from an incomplete read may
    /// still have been there, so the next read counts none of its time rather than all of it.
    pub complete: bool,
}

/// The GPU's client time by owner over the last client window, each a share of the device
/// (`1.0` is the whole GPU busy for the window).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GpuSplit {
    /// Every client's, each process clamped at `1.0`, summed.
    pub clients: f32,
    /// aterm's and its tabs'.
    pub aterm: f32,
    /// The user's processes outside aterm.
    pub others: f32,
    /// Other users' processes, those whose owner was not read, and client time no swept process
    /// claims (a process gone since, or a reused pid).
    pub services: f32,
}

/// The last client read, the next window's start.
#[derive(Clone, Debug)]
struct Baseline {
    /// When it was read.
    at_ns: u64,
    /// Each client entry's time.
    ns: HashMap<u64, u64>,
    /// Every enumerated child, whether or not its counter was readable.
    entries: HashSet<u64>,
    /// Whether it read every child identity ([`GpuClientsSample::complete`]).
    complete: bool,
}

/// Each process's GPU share, from the last two client reads; held between reads and dropped
/// [`WINDOW_MAX_NS`] after the reads that made it.
#[derive(Clone, Debug, Default)]
pub(crate) struct GpuShares {
    /// The last read.
    last: Option<Baseline>,
    /// Each pid's name and share of the device over the last window.
    rates: HashMap<u32, (String, f32)>,
    /// When the read that made [`Self::rates`] was taken; `None` while none is held.
    at: Option<u64>,
    /// The device's busy fractions read since the last client read, summed, and how many: the
    /// window's mean busy, ū.
    busy: (f64, u32),
    /// κ for the held rates ([`Self::kappa`]).
    kappa: f32,
}

impl GpuShares {
    /// Take one sample's device busy fraction, where read, and its client read, if it carries
    /// one, and drop held shares once stale at `now_ns`.
    ///
    /// Over the window between this read and the last: an entry in both contributes its delta, an
    /// entry absent from a complete last enumeration all its time (it was created inside the
    /// window), an entry present without a counter or missed by an incomplete read nothing
    /// (it may be old), and an entry that is gone nothing. The first read, or one more than
    /// [`WINDOW_MAX_NS`] after the last, is a baseline and gives no shares. The device readings
    /// taken since the last client read, this one's included, are the window's mean busy ū, and
    /// with the clients' summed shares R they set [`Self::kappa`].
    pub(crate) fn absorb(
        &mut self,
        read: Option<&GpuClientsSample>,
        busy: Option<f32>,
        now_ns: u64,
    ) {
        if let Some(b) = busy.filter(|b| b.is_finite()) {
            self.busy.0 += f64::from(b.clamp(0.0, 1.0));
            self.busy.1 += 1;
        }
        if let Some(read) = read {
            let now: HashMap<u64, u64> = read.rows.iter().map(|r| (r.entry, r.gpu_ns)).collect();
            if let Some(then) = self.last.take()
                && let Some(window) = read
                    .at_ns
                    .checked_sub(then.at_ns)
                    .filter(|w| *w > 0 && *w <= WINDOW_MAX_NS)
            {
                let mut ran: HashMap<u32, (String, u64)> = HashMap::new();
                for r in &read.rows {
                    let d = match then.ns.get(&r.entry) {
                        Some(before) => r.gpu_ns.saturating_sub(*before),
                        None if then.complete && !then.entries.contains(&r.entry) => r.gpu_ns,
                        None => 0,
                    };
                    let e = ran.entry(r.pid).or_insert_with(|| (r.name.clone(), 0));
                    e.1 = e.1.saturating_add(d);
                }
                self.rates = ran
                    .into_iter()
                    .map(|(pid, (name, ns))| {
                        (pid, (name, (ns as f64 / window as f64).min(1.0) as f32))
                    })
                    .collect();
                self.at = Some(read.at_ns);
                let mean = (self.busy.1 > 0).then(|| (self.busy.0 / f64::from(self.busy.1)) as f32);
                self.kappa = kappa(mean, self.rates.values().map(|(_, s)| *s).sum());
            }
            self.busy = (0.0, 0);
            self.last = Some(Baseline {
                at_ns: read.at_ns,
                ns: now,
                entries: read.entries.iter().copied().collect(),
                complete: read.complete,
            });
        }
        if self
            .at
            .is_some_and(|t| now_ns.saturating_sub(t) > WINDOW_MAX_NS)
        {
            self.rates.clear();
            self.at = None;
        }
    }

    /// Whether shares are held.
    pub(crate) fn held(&self) -> bool {
        self.at.is_some()
    }

    /// The process `pid` named `name`'s share of the device: `None` while no shares are held,
    /// `0.0` for a process with no client, or whose name is not its clients' (the pid was reused).
    pub(crate) fn share(&self, pid: u32, name: &str) -> Option<f32> {
        self.held().then(|| {
            self.rates
                .get(&pid)
                .filter(|(n, _)| crate::model::same_process_name(n, name))
                .map_or(0.0, |(_, share)| *share)
        })
    }

    /// Every client's share, summed, while held.
    pub(crate) fn clients(&self) -> Option<f32> {
        self.held()
            .then(|| self.rates.values().map(|(_, s)| *s).sum())
    }

    /// κ, the factor that turns a scope's summed client time into its share of the device's busy
    /// time ([`kappa`]), while shares are held.
    pub(crate) fn kappa(&self) -> Option<f32> {
        self.held().then_some(self.kappa)
    }
}

/// κ for clients whose shares sum to `clients` (R) over a window in which the device was busy
/// `busy` (ū) of the time: ū / R where the clients ran more than the device was busy, their
/// queues overlapping (and ū read, R > 0), else 1. A scope's share of the device is then
/// κ · Σ r_p over its processes, so the shares of every scope sum to no more than ū and each
/// keeps its proportion: aterm cannot take the whole bar while WindowServer vanishes.
fn kappa(busy: Option<f32>, clients: f32) -> f32 {
    match busy {
        Some(u) if clients > u && clients > 0.0 && u.is_finite() => (u / clients).clamp(0.0, 1.0),
        _ => 1.0,
    }
}

#[cfg(test)]
#[path = "gpu_clients_tests.rs"]
mod tests;
