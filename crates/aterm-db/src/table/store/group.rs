// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent permission to recover one exact checkpoint group. The journal's
//! own checkpoint cannot grant a larger unwitnessed tail.

use super::{CLAIM_LEN, WITNESS_AT, Witness};
use crate::{Error, Result};
use aterm_uds::private_file::{self, PrivateFileError};
use std::path::Path;

const MAGIC: &[u8; 5] = b"ATBG1";
const DOMAIN: &[u8] = b"aterm/commons/table-group/1\0";
const LEN: usize = 5 + 32 + 40 + 40;

pub(super) fn name(claim_name: &str) -> String {
    format!("{claim_name}.group")
}

/// A verified independent receipt prefix and, optionally, its exact next group.
/// Observers still expose only `acknowledged`; the group is a writer recovery grant.
#[derive(Clone, Copy)]
pub(super) struct Boundary {
    acknowledged: Witness,
    end: Option<Witness>,
}

impl Boundary {
    pub(super) const fn acknowledged(self) -> Witness {
        self.acknowledged
    }
    pub(super) const fn end(self) -> Option<Witness> {
        self.end
    }
}

impl From<Witness> for Boundary {
    fn from(acknowledged: Witness) -> Self {
        Self {
            acknowledged,
            end: None,
        }
    }
}

fn binding(claim: &[u8]) -> Result<[u8; 32]> {
    if claim.len() != CLAIM_LEN || claim[4] != b'1' {
        return Err(Error::new("group permission requires ready table custody"));
    }
    let mut hash = aterm_digest::Sha256::new();
    hash.update(DOMAIN);
    hash.update(&claim[..WITNESS_AT]);
    Ok(hash.finalize())
}

fn endpoints(before: Witness, end: Witness) -> Result<()> {
    if end.count <= before.count
        || end.count - before.count > aterm_journal::MAX_APPEND_RECORDS as u64
    {
        return Err(Error::new("invalid table checkpoint group boundary"));
    }
    Ok(())
}

/// Publish before any group record. A full flush of the temporary covers the
/// preceding Ordered custody witness before rename replaces the preceding permit,
/// even when custody and data live on different devices. The following directory
/// flush makes the new permission durable before any journal publication.
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "JournalGroupCustody",
        action = "NextGroup",
        project = "aterm_db::table::store::batch_conformance_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "JournalGroupCustody",
        action = "Prepare",
        project = "aterm_db::table::store::batch_conformance_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "JournalGroupCustody",
        action = "FlushTemporary",
        project = "aterm_db::table::store::batch_conformance_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "JournalGroupCustody",
        action = "RenamePermit",
        project = "aterm_db::table::store::batch_conformance_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "JournalGroupCustody",
        action = "FlushPermitDirectory",
        project = "aterm_db::table::store::batch_conformance_tests::transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "Latch",
        reason = "The journal's device or a refused barrier serves it Strict"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "Apart",
        reason = "Custody lives on another device from the journal"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "Crash",
        reason = "Power loss chooses which unflushed writes survive"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "DropCheckpoint",
        reason = "Power loss takes an unflushed checkpoint behind surviving witness barriers"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "DropWitness",
        reason = "Power loss takes a volatile witness without crossing a surviving entry's prior-witness floor"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "DropPermit",
        reason = "Power loss takes an unflushed rename and retains the preceding permit entry"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "JournalGroupCustody",
        action = "DropPermitData",
        reason = "Power loss takes unflushed temporary-file bytes even when the new directory entry survived"
    )
)]
pub(super) fn prepare(
    dir: &Path,
    claim_name: &str,
    claim: &[u8],
    before: Witness,
    end: Witness,
) -> Result<()> {
    let binding = binding(claim)?;
    endpoints(before, end)?;
    if Witness::decode(&claim[WITNESS_AT..])? != before {
        return Err(Error::new("group permission differs from table custody"));
    }
    #[cfg(test)]
    super::batch_conformance_tests::starting_permission(before, end);
    let mut bytes = Vec::with_capacity(LEN);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&binding);
    bytes.extend_from_slice(&before.bytes());
    bytes.extend_from_slice(&end.bytes());
    let publish: fn(&Path, &str, &[u8]) -> std::io::Result<()> =
        aterm_journal::write_private_durable;
    #[cfg(test)]
    let publish = super::batch_conformance_tests::permission_writer(publish);
    publish(dir, &name(claim_name), &bytes)?;
    #[cfg(test)]
    super::batch_conformance_tests::permission_written(dir, claim_name, claim, before, end);
    Ok(())
}

/// Read after a lock-free journal read, or while holding its exclusive writer
/// lock. A new checkpoint therefore precedes this read of its durable permit.
/// Recheck the claim to reject a permit from a later group under an older snapshot.
pub(super) fn read(dir: &Path, claim_name: &str, claim: &[u8]) -> Result<Boundary> {
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("invalid table custody for group check"));
    }
    let raw = private_file::read_private(dir, &name(claim_name), LEN);
    match private_file::read_private(dir, claim_name, CLAIM_LEN) {
        Ok(current) if current == claim => {}
        Ok(_) | Err(PrivateFileError::Missing) => return Err(Error::snapshot_changed()),
        Err(_) => return Err(Error::new("table custody unavailable during group check")),
    }
    let acknowledged = Witness::decode(&claim[WITNESS_AT..])?;
    let bytes = match raw {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(acknowledged.into()),
        Err(_) => return Err(Error::new("table group permission is unreadable")),
    };
    if bytes.len() != LEN || &bytes[..5] != MAGIC || bytes[5..37] != binding(claim)? {
        return Err(Error::new(
            "table group permission has another custody identity",
        ));
    }
    let before = Witness::decode(&bytes[37..77])?;
    let end = Witness::decode(&bytes[77..])?;
    endpoints(before, end)?;
    if end.count <= acknowledged.count {
        if end.count == acknowledged.count && end != acknowledged {
            return Err(Error::new(
                "completed table group differs from custody witness",
            ));
        }
        return Ok(acknowledged.into());
    }
    if before != acknowledged {
        return Err(Error::new(
            "pending table group differs from custody witness",
        ));
    }
    Ok(Boundary {
        acknowledged,
        end: Some(end),
    })
}
