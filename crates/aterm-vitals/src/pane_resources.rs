// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Physical footprint and directional storage rates of the live processes owned by a local
//! terminal session. A process sweep supplies the ownership, counters, and observation clock.

use std::sync::Arc;

use crate::delegated::ProcessKey;
use crate::events::SessionScope;
use crate::explain::{IdMap, IdSet};
use crate::model::{Membership, ProcSample, Sample};
use crate::pane_history::{BUCKETS, Minute};

/// One observation of the pane's process scope.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Reading {
    /// End of this sweep on the host's monotonic clock.
    pub at_ns: u64,
    /// Physical footprint in bytes summed over readable members. A partial sum is a lower bound.
    pub footprint: Option<u64>,
    /// Observed process members whose physical footprint was unreadable.
    pub footprint_unread: u32,
    /// Number of owned process rows with a unique PID in this sweep.
    pub members: u32,
    /// All process ownership was observed for the scope of this reading.
    pub membership_complete: bool,
    /// Interval between the two readings used for the storage rates, in nanoseconds.
    pub window_ns: Option<u64>,
    /// Bytes read per second, with two counters for every member in the same scope.
    pub read_bps: Option<f64>,
    /// Bytes written per second, with two counters for every member in the same scope.
    pub write_bps: Option<f64>,
}

/// A session's one-minute history, separately bound to its shell's kernel identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    /// Last established shell generation for this local session.
    pub root: Option<ProcessKey>,
    /// Session identity captured with the root in the same host request.
    pub source: Option<SessionScope>,
    /// Latest attempted full observation; absent when the process reading or root was unavailable.
    pub latest: Option<Reading>,
    slots: Minute<Reading>,
}

impl History {
    /// Last twelve closed five-second buckets ending at or before `at_ns`, oldest first.
    #[must_use]
    pub fn buckets(&self, at_ns: u64) -> [Option<Reading>; BUCKETS] {
        self.slots.buckets(at_ns)
    }

    fn bind(&mut self, root: Option<ProcessKey>, shell_pid: u32, source: Option<&SessionScope>) {
        if source != self.source.as_ref()
            || root.is_some_and(|observed| Some(observed) != self.root)
            || self.root.is_some_and(|observed| observed.pid != shell_pid)
        {
            *self = Self {
                root,
                source: source.cloned(),
                ..Self::default()
            };
        }
    }
}

/// Histories keyed by stable local session (pane), independent of keyboard focus and tab layout.
pub type Panes = IdMap<u64, History>;

#[derive(Clone, Copy, Debug)]
struct Counters {
    pane: u64,
    version: Option<i32>,
    read: Option<u64>,
    written: Option<u64>,
}

#[derive(Debug)]
struct Sweep {
    root: Option<ProcessKey>,
    reading: Reading,
    read_delta: Option<u64>,
    write_delta: Option<u64>,
}

impl Sweep {
    fn new(at_ns: u64, membership_complete: bool) -> Self {
        Self {
            root: None,
            reading: Reading {
                at_ns,
                membership_complete,
                ..Reading::default()
            },
            read_delta: Some(0),
            write_delta: Some(0),
        }
    }

    fn add(&mut self, p: &ProcSample, before: Option<&Counters>) {
        self.reading.members = self.reading.members.saturating_add(1);
        if let Some(bytes) = p.footprint {
            self.reading.footprint =
                Some(self.reading.footprint.unwrap_or(0).saturating_add(bytes));
        } else {
            self.reading.footprint_unread = self.reading.footprint_unread.saturating_add(1);
        }
        let add_delta = |total: Option<u64>, now: Option<u64>, last: Option<u64>| {
            total?.checked_add(now?.checked_sub(last?)?)
        };
        self.read_delta = add_delta(self.read_delta, p.disk_read, before.and_then(|b| b.read));
        self.write_delta = add_delta(
            self.write_delta,
            p.disk_written,
            before.and_then(|b| b.written),
        );
    }

    fn publish(
        mut self,
        history: &mut History,
        shell_pid: u32,
        source: Option<&SessionScope>,
        continuous: bool,
    ) {
        history.bind(self.root, shell_pid, source);
        let at_ns = self.reading.at_ns;
        // A departed member has no counter at the right endpoint. Equal counts alone are not
        // enough: add() also requires each current process generation to have the same owner.
        if continuous
            && (self.read_delta.is_some() || self.write_delta.is_some())
            && self.reading.membership_complete
            && let Some(prior) = history
                .latest
                .filter(|r| r.membership_complete && r.members == self.reading.members)
            && let Some(span) = at_ns.checked_sub(prior.at_ns).filter(|v| *v != 0)
        {
            let rate = |bytes: u64| bytes as f64 * 1e9 / span as f64;
            self.reading.window_ns = Some(span);
            self.reading.read_bps = self.read_delta.map(rate);
            self.reading.write_bps = self.write_delta.map(rate);
        }
        history.latest = self.root.map(|_| self.reading);
        history.slots.record(at_ns, history.latest);
    }
}

/// Only the current process counters are needed to produce the next immutable snapshot.
#[derive(Debug, Default)]
pub(crate) struct State {
    pub(crate) panes: Arc<Panes>,
    previous: IdMap<ProcessKey, Counters>,
}

// Bind only an unambiguous launch, as EventScope::session does for this same source.
fn sources(s: &Sample) -> IdMap<u64, Option<&SessionScope>> {
    let mut sources = IdMap::default();
    for src in &s.event_sources {
        sources
            .entry(src.local)
            .and_modify(|found| *found = None)
            .or_insert_with(|| (!src.sid.is_empty() && !src.nonce.is_empty()).then_some(src));
    }
    sources
}

impl State {
    fn current(&mut self, s: &Sample) -> &mut Panes {
        let present: IdSet<_> = s.tabs.iter().map(|t| t.key).collect();
        let histories = Arc::make_mut(&mut self.panes);
        histories.retain(|key, _| present.contains(key));
        histories
    }

    pub(crate) fn missing(&mut self, s: &Sample) {
        // Machine-only reads intentionally leave the preceding process sweep intact.
        if s.machine_only {
            return;
        }
        self.previous.clear();
        let current = self.current(s);
        let sources = sources(s);
        for tab in &s.tabs {
            let h = current.entry(tab.key).or_default();
            h.bind(
                None,
                tab.shell_pid,
                sources.get(&tab.key).copied().flatten(),
            );
            h.latest = None;
            h.slots.record(s.at_ns, None);
        }
    }

    pub(crate) fn observe(
        &mut self,
        s: &Sample,
        procs: &[ProcSample],
        owners: &[Membership],
        pid_counts: &IdMap<u32, usize>,
        continuous: bool,
    ) {
        if s.machine_only {
            return;
        }
        let complete = !(s.procs_truncated || s.ownership_incomplete || s.native_incomplete);
        let mut sweeps: Vec<_> = s
            .tabs
            .iter()
            .map(|t| Sweep::new(s.at_ns, complete && !s.unproved_tabs.contains(&t.key)))
            .collect();
        let mut counters = IdMap::default();
        for (p, owner) in procs.iter().zip(owners) {
            let Some(index) = owner.tab else { continue };
            let tab = &s.tabs[index];
            let sweep = &mut sweeps[index];
            if pid_counts.get(&p.pid) != Some(&1) {
                sweep.reading.membership_complete = false;
                continue;
            }
            let key = p.start.map(|start| ProcessKey { pid: p.pid, start });
            if p.pid == tab.shell_pid && !s.unproved_tabs.contains(&tab.key) {
                sweep.root = key;
            }
            let before = key.and_then(|k| self.previous.get(&k)).filter(|b| {
                b.pane == tab.key && b.version.zip(p.version).is_none_or(|(a, b)| a == b)
            });
            sweep.add(p, before);
            if let Some(key) = key {
                counters.insert(
                    key,
                    Counters {
                        pane: tab.key,
                        version: p.version,
                        read: p.disk_read,
                        written: p.disk_written,
                    },
                );
            }
        }
        self.previous = counters;
        let sources = sources(s);
        let current = self.current(s);
        for (tab, sweep) in s.tabs.iter().zip(sweeps) {
            sweep.publish(
                current.entry(tab.key).or_default(),
                tab.shell_pid,
                sources.get(&tab.key).copied().flatten(),
                continuous,
            );
        }
    }
}
