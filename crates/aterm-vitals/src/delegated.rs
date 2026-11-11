// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Explicit job ownership ledger keyed by kernel process identity.

use std::collections::{HashMap, HashSet};

/// A PID and its kernel start record, in one boot's platform clock unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProcessKey {
    /// The process identifier.
    pub pid: u32,
    /// The kernel's start record, never a caller's timestamp.
    pub start: u64,
}

/// One identity-proven process and its current parent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Process {
    /// This process's identity.
    pub key: ProcessKey,
    /// The current parent PID.
    pub parent: u32,
}

/// An authenticated job root and its display metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Job {
    /// Stable process root, registered before its children are launched.
    pub root: ProcessKey,
    /// The authenticated global session id, not an inherited environment hint.
    pub session: String,
    /// The AI/program label, without command arguments.
    pub ai: String,
    /// A bounded, human-readable job label.
    pub label: String,
}

/// A retained job membership binding a process to its job root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Binding {
    /// The member's kernel identity.
    pub process: ProcessKey,
    /// The registered job root whose identity names this job.
    pub root: ProcessKey,
}

/// Bounded number of simultaneously retained job roots.
pub const MAX_JOBS: usize = 512;
/// Bounded number of observed live members.
pub const MAX_MEMBERS: usize = 16_384;

/// Why a registration could not be retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterError {
    /// A different session or label already owns this exact process.
    Conflict,
    /// The bounded job ledger is full.
    Full,
}

/// Pure ownership state mapping registered job roots and observed members.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    jobs: HashMap<ProcessKey, Job>,
    members: HashMap<ProcessKey, ProcessKey>,
}

impl Ledger {
    /// Whether no job roots remain.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Registers an exact job root idempotently.
    pub fn register(&mut self, job: Job) -> Result<(), RegisterError> {
        if let Some(old) = self.jobs.get(&job.root) {
            return if old == &job {
                Ok(())
            } else {
                Err(RegisterError::Conflict)
            };
        }
        if self.jobs.len() >= MAX_JOBS {
            return Err(RegisterError::Full);
        }
        self.jobs.insert(job.root, job);
        Ok(())
    }

    /// The retained job definitions, in stable identity order.
    #[must_use]
    pub fn jobs(&self) -> Vec<Job> {
        let mut jobs: Vec<_> = self.jobs.values().cloned().collect();
        jobs.sort_by_key(|job| job.root);
        jobs
    }

    /// Returns retained members in stable identity order.
    #[must_use]
    pub fn bindings(&self) -> Vec<Binding> {
        let mut bindings: Vec<_> = self
            .members
            .iter()
            .map(|(&process, &root)| Binding { process, root })
            .collect();
        bindings.sort_by_key(|binding| binding.process);
        bindings
    }

    /// Restores a host-validated member binding for an existing job.
    pub fn restore(&mut self, binding: Binding) -> bool {
        if !self.jobs.contains_key(&binding.root) || self.members.len() >= MAX_MEMBERS {
            return false;
        }
        match self.members.get(&binding.process) {
            Some(root) => *root == binding.root,
            None => {
                self.members.insert(binding.process, binding.root);
                true
            }
        }
    }

    /// Updates membership from one complete process sweep.
    pub fn observe(&mut self, processes: &[Process]) -> Vec<Binding> {
        self.observe_with_unavailable(processes, &HashSet::new())
    }

    /// Updates membership from `processes` while retaining `unavailable` prior keys.
    pub fn observe_with_unavailable(
        &mut self,
        processes: &[Process],
        unavailable: &HashSet<ProcessKey>,
    ) -> Vec<Binding> {
        let mut table = HashMap::new();
        let mut ambiguous = HashSet::new();
        for process in processes {
            if table.insert(process.key.pid, *process).is_some() {
                ambiguous.insert(process.key.pid);
            }
        }
        table.retain(|pid, _| !ambiguous.contains(pid));
        let live: HashSet<_> = table.values().map(|process| process.key).collect();
        self.members.retain(|key, root| {
            (live.contains(key) || unavailable.contains(key)) && self.jobs.contains_key(root)
        });
        let previous = self.members.clone();
        self.members
            .retain(|key, _| unavailable.contains(key) && !live.contains(key));
        for process in processes.iter().take(MAX_MEMBERS) {
            if self.members.len() >= MAX_MEMBERS {
                break;
            }
            if ambiguous.contains(&process.key.pid) {
                continue;
            }
            let mut at = *process;
            let mut path = HashSet::new();
            let mut root = None;
            let mut inherited = None;
            for _ in 0..64 {
                if self.jobs.contains_key(&at.key) {
                    root = Some(at.key);
                    break;
                }
                if !path.insert(at.key) {
                    break;
                }
                inherited = inherited.or_else(|| previous.get(&at.key).copied());
                let Some(parent) = table.get(&at.parent) else {
                    break;
                };
                if parent.key.start > at.key.start {
                    break;
                }
                at = *parent;
            }
            if let Some(root) = root.or(inherited) {
                self.members.insert(process.key, root);
            }
        }
        let owned: HashSet<_> = self.members.values().copied().collect();
        self.jobs.retain(|root, _| {
            live.contains(root) || owned.contains(root) || unavailable.contains(root)
        });
        self.bindings()
    }
}

#[cfg(test)]
#[path = "delegated_tests.rs"]
mod tests;
