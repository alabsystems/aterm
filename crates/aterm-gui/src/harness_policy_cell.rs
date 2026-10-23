// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
//! The supervisor's running policy, in a type of its own.

use crate::app_config::HarnessPolicy;

#[cfg(all(clean_islands, trust_seals))]
#[path = "harness_policy_cell_island.rs"]
mod island;

/// Mints the generation of each policy the watcher posts.
#[derive(Debug)]
pub(crate) struct HarnessStampSource {
    last: u64,
}

/// A policy read from disk, with the generation its source gave it.
#[derive(Debug)]
pub(crate) struct StampedHarnessPolicy {
    generation: u64,
    policy: HarnessPolicy,
}

/// The policy in force, and the generation it came with (`0`: the launch read, before any stamp).
#[derive(Debug)]
pub(crate) struct HarnessPolicyCell {
    in_force: u64,
    policy: HarnessPolicy,
}

impl HarnessStampSource {
    pub(crate) const fn new() -> Self {
        HarnessStampSource { last: 0 }
    }

    /// `policy`, stamped with the next generation.
    pub(crate) const fn stamp(&mut self, policy: HarnessPolicy) -> StampedHarnessPolicy {
        self.last += 1;
        StampedHarnessPolicy {
            generation: self.last,
            policy,
        }
    }
}

#[cfg(test)]
impl StampedHarnessPolicy {
    /// The policy the stamp carries (the conformance drive projects it).
    pub(crate) fn policy(&self) -> &HarnessPolicy {
        &self.policy
    }
}

impl HarnessPolicyCell {
    /// Create the cell with `launch` policy in force until the watcher's first post.
    pub(crate) const fn new(launch: HarnessPolicy) -> Self {
        HarnessPolicyCell {
            in_force: 0,
            policy: launch,
        }
    }

    /// Adopt `stamped` when it is newer than what is in force: the join.
    pub(crate) fn offer(&mut self, stamped: StampedHarnessPolicy) -> bool {
        if self.in_force < stamped.generation {
            self.in_force = stamped.generation;
            self.policy = stamped.policy;
            true
        } else {
            false
        }
    }

    /// The policy in force.
    pub(crate) const fn get(&self) -> &HarnessPolicy {
        &self.policy
    }

    /// The generation of the policy in force (`0`: the launch read).
    pub(crate) const fn generation(&self) -> u64 {
        self.in_force
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(enabled: bool) -> HarnessPolicy {
        let text = if enabled {
            "[harness]\napprove = \"all\"\n"
        } else {
            "[harness]\nenabled = false\n"
        };
        HarnessPolicy::read(text)
    }

    #[test]
    fn the_newest_stamp_wins_in_either_order() {
        for older_first in [true, false] {
            let mut source = HarnessStampSource::new();
            let older = source.stamp(policy(true));
            let newer = source.stamp(policy(false));
            let mut cell = HarnessPolicyCell::new(policy(true));
            let (first, second) = if older_first {
                (older, newer)
            } else {
                (newer, older)
            };
            assert!(cell.offer(first));
            assert_eq!(cell.offer(second), older_first, "a stale stamp is refused");
            assert!(!cell.get().policy.enabled, "the newer read is in force");
        }
    }
}
