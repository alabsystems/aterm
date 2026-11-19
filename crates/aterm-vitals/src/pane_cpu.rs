// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Reported pane CPU observed by the existing process sweeps, on a common five-second grid.

use std::sync::Arc;

use crate::delegated::ProcessKey;
use crate::events::SessionScope;
use crate::explain::{IdMap, ScopeCpu, ScopeCpuPane};
use crate::model::Sample;
use crate::pane_history::Minute;
pub use crate::pane_history::{BUCKET_NS, BUCKETS};
use crate::pane_resources;

/// Published own-counter growth in core equivalents between two fully observed sweeps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// End of the later frontend observation, in nanoseconds since the host's common epoch.
    pub at_ns: u64,
    /// Elapsed time between the two frontend observations, in nanoseconds.
    pub window_ns: u64,
    /// Published CPU-counter growth divided by elapsed time, in cores.
    pub cores: f64,
}

/// CPU reported for one pane's shell generation. Each bucket holds its last sweep's reading;
/// missing coverage remains a gap. A reading contributes only to the bucket in which it ended.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct History {
    /// Most recently established shell PID and start. Earlier generations have no cells here.
    pub root: Option<ProcessKey>,
    /// Original SID and launch nonce recorded by the same process sweep.
    pub source: Option<SessionScope>,
    /// Most recent attempted full sweep's reading when it had complete coverage.
    pub latest: Option<Reading>,
    slots: Minute<Reading>,
}

impl History {
    /// Twelve closed five-second buckets ending at or before `at_ns`, oldest first.
    #[must_use]
    pub fn buckets(&self, at_ns: u64) -> [Option<Reading>; BUCKETS] {
        self.slots.buckets(at_ns)
    }

    fn record(
        &mut self,
        at_ns: u64,
        window_ns: u64,
        pane: Option<&ScopeCpuPane>,
        scope: Option<&pane_resources::History>,
    ) {
        let root = scope.and_then(|h| h.root);
        let source = scope.and_then(|h| h.source.as_ref());
        if self.root != root || self.source.as_ref() != source {
            *self = Self {
                root,
                source: source.cloned(),
                ..Self::default()
            };
            // The CPU interval began under another shell or session principal.
            return;
        }
        self.latest = pane
            .filter(|pane| {
                pane.complete
                    && pane.root.is_some()
                    && pane.root == root
                    && scope.is_some_and(|h| h.latest.is_some())
                    && pane.reported.is_finite()
                    && pane.reported >= 0.0
            })
            .filter(|_| window_ns > 0 && window_ns <= at_ns)
            .map(|pane| Reading {
                at_ns,
                window_ns,
                cores: pane.reported,
            });
        self.slots.record(at_ns, self.latest);
    }
}

/// Live histories indexed by the local pane key, also the producer's local session ID.
pub type Panes = IdMap<u64, History>;

/// Advance from an ordinary process sweep. Machine-only refreshes share the previous snapshot.
pub(crate) fn absorb(
    panes: &mut Arc<Panes>,
    s: &Sample,
    cpu: Option<&ScopeCpu>,
    scopes: &pane_resources::Panes,
) {
    if s.machine_only {
        return;
    }
    let panes = Arc::make_mut(panes);
    panes.retain(|key, _| scopes.contains_key(key));
    if let Some(cpu) = cpu.filter(|cpu| cpu.read_at_ns == s.at_ns) {
        for pane in &cpu.panes {
            panes.entry(pane.key).or_default().record(
                s.at_ns,
                cpu.window_ns,
                Some(pane),
                scopes.get(&pane.key),
            );
        }
    } else {
        for (key, scope) in scopes {
            panes
                .entry(*key)
                .or_default()
                .record(s.at_ns, 0, None, Some(scope));
        }
    }
}
