// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Read-only evidence for explicit recovery. An intact receipt prefix does not
//! establish the directory's original location, authenticate application records,
//! authorize a writer or make its resource rights available again.

use super::{
    ANCHOR_LEN, CLAIM_LEN, Capacity, WITNESS_AT, Witness, claim_dir, directory, replay,
    validate_claim,
};
use crate::{
    Error, Result,
    catalog::Directory,
    table::{Scope, Session},
};
use astream_wire::sync::Digest;
use aterm_json::{Value, json};
use aterm_uds::private_file::{self, PrivateFileError};
use std::path::PathBuf;

/// A bounded local custody inspection, containing no application values or keys.
#[derive(Debug)]
pub struct Inspection {
    /// The requested digest, independently checked against any retained claim.
    pub table: Digest,
    /// Whether independent enrollment custody is absent, unfinished or ready.
    pub claim: Claim,
    /// Ready custody's directory comparison, even when the original binding fails.
    pub directory: Option<DirectoryEvidence>,
    /// Ready custody's independently committed receipt prefix. This is not a
    /// signed-operation or application-policy verification.
    pub prefix: Option<std::result::Result<PrefixEvidence, String>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Claim {
    Missing,
    Pending,
    Ready,
}

/// Original and current filesystem metadata. None of it is a recovery grant.
#[derive(Debug)]
pub struct DirectoryEvidence {
    pub recorded_device: u64,
    pub recorded_inode: u64,
    pub observed: aterm_uds::directory_identity::Identity,
    pub volume_witness_present: bool,
    /// The ordinary store's unchanged guard, not a relaxed recovery check.
    pub binding_error: Option<String>,
}

#[derive(Debug)]
pub struct PrefixEvidence {
    /// Receipts independently acknowledged outside the data directory.
    pub acknowledged_records: u64,
    /// Their original custody-side commitment, not an operation inventory hash.
    pub custody_commitment: Digest,
    /// Records covered by the data journal's durable checkpoint. At most one
    /// may await its custody-side acknowledgment; this inspection never adopts it.
    pub committed_records: usize,
}

impl Inspection {
    /// Metadata-only command representation. Successful inspection grants no
    /// permission to resume writing, including when both checks succeeded.
    #[must_use]
    pub fn json(&self) -> Value {
        let directory = self.directory.as_ref().map(|evidence| json!({
            "recorded_device": evidence.recorded_device,
            "recorded_inode": evidence.recorded_inode,
            "observed_device": evidence.observed.dev,
            "observed_inode": evidence.observed.ino,
            "persistent_volume_uuid": evidence.observed.volume.map(|v| aterm_codec::hex::encode_lower(&v)),
            "volume_witness_present": evidence.volume_witness_present,
            "binding_matches": evidence.binding_error.is_none(),
            "binding_error": evidence.binding_error,
        }));
        let prefix = self.prefix.as_ref().map(|result| match result {
            Ok(evidence) => json!({
                "state": "verified",
                "acknowledged_records": evidence.acknowledged_records,
                "custody_commitment": aterm_codec::hex::encode_lower(&evidence.custody_commitment),
                "committed_records": evidence.committed_records,
                "unwitnessed_records": (evidence.committed_records as u64).checked_sub(evidence.acknowledged_records),
            }),
            Err(error) => json!({"state": "refused", "error": error}),
        });
        json!({
            "kind": "commons-custody", "scope": "local-recovery-evidence",
            "table": aterm_codec::hex::encode_lower(&self.table),
            "claim": match self.claim { Claim::Missing => "missing", Claim::Pending => "pending", Claim::Ready => "ready" },
            "directory": directory, "receipt_prefix": prefix,
            "application_authenticated": false, "can_resume_writer": false,
        })
    }
}

/// Inspect existing, deterministic storage without opening a writer, enrolling
/// a table, changing a claim, backfilling a volume witness or recovering a tail.
/// This path needs no readable catalog: its target's claim must itself bind the
/// requested digest, exact network and this session's writer incarnation.
///
/// # Errors
/// Membership is stale; a retained claim is unreadable, malformed or foreign;
/// directory metadata cannot be read; or custody changes during the observation.
/// A refused receipt-prefix check is retained separately in the returned report.
pub fn inspect(session: &Session<'_>, table: Digest) -> Result<Inspection> {
    inspect_with(session, table, || {})
}

/// Check retained directory custody without reading its data journal. This is
/// only a recovery trigger: a displaced table still needs a verified prefix
/// before succession, and missing or pending claims establish no displacement.
///
/// # Errors
/// Membership is stale, retained custody is malformed or unreadable, or the
/// directory binding changes during observation.
pub(crate) fn directory_binding_lost(session: &Session<'_>, table: Digest) -> Result<bool> {
    Ok(inspect_parts(session, table, false, || {})?
        .directory
        .is_some_and(|evidence| evidence.binding_error.is_some()))
}

/// An independently witnessed prefix for import into a different, normally
/// admitted writer. This carries no authority to reuse the source actor.
#[cfg(any(test, debug_assertions))]
pub(crate) struct Salvage {
    pub scope: Scope,
    pub entries: Vec<(bool, Vec<u8>)>,
}

/// Read a retained primary prefix despite a lost directory binding. The claim
/// still has to name this exact old actor and scope. Every operation is verified
/// under current historical authority before any bytes leave this function.
#[cfg(test)]
pub(crate) fn salvage(
    session: &Session<'_>,
    verifier: &Session<'_>,
    table: Digest,
) -> Result<Salvage> {
    salvage_region(session, verifier, table, super::Region::Primary)
}

#[cfg(any(test, debug_assertions))]
pub(crate) fn salvage_region(
    session: &Session<'_>,
    verifier: &Session<'_>,
    table: Digest,
    region: super::Region,
) -> Result<Salvage> {
    let prefix = witnessed_prefix(session, table, region)?;
    verifier.context(prefix.scope)?;
    for entry in &prefix.entries {
        if entry.conflict {
            super::super::conflict::Conflict::restore(verifier, prefix.scope, entry.wire.wire())?;
        } else {
            verifier.authenticate(prefix.scope, entry.wire.wire())?;
        }
    }
    prefix.recheck(session)?;
    Ok(Salvage {
        scope: prefix.scope,
        entries: prefix
            .entries
            .into_iter()
            .map(|entry| (entry.conflict, entry.wire.into_wire()))
            .collect(),
    })
}

/// Independently witnessed ciphertext, before application authentication. The
/// private constructor checks the whole receipt commitment. Import authenticates
/// each missing record at its normal store entry point and keeps durable progress.
pub(crate) struct WitnessedPrefix {
    scope: Scope,
    entries: Vec<replay::Entry>,
    claims: PathBuf,
    name: String,
    claim: Vec<u8>,
    directory: PathBuf,
    identity: aterm_uds::directory_identity::Identity,
}

impl WitnessedPrefix {
    pub(crate) const fn scope(&self) -> Scope {
        self.scope
    }

    pub(crate) fn import(
        &self,
        source: &Session<'_>,
        session: &Session<'_>,
        store: &mut super::Store,
        current: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        if store.scope() != self.scope {
            return Err(Error::new("preserved prefix has another table scope"));
        }
        current()?;
        store.check(session)?;
        // A reopened store verified these exact bytes under this session. An
        // opaque quarantine entry is insufficient: it must authenticate again.
        let conflicts: std::collections::BTreeMap<_, _> = store
            .conflicts
            .values()
            .map(|stored| (stored.receipt.id, stored.conflict.wire()))
            .collect();
        let mut missing = Vec::new();
        for entry in &self.entries {
            current()?;
            let retained = if entry.conflict {
                conflicts.get(&entry.receipt.id).copied() == Some(entry.wire.wire())
            } else {
                store.records.get(&entry.receipt.id).is_some_and(|stored| {
                    stored.quarantine.is_none() && stored.wire == entry.wire.wire()
                })
            };
            if !retained {
                missing.push(entry);
            }
        }
        drop(conflicts);
        // Positive evidence precedes certificates, as in ordinary salvage.
        for conflict in [false, true] {
            for entry in &missing {
                if entry.conflict == conflict {
                    current()?;
                    if conflict {
                        store.ingest_conflict(session, entry.wire.wire())?;
                    } else {
                        store.ingest(session, entry.wire.wire())?;
                    }
                }
            }
        }
        current()?;
        store.check(session)?;
        self.recheck(source)
    }

    fn recheck(&self, session: &Session<'_>) -> Result<()> {
        session.context(self.scope)?;
        if private_file::read_private(&self.claims, &self.name, CLAIM_LEN)
            .ok()
            .as_deref()
            != Some(self.claim.as_slice())
            || directory::read(&self.directory)? != self.identity
        {
            return Err(Error::snapshot_changed());
        }
        Ok(())
    }
}

pub(crate) fn witnessed_prefix(
    session: &Session<'_>,
    table: Digest,
    region: super::Region,
) -> Result<WitnessedPrefix> {
    session.context(crate::catalog::scope(session.bootstrap.trust()))?;
    let claims = region.claims(session)?;
    let name = super::claim_name_for(&table);
    let claim = private_file::read_private(&claims, &name, CLAIM_LEN)
        .map_err(|_| Error::new("writer succession claim is unavailable"))?;
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("writer succession claim has an invalid length"));
    }
    let scope = Scope::new(
        session.bootstrap.trust(),
        claim[85..101].try_into().expect("validated table UUID"),
        u64::from_le_bytes(claim[101..109].try_into().expect("validated generation")),
    )?;
    if scope.digest() != table || super::validate_claim_region(session, scope, &claim, region)? {
        return Err(Error::new("writer succession claim changed"));
    }
    let directory = Directory::new(session.bootstrap.paths(), scope.trust());
    let directory = if region == super::Region::Recovery {
        directory.recovery()
    } else {
        directory
    };
    let dir = directory.path(scope)?;
    let identity = directory::read(&dir)?;
    let records = match aterm_journal::read_committed(
        &dir,
        "table",
        &claim[..ANCHOR_LEN],
        Capacity::default().journal(),
        None,
    )? {
        aterm_journal::Committed::Rebuilt { records, .. } => records,
        _ => {
            return Err(Error::new(
                "writer succession needs the complete retained prefix",
            ));
        }
    };
    let acknowledged = Witness::decode(&claim[WITNESS_AT..])?;
    let replay = replay::read_observed(records, acknowledged)?;
    let prefix = WitnessedPrefix {
        scope,
        entries: replay.entries,
        claims,
        name,
        claim,
        directory: dir,
        identity,
    };
    prefix.recheck(session)?;
    Ok(prefix)
}

fn inspect_with(
    session: &Session<'_>,
    table: Digest,
    after_prefix: impl FnOnce(),
) -> Result<Inspection> {
    inspect_parts(session, table, true, after_prefix)
}

fn inspect_parts(
    session: &Session<'_>,
    table: Digest,
    verify_prefix: bool,
    after_prefix: impl FnOnce(),
) -> Result<Inspection> {
    session.context(crate::catalog::scope(session.bootstrap.trust()))?;
    let claim_dir = claim_dir(session)?;
    let claim_name = super::claim_name_for(&table);
    let claim = match private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN) {
        Ok(claim) => claim,
        Err(PrivateFileError::Missing) => {
            return Ok(Inspection {
                table,
                claim: Claim::Missing,
                directory: None,
                prefix: None,
            });
        }
        Err(_) => return Err(Error::new("table enrollment claim is unreadable")),
    };
    if claim.len() != CLAIM_LEN {
        return Err(Error::new("table enrollment claim has an invalid length"));
    }
    // A scope is 80 bytes of network trust, a table UUID and a generation.
    // validate_claim compares ALL of those bytes with the current trust below.
    let scope_bytes = &claim[5..5 + super::super::SCOPE_LEN];
    let scope = Scope::new(
        session.bootstrap.trust(),
        scope_bytes[80..96].try_into().expect("table UUID"),
        u64::from_le_bytes(scope_bytes[96..104].try_into().expect("generation")),
    )?;
    if scope.digest() != table {
        return Err(Error::new("table enrollment claim has another digest"));
    }
    session.context(scope)?;
    let pending = validate_claim(session, scope, &claim)?;
    if pending {
        return Ok(Inspection {
            table,
            claim: Claim::Pending,
            directory: None,
            prefix: None,
        });
    }
    let dir = Directory::new(session.bootstrap.paths(), scope.trust()).path(scope)?;
    let identity = directory::read(&dir)?;
    let volume = directory::witness(&claim_dir, &claim_name)?;
    let binding_error = directory::check_observed(&identity, volume.as_deref(), &claim)
        .err()
        .map(reason);
    let stored = &claim[ANCHOR_LEN..WITNESS_AT];
    let directory_evidence = DirectoryEvidence {
        recorded_device: u64::from_le_bytes(stored[..8].try_into().expect("device number")),
        recorded_inode: u64::from_le_bytes(stored[8..].try_into().expect("inode")),
        observed: identity,
        volume_witness_present: volume.is_some(),
        binding_error,
    };
    let prefix = verify_prefix.then(|| inspect_prefix(&dir, &claim).map_err(reason));
    after_prefix();
    session.context(scope)?;
    if private_file::read_private(&claim_dir, &claim_name, CLAIM_LEN)
        .ok()
        .as_deref()
        != Some(claim.as_slice())
        || directory::read(&dir)? != identity
        || directory::witness(&claim_dir, &claim_name)? != volume
    {
        return Err(Error::snapshot_changed());
    }
    Ok(Inspection {
        table,
        claim: Claim::Ready,
        directory: Some(directory_evidence),
        prefix,
    })
}

fn reason(error: Error) -> String {
    aterm_codec::tty::one_line(&error.to_string(), 512)
}

fn inspect_prefix(dir: &std::path::Path, claim: &[u8]) -> Result<PrefixEvidence> {
    let acknowledged = Witness::decode(&claim[WITNESS_AT..])?;
    let records = match aterm_journal::read_committed(
        dir,
        "table",
        &claim[..ANCHOR_LEN],
        Capacity::default().journal(),
        None,
    )? {
        aterm_journal::Committed::Rebuilt { records, .. } => records,
        _ => {
            return Err(Error::new(
                "table inspection did not provide its complete prefix",
            ));
        }
    };
    let committed_records = records.len();
    // The ordinary receipt verifier checks the entire independently acknowledged
    // prefix and its single-tail bound. It discards the tail in memory alone.
    replay::read_observed(records, acknowledged)?;
    Ok(PrefixEvidence {
        acknowledged_records: acknowledged.count,
        custody_commitment: claim[WITNESS_AT + 8..]
            .try_into()
            .expect("receipt commitment"),
        committed_records,
    })
}

#[cfg(test)]
mod tests;
