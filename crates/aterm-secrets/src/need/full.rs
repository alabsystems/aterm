// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LAB'S TABLE AS FULL AS THIS MAC LAST MET IT: `credentials/requests/1` (aterm-db) refuses
//! every append past its operation capacity and lets go of none, and refuses a writer's past that
//! writer's own quota, so a pass that meets either refusal posts, answers and sweeps nothing more,
//! and every pass after it meets the same refusal until the table or this Mac's needs change. A
//! pass the table refused records it here ([`record`]: [`FILE`], 0600, beside the ledger) with
//! which bound refused ([`Bound`]), which pass met it ([`By`]) and what the refusal stood on: the
//! ledger's needs once the pass took in what it read ([`mark`]) and, for the table's own bound,
//! the table's enrollment claim as the refused store held it, which every record the table takes
//! renames anew. The refusal stands ([`standing`]) while those read as recorded; a need raised,
//! retired or moved ends it, and so does a record the table took (one that sheds settled ones, or
//! a replica's) for the table's bound, which other Macs' records do not lift from this Mac's
//! writer quota. A pass's own moves of the ledger (its settling, and what it takes in off the
//! table) carry the refusal with them in the same hold of the ledger's lock ([`carry`]), and a
//! look reads the record and the ledger in one hold of it ([`reading`]), so a look never reads a
//! pass's own moves as the ledger moved. A pass the table took an append of, and refused none,
//! forgets the refusal it recorded itself ([`forget`]). The record decides nothing a pass does:
//! every pass meets the table afresh, and the record is what surfaces say and what a watch
//! hears. Counts, a path and two digests: never a value.

use super::{Need, State};
use crate::client::Paths;
use crate::{Error, fsx};
use std::path::{Path, PathBuf};

/// The record's file name, beside the ledger ([`super::LEDGER`]).
pub const FILE: &str = "requests-full";

/// The largest record read.
const MAX_BYTES: usize = 4096;

/// The largest enrollment claim read: aterm-db's claims are a few hundred bytes.
const CLAIM_MAX: usize = 4096;

/// Which bound refused the append: the table's own, or this Mac's writer quota on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Bound {
    /// The table's operations, or their bytes, every writer's together: no Mac's append is
    /// taken.
    Table,
    /// This Mac's own records on the table, or their bytes: other Macs' appends are still taken.
    Writer,
}

/// Which pass met the refusal: each forgets only the refusal it recorded ([`forget`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum By {
    /// This Mac's member pass: its requests, answers and sweeps.
    Member,
    /// The custodian's pass, on the vault admin's Mac: its parts and decisions.
    Custodian,
}

/// What a full table refused, in the counts every surface says it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Full {
    /// Which bound refused.
    pub bound: Bound,
    /// The operations the table held where the refused pass read it ([`Bound::Table`]), or this
    /// Mac's own records among them ([`Bound::Writer`]).
    pub records: u64,
    /// This Mac's needs that no request on the table stands for yet ([`State::Local`]).
    pub needs_unposted: u64,
}

impl Full {
    /// The one line `sync.requests`, `aterm secrets need` and `aterm secrets status` say it in.
    #[must_use]
    pub fn line(&self) -> String {
        let os = aterm_types::machine_name::OS_NOUN;
        let waits = match self.needs_unposted {
            0 => format!("no need waits unposted in this {os}'s ledger"),
            1 => format!("1 need waits unposted in this {os}'s ledger"),
            n => format!("{n} needs wait unposted in this {os}'s ledger"),
        };
        match self.bound {
            Bound::Table => format!(
                "the lab's credential-requests table is full ({} operations, all it holds): it \
                 takes no request, answer or sweep, and {waits}",
                self.records
            ),
            Bound::Writer => format!(
                "this {os}'s writer quota on the lab's credential-requests table is spent ({} \
                 records of its own, all the quota holds): the table takes no request, answer or \
                 sweep from this {os}, though it still takes other {os}s', and {waits}",
                self.records
            ),
        }
    }
}

/// The ledger's needs as a full table's refusal stands on them: each need's id, state and
/// whether it was posted, in the ledger's order. What a pass rewrites without moving a need (its
/// line, its times) is left out, so only a need raised, retired or moved changes it.
fn mark(needs: &[Need]) -> [u8; 32] {
    let mut hash = aterm_digest::Sha256::new();
    hash.update(b"aterm/requests-full/ledger/1\0");
    for need in needs {
        hash.update(need.id);
        hash.update(need.state.word().as_bytes());
        hash.update([u8::from(need.posted), 0]);
    }
    hash.finalize()
}

fn unposted(needs: &[Need]) -> u64 {
    needs
        .iter()
        .filter(|need| need.state == State::Local)
        .count() as u64
}

/// What [`FILE`] holds.
#[derive(serde::Serialize, serde::Deserialize)]
struct Recorded {
    by: By,
    bound: Bound,
    records: u64,
    needs_unposted: u64,
    /// The table's enrollment claim file.
    claim: PathBuf,
    /// The claim's bytes as the refused store held them, digested: what a refusal of the table's
    /// bound stands on.
    claim_digest: String,
    /// [`mark`] of the ledger once the pass took in what it read.
    ledger: String,
}

fn path(paths: &Paths) -> PathBuf {
    paths.dir.join(FILE)
}

fn hex(digest: &[u8; 32]) -> String {
    aterm_codec::hex::encode_lower(digest)
}

/// Record that the lab's table refused `by`'s pass for its capacity, `bound` refusing: it held
/// `records` operations (this Mac's own, for [`Bound::Writer`]), and its enrollment claim at
/// `claim` held `claim_bytes` (the store's own copy, so a record it took since reads as a change).
/// The ledger is read now, once the pass took in what it read: the [`Full`] recorded, with the
/// needs it leaves unposted. It replaces any refusal recorded before, either pass's.
///
/// # Errors
/// The ledger cannot be read, or the record cannot be written.
pub fn record(
    paths: &Paths,
    by: By,
    bound: Bound,
    records: u64,
    claim: &Path,
    claim_bytes: &[u8],
) -> crate::Result<Full> {
    let needs = super::peek(paths)?;
    let full = Full {
        bound,
        records,
        needs_unposted: unposted(&needs),
    };
    let recorded = Recorded {
        by,
        bound,
        records,
        needs_unposted: full.needs_unposted,
        claim: claim.to_path_buf(),
        claim_digest: hex(&aterm_digest::Sha256::digest(claim_bytes)),
        ledger: hex(&mark(&needs)),
    };
    write(paths, &recorded)?;
    Ok(full)
}

fn write(paths: &Paths, recorded: &Recorded) -> crate::Result<()> {
    let text = aterm_json::to_string(recorded).map_err(|why| Error::new(why.to_string()))?;
    fsx::ensure_private_dir(&paths.dir)?;
    fsx::write_atomic(&path(paths), text.as_bytes(), 0o600)
}

/// Carry a refusal recorded on the ledger as `before` held it over a pass's own moves, which left
/// `after` (called in the ledger lock's hold that saved `after`): it stands on `after` as it stood
/// on `before`, its unposted needs counted afresh, since what a pass takes in for its needs gives
/// the table nothing it takes. A refusal recorded on another ledger (a need raised, retired or
/// moved since by a person or a consumer) is left to read as moved, and so is one that cannot be
/// read.
///
/// # Errors
/// The carried record cannot be written.
pub(super) fn carry(paths: &Paths, before: &[Need], after: &[Need]) -> crate::Result<()> {
    let Ok(Some(mut recorded)) = read(paths) else {
        return Ok(());
    };
    if recorded.ledger != hex(&mark(before)) {
        return Ok(());
    }
    let ledger = hex(&mark(after));
    let needs_unposted = unposted(after);
    if recorded.ledger == ledger && recorded.needs_unposted == needs_unposted {
        return Ok(());
    }
    recorded.ledger = ledger;
    recorded.needs_unposted = needs_unposted;
    write(paths, &recorded)
}

/// Forget the refusal `by`'s pass recorded: one the table since took an append of, refusing
/// none. A refusal the other pass recorded is left, since this pass's appends are not its; a
/// record that cannot be read is forgotten.
///
/// # Errors
/// A record there cannot be removed.
pub fn forget(paths: &Paths, by: By) -> crate::Result<()> {
    if matches!(read(paths), Ok(Some(recorded)) if recorded.by != by) {
        return Ok(());
    }
    match std::fs::remove_file(path(paths)) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(Error::new(format!("{}: {error}", path(paths).display())))
        }
        _ => Ok(()),
    }
}

fn read(paths: &Paths) -> crate::Result<Option<Recorded>> {
    Ok(read_bytes(paths)?.map(|(recorded, _)| recorded))
}

/// The record and the bytes it was read from.
fn read_bytes(paths: &Paths) -> crate::Result<Option<(Recorded, Vec<u8>)>> {
    let path = path(paths);
    let Some(bytes) = fsx::read_private(&path, MAX_BYTES)? else {
        return Ok(None);
    };
    let text = bytes
        .as_str()
        .ok_or_else(|| Error::new(format!("{}: not UTF-8", path.display())))?;
    let recorded = aterm_json::from_str(text)
        .map_err(|why| Error::new(format!("{}: {why}", path.display())))?;
    Ok(Some((recorded, bytes.as_slice().to_vec())))
}

/// What a look reads of the refusal ([`reading`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reading {
    /// The refusal while it stands: recorded, with the ledger's needs as the last pass that met
    /// it left them and, for [`Bound::Table`], the table's enrollment claim as the refused store
    /// held it. `None` with no record, or once either changed (a claim gone counts).
    pub standing: Option<Full>,
    /// Whether a refusal is recorded, standing or not.
    pub recorded: bool,
    /// The files whose change can end a standing refusal, for a watch that wakes the pass: the
    /// record, the ledger, and, for [`Bound::Table`], the claim the record names.
    pub watched: Vec<PathBuf>,
    /// What this reading stood on, digested: the record, the ledger's [`mark`] and, for
    /// [`Bound::Table`], the claim's bytes. Two readings of one key read the same refusal over the
    /// same needs and table; `None` with no record.
    pub key: Option<[u8; 32]>,
}

/// The refusal as a look reads it: with a record there, the record, the claim and the ledger in
/// one hold of the ledger's lock, so the moves a pass makes of its own needs, which carry the
/// record in the same hold ([`carry`]), are never read half made.
///
/// # Errors
/// The record, the claim, the ledger or its lock cannot be read.
pub fn reading(paths: &Paths) -> crate::Result<Reading> {
    let mut watched = vec![path(paths), paths.dir.join(super::LEDGER)];
    if !recorded(paths) {
        return Ok(Reading {
            standing: None,
            recorded: false,
            watched,
            key: None,
        });
    }
    super::ledger::locked(paths, || {
        let Some((recorded, bytes)) = read_bytes(paths)? else {
            return Ok(Reading {
                standing: None,
                recorded: false,
                watched,
                key: None,
            });
        };
        let ledger = hex(&mark(&super::peek(paths)?));
        let mut key = aterm_digest::Sha256::new();
        key.update(b"aterm/requests-full/reading/1\0");
        key.update((bytes.len() as u64).to_be_bytes());
        key.update(&bytes);
        key.update(ledger.as_bytes());
        let mut stands = ledger == recorded.ledger;
        if recorded.bound == Bound::Table {
            watched.push(recorded.claim.clone());
            match fsx::read_private(&recorded.claim, CLAIM_MAX)? {
                Some(claim) => {
                    let digest = hex(&aterm_digest::Sha256::digest(claim.as_slice()));
                    key.update([1]);
                    key.update(digest.as_bytes());
                    stands &= digest == recorded.claim_digest;
                }
                None => {
                    key.update([0]);
                    stands = false;
                }
            }
        }
        Ok(Reading {
            standing: stands.then_some(Full {
                bound: recorded.bound,
                records: recorded.records,
                needs_unposted: recorded.needs_unposted,
            }),
            recorded: true,
            watched,
            key: Some(key.finalize()),
        })
    })
}

/// [`Reading::standing`], read as [`reading`] reads it.
///
/// # Errors
/// As [`reading`].
pub fn standing(paths: &Paths) -> crate::Result<Option<Full>> {
    reading(paths).map(|reading| reading.standing)
}

/// Whether a refusal is recorded, standing or not: [`FILE`] is there.
#[must_use]
pub fn recorded(paths: &Paths) -> bool {
    path(paths).symlink_metadata().is_ok()
}

/// [`Reading::watched`]: the record and the ledger alone before one is recorded, or where the
/// record cannot be read.
#[must_use]
pub fn watched(paths: &Paths) -> Vec<PathBuf> {
    reading(paths).map_or_else(
        |_| vec![path(paths), paths.dir.join(super::LEDGER)],
        |reading| reading.watched,
    )
}
