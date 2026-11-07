// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! What this Mac's own import of its fleet_db replica recorded beside that replica
//! (docs/DESIGN-alab-one-mesh-2026-11-05.md §4.7, §11 A7): whether every cell the replica holds
//! is covered by the registry, how many are not, the replica's keyed digest and the `[everyone]`
//! epoch it was keyed under. Value-free: a count, hex digests and the coordinates of uncovered
//! cells by collection, row key and column, never a cell's text.
//!
//! The record is a local fact of this Mac. fleet_db's write verbs read it
//! ([`refuse_writes`]): once it says `covered`, the replica's rows live in the registry and a
//! write to the replica would edit a copy no one reads, so they point to `aterm db emails`.
//! Before that, fleet_db works as it did. A replica edited after its import is imported again
//! before it is deleted: the deletion compares the digest recorded here with one taken then.
//!
//! **Coverage is judged per cell** against the registry as the import left it: the replica's
//! sheets are planned once more, alone, against the written registry ([`judge`]). A cell is
//! covered when that plan writes nothing for it and finds nothing that keeps it out. A
//! `Secret` value that is a flag word (a has-password `1`, a none `0`,
//! [`crate::typed::secret::flag_says`]) is covered when it agrees with whether the registry's
//! cell is set; any other flag-shaped value (a password under six characters, which the registry
//! never holds) stays uncovered, so the owner sees it before the replica goes. Every other
//! finding about a replica cell (it differs, the owner kept another value, it is conflicted,
//! protected, unmapped, withheld) leaves it uncovered, listed by key and column for the owner.
//! So does a record the replica holds and its dump leaves out (a forked writer's, or one waiting
//! for its parent): it is no cell of the dump, its rows are not in the registry, and it is counted
//! and listed once as `withheld-operations`, with no key, and bound into the keyed digest.

use super::{Kind, Plan};
use crate::{Error, Result};
use aterm_json::{Value, json};
use std::path::Path;

/// The record's file name inside the replica's folder.
pub const FILE: &str = "registry-import.json";

/// The largest record read back.
const MAX_RECORD: usize = 64 * 1024;

/// The most uncovered cells a record lists by coordinate; the count is always whole.
const MAX_LISTED: usize = 256;

/// The HKDF label of a replica's keyed digest: its own domain, apart from the registry's
/// (`att::secret_digest`), so a replica digest never compares equal to a table digest. Its version
/// is the judgement's: a record an earlier judge wrote cannot pass a deletion's digest check, so
/// its replica is imported again first.
const REPLICA_DIGEST: &[u8] = b"aterm/fleet-db/replica-digest/2\0";

/// One replica cell the registry does not cover: where, and the finding's word, never a value.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Uncovered {
    pub kind: &'static str,
    pub collection: Option<String>,
    pub pk: Option<String>,
    pub column: Option<String>,
}

impl Uncovered {
    fn json(&self) -> Value {
        json!({
            "why": self.kind,
            "collection": self.collection,
            "pk": self.pk.as_deref().map(crate::att::command::safe_key),
            "column": self.column,
        })
    }
}

/// Findings that leave the replica's cells as the registry holds them: a label read in its
/// stored spelling, a provenance list cut short, a number held by two rows (the smallest key
/// used), an account imported paused by its note, a row an earlier import made that the replica
/// no longer lists, a Mac matched by its node.
const fn benign(kind: Kind) -> bool {
    matches!(
        kind,
        Kind::LabelRespelled
            | Kind::CoordinatesTruncated
            | Kind::DuplicateNumber
            | Kind::Paused
            | Kind::NoLongerListed
            | Kind::AdoptedByNode
    )
}

/// The replica cells `plan` leaves uncovered, where `plan` is the replica's sheets planned alone
/// against the registry its import wrote, and `flag_agrees` says whether a
/// [`Kind::SecretFlagLike`] finding's replica values are flag words that agree with whether the
/// registry holds a value in its cell. Empty means every cell is covered.
pub fn judge(plan: &Plan, flag_agrees: &dyn Fn(&super::Finding) -> bool) -> Vec<Uncovered> {
    let mut out = std::collections::BTreeSet::new();
    for planned in &plan.planned {
        if planned.step == super::Step::Write {
            let (column, _) = planned
                .column
                .split_once('.')
                .unwrap_or((planned.column.as_str(), ""));
            out.insert(Uncovered {
                kind: "unwritten",
                collection: Some(planned.collection.to_owned()),
                pk: Some(planned.pk.clone()),
                column: Some(column.to_owned()),
            });
        }
    }
    if !plan.rows.is_empty() && out.is_empty() {
        out.insert(Uncovered {
            kind: "unwritten",
            collection: None,
            pk: None,
            column: None,
        });
    }
    for finding in &plan.findings {
        if benign(finding.kind) {
            continue;
        }
        if finding.kind == Kind::SecretFlagLike && flag_agrees(finding) {
            continue;
        }
        out.insert(Uncovered {
            kind: finding.kind.word(),
            collection: finding.collection.map(str::to_owned),
            pk: finding.pk.clone(),
            column: finding.column.clone(),
        });
    }
    out.into_iter().collect()
}

/// A replica's keyed digest: HMAC-SHA-256 over its whole dump (`fleet_db::dump_all` with its
/// passwords) and the ids of the records that dump leaves out, keyed by HKDF of the `[everyone]`
/// epoch key, so two holders compare replicas under the same epoch without a value or an unsalted
/// digest leaving either Mac, and a withheld record that arrives after an import changes it.
pub fn replica_digest(
    dump: &[u8],
    withheld: &[astream_wire::sync::Digest],
    everyone_key: &[u8; 32],
) -> String {
    use ring::hmac;
    use zeroize::Zeroizing;
    let prk = Zeroizing::new(aterm_digest::hkdf_extract(REPLICA_DIGEST, everyone_key));
    let derived = Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, REPLICA_DIGEST));
    let mut context = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, &*derived));
    context.update(dump);
    for id in withheld {
        context.update(id);
    }
    aterm_codec::hex::encode_lower(context.sign().as_ref())
}

/// What an import recorded beside its replica.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// Every replica cell is covered.
    pub covered: bool,
    /// How many are not.
    pub uncovered: usize,
    /// The replica's keyed digest ([`replica_digest`]) when this Mac held an `[everyone]` key.
    pub digest: Option<String>,
    /// That key's epoch id, lowercase hex.
    pub epoch: Option<String>,
    /// This Mac's clock when it judged (a claim, never an ordering input).
    pub at_ms: u64,
}

impl Record {
    /// The record as stored and as the import's report shows it, with `listed` uncovered cells.
    pub fn json(&self, listed: &[Uncovered]) -> Value {
        json!({
            "covered": self.covered,
            "uncovered": self.uncovered,
            "digest": self.digest,
            "epoch": self.epoch,
            "at_ms": self.at_ms,
            "cells": listed.iter().take(MAX_LISTED).map(Uncovered::json).collect::<Vec<_>>(),
        })
    }
}

/// A source's coverage as the import's report shows it, for a source with no record of its own
/// (the mailbox file): `covered`, how many cells are not, and up to [`MAX_LISTED`] of them by
/// coordinate.
pub fn report(uncovered: &[Uncovered]) -> Value {
    json!({
        "covered": uncovered.is_empty(),
        "uncovered": uncovered.len(),
        "cells": uncovered.iter().take(MAX_LISTED).map(Uncovered::json).collect::<Vec<_>>(),
    })
}

/// Write `record` into the replica's folder `dir` (0600, atomically), with up to
/// [`MAX_LISTED`] of its uncovered cells by coordinate.
///
/// # Errors
/// The folder is not private, or the file cannot be written.
pub fn write(dir: &Path, record: &Record, listed: &[Uncovered]) -> Result<()> {
    let bytes = aterm_json::to_vec(&record.json(listed))
        .map_err(|_| Error::new("the import's record could not be encoded"))?;
    aterm_uds::private_file::write_private(dir, FILE, &bytes)
        .map_err(|_| Error::new("the import's record could not be written beside the replica"))
}

/// The record beside the replica at `dir`: `None` where no import of it recorded one.
///
/// # Errors
/// A record is there and cannot be read or is malformed: whether the replica is covered is
/// unknown.
pub fn read(dir: &Path) -> Result<Option<Record>> {
    use aterm_uds::private_file::{PrivateFileError, read_private};
    let bytes = match read_private(dir, FILE, MAX_RECORD) {
        Ok(bytes) => bytes,
        Err(PrivateFileError::Missing) => return Ok(None),
        Err(_) => {
            return Err(Error::new(
                "the import's record beside this replica cannot be read",
            ));
        }
    };
    let malformed = || Error::new("the import's record beside this replica is malformed");
    let value: Value = aterm_json::from_slice(&bytes).map_err(|_| malformed())?;
    let text = |name: &str| -> Result<Option<String>> {
        match value.get(name) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if aterm_codec::hex::is_lower(s, s.len()) => Ok(Some(s.clone())),
            Some(_) => Err(malformed()),
        }
    };
    Ok(Some(Record {
        covered: value
            .get("covered")
            .and_then(Value::as_bool)
            .ok_or_else(malformed)?,
        uncovered: value
            .get("uncovered")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(malformed)?,
        digest: text("digest")?,
        epoch: text("epoch")?,
        at_ms: value.get("at_ms").and_then(Value::as_u64).unwrap_or(0),
    }))
}

/// fleet_db's write verbs on the replica at `dir`: refused once this Mac's own import recorded
/// it `covered`, with the words that point to the registry's views. A record that cannot be
/// read refuses too, since the replica may be covered.
///
/// # Errors
/// The replica is covered, or its record cannot be read.
pub fn refuse_writes(dir: &Path) -> Result<()> {
    match read(dir)? {
        Some(record) if record.covered => Err(Error::new(
            "this Mac's fleet_db replica is imported into the registry (covered): its rows live \
             there now, so edit them with `aterm db emails --edit` or `aterm db emails set \
             <address> <column> [--stdin]` (and `computers`, `phones`, `ai-accounts`)",
        )),
        _ => Ok(()),
    }
}
