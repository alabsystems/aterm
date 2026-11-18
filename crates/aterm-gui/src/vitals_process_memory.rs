// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Immutable memory evidence from the engine's complete process-reading lane.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aterm_agent::harness::native_act::BootStamp;
use aterm_agent::harness::relaunch::ProcessLifetime;
use aterm_agent::supervise::memory::{AgentMemory, Pressure};
use aterm_session::{LaunchNonce, SessionId};
use aterm_sysprobe::{ProcessReading, Sweep};
use aterm_uds::process::{Birth, ProcessStart};
use aterm_vitals::model::{MemRates, MemSample, Snapshot};

/// Original read boundary on both the ordering clock and a clock that counts suspension.
/// These clocks bound age, not an atomic census or an interval without suspension. The
/// process lane separately validates the boot and PID namespace around its native reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservedAt {
    pub(crate) instant: Instant,
    pub(crate) continuous_ns: Option<u64>,
}

impl ObservedAt {
    /// Read the suspension-inclusive clock first, so a suspension between these two reads
    /// cannot give old input a new suspension-inclusive timestamp.
    pub(crate) fn capture(
        instant_clock: fn() -> Instant,
        continuous_clock: fn() -> Option<u64>,
    ) -> Self {
        let continuous_ns = continuous_clock();
        Self {
            instant: instant_clock(),
            continuous_ns,
        }
    }

    /// Finish a freshness check with the suspension-inclusive read. A suspension between
    /// the two current-clock reads must already count in the age being checked.
    pub(crate) fn now(
        instant_clock: fn() -> Instant,
        continuous_clock: fn() -> Option<u64>,
    ) -> Self {
        let instant = instant_clock();
        Self {
            instant,
            continuous_ns: continuous_clock(),
        }
    }

    /// Both clocks must be readable and ordered. Neither substitutes for the other.
    pub(crate) fn age_at(self, now: Self) -> Option<Duration> {
        let awake = now.instant.checked_duration_since(self.instant)?;
        let inclusive = Duration::from_nanos(now.continuous_ns?.checked_sub(self.continuous_ns?)?);
        Some(awake.max(inclusive))
    }

    pub(crate) fn within(self, now: Self, ttl: Duration) -> bool {
        self.age_at(now).is_some_and(|age| age <= ttl)
    }
}

/// A memory-age clock only. Windows' coarse uptime does not qualify native CPU cutoffs.
pub(crate) fn continuous_now() -> Option<u64> {
    #[cfg(windows)]
    {
        u64::try_from(aterm_sysprobe::power::windows::uptime().as_nanos()).ok()
    }
    #[cfg(not(windows))]
    {
        aterm_uds::process::continuous_ns()
    }
}

/// A pane generation supplied with the same request that selected its shell root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PaneSource {
    pub(crate) local: u64,
    pub(crate) sid: SessionId,
    pub(crate) nonce: LaunchNonce,
}

/// A supervised root already captured by the harness, without another native observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AgentRoot {
    pub(crate) sid: String,
    pub(crate) lifetime: ProcessLifetime,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Context {
    pub(crate) scope: BootStamp,
    pub(crate) namespace: Option<String>,
}

/// Process-evidence probes run in the existing process lane; its enclosing source boundary
/// also reads the age clock. Test readers inject inert arms.
#[derive(Clone, Copy)]
pub(crate) struct Probes {
    pub(crate) continuous_ns: fn() -> Option<u64>,
    pub(crate) context: fn() -> Option<Context>,
    pub(crate) roots: fn() -> Vec<AgentRoot>,
    pub(crate) start: fn(u32) -> Option<ProcessStart>,
    pub(crate) consoles:
        fn(&[ProcessReading], &[u32]) -> Option<aterm_sysprobe::windows::ConsoleFacts>,
}

impl Probes {
    pub(crate) const INERT: Self = Self {
        continuous_ns: || None,
        context: || None,
        roots: Vec::new,
        start: |_| None,
        consoles: |_, _| None,
    };

    pub(crate) const NATIVE: Self = Self {
        continuous_ns: continuous_now,
        context: native_context,
        roots: crate::harness_host::memory_roots,
        start: native_start,
        consoles: native_consoles,
    };
}

fn native_context() -> Option<Context> {
    let scope = BootStamp::current()?;
    let namespace = aterm_uds::process::pid_namespace();
    (BootStamp::current().as_ref() == Some(&scope)
        && aterm_uds::process::pid_namespace() == namespace)
        .then_some(Context { scope, namespace })
}

fn native_start(pid: u32) -> Option<ProcessStart> {
    #[cfg(target_vendor = "apple")]
    {
        i32::try_from(pid)
            .ok()
            .and_then(aterm_uds::process::read_process_start)
    }
    #[cfg(not(target_vendor = "apple"))]
    {
        let _ = pid;
        None
    }
}

fn native_consoles(
    rows: &[ProcessReading],
    roots: &[u32],
) -> Option<aterm_sysprobe::windows::ConsoleFacts> {
    #[cfg(all(windows, target_pointer_width = "64"))]
    {
        Some(aterm_sysprobe::windows::read_console_facts(rows, roots))
    }
    #[cfg(not(all(windows, target_pointer_width = "64")))]
    {
        let _ = (rows, roots);
        None
    }
}

/// Scope and requested lifetimes captured before the same lane reads its census.
pub(crate) struct Pending {
    probes: Probes,
    context: Option<Context>,
    roots: Vec<AgentRoot>,
    roots_complete: bool,
    at: ObservedAt,
}

impl Pending {
    pub(crate) fn begin(probes: Probes, at: ObservedAt) -> Self {
        let mut roots = (probes.roots)();
        let roots_complete = roots.len() <= aterm_sysprobe::MAX_GROUP_PIDS;
        roots.truncate(aterm_sysprobe::MAX_GROUP_PIDS);
        Self {
            context: (probes.context)(),
            roots,
            roots_complete,
            at,
            probes,
        }
    }

    /// Preserve only memory and membership facts, before ownership metadata can fill a row's
    /// otherwise unread generation. No counter or birth is reconstructed from display rows.
    pub(crate) fn finish(self, sweep: &Sweep) -> Captured {
        let mut roots = Vec::new();
        let mut rows = BTreeMap::new();
        let mut unique = true;
        for row in &sweep.procs {
            if rows.insert(row.pid, row).is_some() {
                unique = false;
            }
        }
        let mut sids = BTreeMap::new();
        let mut pids = BTreeMap::new();
        for root in &self.roots {
            *sids.entry(root.sid.as_str()).or_insert(0usize) += 1;
            *pids.entry(root.lifetime.pid()).or_insert(0usize) += 1;
        }
        if let Some(context) = self.context.as_ref() {
            for root in &self.roots {
                let pid = root.lifetime.pid();
                let Some(row) = rows.get(&pid) else {
                    continue;
                };
                if !unique || sids.get(root.sid.as_str()) != Some(&1) || pids.get(&pid) != Some(&1)
                {
                    continue;
                }
                let Some(start) = row.start.filter(|start| *start > 0) else {
                    continue;
                };
                let birth = row.birth.map(Birth).or_else(|| {
                    let observed = (self.probes.start)(pid)?;
                    (observed.boot.0 == start).then_some(Birth::from(observed.wall?))
                });
                if birth.is_some_and(|birth| {
                    root.lifetime.matches_observation(
                        &context.scope,
                        context.namespace.as_deref(),
                        pid,
                        birth,
                    )
                }) {
                    roots.push(root.clone());
                }
            }
        }
        let pids: Vec<_> = roots.iter().map(|root| root.lifetime.pid()).collect();
        let consoles = (self.probes.consoles)(&sweep.procs, &pids);
        let stable = self.context.is_some() && (self.probes.context)() == self.context;
        let mut at = self.at;
        if !stable
            || (self.probes.continuous_ns)()
                .zip(at.continuous_ns)
                .is_none_or(|(now, before)| now < before)
        {
            at.continuous_ns = None;
        }
        if !stable {
            roots.clear();
        }
        Captured {
            rows: sweep.procs.iter().map(memory_row).collect(),
            roots,
            consoles,
            at,
            complete: !sweep.truncated && stable && unique && self.roots_complete,
        }
    }
}

fn memory_row(row: &ProcessReading) -> ProcessReading {
    ProcessReading {
        pid: row.pid,
        start: row.start,
        birth: row.birth,
        ppid: row.ppid,
        footprint: row.footprint,
        pgid: row.pgid,
        uid: row.uid,
        ..ProcessReading::default()
    }
}

pub(crate) struct Captured {
    rows: Vec<ProcessReading>,
    roots: Vec<AgentRoot>,
    consoles: Option<aterm_sysprobe::windows::ConsoleFacts>,
    at: ObservedAt,
    complete: bool,
}

/// Published atomically with the whole snapshot it supplied, never a machine-only update.
#[derive(Clone, Debug)]
pub(crate) struct Receipt {
    pub(crate) seq: u64,
    pub(crate) observed_at: ObservedAt,
    pub(crate) panes: Vec<PaneSource>,
    rows: Arc<[ProcessReading]>,
    roots: Vec<AgentRoot>,
    consoles: Option<aterm_sysprobe::windows::ConsoleFacts>,
    complete: bool,
    machine: Option<MemSample>,
    rates: Option<MemRates>,
}

impl Captured {
    pub(crate) fn publish(self, snap: &Snapshot, panes: Vec<PaneSource>) -> Receipt {
        Receipt {
            seq: snap.seq,
            observed_at: self.at,
            panes,
            rows: self.rows.into(),
            roots: self.roots,
            consoles: self.consoles,
            complete: self.complete,
            machine: snap.mem.as_ref().map(|mem| mem.sample),
            rates: snap.mem.as_ref().and_then(|mem| mem.rates),
        }
    }
}

impl Receipt {
    /// Bind a sample to the live pane only when both keys are unique and its launch agrees.
    pub(crate) fn nonce_for(
        &self,
        local: u64,
        sid: &SessionId,
        nonce: LaunchNonce,
    ) -> Option<String> {
        let mut matching = self
            .panes
            .iter()
            .filter(|source| source.local == local || source.sid == *sid);
        let source = matching.next()?;
        (matching.next().is_none()
            && source.local == local
            && source.sid == *sid
            && source.nonce == nonce)
            .then(|| source.nonce.to_hex())
    }

    /// Known current cohort bytes from this census. Explicit unread membership or footprint
    /// evidence supplies no ceiling input; it never becomes zero or an exact historical total.
    pub(crate) fn agent(&self, sid: &str, lifetime: &ProcessLifetime) -> Option<AgentMemory> {
        if !self.complete
            || !self
                .roots
                .iter()
                .any(|root| root.sid == sid && root.lifetime == *lifetime)
        {
            return None;
        }
        let members: Vec<_> = if cfg!(windows) {
            let tree = self.consoles.as_ref()?.tree(&self.rows, lifetime.pid());
            if tree.incomplete {
                return None;
            }
            tree.members
        } else {
            let root = self.rows.iter().find(|row| row.pid == lifetime.pid())?;
            if root.pgid == 0 {
                return None;
            }
            self.rows
                .iter()
                .filter(|row| row.pgid == root.pgid)
                .collect()
        };
        let mut bytes = 0u64;
        for row in members {
            row.start.filter(|start| *start > 0)?;
            bytes = bytes.checked_add(row.footprint?)?;
        }
        let machine = self.machine.filter(|mem| mem.total > 0);
        Some(AgentMemory {
            footprint_mib: bytes >> 20,
            physical_mib: machine.map(|mem| mem.total >> 20),
            available_mib: machine.map(|mem| mem.available >> 20),
            used_mib: machine.map(|mem| mem.used() >> 20),
            pressure: machine.and_then(|mem| match mem.pressure {
                aterm_vitals::Pressure::Normal => Some(Pressure::Normal),
                aterm_vitals::Pressure::Warn => Some(Pressure::Warn),
                aterm_vitals::Pressure::Critical => Some(Pressure::Critical),
                aterm_vitals::Pressure::Unknown => None,
            }),
            swap_kib_s: self.rates.and_then(|rates| {
                let rate = rates.swap_bps() / 1024.0;
                (rate.is_finite() && rate >= 0.0).then_some(rate.min(f64::from(u32::MAX)) as u32)
            }),
            largest_other_mib: None,
        })
    }
}

#[cfg(test)]
#[path = "vitals_memory_clock_tests.rs"]
mod clock_tests;
