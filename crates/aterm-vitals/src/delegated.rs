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
    /// The current parent PID; it is resolved against this same sweep.
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
mod tests {
    use super::*;

    fn p(pid: u32, start: u64, parent: u32) -> Process {
        Process {
            key: ProcessKey { pid, start },
            parent,
        }
    }

    fn job(root: ProcessKey, session: &str) -> Job {
        Job {
            root,
            session: session.into(),
            ai: "codex".into(),
            label: "build".into(),
        }
    }

    #[test]
    fn two_jobs_under_one_daemon_never_claim_the_daemon_or_siblings() {
        let mut ledger = Ledger::default();
        ledger.register(job(p(20, 2, 10).key, "a")).unwrap();
        ledger.register(job(p(30, 3, 10).key, "b")).unwrap();
        let bindings = ledger.observe(&[
            p(10, 1, 1),
            p(20, 2, 10),
            p(21, 4, 20),
            p(30, 3, 10),
            p(31, 5, 30),
            p(40, 6, 10),
        ]);
        assert_eq!(bindings.len(), 4);
        assert_eq!(bindings.iter().filter(|b| b.root.pid == 20).count(), 2);
        assert_eq!(bindings.iter().filter(|b| b.root.pid == 30).count(), 2);
    }

    #[test]
    fn detached_members_survive_parent_exit_but_not_pid_reuse() {
        let mut ledger = Ledger::default();
        let root = p(20, 2, 10).key;
        ledger.register(job(root, "a")).unwrap();
        ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
        assert_eq!(
            ledger.observe(&[p(21, 3, 1)]),
            vec![Binding {
                process: p(21, 3, 1).key,
                root
            }]
        );
        assert!(ledger.observe(&[p(21, 9, 1)]).is_empty());
        assert!(ledger.jobs().is_empty());
    }

    #[test]
    fn unreadable_prior_keys_stay_dormant_and_cannot_bridge_new_ancestry() {
        let mut ledger = Ledger::default();
        let root = p(20, 2, 10).key;
        let member = p(21, 3, 20).key;
        ledger.register(job(root, "a")).unwrap();
        ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
        let unavailable = HashSet::from([member, p(99, 9, 1).key]);
        let retained = vec![Binding {
            process: member,
            root,
        }];
        assert_eq!(
            ledger.observe_with_unavailable(&[p(22, 4, 21)], &unavailable),
            retained
        );
        assert_eq!(ledger.jobs().len(), 1);
        assert_eq!(
            ledger.observe_with_unavailable(&[p(21, 9, 1), p(23, 10, 21)], &unavailable),
            retained
        );
        assert_eq!(ledger.observe(&[p(21, 3, 1), p(22, 4, 21)]).len(), 2);
        assert!(ledger.observe(&[p(21, 9, 1), p(23, 10, 21)]).is_empty());
        assert!(ledger.is_empty());
    }

    #[test]
    fn an_unreadable_registered_root_is_retained_without_inventing_members() {
        let mut ledger = Ledger::default();
        let root = p(20, 2, 10).key;
        ledger.register(job(root, "a")).unwrap();
        assert!(
            ledger
                .observe_with_unavailable(&[p(21, 3, 20)], &HashSet::from([root]))
                .is_empty()
        );
        assert_eq!(ledger.jobs().len(), 1);
        assert_eq!(ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]).len(), 2);
        assert!(ledger.observe(&[]).is_empty());
        assert!(ledger.is_empty());
    }

    #[test]
    fn children_of_an_observed_detached_member_inherit_its_job() {
        let mut ledger = Ledger::default();
        let root = p(20, 2, 10).key;
        ledger.register(job(root, "a")).unwrap();
        ledger.observe(&[p(20, 2, 10), p(21, 3, 20)]);
        let bindings = ledger.observe(&[p(21, 3, 1), p(22, 4, 21)]);
        assert_eq!(bindings.len(), 2);
        assert!(bindings.iter().all(|binding| binding.root == root));
    }

    #[test]
    fn nested_registration_wins_once_and_newer_parent_never_splices_a_chain() {
        let mut ledger = Ledger::default();
        let outer = p(20, 2, 1).key;
        let inner = p(21, 3, 20).key;
        ledger.register(job(outer, "a")).unwrap();
        ledger.observe(&[p(20, 2, 1), p(21, 3, 20), p(22, 4, 21)]);
        ledger.register(job(inner, "b")).unwrap();
        let bindings = ledger.observe(&[p(20, 2, 1), p(21, 3, 20), p(22, 4, 21), p(23, 1, 20)]);
        assert_eq!(
            bindings.iter().find(|b| b.process.pid == 22).unwrap().root,
            inner
        );
        assert!(!bindings.iter().any(|b| b.process.pid == 23));
        assert_eq!(
            ledger.register(job(inner, "c")),
            Err(RegisterError::Conflict)
        );
    }

    #[test]
    fn carry_requires_a_registered_job_and_exact_live_member() {
        let root = p(20, 2, 1).key;
        let member = Binding {
            process: p(21, 3, 1).key,
            root,
        };
        let mut restored = Ledger::default();
        assert!(!restored.restore(member));
        restored.register(job(root, "a")).unwrap();
        assert!(restored.restore(member));
        assert_eq!(restored.observe(&[p(21, 3, 1)]), vec![member]);
        assert!(restored.observe(&[p(21, 4, 1)]).is_empty());
    }
}
