// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! ATT/1 tables on this Mac's Commons (docs/DESIGN-alab-lab-model-2026-11-01.md §3, §8): the
//! store side of [`crate::typed`] and the generic `aterm db table` verbs ([`command`]).
//!
//! - [`View`]: one read of a typed table, the per-Mac quota applied: past-quota automatic
//!   records never project, owned rows count only at their signer's own key.
//! - [`Handle`]: read, apply a grid save with compare-before-write, revert one operation, and
//!   list history, for any table in [`TABLES`].
//! - [`command`]: `aterm db table ls|schema|get|scan|history|check|revert|export|apply|edit`,
//!   addresses masked unless `--show-addresses` is given, and the amacs grid round trip.

pub mod command;
mod edit;

use crate::{
    Error, Result,
    alab::registry,
    catalog::Descriptor,
    database::{Change, Command, MAX_WRITES},
    documents::{application, log::Log},
    table::Session,
    typed::{
        self, Collection, Table,
        cell::{self, Cells, Projection},
        grid::{self, Plan, Stale},
        owned::{self, Op, OwnedRead, Quota, SchemaState},
    },
};
use astream_wire::sync::{Actor, Digest};
use aterm_json::Value;
use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
use std::collections::BTreeMap;

pub(crate) use edit::edit as edit_grid;
pub(crate) use edit::show as show_grid;
pub use edit::{Edited, GridSecrets, Opener, amacs, in_editor_tab, open_secrets};

/// How a table keys the row a new grid row becomes, from the typed values of its cells and the
/// table's cells as they are (an account's key needs its login's address).
pub type Birth = fn(&Collection, &BTreeMap<String, Value>, &Cells) -> Result<String>;

/// A table's rules on what the owner writes, over each row's changes (one record each) and the
/// table's cells as they are under the writer: it answers the first write the rules stop
/// ([`Refused`]), and may add to a row's record the cells its rules derive from one the write
/// changes. It runs on a grid save ([`Handle::apply`]), an owner verb's write
/// ([`Handle::commit_owner`], [`Handle::commit_owner_sealed`]) and a revert ([`Handle::revert`],
/// which writes the exact compensation and none of the additions); a plan written through
/// [`Handle::commit_derived`] or [`Handle::commit_sealed_until`] (the importer's, `adopt`'s,
/// `prefer`'s) does not pass it.
///
/// # Errors
/// A change is not a typed cell of its collection.
pub type Guard = fn(&mut [Vec<crate::database::Change>], &Cells) -> Result<Option<Refused>>;

/// A table rule's refusal of an owner write ([`Guard`]): the collection, row and column it stops
/// and the rule, by keys and column names, never a value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refused {
    pub collection: &'static str,
    pub pk: String,
    pub column: String,
    pub rule: String,
}

impl Refused {
    /// The rule with the column it stops, as a grid problem reads it (its place names the row).
    pub fn problem_rule(&self) -> String {
        format!("{}.{}: {}", self.collection, self.column, self.rule)
    }
}

impl From<Refused> for Error {
    fn from(refused: Refused) -> Self {
        Self::new(&format!(
            "{}.{}: row {}: {}",
            refused.collection,
            refused.column,
            command::safe_key(&refused.pk),
            refused.rule
        ))
    }
}

/// A keyed comparison of a collection. The epoch is part of the output: digests
/// from different epochs cannot be called a table mismatch.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SecretDigest {
    pub epoch: String,
    pub rows: usize,
    pub with_secret: usize,
    pub digest: String,
}

/// A key rotation is distinct from a differing table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestComparison {
    Equal,
    Different,
    DifferentEpoch,
}

impl SecretDigest {
    /// Compare only under the same named digest epoch.
    pub fn compare(&self, other: &Self) -> DigestComparison {
        if self.epoch != other.epoch {
            DigestComparison::DifferentEpoch
        } else if self == other {
            DigestComparison::Equal
        } else {
            DigestComparison::Different
        }
    }
}

impl std::fmt::Display for SecretDigest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "epoch={} rows={} with_secret={} digest={}",
            self.epoch, self.rows, self.with_secret, self.digest
        )
    }
}

/// Compare the logical values, not randomized ciphertext, in one collection. `key`
/// is the current everyone epoch key; `resolve` supplies the table audience key for
/// each stored cell's epoch, including historical epochs after rotation. Conflicting
/// secrets are compared as a sorted set of values, independent of operation IDs.
///
/// # Errors
/// A cell's historical key is unavailable, its seal fails, or values cannot encode.
pub fn secret_digest(
    table: &str,
    collection: &Collection,
    cells: &Cells,
    epoch: &Digest,
    key: &[u8; 32],
    resolve: impl Fn(&Digest) -> Result<zeroize::Zeroizing<[u8; 32]>>,
) -> Result<SecretDigest> {
    use crate::typed::{Ty, secret};
    use ring::hmac;
    use zeroize::Zeroizing;
    let prk = Zeroizing::new(aterm_digest::hkdf_extract(
        b"aterm/att/secret-digest/1\0",
        key,
    ));
    let derived = Zeroizing::new(aterm_digest::hkdf_expand_32(
        &prk,
        b"aterm/att/secret-digest/1\0",
    ));
    let mut mac = hmac::Context::with_key(&hmac::Key::new(hmac::HMAC_SHA256, &*derived));
    let mut part = |bytes: &[u8]| {
        mac.update(&(bytes.len() as u64).to_le_bytes());
        mac.update(bytes);
    };
    part(table.as_bytes());
    part(collection.name.as_bytes());
    let projection = cell::project(collection, cells);
    part(&(projection.rows.len() as u64).to_le_bytes());
    let mut with_secret = 0;
    let mut epoch_keys: BTreeMap<Digest, Zeroizing<[u8; 32]>> = BTreeMap::new();
    for (pk, row) in &projection.rows {
        part(pk.as_bytes());
        part(&(row.fields.len() as u64).to_le_bytes());
        let mut populated = false;
        for (name, field) in &row.fields {
            part(name.as_bytes());
            if collection
                .column(name)
                .is_some_and(|c| matches!(c.ty, Ty::Secret))
            {
                part(b"secret");
                let candidates: &[cell::Candidate] = match field {
                    cell::Field::Value(c) => std::slice::from_ref(c),
                    cell::Field::Conflict { candidates, .. } => candidates.as_slice(),
                    cell::Field::Set(_) => return Err(Error::new("a secret is not a set")),
                };
                let mut values: Vec<Zeroizing<Vec<u8>>> = Vec::with_capacity(candidates.len());
                for candidate in candidates {
                    let value = &candidate.value.x;
                    if value.is_null() {
                        values.push(Zeroizing::new(vec![0]));
                        continue;
                    }
                    let token = value
                        .as_str()
                        .ok_or_else(|| Error::new("invalid secret cell"))?;
                    let token_epoch = secret::epoch(token)?;
                    let old_key = match epoch_keys.entry(token_epoch) {
                        std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                        std::collections::btree_map::Entry::Vacant(entry) => {
                            entry.insert(resolve(&token_epoch)?)
                        }
                    };
                    let plain = secret::open(
                        token,
                        old_key,
                        secret::Cell {
                            table,
                            collection: collection.name,
                            row: pk,
                            column: name,
                        },
                    )?;
                    let text = plain.plaintext().as_bytes();
                    let mut bytes = Zeroizing::new(Vec::with_capacity(1 + text.len()));
                    bytes.push(1);
                    bytes.extend_from_slice(text);
                    values.push(bytes);
                    populated = true;
                }
                values.sort_unstable_by(|a, b| a.as_slice().cmp(b.as_slice()));
                values.dedup_by(|a, b| a.as_slice() == b.as_slice());
                part(&(values.len() as u64).to_le_bytes());
                for value in values.iter() {
                    part(value);
                }
            } else {
                part(b"value");
                match field {
                    cell::Field::Value(c) => {
                        let bytes = aterm_json::to_vec(&c.value.x)
                            .map_err(|_| Error::new("cannot encode digest field"))?;
                        part(&1u64.to_le_bytes());
                        part(&bytes);
                    }
                    cell::Field::Conflict { candidates, .. } => {
                        let mut encoded: Vec<Vec<u8>> = candidates
                            .iter()
                            .map(|c| aterm_json::to_vec(&c.value.x))
                            .collect::<std::result::Result<_, _>>()
                            .map_err(|_| Error::new("cannot encode digest field"))?;
                        encoded.sort_unstable();
                        encoded.dedup();
                        part(&(encoded.len() as u64).to_le_bytes());
                        for value in encoded {
                            part(&value);
                        }
                    }
                    cell::Field::Set(_) => {
                        let members: Vec<_> = field.members().into_iter().collect();
                        let bytes = aterm_json::to_vec(&members)
                            .map_err(|_| Error::new("cannot encode digest field"))?;
                        part(&1u64.to_le_bytes());
                        part(&bytes);
                    }
                }
            }
        }
        with_secret += usize::from(populated);
    }
    Ok(SecretDigest {
        epoch: aterm_codec::hex::encode_lower(epoch),
        rows: projection.rows.len(),
        with_secret,
        digest: aterm_codec::hex::encode_lower(mac.sign().as_ref()),
    })
}

#[cfg(test)]
mod secret_digest_tests {
    use super::*;
    use crate::database::{Candidate, Cell as Stored};
    use crate::typed::{
        Column, Derive, Kind, Ty, Writers,
        secret::{Cell, Secret},
    };

    const COLLECTION: Collection = Collection {
        name: "mailbox",
        version: 1,
        kind: Kind::Cells,
        writers: Writers::Owner,
        prefix: "mb",
        derive: Derive::Random,
        columns: &[
            Column::new("row", Ty::Enum(&["live", "ended"])),
            Column::new("password", Ty::Secret).null(),
        ],
        unique: &[],
        max_row: 1024,
    };

    fn cells(password: &str, epoch: Digest, key: [u8; 32]) -> Cells {
        let token = Secret::new(password.into())
            .seal(
                &key,
                &epoch,
                Cell {
                    table: "alab/registry",
                    collection: "mailbox",
                    row: "mb-test",
                    column: "password",
                },
            )
            .unwrap();
        BTreeMap::from([(
            ("mailbox".into(), "mb-test.password".into()),
            Stored {
                stopped: false,
                candidates: vec![Candidate {
                    operation: [1; 32],
                    value: cell::CellValue::owner(1, Value::String(token)).encode(),
                }],
            },
        )])
    }

    #[test]
    fn digest_compares_logical_values_and_names_its_epoch() {
        let epoch = [2; 32];
        let key = [3; 32];
        let digest = |cells: &Cells, epoch: &Digest, key: &[u8; 32]| {
            secret_digest("alab/registry", &COLLECTION, cells, epoch, key, |_| {
                Ok(zeroize::Zeroizing::new([3; 32]))
            })
            .unwrap()
        };
        let first = cells("fixture-one", epoch, key);
        let resealed = cells("fixture-one", epoch, key);
        assert_ne!(first, resealed);
        let one = digest(&first, &epoch, &key);
        assert_eq!(one, digest(&resealed, &epoch, &key));
        assert_ne!(
            one.digest,
            digest(&cells("fixture-two", epoch, key), &epoch, &key).digest
        );
        let rotated = digest(&first, &[4; 32], &[5; 32]);
        assert_ne!(one.digest, rotated.digest);
        assert_ne!(one.epoch, rotated.epoch);
        assert_eq!(one.compare(&rotated), DigestComparison::DifferentEpoch);
        assert_eq!((one.rows, one.with_secret), (1, 1));
        assert!(one.to_string().starts_with(&format!("epoch={}", one.epoch)));
        assert!(!one.to_string().contains("fixture-one"));
    }
}

/// A typed catalog table this build compiles.
#[derive(Clone, Copy, Debug)]
pub struct Compiled {
    pub table: &'static Table,
    /// The table descriptor's version.
    pub version: u32,
    pub birth: Birth,
    /// Its rules on the owner's writes.
    pub guard: Guard,
    /// Which generation of its table family this is (`registry` 1, `registry.2` 2, design §3.8).
    pub generation: u32,
    /// The next generation a roll restates this one into, when this build compiles one.
    pub successor: Option<&'static Compiled>,
}

impl Compiled {
    /// This table's catalog declaration in `trust`'s lab.
    ///
    /// # Errors
    /// The table's words break the descriptor grammar.
    pub fn descriptor(&self, trust: aterm_secrets::commons::journal::Trust) -> Result<Descriptor> {
        Descriptor::application(trust, self.table.application, self.table.role, self.version)
    }

    /// This generation and every later one this build compiles, oldest first.
    pub fn chain(&'static self) -> Vec<&'static Compiled> {
        std::iter::successors(Some(self), |compiled| compiled.successor).collect()
    }

    /// The key a new grid row in `collection` is born under ([`Birth`]), refused when the
    /// table already holds a row there: a `--where` export can leave that row out of the
    /// file, and a birth never writes into a row the owner did not see (design §8).
    ///
    /// # Errors
    /// The table's birth rule stops the key, or the table holds that row already.
    pub fn born(
        &self,
        collection: &Collection,
        values: &BTreeMap<String, Value>,
        cells: &Cells,
    ) -> Result<String> {
        let pk = (self.birth)(collection, values, cells)?;
        if grid::holds_row(collection, cells, &pk) {
            return Err(Error::new(&format!(
                "this new row is the row {pk}, which the table already holds; export without \
                 a filter and edit that row"
            )));
        }
        Ok(pk)
    }

    /// `<application>/<role>`.
    pub fn name(&self) -> String {
        format!("{}/{}", self.table.application, self.table.role)
    }

    pub(crate) fn matches_name(&self, name: &str) -> bool {
        name.strip_prefix(self.table.application)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|role| role == self.table.role)
    }

    /// The cells or owned collection named `name`.
    ///
    /// # Errors
    /// The table has no such collection.
    pub fn collection(&self, name: &str) -> Result<&'static Collection> {
        self.table
            .collection(name)
            .ok_or_else(|| Error::new(&format!("{}: no collection of that name", self.name())))
    }
}

/// `alab/registry`, the lab model's owner data, until a roll seals it ([`REGISTRY_2`]).
pub const REGISTRY: Compiled = Compiled {
    table: &registry::REGISTRY,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    generation: 1,
    successor: Some(&REGISTRY_2),
};

/// `alab/registry.2`, the registry's second generation (design §3.8, step 13).
pub const REGISTRY_2: Compiled = Compiled {
    table: &registry::REGISTRY_2,
    version: registry::VERSION,
    birth: registry::birth,
    guard: registry::guard,
    generation: 2,
    successor: None,
};

/// Every typed catalog table this build compiles.
pub const TABLES: &[Compiled] = &[REGISTRY, REGISTRY_2];

/// The generation of `chain` (oldest first) readers and writers use in `snapshot`: the oldest
/// one subscribed here. A roll declares the next generation and restates into it while the old
/// one stays subscribed, so every Mac keeps reading the old one until the roll seals it; then
/// the old one's declarations conflict and the next is the oldest subscribed. With none
/// subscribed it is the first, whose read then says why.
pub(crate) fn current_in(
    snapshot: &crate::catalog::Snapshot,
    trust: aterm_secrets::commons::journal::Trust,
    chain: &[&'static Compiled],
) -> &'static Compiled {
    chain
        .iter()
        .copied()
        .find(|compiled| {
            compiled.descriptor(trust).is_ok_and(|descriptor| {
                snapshot
                    .entries
                    .get(&descriptor.scope.digest())
                    .is_some_and(|entry| entry.state == crate::catalog::State::Subscribed)
            })
        })
        .unwrap_or(chain[0])
}

/// The generation of `first`'s family this Mac's catalog says to use ([`current_in`]), read as
/// an observer: `first` itself where it has no successor, where there is no Commons, or where
/// the catalog cannot be read (the read that follows then says why).
pub fn current(paths: &Paths, first: &'static Compiled) -> &'static Compiled {
    let chain = first.chain();
    if chain.len() == 1 {
        return first;
    }
    application::peek_catalog(paths)
        .ok()
        .flatten()
        .map_or(first, |(trust, snapshot)| {
            current_in(&snapshot, trust, &chain)
        })
}

/// The compiled table of `table`'s family at `generation`.
fn generation_of(table: &Table, generation: u64) -> Option<&'static Compiled> {
    let target_family = table.role.split('.').next().unwrap_or(table.role);
    TABLES.iter().find(|compiled| {
        compiled.table.application == table.application
            && compiled
                .table
                .role
                .split('.')
                .next()
                .unwrap_or(compiled.table.role)
                == target_family
            && u64::from(compiled.generation) == generation
    })
}

/// The compiled table named `<application>/<role>`.
///
/// # Errors
/// This build compiles no such table.
pub fn find(name: &str) -> Result<&'static Compiled> {
    TABLES
        .iter()
        .find(|compiled| compiled.matches_name(name))
        .ok_or_else(|| {
            Error::new("no typed table of that name in this build (see `aterm db table ls`)")
        })
}

/// A typed record: a canonical `Write` or `Revert`. A write never stops a cell (clearing
/// writes `x: null`, design §3.2), so a bare absence is refused; a revert may restore one.
/// Typed values are judged by the readers, so a newer build's collection or column stays.
pub(crate) fn decode(payload: &[u8]) -> Result<Command> {
    let command = Command::decode(payload)?;
    if command.encode()? != payload {
        return Err(Error::new("typed table payload is not canonical"));
    }
    match &command {
        Command::Write { changes, .. } if changes.iter().all(|c| c.value.is_some()) => Ok(command),
        Command::Revert { .. } => Ok(command),
        Command::Write { .. } => Err(Error::new("a typed table write never stops a cell")),
        _ => Err(Error::new(
            "typed table records are writes and reverts only",
        )),
    }
}

/// One read of a typed table, the quota applied.
#[derive(Clone, Debug)]
pub struct View {
    /// Every eligible operation, past-quota ones included (the schema reader and history read
    /// them).
    pub ops: Vec<Op>,
    /// Each writer's automatic records, and the ones past the table's `self_quota`, which never
    /// project.
    pub quota: Quota,
    /// The projected cells without the past-quota records.
    pub cells: Cells,
    /// The owned collections' rows, past-quota records left out, each at its signer's own key.
    pub owned: BTreeMap<&'static str, OwnedRead>,
    /// No retained operation could still become visible here (`Log::awaited`: none waits on a
    /// causal parent that has not arrived, and this Mac's own writer has nothing withheld), so
    /// a comparison against these cells misses nothing a later projection would add.
    pub complete: bool,
    /// Retained operations this build never projects and that cannot become visible: refused
    /// payloads (a newer build's encoding, a reused request id), cut-off or forked writers'
    /// records and quarantined payloads. They never hold a grid save or an import back;
    /// `check` lists them.
    pub refused: Vec<Digest>,
}

impl View {
    pub(crate) fn read(table: &Table, session: &Session<'_>, log: &Log) -> Self {
        let (eligible, merges) = log.eligible_and_merges();
        let ops: Vec<Op> = eligible
            .into_iter()
            .filter_map(|(operation, command)| Op::from_eligible(0, operation, command))
            .collect();
        let quota = owned::quota(table, &ops, &merges);
        let filtered;
        let within: &[Op] = if quota.over.is_empty() {
            &ops
        } else {
            filtered = ops
                .iter()
                .filter(|op| !quota.over.contains(&op.id))
                .cloned()
                .collect::<Vec<Op>>();
            &filtered
        };
        let resolve = owned::commons::historical(session);
        let awaited = log.awaited(session.actor());
        let (cells, refused) = log.cells_and_refused(&quota.over, &awaited);
        let archived = archived_ops(table, session, within);
        let owned = table
            .collections
            .iter()
            .filter(|collection| collection.kind == typed::Kind::Owned)
            .map(|collection| {
                let mut read = owned::read_owned(collection, within, &resolve);
                // A row a Mac wrote in this generation outranks the one a roll carried for it.
                let carried = owned::read_owned(collection, &archived, &resolve);
                read.inert += carried.inert;
                for (key, row) in carried.rows {
                    read.rows.entry(key).or_insert(row);
                }
                (collection.name, read)
            })
            .collect();
        // A later generation is whole only once a roll's cover row is here: before that its
        // history is still arriving from the Macs that restated it.
        let restated = table.collection(registry::ROLL.name).is_none()
            || cells
                .keys()
                .any(|(collection, _)| collection == registry::ROLL.name);
        Self {
            cells,
            complete: awaited.is_empty() && restated,
            refused,
            ops,
            quota,
            owned,
        }
    }

    /// One cells collection's rows.
    pub fn project(&self, collection: &Collection) -> Projection {
        cell::project(collection, &self.cells)
    }

    /// The stored schema row of `collection`: the highest version any eligible operation wrote.
    pub fn schema(&self, collection: &str) -> SchemaState {
        owned::schema_state(&self.ops, collection)
    }
}

/// The records a roll carried into `table`'s `archived` rows (design §3.8), each authenticated
/// under the scope of the generation that sealed it: its signer, roster and operation digest are
/// the record's own, so its owned rows go through the owned rule exactly as they did there.
///
/// They are read from every eligible operation `ops` holds that wrote an archived row, not from
/// the cells' current values: any Writer may write any archived cell, so a later write over a
/// carried record (junk, or an older genuine record of the same row) must not hide it. Every
/// record that authenticates stands, and the owned rule ranks them as it ranked them in the
/// generation that sealed them, so an older record never outranks a newer one. A row whose
/// record does not authenticate, whose key does not name its operation, or whose generation
/// this build does not compile is left out; nothing a later writer wrote can stand for another
/// Mac's row or take one away.
pub(crate) fn archived_ops(table: &Table, session: &Session<'_>, ops: &[Op]) -> Vec<Op> {
    let Some(collection) = table.collection(registry::ARCHIVED.name) else {
        return Vec::new();
    };
    let trust = session.trust();
    let mut out: BTreeMap<Digest, Op> = BTreeMap::new();
    for op in ops.iter().filter(|op| !op.revert) {
        // One archived row's cells go in one record (`pack` never splits a row).
        let mut rows: BTreeMap<&str, (Option<u64>, Option<String>)> = BTreeMap::new();
        for change in op
            .changes
            .iter()
            .filter(|change| change.collection == collection.name)
        {
            let Ok(key) = cell::CellKey::parse(&change.key) else {
                continue;
            };
            let Some(value) = change
                .value
                .as_ref()
                .and_then(|value| cell::CellValue::decode(value).ok())
            else {
                continue;
            };
            let slot = rows.entry(key.pk).or_default();
            match (key.col, key.elem) {
                ("generation", None) => slot.0 = value.x.as_u64(),
                ("wire", None) => {
                    if let Value::String(wire) = value.x {
                        slot.1 = Some(wire);
                    }
                }
                _ => {}
            }
        }
        for (pk, (generation, wire)) in rows {
            let (Some(generation), Some(wire)) = (generation, wire) else {
                continue;
            };
            let Some(compiled) = generation_of(table, generation) else {
                continue;
            };
            let (Some(wire), Ok(descriptor)) = (
                aterm_secrets::b64::decode_padded(&wire),
                compiled.descriptor(trust),
            ) else {
                continue;
            };
            let Ok(record) = session.authenticate(descriptor.scope, &wire) else {
                continue;
            };
            let header = record.header();
            if out.contains_key(&header.id) || typed::keys::archived_pk(&header.id) != pk {
                continue;
            }
            let Some(changes) = decode(record.payload())
                .ok()
                .and_then(|command| command.changes().map(<[Change]>::to_vec))
            else {
                continue;
            };
            out.insert(
                header.id,
                Op {
                    id: header.id,
                    actor: header.actor,
                    sequence: header.sequence,
                    roster: *record.roster(),
                    period: 0,
                    revert: false,
                    changes: changes
                        .into_iter()
                        .filter(|change| {
                            table
                                .collection(&change.collection)
                                .is_some_and(|c| c.kind == typed::Kind::Owned)
                        })
                        .collect(),
                },
            );
        }
    }
    out.into_values().collect()
}

/// Pack rows of changes into records of at most [`MAX_WRITES`] changes and a payload within
/// the Commons body limit, never splitting a row.
///
/// # Errors
/// One row alone passes a record's limits.
pub(crate) fn pack(rows: Vec<Vec<Change>>) -> Result<Vec<Vec<Change>>> {
    // `Command::Write::encode()` wraps the serialized `changes` JSON array (`[...]`) in the
    // fixed 78-byte envelope `{"changes":...,"kind":"write","request":"<32-hex>","v":1}`.
    const WRITE_ENVELOPE_BYTES: usize = 78;
    let mut records: Vec<Vec<Change>> = Vec::new();
    let mut last_cmd_bytes = 0usize;
    for row in rows {
        if row.len() > MAX_WRITES {
            return Err(Error::new("one typed row passes a record's limits"));
        }
        let row_json_len = aterm_json::to_vec(&row)
            .map_err(|_| Error::new("one typed row passes a record's limits"))?
            .len();
        let row_cmd_bytes = WRITE_ENVELOPE_BYTES + row_json_len;
        if row_cmd_bytes > crate::crypto::MAX_BODY {
            return Err(Error::new("one typed row passes a record's limits"));
        }
        match records.last_mut() {
            Some(last) if last.len() + row.len() <= MAX_WRITES => {
                let combined_cmd_bytes = if last.is_empty() {
                    row_cmd_bytes
                } else if row.is_empty() {
                    last_cmd_bytes
                } else {
                    last_cmd_bytes + row_json_len - 1
                };
                if combined_cmd_bytes <= crate::crypto::MAX_BODY {
                    last.extend(row);
                    last_cmd_bytes = combined_cmd_bytes;
                } else {
                    last_cmd_bytes = row_cmd_bytes;
                    records.push(row);
                }
            }
            _ => {
                last_cmd_bytes = row_cmd_bytes;
                records.push(row);
            }
        }
    }
    Ok(records)
}

/// What a grid save wrote.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Applied {
    /// Records appended.
    pub records: usize,
    /// Registers written.
    pub cells: usize,
    /// Edits the table already held.
    pub unchanged: usize,
    /// Cells that changed since the export: left alone.
    pub stale: Vec<Stale>,
    /// Keys of born rows written by this save.
    pub born: Vec<String>,
    /// Keys of rows retired or ended by this save.
    pub retired: Vec<String>,
    /// The operations appended.
    pub operations: Vec<Digest>,
    /// The table's rules refused the save ([`Compiled::guard`]): nothing of it was written.
    pub refused: Option<Refused>,
}

/// What a table's capacity asks of the owner (design §3.9), as `check` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Capacity {
    /// A writer's automatic records, with the merges they cost, reached the table's quota:
    /// its later facts, observations and births are inert until the table rolls.
    QuotaSpent { writer: Actor, spent: usize },
    /// Roll the table to its next generation (`registry.2`, step 13).
    Roll(Roll),
}

/// Why a table should roll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Roll {
    /// A writer's quota is spent; only a new generation gives it room again.
    Quota,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's records.
    Records,
    /// The table holds [`ROLL_AT_PERCENT`] of the store's bytes.
    Bytes,
}

/// The share of the store's records or bytes at which a table should roll.
pub const ROLL_AT_PERCENT: usize = 75;

/// What `view`'s quota and the table's `usage` ask of the owner: each writer whose quota is
/// spent, and a roll once any is, or once the table holds [`ROLL_AT_PERCENT`] of the store's
/// records or bytes. Automatic writes alone never reach 75 % (128 Macs at 24 records is about
/// a third), so a spent quota is a roll trigger of its own.
pub fn capacity(table: &Table, view: &View, usage: &Usage) -> Vec<Capacity> {
    use crate::table::store::{MAX_BYTES, MAX_RECORDS};
    let mut found: Vec<Capacity> = view
        .quota
        .spent
        .iter()
        .filter(|(_, spent)| **spent >= table.self_quota)
        .map(|(writer, spent)| Capacity::QuotaSpent {
            writer: *writer,
            spent: *spent,
        })
        .collect();
    let roll = if !found.is_empty() {
        Some(Roll::Quota)
    } else if usage.records * 100 >= MAX_RECORDS * ROLL_AT_PERCENT {
        Some(Roll::Records)
    } else if usage.bytes * 100 >= MAX_BYTES * ROLL_AT_PERCENT {
        Some(Roll::Bytes)
    } else {
        None
    };
    found.extend(roll.map(Capacity::Roll));
    found
}

/// The records and bytes a table holds here, in all and authored by this Mac.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Usage {
    /// This Mac's writer in the table.
    pub actor: Actor,
    pub records: usize,
    pub bytes: usize,
    pub mine_records: usize,
    pub mine_bytes: usize,
}

/// One eligible operation as history shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub operation: Digest,
    pub actor: Actor,
    /// The writer's Commons machine, from the binding in the roster the record names.
    pub machine: Option<[u8; 16]>,
    /// The writer's clock claim, never an ordering input.
    pub authored_ms: u64,
    pub request: Option<[u8; 16]>,
    pub reverts: Option<Digest>,
    pub changes: Vec<Change>,
}

/// The Secret-cell sealer of table `name` in `session`: the current audience epoch seals, and a
/// stored token opens under the epoch it names, through the session's retained handoffs.
///
/// # Errors
/// The current audience key is unavailable.
pub(crate) fn sealer<'s>(
    name: &'s str,
    session: &'s Session<'_>,
    scope: crate::table::Scope,
) -> Result<impl typed::secret::Sealer + 's> {
    let epoch = session.secret_basis();
    let key = session.secret_key(scope, &epoch)?;
    let cache: std::cell::RefCell<BTreeMap<Digest, zeroize::Zeroizing<[u8; 32]>>> =
        std::cell::RefCell::new(BTreeMap::new());
    Ok(typed::secret::Keyed {
        table: name,
        epoch,
        key,
        resolve: move |basis: &Digest| {
            if let Some(cached) = cache.borrow().get(basis) {
                return Ok(cached.clone());
            }
            let resolved = session.secret_key(scope, basis)?;
            cache.borrow_mut().insert(*basis, resolved.clone());
            Ok(resolved)
        },
    })
}

/// Observe the table again while an observation answers busy, until `deadline`: a writer that
/// advanced the table mid-read (a repair round landing, another Mac's edit arriving) makes the
/// observer refuse its result and ask for a fresh observation ([`Error::snapshot_changed`]), so a
/// read racing a sync takes the newer table instead of failing. Any other answer, and a busy one
/// once `deadline` passed, is returned as it is. Only pure reads come here: each attempt is a
/// whole new observation, and none writes.
///
/// # Errors
/// What the last attempt answered.
pub(crate) fn reopened<T>(
    deadline: std::time::Instant,
    mut observe: impl FnMut() -> Result<T>,
) -> Result<T> {
    loop {
        match observe() {
            Err(error) if error.is_busy() && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            answered => return answered,
        }
    }
}

/// A typed table on this Mac's Commons.
pub struct Handle {
    paths: Paths,
    compiled: &'static Compiled,
}

impl Handle {
    /// The table `compiled` names, at the generation this Mac's catalog says to use
    /// ([`current`]): after a roll, `alab/registry` reaches `alab/registry.2`.
    pub fn at(paths: Paths, compiled: &'static Compiled) -> Self {
        let compiled = current(&paths, compiled);
        Self { paths, compiled }
    }

    /// The table this handle reaches.
    pub const fn compiled(&self) -> &'static Compiled {
        self.compiled
    }

    /// Digest a collection under an explicitly named everyone epoch. Historical
    /// cell keys stay inside the pinned table session.
    ///
    /// # Errors
    /// The table, collection, historical cell key or authenticated value is unavailable.
    pub fn secret_digest(
        &self,
        collection: &str,
        epoch: &Digest,
        everyone_key: &[u8; 32],
    ) -> Result<SecretDigest> {
        let collection = self.compiled.collection(collection)?;
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let view = View::read(
                    self.compiled.table,
                    session,
                    &Log::read_with(session, store, scope, decode)?,
                );
                secret_digest(
                    &self.compiled.name(),
                    collection,
                    &view.cells,
                    epoch,
                    everyone_key,
                    |basis| session.secret_key(scope, basis),
                )
            })
        })
    }

    /// Seal an authored Secret for this table without exposing its audience key.
    ///
    /// # Errors
    /// The table cannot be observed, the column is not Secret, or sealing fails.
    pub fn seal_secret(
        &self,
        collection: &str,
        row: &str,
        column: &str,
        value: &typed::secret::Secret,
    ) -> Result<Value> {
        let schema = self.compiled.collection(collection)?;
        if !schema
            .column(column)
            .is_some_and(|c| matches!(c.ty, typed::Ty::Secret))
            || !typed::pk_grammar(row)
        {
            return Err(Error::new("invalid Secret cell destination"));
        }
        application::observe_store(&self.paths, self.descriptor(), |session, _, scope| {
            let basis = session.secret_basis();
            let key = session.secret_key(scope, &basis)?;
            value
                .seal(
                    &key,
                    &basis,
                    typed::secret::Cell {
                        table: &self.compiled.name(),
                        collection,
                        row,
                        column,
                    },
                )
                .map(Value::String)
        })
    }

    /// Reveal a stored Secret through the single audited plaintext exit.
    ///
    /// # Errors
    /// The table or historical audience key is unavailable, the token does not
    /// authenticate, or the local reveal journal cannot be committed.
    pub fn reveal_secret<T>(
        &self,
        cell: typed::secret::Cell<'_>,
        token: &str,
        journal: &crate::alab::secret::Journal,
        surface: crate::alab::secret::Surface,
        when_ms: u64,
        consume: impl FnOnce(&str) -> T,
    ) -> Result<T> {
        if !self.compiled.matches_name(cell.table) {
            return Err(Error::new("secret cell belongs to another table"));
        }
        application::observe_store(&self.paths, self.descriptor(), |session, _, scope| {
            let basis = typed::secret::epoch(token)?;
            let key = session.secret_key(scope, &basis)?;
            crate::alab::secret::reveal(journal, token, &key, cell, surface, when_ms, consume)
        })
    }

    fn descriptor(&self) -> impl FnOnce(&Bootstrap) -> Result<Descriptor> + use<> {
        let table = self.compiled.table;
        let version = self.compiled.version;
        move |bootstrap| {
            Descriptor::application(bootstrap.trust(), table.application, table.role, version)
        }
    }

    /// Read the table this Mac holds, without enrolling it.
    ///
    /// # Errors
    /// No Commons, the table is not declared or not subscribed here, or its storage is damaged.
    pub fn view(&self) -> Result<View> {
        let table = self.compiled.table;
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                Ok(View::read(
                    table,
                    session,
                    &Log::read_with(session, store, scope, decode)?,
                ))
            })
        })
    }

    /// The view and what the table's storage holds here.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn usage(&self) -> Result<(View, Usage)> {
        let table = self.compiled.table;
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let used = log.local_use();
                Ok((
                    View::read(table, session, &log),
                    Usage {
                        actor: session.actor(),
                        records: used.records,
                        bytes: used.bytes,
                        mine_records: used.writer_records,
                        mine_bytes: used.writer_bytes,
                    },
                ))
            })
        })
    }

    /// Every eligible operation, oldest claim first, with its writer's machine resolved through
    /// the roster the record names, so a removed Mac's edits stay attributable.
    ///
    /// # Errors
    /// As [`Self::view`].
    pub fn history(&self) -> Result<Vec<Entry>> {
        reopened(crate::admission::deadline(), || {
            application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
                let log = Log::read_with(session, store, scope, decode)?;
                let resolve = owned::commons::historical(session);
                let mut entries: Vec<Entry> = log
                    .into_eligible()
                    .filter_map(|(operation, command)| {
                        Some(Entry {
                            operation: operation.header.id,
                            actor: operation.header.actor,
                            machine: resolve(&operation.roster, &operation.header.actor),
                            authored_ms: operation.authored_ms,
                            request: operation.request,
                            reverts: operation.reverts,
                            changes: command.into_changes()?,
                        })
                    })
                    .collect();
                entries.sort_unstable_by_key(|e| (e.authored_ms, e.operation));
                Ok(entries)
            })
        })
    }

    /// Write a grid save (design §8): under the store's writer, re-read the table and settle
    /// `plan` against it ([`grid::settle`]): each changed register is written only while its
    /// candidates are still the exported ones, a cell that changed meanwhile is left alone and
    /// returned as stale, what is left passes the table's rules ([`Compiled::guard`]), and one
    /// row's changes go in one owner record (at most 64 changes, a fresh random request id each).
    /// A save the rules refuse writes nothing and comes back with [`Applied::refused`] set, for
    /// the grid to show at that row as it shows a problem.
    ///
    /// # Errors
    /// No Commons writer; this Mac's history of the table is incomplete (an operation still
    /// waiting on a causal parent could hold a candidate the comparison cannot see); or a record
    /// cannot be written.
    pub fn apply(&self, collection: &Collection, plan: &Plan) -> Result<Applied> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log);
                if !view.complete {
                    return Err(Error::new(
                        "this table's history here is incomplete; a grid save waits for a complete scan",
                    ));
                }
                // A revealed `Secret` cell's edit is sealed here, in the writing session, under
                // its current epoch; nothing else is held plaintext past this point.
                let sealed;
                let plan = if plan.rows.iter().flat_map(|(_, e)| e).any(|e| e.secret) {
                    let mut owned = plan.clone();
                    let name = self.compiled.name();
                    grid::seal(
                        collection,
                        &mut owned,
                        &sealer(&name, session, scope)?,
                        &view.cells,
                    )?;
                    sealed = owned;
                    &sealed
                } else {
                    plan
                };
                let mut settled = grid::settle(collection, plan, &view.cells);
                if let Some(refused) = (self.compiled.guard)(&mut settled.rows, &view.cells)? {
                    return Ok(Applied {
                        refused: Some(refused),
                        ..Applied::default()
                    });
                }
                let mut applied = Applied {
                    unchanged: settled.unchanged,
                    stale: settled.stale,
                    born: settled.born,
                    retired: settled.retired,
                    ..Applied::default()
                };
                for changes in pack(settled.rows)? {
                    let count = changes.len();
                    let command = Command::Write {
                        request: crate::crypto::random()?,
                        changes,
                    };
                    let receipt = application::append(session, store, &command.encode()?)?;
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                }
                Ok(applied)
            },
        )
    }

    /// Write rows `derive` computes from the table as it is under the store's writer (the lab
    /// importer's three-way plan, design §9), declaring and enrolling the table when `declare`:
    /// the plan is made on a complete history only, so a withheld operation can never hide an
    /// owner's edit from the comparison; its rows are packed into records of at most 64 changes,
    /// each with a content-derived request id, so a retry returns its first receipt and a re-run
    /// of the same plan writes nothing.
    ///
    /// # Errors
    /// No Commons writer; this Mac's history of the table is incomplete; `derive` fails; or a
    /// record cannot be written.
    pub(crate) fn commit_derived<T>(
        &self,
        declare: bool,
        derive: impl FnOnce(&View) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(declare, false, crate::admission::deadline(), |view, _| {
            derive(view)
        })
    }

    /// [`Self::commit_derived`] held to `deadline`, for a command a person runs and waits on (a
    /// product open's budget is no measure of a pass that has to happen), whose plan also seals
    /// and compares the table's Secret cells (`typed::secret::Sealer`): the importer's
    /// `password` and `number` cells. The audience key stays inside the session that writes the
    /// plan.
    ///
    /// # Errors
    /// As [`Self::commit_derived`], a Secret's key is unavailable, or `deadline` passes first.
    pub(crate) fn commit_sealed_until<T>(
        &self,
        declare: bool,
        deadline: std::time::Instant,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(declare, false, deadline, derive)
    }

    /// Read the table with its Secret cells' sealer, as [`Self::commit_sealed_until`] plans, writing
    /// nothing: the importer's dry run and comparisons, `check`'s duplicate numbers.
    ///
    /// # Errors
    /// As [`Self::view`], or a Secret's key is unavailable.
    pub(crate) fn view_sealed<T>(
        &self,
        read: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<T>,
    ) -> Result<T> {
        let compiled = self.compiled;
        application::observe_store(&self.paths, self.descriptor(), |session, store, scope| {
            let view = View::read(
                compiled.table,
                session,
                &Log::read_with(session, store, scope, decode)?,
            );
            let name = compiled.name();
            read(&view, &sealer(&name, session, scope)?)
        })
    }

    /// Observe the table's Secret-cell sealer without re-reading or projecting its log, for a
    /// caller that already holds the projected [`View`] (`check`'s duplicate phone numbers).
    ///
    /// # Errors
    /// As [`Self::view`], or a Secret's key is unavailable.
    pub(crate) fn with_sealer<T>(
        &self,
        read: impl FnOnce(&dyn typed::secret::Sealer) -> Result<T>,
    ) -> Result<T> {
        let compiled = self.compiled;
        application::observe_store(&self.paths, self.descriptor(), |session, _, scope| {
            let name = compiled.name();
            read(&sealer(&name, session, scope)?)
        })
    }

    /// [`Self::commit_derived`] for the owner's own word (`aterm db alab add|retire|relabel`): each
    /// record takes a random request id, as a grid save does, so the owner saying a value again
    /// after saying another (a relabel back and forth, a retire undone and redone) is written,
    /// never taken for a retry of the first record. A repeat of a value the table holds writes
    /// nothing anyway: the verbs write only the cells that change. The rows pass the table's
    /// rules first ([`Compiled::guard`]), as a grid save's do.
    ///
    /// # Errors
    /// As [`Self::commit_derived`], or the table's rules refuse the rows.
    pub(crate) fn commit_owner<T>(
        &self,
        derive: impl FnOnce(&View) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(false, true, crate::admission::deadline(), |view, _| {
            derive(view)
        })
    }

    /// [`Self::commit_owner`] with the table's sealer: the owner's own word that writes a `Secret`
    /// cell (`aterm db emails set <row> password --stdin`, the grid's revealed column), sealed
    /// inside the writing session under its current epoch. Each record's request id is random.
    ///
    /// # Errors
    /// As [`Self::commit_owner`], or a Secret's key is unavailable.
    pub(crate) fn commit_owner_sealed<T>(
        &self,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        self.commit_planned(false, true, crate::admission::deadline(), derive)
    }

    /// Reveal each of `cells` (`(collection, row, column, token)`) through the one audited
    /// plaintext exit, in one store session: each is journaled ([`crate::alab::secret::reveal`])
    /// before its value is kept, and comes back as a [`typed::secret::Secret`], which prints
    /// masked and is zeroized on drop. A cell whose epoch this Mac holds no key of, or whose token
    /// does not open at its coordinates, comes back `None` and is never journaled, so one such
    /// cell masks itself and no other.
    ///
    /// # Errors
    /// The store cannot be read, or a reveal could not be journaled: nothing is returned then.
    pub fn reveal_cells(
        &self,
        cells: &[(&str, &str, &str, &str)],
        journal: &crate::alab::secret::Journal,
        surface: crate::alab::secret::Surface,
        when_ms: u64,
    ) -> Result<Vec<Option<typed::secret::Secret>>> {
        let table = self.compiled.name();
        application::observe_store(&self.paths, self.descriptor(), |session, _, scope| {
            let mut keys: BTreeMap<Digest, Option<zeroize::Zeroizing<[u8; 32]>>> = BTreeMap::new();
            cells
                .iter()
                .map(|&(collection, row, column, token)| {
                    let cell = typed::secret::Cell {
                        table: &table,
                        collection,
                        row,
                        column,
                    };
                    let Ok(epoch) = typed::secret::epoch(token) else {
                        return Ok(None);
                    };
                    let Some(key) = keys
                        .entry(epoch)
                        .or_insert_with(|| session.secret_key(scope, &epoch).ok())
                        .as_ref()
                    else {
                        return Ok(None);
                    };
                    let Ok(plain) = typed::secret::open(token, key, cell) else {
                        return Ok(None);
                    };
                    crate::alab::secret::record_reveal(journal, cell, surface, when_ms)?;
                    Ok(Some(plain))
                })
                .collect()
        })
    }

    /// The plan `derive` makes, written: `owner` is the owner's own word, whose records take
    /// random request ids and whose rows pass the table's rules ([`Compiled::guard`]); otherwise
    /// each record's request id derives from its content, so a retry returns its first receipt.
    fn commit_planned<T>(
        &self,
        declare: bool,
        owner: bool,
        deadline: std::time::Instant,
        derive: impl FnOnce(&View, &dyn typed::secret::Sealer) -> Result<(T, Vec<Vec<Change>>)>,
    ) -> Result<(T, Applied)> {
        let table = self.compiled.table;
        let name = self.compiled.name();
        application::with_store_until(
            &self.paths,
            declare,
            self.descriptor(),
            deadline,
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let view = View::read(table, session, &log);
                if !view.complete {
                    return Err(Error::new(
                        "this table's history here is incomplete; an import waits for a complete scan",
                    ));
                }
                let (planned, mut rows) = derive(&view, &sealer(&name, session, scope)?)?;
                if owner && let Some(refused) = (self.compiled.guard)(&mut rows, &view.cells)? {
                    return Err(refused.into());
                }
                let mut applied = Applied::default();
                let mut appended = BTreeMap::new();
                for changes in pack(rows)? {
                    let count = changes.len();
                    let request = if owner {
                        crate::crypto::random()?
                    } else {
                        typed::request_id(table.application, table.role, &changes)?
                    };
                    let command = Command::Write { request, changes };
                    if !owner
                        && let Some(first) =
                            retry_or_appended(&log, session.actor(), &command, &appended)?
                    {
                        applied.unchanged += count;
                        applied.operations.push(first);
                        continue;
                    }
                    let receipt = application::append(session, store, &command.encode()?)?;
                    if !owner {
                        appended.insert(request, (receipt.id, command));
                    }
                    applied.records += 1;
                    applied.cells += count;
                    applied.operations.push(receipt.id);
                }
                Ok((planned, applied))
            },
        )
    }

    /// Append the exact compensation of `operation` while every key it wrote still holds it as
    /// the sole candidate (`Log::compensate`); a later edit stays. The target, current value and
    /// preimage use the same quota-filtered history as [`Self::view`]. The compensation passes
    /// the table's rules as any owner write does ([`Compiled::guard`]), and is written exactly:
    /// a cell the rules would add is not. An exact retry of `request` returns the first operation.
    ///
    /// # Errors
    /// No Commons writer, the operation is not an eligible typed record here, a key it wrote
    /// changed since, or the table's rules refuse its compensation.
    pub fn revert(&self, request: [u8; 16], operation: Digest) -> Result<Digest> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            false,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                if let Some(id) = log.revert_retry(session.actor(), request, operation)? {
                    return Ok(id);
                }
                let view = View::read(table, session, &log);
                let command = log.compensate(request, operation, &view.quota.over)?;
                if let Some(changes) = command.changes()
                    && let Some(refused) =
                        (self.compiled.guard)(&mut [changes.to_vec()], &view.cells)?
                {
                    return Err(refused.into());
                }
                Ok(application::append(session, store, &command.encode()?)?.id)
            },
        )
    }

    /// Publish the compiled schema rows (design §3.5), an owner verb: only where the stored row
    /// is absent or below the compiled version, never automatically when a build opens the
    /// table, never below a version already visible and never a second row at an equal version.
    /// Returns the records written.
    ///
    /// # Errors
    /// No Commons writer, or the table cannot be read or written.
    pub fn publish_schema(&self) -> Result<usize> {
        let table = self.compiled.table;
        application::with_store(
            &self.paths,
            true,
            self.descriptor(),
            |session, store, scope| {
                session.writer()?;
                let log = Log::read_with(session, store, scope, decode)?;
                log.writable(session.actor())?;
                let mut written = 0;
                let mut appended = BTreeMap::new();
                for changes in pack(schema_rows(table, &View::read(table, session, &log)))? {
                    let request = typed::request_id(table.application, table.role, &changes)?;
                    let command = Command::Write { request, changes };
                    if retry_or_appended(&log, session.actor(), &command, &appended)?.is_some() {
                        continue;
                    }
                    let receipt = application::append(session, store, &command.encode()?)?;
                    appended.insert(request, (receipt.id, command));
                    written += 1;
                }
                Ok(written)
            },
        )
    }
}

/// Check whether `command` is an exact retry of an eligible record in `log` or of a record already
/// appended in the current write batch (`appended`), without re-reading the store log on every
/// append.
pub(crate) fn retry_or_appended(
    log: &Log,
    actor: Actor,
    command: &Command,
    appended: &BTreeMap<[u8; 16], (Digest, Command)>,
) -> Result<Option<Digest>> {
    if let Some(first) = log.retry(actor, command)? {
        return Ok(Some(first));
    }
    if let Some((first, prev)) = appended.get(&command.request()) {
        if prev != command {
            return Err(Error::new(
                "request identity reused with different commands",
            ));
        }
        return Ok(Some(*first));
    }
    Ok(None)
}

/// The schema rows a build compiled at these versions may publish over `view`: a collection
/// whose stored row is absent or below the compiled version. Never one below a version already
/// visible, and never a second row at an equal version (that would split it).
pub(crate) fn schema_rows(table: &Table, view: &View) -> Vec<Vec<Change>> {
    table
        .collections
        .iter()
        .filter(|collection| match view.schema(collection.name) {
            SchemaState::Absent => true,
            SchemaState::One { version, .. } | SchemaState::Split { version } => {
                version < collection.version
            }
        })
        .map(|collection| {
            vec![Change {
                collection: typed::SCHEMA.to_owned(),
                key: collection.name.to_owned(),
                value: Some(collection.schema_row()),
            }]
        })
        .collect()
}

/// Read a regular UTF-8 file of at most `limit` bytes without following a final symlink or
/// blocking on a FIFO (`LOCK_OPEN_FLAGS`).
pub(crate) fn read_regular_utf8(
    path: &std::path::Path,
    limit: usize,
    unreadable: &str,
    oversize: &str,
    non_utf8: &str,
) -> Result<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let err = || Error::new(unreadable);
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(path)
        .map_err(|_| err())?;
    let meta = file.metadata().map_err(|_| err())?;
    if !meta.is_file() {
        return Err(err());
    }
    if meta.len() > limit as u64 {
        return Err(Error::new(oversize));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len().saturating_add(1)).unwrap_or(0));
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| err())?;
    if bytes.len() > limit {
        return Err(Error::new(oversize));
    }
    String::from_utf8(bytes).map_err(|_| Error::new(non_utf8))
}

#[cfg(test)]
mod tests;
