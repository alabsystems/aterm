// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owns status, agent and rollout state together with both kinds of deadline.

use std::collections::hash_map::{Entry, OccupiedEntry};
use std::collections::{BTreeSet, HashMap};
use std::ops::{Deref, DerefMut};
use std::time::{Duration, Instant};

use super::{
    AGENT_MIN_INTERVAL, AgentWatch, PROGRAM_RECHECK, SessionSlot, StatusPolicy,
    unresolved_program_recheck,
};
use crate::codex_rollout_tap::RolloutAsk;

#[derive(Debug)]
pub(super) struct ScheduledStatuses {
    records: HashMap<u64, Record>,
    observations: BTreeSet<(Instant, u64)>,
    transitions: BTreeSet<(Instant, u64)>,
    policy: StatusPolicy,
    min_interval: Duration,
    #[cfg(test)]
    reads: std::cell::Cell<u64>,
}

#[derive(Debug, Default)]
pub(super) struct Record {
    pub(super) classified: Option<ScheduledStatus>,
    pub(super) agent: Option<AgentWatch>,
    pub(super) rollout: Option<Box<RolloutAsk>>,
    owed: Option<Instant>,
}

#[derive(Debug)]
pub(super) struct ScheduledStatus {
    state: SessionSlot,
    pub(super) next_due: Instant,
}

impl Deref for ScheduledStatus {
    type Target = SessionSlot;

    fn deref(&self) -> &Self::Target {
        &self.state
    }
}

impl DerefMut for ScheduledStatus {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.state
    }
}

impl Record {
    fn observation(&self) -> Option<Instant> {
        self.classified.as_ref().map(|slot| slot.next_due)
    }

    fn next_wake(&self, min_interval: Duration) -> Option<Instant> {
        let slot = self.classified.as_ref()?;
        let due = slot.next_due;
        let agent = self
            .agent
            .as_ref()
            .and_then(|watch| agent_wake(watch, due, min_interval));
        let rollout = self
            .rollout
            .as_deref()
            .and_then(RolloutAsk::wake)
            .map(|at| at.max(due));
        let status = slot.fsm.owed_wake().map(|at| at.max(due));
        [status, agent, rollout].into_iter().flatten().min()
    }

    fn is_empty(&self) -> bool {
        self.classified.is_none() && self.agent.is_none() && self.rollout.is_none()
    }
}

/// Refreshes both indexes on normal return and unwinding. Only this module owns a guard:
/// callers receive a scoped borrow whose lifetime cannot escape the editing closure.
struct Edit<'a> {
    entry: Option<OccupiedEntry<'a, u64, Record>>,
    observations: &'a mut BTreeSet<(Instant, u64)>,
    transitions: &'a mut BTreeSet<(Instant, u64)>,
    before_observation: Option<Instant>,
    before_transition: Option<Instant>,
    min_interval: Duration,
}

impl Deref for Edit<'_> {
    type Target = Record;

    fn deref(&self) -> &Self::Target {
        self.entry.as_ref().expect("owned until Drop").get()
    }
}

impl DerefMut for Edit<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.entry.as_mut().expect("owned until Drop").get_mut()
    }
}

impl Drop for Edit<'_> {
    fn drop(&mut self) {
        let mut entry = self.entry.take().expect("Drop takes its owned entry once");
        let id = *entry.key();
        let observation = entry.get().observation();
        let transition = entry.get().next_wake(self.min_interval);
        if observation != self.before_observation {
            if let Some(at) = self.before_observation {
                self.observations.remove(&(at, id));
            }
            if let Some(at) = observation {
                self.observations.insert((at, id));
            }
        }
        if transition != self.before_transition {
            if let Some(at) = self.before_transition {
                self.transitions.remove(&(at, id));
            }
            if let Some(at) = transition {
                self.transitions.insert((at, id));
            }
        }
        entry.get_mut().owed = transition;
        if entry.get().is_empty() {
            entry.remove();
        }
    }
}

impl ScheduledStatuses {
    pub(super) fn new(policy: StatusPolicy, min_interval: Duration) -> Self {
        Self {
            records: HashMap::new(),
            observations: BTreeSet::new(),
            transitions: BTreeSet::new(),
            policy,
            min_interval,
            #[cfg(test)]
            reads: std::cell::Cell::new(0),
        }
    }

    pub(super) const fn min_interval(&self) -> Duration {
        self.min_interval
    }

    pub(super) fn edit<R>(&mut self, id: u64, change: impl FnOnce(&mut Record) -> R) -> R {
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        let entry = match self.records.entry(id) {
            Entry::Occupied(entry) => entry,
            Entry::Vacant(entry) => entry.insert_entry(Record::default()),
        };
        let before_observation = entry.get().observation();
        let before_transition = entry.get().owed;
        let mut edit = Edit {
            entry: Some(entry),
            observations: &mut self.observations,
            transitions: &mut self.transitions,
            before_observation,
            before_transition,
            min_interval: self.min_interval,
        };
        change(&mut edit)
    }

    pub(super) fn edit_existing<R>(
        &mut self,
        id: u64,
        change: impl FnOnce(&mut Record) -> R,
    ) -> Option<R> {
        self.records
            .contains_key(&id)
            .then(|| self.edit(id, change))
    }

    pub(super) fn with_agent<R>(
        &mut self,
        id: u64,
        change: impl FnOnce(&mut AgentWatch) -> R,
    ) -> R {
        self.edit(id, |record| {
            change(record.agent.get_or_insert_with(|| AgentWatch {
                pgid: -1,
                ..AgentWatch::default()
            }))
        })
    }

    pub(super) fn with_classified<R>(
        &mut self,
        id: u64,
        now: Instant,
        change: impl FnOnce(&mut ScheduledStatus) -> R,
    ) -> R {
        let policy = self.policy;
        self.edit(id, |record| {
            change(record.classified.get_or_insert_with(|| ScheduledStatus {
                state: SessionSlot::new(policy, now),
                next_due: now,
            }))
        })
    }

    pub(super) fn edit_classified<R>(
        &mut self,
        id: u64,
        change: impl FnOnce(&mut ScheduledStatus) -> R,
    ) -> Option<R> {
        self.edit_existing(id, |record| record.classified.as_mut().map(change))
            .flatten()
    }

    pub(super) fn get(&self, id: &u64) -> Option<&ScheduledStatus> {
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        self.records.get(id)?.classified.as_ref()
    }

    pub(super) fn agent(&self, id: &u64) -> Option<&AgentWatch> {
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        self.records.get(id)?.agent.as_ref()
    }

    #[cfg(test)]
    pub(super) fn rollout(&self, id: &u64) -> Option<&RolloutAsk> {
        #[cfg(test)]
        self.reads.set(self.reads.get() + 1);
        self.records.get(id)?.rollout.as_deref()
    }

    pub(super) fn contains_key(&self, id: &u64) -> bool {
        self.get(id).is_some()
    }

    pub(super) fn reschedule(&mut self, id: u64, due: Instant) {
        self.edit_classified(id, |slot| slot.next_due = due);
    }

    pub(super) fn any_due(&self, now: Instant) -> bool {
        self.observations
            .first()
            .is_some_and(|(due, _)| *due <= now)
    }

    pub(super) fn due_ids(&self, now: Instant) -> Vec<u64> {
        self.observations
            .iter()
            .take_while(|(due, _)| *due <= now)
            .map(|(_, id)| *id)
            .collect()
    }

    pub(super) fn next_wake(&self) -> Option<Instant> {
        self.transitions.first().map(|(due, _)| *due)
    }

    pub(super) fn remove(&mut self, id: &u64) {
        if let Some(record) = self.records.remove(id) {
            if let Some(at) = record.observation() {
                self.observations.remove(&(at, *id));
            }
            if let Some(at) = record.owed {
                self.transitions.remove(&(at, *id));
            }
        }
    }

    pub(super) fn reconfigure(
        &mut self,
        policy: StatusPolicy,
        min_interval: Duration,
        now: Instant,
    ) -> bool {
        if self.policy.quiet_after == policy.quiet_after
            && self.policy.dwell == policy.dwell
            && self.min_interval == min_interval
        {
            return false;
        }
        self.policy = policy;
        self.min_interval = min_interval;
        let latest = now + min_interval;
        self.observations.clear();
        self.transitions.clear();
        for (id, record) in &mut self.records {
            if let Some(slot) = &mut record.classified {
                slot.fsm.policy = policy;
                slot.next_due = slot.next_due.min(latest);
                self.observations.insert((slot.next_due, *id));
            }
            record.owed = record.next_wake(min_interval);
            if let Some(at) = record.owed {
                self.transitions.insert((at, *id));
            }
        }
        true
    }

    pub(super) fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    pub(super) fn clear(&mut self) {
        self.records.clear();
        self.observations.clear();
        self.transitions.clear();
    }

    #[cfg(test)]
    pub(super) fn records(&self) -> impl Iterator<Item = (&u64, &Record)> {
        self.records.iter()
    }

    #[cfg(test)]
    pub(super) fn reset_reads(&self) {
        self.reads.set(0);
    }

    #[cfg(test)]
    pub(super) fn reads(&self) -> u64 {
        self.reads.get()
    }

    #[cfg(test)]
    pub(super) fn index_sizes(&self) -> (usize, usize, usize) {
        (
            self.records.len(),
            self.observations.len(),
            self.transitions.len(),
        )
    }

    #[cfg(test)]
    pub(super) fn transition_keys(&self) -> Vec<(Instant, u64)> {
        self.transitions.iter().copied().collect()
    }
}

fn agent_wake(watch: &AgentWatch, due: Instant, min_interval: Duration) -> Option<Instant> {
    let followup = watch.followup.then(|| {
        watch.output_at.map_or(due, |at| {
            at.checked_add(min_interval).unwrap_or(at).max(due)
        })
    });
    let unresolved = if watch.pgid > 0 && watch.program_seen == Some(None) {
        watch
            .resolved_at
            .map(|at| (at + unresolved_program_recheck(watch.resolve_attempts)).max(due))
    } else {
        None
    };
    let confirmation = watch
        .deferred_name_recheck_at
        .filter(|_| watch.pgid > 0)
        .map(|at| at.max(due));
    let build_wait = watch
        .resolved_at
        .filter(|_| watch.build_waiting && watch.pgid > 0)
        .map(|at| (at + PROGRAM_RECHECK).max(due));
    let settle = watch.rest_at.map(|at| (at + AGENT_MIN_INTERVAL).max(due));
    let kimi = watch.kimi_again.then(|| {
        watch
            .read_at
            .map_or(due, |at| (at + AGENT_MIN_INTERVAL).max(due))
    });
    let unread = (watch.kimi
        && watch
            .input
            .look
            .is_some_and(|look| look.known && !look.read))
    .then_some(due);
    [
        followup,
        unresolved,
        confirmation,
        build_wait,
        settle,
        kimi,
        unread,
    ]
    .into_iter()
    .flatten()
    .min()
}
