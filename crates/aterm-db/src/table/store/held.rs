// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What a table store holds, read beside any live writer in another process, with no session and
//! no secrets: the enrollment claim's witness says how far the records are acknowledged, and the
//! committed journal holds them. A record past the witness never leaves this reader.

use super::{
    ANCHOR_LEN, AppendKind, CLAIM_LEN, CONFLICT_SUBJECT, Capacity, Region, SUBJECT, WITNESS_AT,
    Witness,
};
use crate::{Error, Result, table::Scope};
use astream_wire::sync::Digest;
use aterm_digest::Sha256;
use aterm_secrets::{client::Paths, commons::journal::Trust};
use aterm_uds::private_file;
use std::path::{Path, PathBuf};

/// One record a table store holds at `offset`, read beside any live writer, never past the
/// claim's witness (no unacknowledged tail leaves the Mac).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Held {
    pub offset: u64,
    pub kind: AppendKind,
    pub wire: Vec<u8>,
}

/// The claim directory and the data root of `region` under `paths`: the directories a store
/// enrolled through the catalog's [`crate::catalog::Directory`] uses (`claim_dir_of` of the
/// Commons custody at `<paths.dir>/commons/control`, and the catalog's table roots).
fn dirs(paths: &Paths, region: Region) -> (PathBuf, PathBuf) {
    let commons = paths.dir.join("commons");
    match region {
        Region::Primary => (commons.join("table-claims"), commons.join("tables")),
        Region::Recovery => (
            commons.join("recovery/table-claims"),
            commons.join("recovery/tables"),
        ),
    }
}

const fn region(recovery: bool) -> Region {
    if recovery {
        Region::Recovery
    } else {
        Region::Primary
    }
}

/// The scope a ready claim of `region` names, and its witness; `None` for a pending or foreign
/// claim.
fn ready(claim: &[u8], region: Region) -> Result<Option<(Scope, Witness)>> {
    if claim.len() != CLAIM_LEN || &claim[..4] != region.magic() {
        return Err(Error::new("table enrollment claim is damaged"));
    }
    if claim[4] != b'1' {
        return Ok(None);
    }
    let scope = &claim[5..5 + crate::table::SCOPE_LEN];
    let trust = Trust {
        network: scope[..16].try_into().expect("16 bytes"),
        authority: scope[16..48].try_into().expect("32 bytes"),
        genesis: scope[48..80].try_into().expect("32 bytes"),
    };
    let scope = Scope::new(
        trust,
        scope[80..96].try_into().expect("16 bytes"),
        u64::from_le_bytes(scope[96..104].try_into().expect("8 bytes")),
    )?;
    Ok(Some((scope, Witness::decode(&claim[WITNESS_AT..])?)))
}

/// The records of table `table` (its primary store, or its recovery store where `recovery`) at
/// offsets `from..upto` (upto clamped to the witness count). No Session, no secrets: the claim
/// file and `aterm_journal::read_committed`. The acknowledged prefix is checked against the
/// claim's witness chain before any record is returned.
///
/// # Errors
/// The claim is missing, pending, damaged or names another table, or the journal lost or
/// changed acknowledged records.
pub fn read_held(
    paths: &Paths,
    table: Digest,
    recovery: bool,
    from: u64,
    upto: u64,
) -> Result<Vec<Held>> {
    let region = region(recovery);
    let (claim_dir, data_root) = dirs(paths, region);
    let hex = aterm_codec::hex::encode_lower(&table);
    let claim = private_file::read_private(&claim_dir, &format!("{hex}.claim"), CLAIM_LEN)
        .map_err(|_| Error::new("table enrollment claim is missing or damaged"))?;
    let (scope, acknowledged) = ready(&claim, region)?.ok_or_else(Error::enrollment_pending)?;
    if scope.digest() != table {
        return Err(Error::new("table enrollment claim names another table"));
    }
    let upto = upto.min(acknowledged.count);
    if from >= upto {
        return Ok(Vec::new());
    }
    read_journal(&data_root.join(hex), &claim, acknowledged, from, upto)
}

/// The witnessed records `from..upto` of the journal in `dir` under `claim`'s anchor. A writer
/// may append after the claim was read: the records past `acknowledged` are neither checked nor
/// returned.
fn read_journal(
    dir: &Path,
    claim: &[u8],
    acknowledged: Witness,
    from: u64,
    upto: u64,
) -> Result<Vec<Held>> {
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
                "table journal did not provide its complete prefix",
            ));
        }
    };
    let mut witness = Witness::default();
    let mut held = Vec::new();
    for record in records {
        if witness.count == acknowledged.count {
            break;
        }
        let kind = match record.subject.as_str() {
            SUBJECT => AppendKind::Operation,
            CONFLICT_SUBJECT => AppendKind::Conflict,
            _ => return Err(Error::new("foreign table journal record")),
        };
        if record.body.len() < 8 {
            return Err(Error::new("foreign table journal record"));
        }
        let received_ms = u64::from_le_bytes(record.body[..8].try_into().expect("8 bytes"));
        let wire = &record.body[8..];
        witness = witness.advance(record.seq.0, Sha256::digest(wire), received_ms)?;
        if (from..upto).contains(&record.seq.0) {
            held.push(Held {
                offset: record.seq.0,
                kind,
                wire: wire.to_vec(),
            });
        }
    }
    if witness != acknowledged {
        return Err(Error::new(
            "acknowledged table history differs from custody witness",
        ));
    }
    Ok(held)
}

/// Every enrolled table store (ready claims under the primary and recovery claim dirs): its
/// digest, whether it is a recovery store, and its witnessed length. A pending, damaged or
/// misnamed claim holds no record this reader can vouch for, and is left out.
///
/// # Errors
/// A claim directory cannot be listed.
pub fn held_lengths(paths: &Paths) -> Result<Vec<(Digest, bool, u64)>> {
    let mut held = Vec::new();
    for recovery in [false, true] {
        let region = region(recovery);
        let (claim_dir, _) = dirs(paths, region);
        let entries = match std::fs::read_dir(&claim_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut names = Vec::new();
        for entry in entries {
            let Ok(name) = entry?.file_name().into_string() else {
                continue;
            };
            let mut table = [0; 32];
            if name.strip_suffix(".claim").is_some_and(|hex| {
                aterm_codec::hex::is_lower(hex, 64)
                    && aterm_codec::hex::decode_into(hex.as_bytes(), &mut table)
            }) {
                names.push((table, name));
            }
        }
        names.sort_unstable();
        for (table, name) in names {
            let Ok(claim) = private_file::read_private(&claim_dir, &name, CLAIM_LEN) else {
                continue;
            };
            if let Ok(Some((scope, witness))) = ready(&claim, region)
                && scope.digest() == table
            {
                held.push((table, recovery, witness.count));
            }
        }
    }
    Ok(held)
}

#[cfg(test)]
pub(super) fn dirs_of(paths: &Paths, recovery: bool) -> (PathBuf, PathBuf) {
    dirs(paths, region(recovery))
}
