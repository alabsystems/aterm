// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Quorum leader election, epoch-fenced replica promotion, and divergent-follower rejoin
//! (`docs/ROADMAP.md` "replicated-tier failure-injection matrix").
//!
//! Coordinates multi-node failover over [`crate::BrokerLog`] and [`crate::Broker`]:
//!
//! 1. **Majority-quorum candidate election ([`elect_highest_candidate`]):** requires a
//!    strict majority (`cluster_size / 2 + 1`) of non-diverged cluster nodes, clamping vote
//!    counts through Clean-origin [`failover_quorum_clamp`] and selecting the candidate
//!    with the highest `(generation, head_offset)`.
//! 2. **Epoch-advancing replica promotion ([`promote_replica_log`]):** advances the
//!    cluster [`LogIdentity`] generation monotonically via Clean-origin
//!    [`failover_epoch_advance`], records a [`Lineage`] step `(new_generation, head_offset)`
//!    in `<log>.id` BEFORE removing `<log>.replica` so client cursors `< head_offset`
//!    continue seamlessly via [`LogIdentity::continues`], and ensures followers that adopt
//!    the new epoch immediately fence any deposed zombie leader via
//!    [`failover_fences_stale`].
//! 3. **Divergent-tail truncation & follower rejoin ([`repair_follower_for_leader`]):**
//!    converts a deposed leader or divergent follower into a clean replica truncated back
//!    to the common committed prefix so [`crate::Broker::rejoin_followers`] catches it up.

use crate::logid::{Lineage, LogIdentity};
use crate::store::BrokerLog;
use astream_wire::Offset;
use std::io;
use std::path::Path;

#[cfg(clean_islands)]
#[path = "failover_island.rs"]
mod failover_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{failover_epoch_advance, failover_quorum_clamp};
#[cfg(clean_islands)]
pub use failover_island::{failover_epoch_advance, failover_quorum_clamp};

contract_fn! {
    /// Verified check that a replicated record's leader generation `leader_gen` is stale
    /// relative to a replica's adopted generation `adopted_gen` (`leader_gen < adopted_gen`),
    /// linked via `clean-kernel-defeq` to `failover_fences_stale_isl`.
    #[must_use]
    pub const fn failover_fences_stale(leader_gen: u64, adopted_gen: u64) -> bool
    clauses [ensures result == (leader_gen < adopted_gen)]
    {
        leader_gen < adopted_gen
    }
}

/// Durable log snapshot of one cluster node participating in a failover election.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectionCandidate {
    /// Node index within `0..cluster_size`.
    pub node_id: usize,
    /// Node's settled [`LogIdentity`] on disk.
    pub identity: LogIdentity,
    /// Node's durable `head` offset (`next`).
    pub head_offset: u64,
    /// Node's retention `base` offset.
    pub base_offset: u64,
    /// Whether the node's replica log was marked diverged during replication.
    pub diverged: bool,
}

/// Result of a majority-quorum leader election ([`elect_highest_candidate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElectionWinner {
    /// Winning node index (`0..cluster_size`).
    pub node_id: usize,
    /// Winning node's durable `head` offset.
    pub head_offset: u64,
    /// Winning node's retention `base` offset.
    pub base_offset: u64,
    /// Highest `(id, generation)` epoch observed across the quorum for the winning spine.
    pub highest_epoch: LogIdentity,
    /// Clamped count of eligible quorum votes (`<= cluster_size`).
    pub quorum_votes: u64,
    /// Required strict majority (`cluster_size / 2 + 1`).
    pub required_quorum: u64,
}

/// Outcome of promoting a replica log to cluster leader ([`promote_replica_log`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotedReplica {
    /// Promoted leader's new [`LogIdentity`] (`generation` strictly advanced when `head > 0`).
    pub identity: LogIdentity,
    /// Promoted leader's [`Lineage`] (carrying `(new_generation, head_offset)`).
    pub lineage: Lineage,
    /// Durable `head` offset at promotion time.
    pub head_offset: u64,
    /// Durable retention `base` offset at promotion time.
    pub base_offset: u64,
}

/// Select the most up-to-date leader candidate from a strict majority quorum
/// (`cluster_size / 2 + 1`) of distinct, non-diverged nodes in `0..cluster_size`.
///
/// Candidates are ranked lexicographically by `(identity.generation, head_offset)`,
/// breaking ties by lowest `node_id`.
pub fn elect_highest_candidate(
    cluster_size: usize,
    candidates: &[ElectionCandidate],
) -> Result<ElectionWinner, String> {
    if cluster_size == 0 || cluster_size > 1024 {
        return Err(format!(
            "invalid cluster_size {cluster_size}: expected 1..=1024"
        ));
    }
    let mut seen = Vec::new();
    let mut s = 0usize;
    while s < cluster_size {
        seen.push(false);
        s = s.saturating_add(1);
    }

    let mut eligible_votes = 0u64;
    let mut best: Option<ElectionCandidate> = None;

    let mut i = 0usize;
    while i < candidates.len() {
        let cand = candidates[i];
        i = i.saturating_add(1);
        if cand.node_id >= cluster_size {
            return Err(format!(
                "candidate node_id {} out of range for cluster_size {cluster_size}",
                cand.node_id
            ));
        }
        if seen.get(cand.node_id).copied().unwrap_or(true) {
            return Err(format!(
                "duplicate election candidate node_id {}",
                cand.node_id
            ));
        }
        if let Some(slot) = seen.get_mut(cand.node_id) {
            *slot = true;
        }
        if cand.diverged {
            continue;
        }
        eligible_votes = eligible_votes.saturating_add(1);
        let replace = match best {
            None => true,
            Some(cur) => {
                if cand.identity.generation != cur.identity.generation {
                    cand.identity.generation > cur.identity.generation
                } else if cand.head_offset != cur.head_offset {
                    cand.head_offset > cur.head_offset
                } else {
                    cand.node_id < cur.node_id
                }
            }
        };
        if replace {
            best = Some(cand);
        }
    }

    let cluster_u64 = cluster_size as u64;
    let quorum_votes = failover_quorum_clamp(eligible_votes, cluster_u64);
    let required_quorum = (cluster_u64 / 2).saturating_add(1);
    if quorum_votes < required_quorum {
        return Err(format!(
            "insufficient quorum: {quorum_votes} eligible node(s) reachable, need {required_quorum} of {cluster_size}"
        ));
    }

    let Some(winner) = best else {
        return Err("no eligible candidate in quorum".to_string());
    };

    // Track the highest generation across any quorum member sharing the winner's log id.
    let mut max_gen = winner.identity.generation;
    let mut j = 0usize;
    while j < candidates.len() {
        let cand = candidates[j];
        j = j.saturating_add(1);
        if !cand.diverged && cand.identity.id == winner.identity.id {
            max_gen = failover_epoch_advance(max_gen, cand.identity.generation);
        }
    }

    Ok(ElectionWinner {
        node_id: winner.node_id,
        head_offset: winner.head_offset,
        base_offset: winner.base_offset,
        highest_epoch: LogIdentity {
            id: winner.identity.id,
            generation: max_gen,
        },
        quorum_votes,
        required_quorum,
    })
}

/// Inspect a stopped node log at `log_path` under `Strict` durability and return its
/// [`ElectionCandidate`] state for [`elect_highest_candidate`].
pub fn inspect_node_log(node_id: usize, log_path: &Path) -> io::Result<ElectionCandidate> {
    let mut log = BrokerLog::open(log_path)?;
    let cand = ElectionCandidate {
        node_id,
        identity: log.identity(),
        head_offset: log.head().0,
        base_offset: log.base().0,
        diverged: log.is_diverged(),
    };
    log.close_clean()?;
    Ok(cand)
}

/// Promote a stopped replica log at `log_path` to become the new cluster leader:
///
/// - Verifies the log is currently a declared replica (`<log>.replica`) and not diverged.
/// - Advances `<log>.id` generation monotonically past `min_cluster_epoch` (and records a
///   [`Lineage`] step `(new_generation, head)` when `head > 0`), durably flushing both
///   `<log>` and `<log>.id` BEFORE removing `<log>.replica`.
/// - Closes the log cleanly so [`crate::Broker::open_replicated`] immediately serves the
///   new leader epoch.
pub fn promote_replica_log(
    log_path: &Path,
    min_cluster_epoch: Option<LogIdentity>,
) -> io::Result<PromotedReplica> {
    let mut log = BrokerLog::open(log_path)?;
    let (identity, lineage) = log.promote_replica(min_cluster_epoch)?;
    let head_offset = log.head().0;
    let base_offset = log.base().0;
    log.close_clean()?;
    Ok(PromotedReplica {
        identity,
        lineage,
        head_offset,
        base_offset,
    })
}

/// Prepare a stopped follower (or deposed ex-leader) log at `follower_log_path` to rejoin
/// under a promoted leader whose fork point is at `common_head`:
///
/// - Ensures `<log>.replica` is durably declared (`declare_replica`) so a deposed leader
///   never fires local wills on reopen.
/// - If `log.head() > common_head` (e.g. uncommitted/unquorumed split-brain tail records
///   written before failover) or if the replica was flagged diverged, truncates the tail
///   cleanly to `common_head` via [`BrokerLog::truncate_replica_to`].
///
/// Returns the number of dropped tail records.
pub fn repair_follower_for_leader(
    follower_log_path: &Path,
    common_head: Offset,
) -> io::Result<u64> {
    let mut log = BrokerLog::open(follower_log_path)?;
    if !log.is_replica() {
        log.declare_replica()?;
    }
    let cur_head = log.head();
    let dropped = if cur_head.0 > common_head.0 {
        log.truncate_replica_to(common_head)?
    } else {
        0
    };
    log.close_clean()?;
    Ok(dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8, generation: u64) -> LogIdentity {
        LogIdentity {
            id: [byte; 16],
            generation,
        }
    }

    #[test]
    fn election_requires_strict_majority_and_picks_highest_generation_then_head() {
        // 1 of 3 reachable -> refused (need 2 of 3).
        let err = elect_highest_candidate(
            3,
            &[ElectionCandidate {
                node_id: 1,
                identity: id(0xAA, 0),
                head_offset: 50,
                base_offset: 0,
                diverged: false,
            }],
        )
        .unwrap_err();
        assert!(err.contains("insufficient quorum"));

        // Diverged node does not count toward quorum!
        let err_div = elect_highest_candidate(
            3,
            &[
                ElectionCandidate {
                    node_id: 1,
                    identity: id(0xAA, 0),
                    head_offset: 50,
                    base_offset: 0,
                    diverged: false,
                },
                ElectionCandidate {
                    node_id: 2,
                    identity: id(0xAA, 0),
                    head_offset: 55,
                    base_offset: 0,
                    diverged: true,
                },
            ],
        )
        .unwrap_err();
        assert!(err_div.contains("insufficient quorum"));

        // Higher generation beats higher stale head_offset!
        let win = elect_highest_candidate(
            3,
            &[
                ElectionCandidate {
                    node_id: 0,
                    identity: id(0xAA, 0),
                    head_offset: 99,
                    base_offset: 0,
                    diverged: false,
                },
                ElectionCandidate {
                    node_id: 2,
                    identity: id(0xAA, 1),
                    head_offset: 40,
                    base_offset: 0,
                    diverged: false,
                },
            ],
        )
        .unwrap();
        assert_eq!(win.node_id, 2);
        assert_eq!(win.head_offset, 40);
        assert_eq!(win.highest_epoch, id(0xAA, 1));
        assert_eq!(win.quorum_votes, 2);
        assert_eq!(win.required_quorum, 2);
    }
}
