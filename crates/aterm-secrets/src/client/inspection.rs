// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Nonsecret diagnostics from authenticated local membership and admin snapshots.

use super::{Admin, admin_keys};
use crate::{
    Result,
    age::Recipient,
    commons::bootstrap::Descriptor,
    commons::journal::Trust,
    text::{Audience, Document},
};
use std::collections::BTreeMap;

/// Capacity of the authenticated local membership history. Complete records
/// and waiting records consume the held limit; these are not application rows.
/// Neither this snapshot nor spare capacity grants membership or proves sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MembershipHistoryInspection {
    pub network: [u8; 16],
    pub complete: crate::membership_log::RecordCounts,
    /// SHA-256 of `aterm/commons/membership-record-inventory/1\0`, the little-endian
    /// u64 count, and the sorted complete record IDs. Waiting records are excluded.
    /// Equal fingerprints identify the same observed set, not current access or connectivity.
    pub complete_inventory_sha256: [u8; 32],
    pub waiting: usize,
    pub bytes: usize,
    pub budgets: crate::membership_log::RecordBudgets,
}

impl MembershipHistoryInspection {
    /// All retained records, including records waiting for a parent.
    #[must_use]
    pub const fn retained_records(&self) -> usize {
        self.complete.held().saturating_add(self.waiting)
    }

    /// Free record slots, not permission or a promise that an operation fits.
    #[must_use]
    pub const fn remaining_records(&self) -> usize {
        self.budgets.held.saturating_sub(self.retained_records())
    }
}

/// Read public counts and a record-set fingerprint from the authenticated local log, including
/// when Commons control is excluded. Requires no admin key and writes nothing.
/// A missing or unauthenticated history returns `None`, never an empty count.
///
/// # Errors
/// The local state cannot be read or authenticated, or a writer holds it until
/// `deadline`. The deadline bounds lock contention, not authenticated replay.
pub fn inspect_membership_history_until(
    paths: &super::Paths,
    deadline: std::time::Instant,
) -> Result<Option<MembershipHistoryInspection>> {
    let _lock = match super::StateLock::observe_classified(paths)? {
        super::StateObservation::Absent => return Ok(None),
        super::StateObservation::Held(lock) => lock,
        super::StateObservation::Busy => super::StateLock::observe_until(paths, deadline)?
            .ok_or_else(|| {
                crate::Error::of_kind(
                    crate::ErrorKind::VaultBusy,
                    "local membership history is busy",
                )
            })?,
    };
    let Some(config) = super::config(paths)? else {
        return Ok(None);
    };
    let Some(log) = super::log_entries::trusted_local_log(paths, &config)? else {
        return Ok(None);
    };
    let crate::membership_log::Body::Genesis { root, .. } = log.genesis().body() else {
        return Ok(None);
    };
    Ok(Some(MembershipHistoryInspection {
        network: root.network,
        complete: log.counts(),
        complete_inventory_sha256: membership_inventory(&log),
        waiting: log.waiting(),
        bytes: log.total_bytes(),
        budgets: log.budgets(),
    }))
}

/// The newest `[everyone]` epoch this Mac holds, read from the authenticated local log under a
/// shared lock, writing nothing ([`crate::commons::everyone_key`]): its id and key.
/// `None` where there is no local history, no identity, or this Mac does not hold the newest
/// epoch's key.
///
/// # Errors
/// As [`inspect_membership_history_until`], or a wrap addressed to this Mac does not open.
pub(crate) fn newest_everyone_until(
    paths: &super::Paths,
    deadline: std::time::Instant,
) -> Result<
    Option<(
        crate::membership_log::RecordId,
        zeroize::Zeroizing<[u8; 32]>,
    )>,
> {
    let _lock = match super::StateLock::observe_classified(paths)? {
        super::StateObservation::Absent => return Ok(None),
        super::StateObservation::Held(lock) => lock,
        super::StateObservation::Busy => super::StateLock::observe_until(paths, deadline)?
            .ok_or_else(|| {
                crate::Error::of_kind(
                    crate::ErrorKind::VaultBusy,
                    "local membership history is busy",
                )
            })?,
    };
    let Some(config) = super::config(paths)? else {
        return Ok(None);
    };
    let Some(log) = super::log_entries::trusted_local_log(paths, &config)? else {
        return Ok(None);
    };
    let Some(identity) = super::read_identity(paths)? else {
        return Ok(None);
    };
    newest_everyone(&log, &identity)
}

/// The lowest id among the causally newest counting `[everyone]` epochs (M6's tie-break) whose
/// key `identity` holds, with that key. No older epoch stands in: members with one log name one
/// epoch, and a Mac that lacks the newest key (a wrap gap, a removal) gets `None`.
///
/// # Errors
/// A wrap addressed to `identity` does not open.
pub(crate) fn newest_everyone(
    log: &crate::membership_log::Log,
    identity: &crate::age::Identity,
) -> Result<
    Option<(
        crate::membership_log::RecordId,
        zeroize::Zeroizing<[u8; 32]>,
    )>,
> {
    let newest = log.newest_epochs(&crate::membership_log::EpochAudience::Everyone);
    let Some(lowest) = newest.iter().min() else {
        return Ok(None);
    };
    let mut held = log.unwrap_epochs(identity)?;
    Ok(held.remove(lowest).map(|key| (*lowest, key)))
}

fn membership_inventory(log: &crate::membership_log::Log) -> [u8; 32] {
    let mut ids: Vec<_> = log.records().map(|record| record.id()).collect();
    ids.sort_unstable();
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/commons/membership-record-inventory/1\0");
    hash.update((ids.len() as u64).to_le_bytes());
    for id in ids {
        hash.update(id);
    }
    hash.finalize()
}

pub(super) struct PublishedControl {
    entry_present: bool,
    audience: Option<Audience>,
    pub(super) descriptor: Option<Descriptor>,
    pub(super) recipients: BTreeMap<Recipient, String>,
}

impl PublishedControl {
    pub(super) fn read(doc: &Document) -> Result<Self> {
        let audience = doc.commons_audience().cloned();
        let recipients = doc
            .machines()
            .into_iter()
            .filter(|machine| match &audience {
                Some(Audience::Everyone) => true,
                Some(Audience::Hosts(hosts)) => hosts.contains(&machine.name),
                None => false,
            })
            .map(|machine| {
                Recipient::parse(&machine.recipient)
                    .map(|recipient| (recipient, machine.name))
                    .map_err(|_| crate::Error::new("published control recipient is invalid"))
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            entry_present: admin_keys::has_control(doc),
            audience,
            descriptor: admin_keys::control_descriptor(doc)?,
            recipients,
        })
    }
}

/// Published control metadata, not a current roster or permission to create a
/// network. Absence cannot establish that this authority was never used before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommonsControlInspection {
    pub vault_version: u64,
    pub authority: [u8; 32],
    pub lifetime_bound: bool,
    pub entry_present: bool,
    /// Published audience labels, not a current membership or access decision.
    pub audience: Option<Audience>,
    pub descriptor: Option<Trust>,
}

impl Admin {
    /// Inspect only the authenticated published document, ignoring mutable
    /// drafts. This writes no state, prepares no keys and signs no records.
    pub fn inspect_commons_control(&self) -> Result<CommonsControlInspection> {
        Ok(CommonsControlInspection {
            vault_version: self.seq(),
            authority: self.commons_authority()?,
            lifetime_bound: self.commons_bound,
            entry_present: self.published_control.entry_present,
            audience: self.published_control.audience.clone(),
            descriptor: self
                .published_control
                .descriptor
                .as_ref()
                .map(Descriptor::trust),
        })
    }
}

#[cfg(test)]
#[path = "inspection_tests.rs"]
mod tests;
