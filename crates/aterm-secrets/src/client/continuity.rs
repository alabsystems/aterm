// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A Mac that comes back with a new vault key keeps its seat and its label by its proof alone
//! (owner, 2026-11-02: the owner's labels drifting is a defect;
//! `docs/DESIGN-automatic-lan-admission-2026-11-02.md` "Seat continuity").
//!
//! The only proof that a Mac is a seat's Mac is re-admission by proof ([`super::readmit`]): the
//! Mac proves, on a fresh challenge, the fleet node key the membership log reads as that seat's,
//! and a member removes the old seat and seats the new key under its label. A new Mac proves
//! itself to a member before it asks anyone (`crate::members::join`), so a Mac back on the home
//! ALab LAN takes its seat back by that proof and no plain ask gets ahead of it. A host name is
//! not a proof: anyone can ask under any name. So a name decides nothing here, it only routes:
//!
//! - **A new seat under another seat's label is on the record.** Where an admin's approver lets
//!   a Mac in (a member's Allow, a join password) under the `m<N>` label of a standing seat
//!   ([`seat_named`]), it is let in under the next free label, and the approver's log says so.
//!   `aterm members list` names the pair ([`splits`]), read from the log on demand: no entry,
//!   alert or step asks a person to act on it, since a name proves nothing about which of the
//!   two seats is gone (a second account on one Mac asks under that Mac's name too, and a
//!   stranger can ask under any).
//!
//! **Seats split already.** Two standing seats the log binds one node key to by authority are one
//! Mac (or a copy of its disk) seated twice. No fold the 0.147 and 0.149 readers take is sound:
//!
//! - A plain `Remove` of the old seat signed after the newer seat's `Admit` is no move: the
//!   evaluator supersedes a removed seat by its node key only for an `Admit` after the `Remove`.
//!   So the shared node key reads as removed, and the newer seat goes out with the old one (and a
//!   0.147 reader supersedes by no node key at all).
//! - The fold that would keep the old label (remove both seats, then admit the newer key again
//!   under the old label) is one a member would sign on its own. Members that fold on their own
//!   race, and two members' `Remove`s of one seat at one cutoff are a quorum `Remove`, which no
//!   Allow undoes.
//!
//! So [`splits`] lists the pair with what the log proves ([`SplitBy`]), and nothing is signed.

use super::*;
use crate::membership_log::{EpochAudience, Log, Member, MemberId};
use crate::swarm_roster::leading_m_label;
use std::collections::BTreeSet;

/// Whether the log's member `actor` stands: let in, and neither removed, held out by its node
/// key, nor left out of the newest `[everyone]` epoch on purpose.
fn stands(log: &Log, actor: &MemberId, member: &Member) -> bool {
    let recipient = member.binding.recipient();
    !log.recipient_is_removed(&recipient)
        && !log.membership().member_node_removed(member)
        && !log.audience_left_out(&EpochAudience::Everyone, actor)
        && !log.audience_left_out_recipient(&EpochAudience::Everyone, &recipient)
}

/// The label of the standing seat whose label is `host`'s leading `m<N>` (`m110` names seat
/// `m110`; any other host names none): the label the owner gives a Mac by its name. `None` where
/// no standing seat holds that label.
pub(super) fn seat_named(log: &Log, host: &str) -> Option<String> {
    let (_, wanted) = leading_m_label(host)?;
    log.membership()
        .members()
        .iter()
        .find(|(actor, member)| member.label == wanted && stands(log, actor, member))
        .map(|(_, member)| member.label.clone())
}

/// What ties a newer seat to an older one ([`Split`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SplitBy {
    /// The log binds one node key to both seats by authority (each seat's genesis listing, own
    /// `Node` record, or move by proof): one Mac, or a copy of its disk, holds both. A `Remove`
    /// of either reads that node key as removed and so takes the other seat out with it.
    Node,
    /// The newer seat asked under the older seat's `m<N>` label, and nothing in the log ties the
    /// two: a stranger, a second account on that Mac, or that Mac back with a new key.
    Name,
}

/// Two standing seats of one Mac, or of a Mac that asked under another seat's label: a fact read
/// from the log ([`splits`]).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Split {
    /// The newer seat's label (`m160`).
    pub label: String,
    /// The older seat's label (`m110`).
    pub of: String,
    /// What ties them.
    pub by: SplitBy,
}

impl Split {
    /// The line `aterm members list` prints for the pair.
    #[must_use]
    pub fn line(&self) -> String {
        match self.by {
            SplitBy::Node => format!(
                "split {} of={} by=node: one Mac holds both seats' node key, and removing either \
                 seat takes the other out",
                self.label, self.of
            ),
            SplitBy::Name => format!(
                "split {} of={} by=name: {} asked as {}, whose seat stands, and nothing proves one \
                 Mac holds both",
                self.label, self.of, self.label, self.of
            ),
        }
    }
}

/// Every pair of standing seats [`SplitBy`] ties in `log`, newer seat first, by its label; a pair
/// the node key ties is not listed again by name. Read from the log alone, so every member lists
/// the same pairs.
#[must_use]
pub fn splits(log: &Log) -> Vec<Split> {
    let membership = log.membership();
    let standing: Vec<(&MemberId, &Member)> = membership
        .members()
        .iter()
        .filter(|(actor, member)| stands(log, actor, member))
        .collect();
    let index = |member: &Member| log.record_index(&member.entry).unwrap_or(0);
    let mut out = BTreeSet::new();
    for &(actor, member) in &standing {
        let by_node = member
            .node_key
            .filter(|node| *node != [0; 32])
            .and_then(|node| {
                standing
                    .iter()
                    .filter(|(other, held)| {
                        *other != actor
                            && held.node_key == Some(node)
                            && index(held) < index(member)
                            && super::readmit::bound_by_authority(log, other, &node)
                            && super::readmit::bound_by_authority(log, actor, &node)
                    })
                    .min_by_key(|(_, held)| index(held))
            });
        if let Some((_, older)) = by_node {
            out.insert(Split {
                label: member.label.clone(),
                of: older.label.clone(),
                by: SplitBy::Node,
            });
            continue;
        }
        let Some((_, asked)) = leading_m_label(&member.name) else {
            continue;
        };
        if asked == member.label {
            continue;
        }
        if let Some((_, older)) = standing
            .iter()
            .find(|(other, held)| *other != actor && held.label == asked)
        {
            out.insert(Split {
                label: member.label.clone(),
                of: older.label.clone(),
                by: SplitBy::Name,
            });
        }
    }
    out.into_iter().collect()
}

/// The membership log this Mac's vault pin trusts, where it holds one and can read it.
pub(super) fn trusted_log(paths: &Paths) -> Option<Log> {
    super::remove::trusted_store(paths)
        .ok()
        .flatten()
        .and_then(|store| store.log().cloned())
}

/// [`splits`] of the membership log this Mac's vault pin trusts; none where it holds no such
/// log.
///
/// # Errors
/// This Mac's vault configuration or records cannot be read.
pub fn seat_splits(paths: &Paths) -> crate::Result<Vec<Split>> {
    let Some(store) = super::remove::trusted_store(paths)? else {
        return Ok(Vec::new());
    };
    Ok(store.log().map(splits).unwrap_or_default())
}

#[cfg(test)]
#[path = "continuity_tests.rs"]
mod tests;
