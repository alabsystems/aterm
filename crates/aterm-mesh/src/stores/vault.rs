// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's stores, as the node service hands them to its mesh: the Commons table stores
//! (`aterm_db`), read and written under the same per-batch checks a repair batch runs
//! (`aterm_db::replica::Replica::mesh_ingest`), and the membership log's records file
//! (`aterm_secrets::client`).
//!
//! **The sinks.** [`Stores::hook`] installs this process's store sinks once: each durable append
//! to a table store, and each durable write of the records file, feeds the egress open now. A
//! process that hosts no mesh (a CLI's `db` or `members` write, a window) installs [`wake_sinks`]
//! instead, which send the node service a [`Wake`] naming the store and its new length.

use super::{Digest, Held, Kind, ProofKey, Stores, TableKey, Verdict};
use crate::egress::{Feed, Wake, sinks};
use aterm_db::replica::Replica;
use aterm_secrets::client::Paths;
use aterm_secrets::membership_log::{MemberVerdict, Record};
use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long one batch waits for the stores' local admission.
const ADMISSION: Duration = Duration::from_secs(10);

/// The vault's stores.
#[derive(Clone, Debug)]
pub struct VaultStores {
    paths: Paths,
    config: PathBuf,
}

impl VaultStores {
    /// The stores of the vault `paths` keeps, under the Commons policy in `config` (`aterm.toml`).
    #[must_use]
    pub const fn new(paths: Paths, config: PathBuf) -> Self {
        Self { paths, config }
    }

    fn replica(&self) -> aterm_db::Result<Replica> {
        Replica::commons_until(
            self.paths.clone(),
            self.config.clone(),
            Instant::now() + ADMISSION,
        )
    }

    fn members(&self) -> io::Result<aterm_secrets::membership_log::Store> {
        aterm_secrets::client::load_records(&self.paths)
            .map_err(|e| io::Error::other(e.to_string()))
    }
}

fn io_of(error: &aterm_db::Error) -> io::Error {
    io::Error::other(error.0.clone())
}

fn kind_of(kind: Kind) -> aterm_db::replica::Kind {
    match kind {
        Kind::Operation => aterm_db::replica::Kind::Operation,
        Kind::Conflict => aterm_db::replica::Kind::Conflict,
    }
}

fn verdict_of(verdict: aterm_db::replica::Verdict) -> Verdict {
    match verdict {
        aterm_db::replica::Verdict::Applied(_) => Verdict::Applied,
        aterm_db::replica::Verdict::Waiting(why) => Verdict::Waiting(why),
        aterm_db::replica::Verdict::Refused(why) => Verdict::Refused(why),
        aterm_db::replica::Verdict::Busy(why) => Verdict::Busy(why),
    }
}

/// A batch the stores could not admit: tried again after the next membership change where the
/// failure names this Mac's view (it holds no seat yet), and soon where it was contention.
fn unadmitted(error: &aterm_db::Error, n: usize) -> Vec<Verdict> {
    let why = error.0.clone();
    let verdict = if error.is_awaiting_membership() {
        Verdict::Waiting(why)
    } else {
        Verdict::Busy(why)
    };
    vec![verdict; n]
}

impl Stores for VaultStores {
    fn ingest_table(&self, table: Digest, kind: Kind, wires: &[Vec<u8>]) -> Vec<Verdict> {
        let ingested = self
            .replica()
            .and_then(|replica| replica.mesh_ingest(table, kind_of(kind), wires));
        match ingested {
            Ok(verdicts) => verdicts.into_iter().map(verdict_of).collect(),
            Err(e) => unadmitted(&e, wires.len()),
        }
    }

    fn ingest_members(&self, records: &[Vec<u8>]) -> Vec<Verdict> {
        let mut verdicts: Vec<Option<Verdict>> = vec![None; records.len()];
        let mut decoded = Vec::with_capacity(records.len());
        let mut at = Vec::with_capacity(records.len());
        for (i, bytes) in records.iter().enumerate() {
            match Record::decode(bytes) {
                Ok(record) => {
                    decoded.push(record);
                    at.push(i);
                }
                Err(e) => verdicts[i] = Some(Verdict::Refused(e.to_string())),
            }
        }
        if !decoded.is_empty() {
            match aterm_secrets::client::ingest_mesh_records(&self.paths, decoded) {
                Ok(outcomes) => {
                    for (i, (_, outcome)) in at.iter().zip(outcomes) {
                        verdicts[*i] = Some(match outcome {
                            MemberVerdict::Applied => Verdict::Applied,
                            MemberVerdict::Waiting(why) => Verdict::Waiting(why),
                            MemberVerdict::Refused(why) => Verdict::Refused(why),
                        });
                    }
                }
                Err(e) => {
                    for i in &at {
                        verdicts[*i] = Some(Verdict::Busy(e.to_string()));
                    }
                }
            }
        }
        verdicts
            .into_iter()
            .map(|v| v.unwrap_or_else(|| Verdict::Busy("no outcome".to_owned())))
            .collect()
    }

    fn table_lengths(&self) -> io::Result<Vec<(TableKey, u64)>> {
        Ok(aterm_db::table::store::held_lengths(&self.paths)
            .map_err(|e| io_of(&e))?
            .into_iter()
            .map(|(table, recovery, length)| (TableKey { table, recovery }, length))
            .collect())
    }

    fn table_held(&self, key: TableKey, from: u64, upto: u64) -> io::Result<Vec<Held>> {
        Ok(
            aterm_db::table::store::read_held(&self.paths, key.table, key.recovery, from, upto)
                .map_err(|e| io_of(&e))?
                .into_iter()
                .map(|held| Held {
                    offset: held.offset,
                    kind: match held.kind {
                        aterm_db::table::store::AppendKind::Operation => Kind::Operation,
                        aterm_db::table::store::AppendKind::Conflict => Kind::Conflict,
                    },
                    wire: held.wire,
                })
                .collect(),
        )
    }

    fn members_held(&self, from: usize) -> io::Result<(Vec<Vec<u8>>, usize)> {
        let store = self.members()?;
        let Some(log) = store.log() else {
            return Ok((Vec::new(), 0));
        };
        let records = log
            .records()
            .skip(from)
            .map(|record| record.bytes().to_vec())
            .collect();
        Ok((records, log.held_count()))
    }

    fn tables(&self) -> io::Result<BTreeSet<Digest>> {
        self.replica()
            .and_then(|replica| replica.mesh_tables())
            .map_err(|e| io_of(&e))
    }

    fn table_inventory(&self, table: Digest) -> io::Result<Option<BTreeSet<Digest>>> {
        self.replica()
            .and_then(|replica| replica.mesh_inventory(table))
            .map_err(|e| io_of(&e))
    }

    fn table_wires(&self, table: Digest, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        self.replica()
            .and_then(|replica| replica.mesh_wires(table, ids))
            .map_err(|e| io_of(&e))
    }

    fn table_proofs(&self, table: Digest) -> io::Result<Option<BTreeSet<ProofKey>>> {
        self.replica()
            .and_then(|replica| replica.mesh_proofs(table))
            .map_err(|e| io_of(&e))
    }

    fn table_proof_wires(&self, table: Digest, keys: &[ProofKey]) -> io::Result<Vec<Vec<u8>>> {
        self.replica()
            .and_then(|replica| replica.mesh_proof_wires(table, keys))
            .map_err(|e| io_of(&e))
    }

    fn members_inventory(&self) -> io::Result<BTreeSet<Digest>> {
        Ok(self.members()?.complete_inventory())
    }

    fn members_wires(&self, ids: &[Digest]) -> io::Result<Vec<Vec<u8>>> {
        let wanted: BTreeSet<Digest> = ids.iter().copied().collect();
        let store = self.members()?;
        Ok(store.log().map_or_else(Vec::new, |log| {
            log.records()
                .filter(|record| wanted.contains(record.id()))
                .map(|record| record.bytes().to_vec())
                .collect()
        }))
    }

    fn hook(&self) {
        install();
    }
}

/// The node service's state dir a process that hosts no open mesh wakes ([`wake_sinks`]).
static WAKE_TO: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Install this process's store sinks, once: each durable append to a table store, and each
/// durable write of the records file, feeds this process's open egress where one is open, and
/// otherwise sends the node service named by [`wake_sinks`] a [`Wake`]. Neither waits.
fn install() {
    sinks::install_once(|| {
        aterm_db::table::store::set_append_sink(|appended| {
            if let Some(egress) = sinks::open() {
                egress.feed(table_feed(appended));
            } else if let Some(state_dir) = WAKE_TO.get() {
                crate::egress::wake(
                    state_dir,
                    &Wake::Table {
                        key: TableKey {
                            table: appended.table,
                            recovery: appended.recovery,
                        },
                        length: appended.length,
                    },
                );
            }
        });
        aterm_secrets::membership_log::on_written(|written| {
            if let Some(egress) = sinks::open() {
                egress.feed(members_feed(written.store, egress.members_cursor()));
            } else if let Some(state_dir) = WAKE_TO.get() {
                let length = written
                    .store
                    .log()
                    .map_or(0, aterm_secrets::membership_log::Log::held_count);
                crate::egress::wake(state_dir, &Wake::Members { length });
            }
        });
    });
}

fn kind_of_appended(kind: aterm_db::table::store::AppendKind) -> Kind {
    match kind {
        aterm_db::table::store::AppendKind::Operation => Kind::Operation,
        aterm_db::table::store::AppendKind::Conflict => Kind::Conflict,
    }
}

/// What one durable append to a table store tells the egress in this process.
fn table_feed(appended: &aterm_db::table::store::Appended<'_>) -> Feed {
    Feed::Table {
        key: TableKey {
            table: appended.table,
            recovery: appended.recovery,
        },
        length: appended.length,
        record: Some((
            Held {
                offset: appended.offset,
                kind: kind_of_appended(appended.kind),
                wire: appended.wire.to_vec(),
            },
            appended.via == aterm_db::table::store::Via::Mesh,
        )),
    }
}

/// What one durable write of the records file tells the egress in this process: the complete
/// records from the egress's cursor on.
fn members_feed(store: &aterm_secrets::membership_log::Store, cursor: Option<usize>) -> Feed {
    let Some(log) = store.log() else {
        return Feed::Members {
            length: 0,
            records: None,
        };
    };
    let from = cursor.unwrap_or(0);
    Feed::Members {
        length: log.held_count(),
        records: Some((
            from,
            log.records()
                .skip(from)
                .map(|record| record.bytes().to_vec())
                .collect(),
        )),
    }
}

/// Install this process's store sinks ([`install`]) for any process of the installation whose
/// node service's state dir is `state_dir`: while this process hosts no open mesh, each durable
/// append and each durable write of the records file sends that service a [`Wake`] naming the
/// store and its new length. The first call's `state_dir` holds for the process.
pub fn wake_sinks(state_dir: &Path) {
    let _ = WAKE_TO.set(state_dir.to_path_buf());
    install();
}

#[cfg(test)]
mod tests {
    use super::*;
    use aterm_db::table::store::{AppendKind, Appended, Via};

    /// A store append feeds the egress its record with its provenance: a local append (a write,
    /// or what an old peer's repair round brought) is published, one the mesh brought is not.
    #[test]
    fn an_append_feeds_its_record_marked_by_how_it_came() {
        for (via, mesh_borne) in [(Via::Local, false), (Via::Mesh, true)] {
            let appended = Appended {
                table: [7; 32],
                recovery: true,
                kind: AppendKind::Conflict,
                offset: 4,
                length: 5,
                via,
                wire: b"wire",
            };
            let Feed::Table {
                key,
                length,
                record: Some((held, borne)),
            } = table_feed(&appended)
            else {
                panic!("a table feed with its record");
            };
            assert_eq!(
                key,
                TableKey {
                    table: [7; 32],
                    recovery: true
                }
            );
            assert_eq!((length, held.offset, held.kind), (5, 4, Kind::Conflict));
            assert_eq!(held.wire, b"wire");
            assert_eq!(borne, mesh_borne);
        }
    }
}
