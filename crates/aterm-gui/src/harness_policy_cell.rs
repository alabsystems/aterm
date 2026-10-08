// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
//! The supervisor's running policy, in a type of its own.
//!
//! The policy the window runs changes in exactly one place,
//! [`HarnessPolicyCell::offer`], and `offer` cannot be called without a
//! [`StampedHarnessPolicy`]. A stamp is made only by
//! [`HarnessStampSource::stamp`], and the one source is the config watcher's
//! (`config_watcher::spawn`): each read of `aterm.toml` it posts is stamped
//! with the next generation. So "the watcher is the policy's one writer"
//! (design `docs/DESIGN-trust-temporal-models-2026-09-29.md` §1, §3a) is what
//! the types say, and `offer` is a join: a stamp no newer than the one in
//! force changes nothing, wherever its wake lands.
//!
//! The fields are private and nothing here is generic: every fn that can
//! change a field or build one of these is in this file.

use crate::app_config::HarnessPolicy;

/// Mints the generation of each policy the watcher posts: strictly
/// increasing, one source per watcher thread.
#[derive(Debug)]
pub(crate) struct HarnessStampSource {
    last: u64,
}

/// A policy read from disk, with the generation its source gave it. Not
/// `Clone`: a stamp is offered once.
#[derive(Debug)]
pub(crate) struct StampedHarnessPolicy {
    generation: u64,
    policy: HarnessPolicy,
}

/// The policy in force, and the generation it came with (`0`: the launch
/// read, before any stamp).
#[derive(Debug)]
pub(crate) struct HarnessPolicyCell {
    in_force: u64,
    policy: HarnessPolicy,
}

impl HarnessStampSource {
    pub(crate) fn new() -> Self {
        HarnessStampSource { last: 0 }
    }

    /// `policy`, stamped with the next generation.
    pub(crate) fn stamp(&mut self, policy: HarnessPolicy) -> StampedHarnessPolicy {
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
    /// The cell at launch: `launch` is the policy the launch read of
    /// `aterm.toml` wrote, in force until the watcher's first post.
    pub(crate) fn new(launch: HarnessPolicy) -> Self {
        HarnessPolicyCell {
            in_force: 0,
            policy: launch,
        }
    }

    /// Adopt `stamped` when it is newer than what is in force: the join.
    /// Whether it was adopted.
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
    pub(crate) fn get(&self) -> &HarnessPolicy {
        &self.policy
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

    /// The join: the newest stamp wins whatever order the two arrive in, and a
    /// stamp offered after a newer one changes nothing.
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
