// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Coherence-group transactional apply (§7).

use std::collections::BTreeMap;

use crate::gate::ApplyDecision;
use crate::manifest::{Channel, Index};

/// A coherence group to apply atomically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// The coherence-group name, or `None` for an ungrouped singleton program.
    pub group: Option<String>,
    /// The member program names, sorted.
    pub members: Vec<String>,
}

/// Partition a channel's pinned programs into coherence groups (§7).
#[must_use]
pub fn plan_groups(index: &Index, channel: &Channel) -> Vec<Group> {
    plan_groups_with(index, channel, &|_| false)
}

/// [`plan_groups`] on this machine.
#[must_use]
pub fn plan_groups_here(
    layout: &crate::store::Layout,
    index: &Index,
    channel: &Channel,
    triple: &str,
) -> Vec<Group> {
    plan_groups_with(index, channel, &|program| {
        matches!(
            crate::alab::source_in(layout, index, channel, program, triple),
            crate::alab::Source::Index { .. }
        )
    })
}

/// [`plan_groups`] with `index_owns` naming the vendor-table programs the index lane owns here.
#[must_use]
pub fn plan_groups_with(
    index: &Index,
    channel: &Channel,
    index_owns: &dyn Fn(&str) -> bool,
) -> Vec<Group> {
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut singletons: Vec<String> = Vec::new();
    for program in channel.pin.keys() {
        if crate::vendor_direct::is_vendor(program) && !index_owns(program) {
            continue;
        }
        let Some(p) = index.program(program) else {
            continue;
        };
        match &p.coherence_group {
            Some(g) => grouped.entry(g.clone()).or_default().push(program.clone()),
            None => singletons.push(program.clone()),
        }
    }
    let mut out: Vec<Group> = grouped
        .into_iter()
        .map(|(g, mut members)| {
            members.sort();
            Group {
                group: Some(g),
                members,
            }
        })
        .collect();
    singletons.sort();
    out.extend(singletons.into_iter().map(|m| Group {
        group: None,
        members: vec![m],
    }));
    out
}

/// The result of applying one group transactionally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnOutcome {
    /// Every member was already on its pinned build — nothing to do.
    UpToDate,
    /// The listed members were staged and flipped to their pinned builds.
    Applied(Vec<String>),
    /// The listed members' pins are yanked/below-floor — the group is tombstoned, nothing applied.
    Tombstoned(Vec<String>),
    /// The listed members are held by a local pin.
    Pinned(Vec<String>),
    /// The group is held on its current builds because `member`'s pinned `build` publishes no
    /// artifact for `triple` — a tuple that cannot fully exist on this host is a correct state, not
    /// a failure, so nothing is staged or flipped and the next pass retries.
    Unpublished {
        member: String,
        build: u64,
        triple: String,
    },
    /// The group holds the Trust toolchain and its flip waits for quiet ([`crate::quiet`]).
    Deferred {
        members: Vec<String>,
        why: String,
        since: i64,
    },
    /// A member failed; the whole group was aborted.
    Aborted {
        failed: String,
        during_flip: bool,
        /// The failed stage's or flip's own cause.
        why: String,
        /// The members that had already flipped when `failed`'s flip failed, in flip order,
        /// each with what its rollback answered: `Err` names what it could not put back.
        /// Empty for a stage failure, and for a flip failure of the first member.
        rolled_back: Vec<(String, Result<(), String>)>,
    },
}

/// Run one coherence group as an all-or-nothing transaction over injected actions.
///
/// A stage or flip that fails says why, and the group's [`TxnOutcome::Aborted`] carries that
/// cause; after a failed flip it also carries each member already flipped with its rollback's
/// own answer, so a rollback that could not put a member back is never reported as done.
pub fn transact(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    flip: &mut dyn FnMut(&str) -> Result<(), String>,
    rollback: &mut dyn FnMut(&str) -> Result<(), String>,
) -> TxnOutcome {
    transact_holding(decisions, stage, &mut || None, flip, rollback)
}

/// [`transact`] with a hold asked once, after every member staged and before the first flip.
pub fn transact_holding(
    decisions: &[(String, ApplyDecision)],
    stage: &mut dyn FnMut(&str) -> Result<(), String>,
    hold: &mut dyn FnMut() -> Option<(String, i64)>,
    flip: &mut dyn FnMut(&str) -> Result<(), String>,
    rollback: &mut dyn FnMut(&str) -> Result<(), String>,
) -> TxnOutcome {
    let tombstoned: Vec<String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Tombstone)
        .map(|(n, _)| n.clone())
        .collect();
    if !tombstoned.is_empty() {
        return TxnOutcome::Tombstoned(tombstoned);
    }

    let installs: Vec<&String> = decisions
        .iter()
        .filter(|(_, d)| *d == ApplyDecision::Install)
        .map(|(n, _)| n)
        .collect();
    if installs.is_empty() {
        return TxnOutcome::UpToDate;
    }

    for name in &installs {
        if let Err(why) = stage(name) {
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: false,
                why,
                rolled_back: Vec::new(),
            };
        }
    }

    if let Some((why, since)) = hold() {
        return TxnOutcome::Deferred {
            members: installs.into_iter().cloned().collect(),
            why,
            since,
        };
    }

    let mut flipped: Vec<&str> = Vec::new();
    for name in &installs {
        if let Err(why) = flip(name) {
            let rolled_back = flipped
                .iter()
                .map(|done| ((*done).to_string(), rollback(done)))
                .collect();
            return TxnOutcome::Aborted {
                failed: (*name).clone(),
                during_flip: true,
                why,
                rolled_back,
            };
        }
        flipped.push(name);
    }
    TxnOutcome::Applied(installs.into_iter().cloned().collect())
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
