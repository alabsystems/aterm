// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded application repair after the sealed control exchange. Tables and paths
//! are provisioned locally or discovered through the signed Commons catalog;
//! peers never choose a local path. Every local batch
//! reopens secrets custody and checks current peer membership. No journal lock
//! crosses network I/O. Dynamic subscriptions default on within an enrolled lab.

use crate::{
    Error, Result,
    control::{self, Side},
    table::{Scope, Session, conflict::ProofKey, store::Store},
};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_secrets::{
    client::Paths,
    commons::{bootstrap::Bootstrap, journal::Trust},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::PathBuf,
};

const MAX_TABLES: usize = 64;
const MAX_RECORDS: usize = 8192;
const MAX_PROOFS: usize = MAX_RECORDS * 2 + aterm_secrets::commons::constraints::MAX_BINDINGS;
const BATCH: usize = 16;
const PROBE: &[u8] = b"ACTP3";
const READY: &[u8] = b"ACTA3";

/// A completed bounded application round, never a claim of global convergence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// The selected table, if both peers had one in common.
    pub table: Option<Digest>,
    /// Local retained operation count before this table's positive repair.
    /// This is inventory knowledge, never a claim of complete network history.
    pub retained: usize,
    pub sent: usize,
    pub received: usize,
    pub deferred: usize,
    pub keys_received: usize,
    pub conflicts_sent: usize,
    pub conflicts_received: usize,
}
/// Volatile fairness hints. Inventory and durable receipts decide what is missing.
#[derive(Clone, Default, Debug)]
pub struct Repair {
    history: Option<Digest>,
    table: Option<Digest>,
    records: BTreeMap<Digest, Digest>,
    conflicts: BTreeMap<Digest, ProofKey>,
}

/// A locally provisioned table set. The existing control channel supplies the
/// authenticated peer actor; merely constructing this object opens no network.
pub struct Replica {
    paths: Paths,
    trust: Trust,
    actor: Actor,
    tables: BTreeMap<Digest, (Scope, PathBuf)>,
    commons: Option<(crate::catalog::Directory, PathBuf)>,
}
impl Replica {
    pub fn new(paths: Paths, tables: Vec<(Scope, PathBuf)>) -> Result<Self> {
        if tables.len() > MAX_TABLES {
            return Err(Error::new("too many provisioned tables"));
        }
        let bootstrap = Bootstrap::cached(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("table repair has no secrets custody"))?;
        let session = Session::open(&bootstrap)?;
        let trust = bootstrap.trust();
        let actor = session.actor();
        let mut map = BTreeMap::new();
        for (scope, path) in tables {
            if scope.trust() != trust || map.insert(scope.digest(), (scope, path)).is_some() {
                return Err(Error::new("duplicate table or foreign table trust"));
            }
        }
        Ok(Self {
            paths,
            trust,
            actor,
            tables: map,
            commons: None,
        })
    }
    /// Automatic catalog and application subscriptions. Construction performs
    /// no table initialization and does not stop control repair on table damage.
    pub fn commons(paths: Paths, config: PathBuf) -> Result<Self> {
        let bootstrap = Bootstrap::cached(&paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("Commons repair has no secrets custody"))?;
        let trust = bootstrap.trust();
        let actor = bootstrap
            .request()
            .map_err(crate::table::failure)?
            .binding()
            .actor();
        let directory = crate::catalog::Directory::new(&paths, trust);
        Ok(Self {
            paths,
            trust,
            actor,
            tables: BTreeMap::new(),
            commons: Some((directory, config)),
        })
    }
    pub(crate) fn matches(&self, trust: Trust, actor: Actor) -> bool {
        self.trust == trust && self.actor == actor
    }

    fn observe<T>(
        &self,
        peer: Actor,
        expected: Option<[u8; 64]>,
        run: impl FnOnce(&Session<'_>) -> Result<T>,
    ) -> Result<T> {
        let bootstrap = Bootstrap::cached(&self.paths)
            .map_err(crate::table::failure)?
            .ok_or_else(|| Error::new("table repair lost secrets custody"))?;
        if bootstrap.trust() != self.trust {
            return Err(Error::new("table repair trust changed"));
        }
        #[cfg(test)]
        tests::session_boundary(0);
        let session = Session::open(&bootstrap)?;
        #[cfg(test)]
        tests::session_boundary(1);
        if session.actor() != self.actor {
            return Err(Error::new("table repair local identity changed"));
        }
        session.peer(peer)?;
        if expected.is_some_and(|expected| session.observation().ok() != Some(expected)) {
            return Err(Error::new("control history changed during table repair"));
        }
        run(&session)
    }

    fn round(&self, peer: Actor) -> Result<Round<'_>> {
        let policy = self
            .commons
            .as_ref()
            .map(|(_, config)| crate::catalog::policy::Policy::read(config))
            .transpose()?;
        Ok(Round {
            replica: self,
            observation: self.observe(peer, None, |session| session.observation())?,
            policy,
            tables: self.tables.clone(),
            active_table: std::cell::Cell::new(None),
        })
    }

    /// Optional upgrade on an already authenticated, deadline-bounded channel.
    /// Legacy peers close here; the caller preserves their successful control
    /// report on any error. Application errors convey no convergence claim.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "NewMember",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "LostKey",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Legacy",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Retired",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Damaged",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "LostAck",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "Retry",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableRepair",
            action = "DamagedNeighbor",
            project = "aterm_db::replica::tests::repair_transition"
        )
    )]
    pub(crate) fn exchange(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        let round = self.round(peer)?;
        round.exchange(stream, peer, side, repair)
    }
}

#[derive(Clone, Copy)]
enum BatchKind {
    Operation,
    Conflict,
}

struct Inventory {
    known: BTreeSet<Digest>,
    offered: BTreeSet<Digest>,
}

struct Round<'a> {
    replica: &'a Replica,
    observation: [u8; 64],
    policy: Option<crate::catalog::policy::Policy>,
    tables: BTreeMap<Digest, (Scope, PathBuf)>,
    active_table: std::cell::Cell<Option<Digest>>,
}
impl std::ops::Deref for Round<'_> {
    type Target = Replica;
    fn deref(&self) -> &Replica {
        self.replica
    }
}
impl Round<'_> {
    fn store(&self, session: &Session<'_>, id: Digest) -> Result<Store> {
        let (scope, path) = &self.tables[&id];
        if let Some((directory, _)) = &self.commons {
            directory.enroll(session, *scope)
        } else {
            Store::open(session, path, *scope)
        }
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Attempt",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Retry",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbRepairPolicy",
            action = "Restore",
            project = "aterm_db::replica::tests::policy::policy_transition"
        )
    )]
    fn local<T>(&self, peer: Actor, run: impl FnOnce(&Session<'_>) -> Result<T>) -> Result<T> {
        self.replica
            .observe(peer, Some(self.observation), |session| {
                // Custody and journal replay can take time. Read local policy
                // after opening the locked session, before admitting this batch.
                // Config writers need not take that lock: later edits take
                // effect at the next admission, not retroactively on this one.
                if let Some((directory, config)) = &self.commons {
                    let policy = crate::catalog::policy::Policy::read(config)?;
                    if !policy.enabled
                        || Some(&policy) != self.policy.as_ref()
                        || policy
                            .excluded
                            .contains(&directory.catalog_scope().digest())
                    {
                        return Err(Error::new(
                            "Commons subscription policy changed or disabled",
                        ));
                    }
                }
                if let Some((directory, _)) = &self.commons
                    && let Some(id) = self.active_table.get()
                    && !directory.permits(
                        session,
                        self.policy.as_ref().expect("Commons policy captured"),
                        id,
                    )?
                {
                    return Err(Error::new("table subscription changed during repair"));
                }
                run(session)
            })
    }

    fn exchange(
        mut self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        self.local(peer, |_| Ok(()))?;
        match side {
            Side::Initiator => {
                self.send_probe(stream, PROBE)?;
                self.receive_probe(stream, READY)?;
            }
            Side::Responder => {
                self.receive_probe(stream, PROBE)?;
                self.local(peer, |_| Ok(()))?;
                self.send_probe(stream, READY)?;
            }
        }
        let mut report = Report::default();
        self.keys(stream, peer, side, repair, &mut report)?;
        if let Some((directory, _)) = &self.commons {
            self.tables = self.local(peer, |session| {
                directory.subscriptions(
                    session,
                    self.policy.as_ref().expect("Commons policy captured"),
                )
            })?;
        }
        repair.records.retain(|id, _| self.tables.contains_key(id));
        repair
            .conflicts
            .retain(|id, _| self.tables.contains_key(id));
        let local: BTreeSet<_> = self.local(peer, |_| Ok(self.tables.keys().copied().collect()))?;
        let remote = exchange_ids(stream, side, &local, MAX_TABLES, || {
            self.local(peer, |_| Ok(()))
        })?;
        let common: BTreeSet<_> = local.intersection(&remote).copied().collect();
        let selected = match side {
            Side::Initiator => {
                let chosen = cycle(&common, repair.table).next().copied();
                self.local(peer, |_| Ok(()))?;
                control::send_frame(stream, chosen.as_ref().map_or(&[][..], |id| id.as_slice()))?;
                chosen
            }
            Side::Responder => {
                let chosen = control::receive_frame(stream, 32)?;
                if chosen.is_empty() {
                    None
                } else {
                    Some(
                        chosen
                            .as_slice()
                            .try_into()
                            .map_err(|_| Error::new("invalid selected table"))?,
                    )
                }
            }
        };
        let Some(id) = selected else {
            return Ok(report);
        };
        if !common.contains(&id) {
            return Err(Error::new("peer selected an unprovisioned table"));
        }
        repair.table = Some(id);
        report.table = Some(id);
        self.table(stream, peer, side, id, repair, &mut report)?;
        Ok(report)
    }

    fn send_probe(&self, stream: &mut impl Write, tag: &[u8]) -> Result<()> {
        control::send_frame(stream, &[tag, &self.observation].concat())
    }

    fn receive_probe(&self, stream: &mut impl Read, tag: &[u8]) -> Result<()> {
        let frame = control::receive_frame(stream, tag.len() + self.observation.len())?;
        if frame != [tag, &self.observation].concat() {
            return Err(Error::new(
                "application repair needs matching control history",
            ));
        }
        Ok(())
    }

    fn keys(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        let (needs, next) =
            self.local(peer, |session| session.history_needs(repair.history, BATCH))?;
        repair.history = next;
        let local: BTreeSet<_> = needs.into_iter().collect();
        let remote = exchange_ids(stream, side, &local, BATCH, || self.local(peer, |_| Ok(())))?;
        match side {
            Side::Initiator => {
                self.send_keys(stream, peer, &remote)?;
                self.receive_keys(stream, peer, &local, report)?;
            }
            Side::Responder => {
                self.receive_keys(stream, peer, &local, report)?;
                self.send_keys(stream, peer, &remote)?;
            }
        }
        Ok(())
    }
    fn send_keys(
        &self,
        stream: &mut impl Write,
        peer: Actor,
        needs: &BTreeSet<Digest>,
    ) -> Result<()> {
        let keys = self.local(peer, |session| {
            session.handoffs(peer, &needs.iter().copied().collect::<Vec<_>>())
        })?;
        for key in keys {
            stream.write_all(&[u8::from(key.is_some())])?;
            if let Some(key) = key {
                control::send_frame(stream, &key)?;
            }
        }
        stream.flush()?;
        Ok(())
    }
    fn receive_keys(
        &self,
        stream: &mut impl Read,
        peer: Actor,
        needs: &BTreeSet<Digest>,
        report: &mut Report,
    ) -> Result<()> {
        let mut keys = Vec::with_capacity(needs.len());
        for source in needs {
            let mut present = [0];
            stream.read_exact(&mut present)?;
            match present[0] {
                0 => {
                    keys.push((*source, None));
                }
                1 => {
                    let key = control::receive_frame(
                        stream,
                        aterm_secrets::commons::history::MAX_HANDOFF,
                    )?;
                    keys.push((*source, Some(key)));
                }
                _ => return Err(Error::new("invalid historical key response")),
            }
        }
        // One bounded, fully buffered batch. Validate current membership after
        // its final network read; never hold custody locks while receiving it.
        let accepted = self.local(peer, |session| {
            Ok(keys
                .iter()
                .filter(|(source, key)| {
                    key.as_ref()
                        .is_some_and(|key| session.import_handoff(*source, key).is_ok())
                })
                .count())
        })?;
        report.keys_received += accepted;
        report.deferred += keys.len() - accepted;
        Ok(())
    }

    fn inventory(&self, peer: Actor, id: Digest) -> Result<Option<Inventory>> {
        self.local(peer, |session| {
            let Ok(store) = self.store(session, id) else {
                return Ok(None);
            };
            Ok(Some(Inventory {
                known: store.inventory(session)?.copied().collect(),
                offered: store.replication_inventory(session)?.copied().collect(),
            }))
        })
    }
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbTableConflict",
            action = "Replicate",
            project = "aterm_db::table::store::replay::tests::conflict::conflict_transition"
        )
    )]
    fn conflicts(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        id: Digest,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<bool> {
        let local = self.local(peer, |session| {
            let Ok(store) = self.store(session, id) else {
                return Ok(None);
            };
            store.proof_inventory(session).map(Some)
        })?;
        let remote = exchange_inventory(stream, side, local.as_ref(), MAX_PROOFS, || {
            self.local(peer, |_| Ok(()))
        })?;
        let (Some(local), Some(remote)) = (local, remote) else {
            report.deferred += 1;
            return Ok(false);
        };
        let proofs: Vec<_> = cycle(&local, repair.conflicts.get(&id).copied())
            .filter(|key| {
                let mut fork = [0; 81];
                fork[..48].copy_from_slice(&key[..48]);
                fork[48] = 1;
                !remote.contains(*key) && !remote.contains(&fork)
            })
            .take(BATCH)
            .copied()
            .collect();
        if let Some(last) = proofs.last() {
            repair.conflicts.insert(id, *last);
        }
        let batch = self.local(peer, |session| {
            let store = self.store(session, id)?;
            proofs
                .iter()
                .map(|key| {
                    store
                        .proof_wire(session, key)?
                        .ok_or_else(|| Error::new("table conflict disappeared"))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        match side {
            Side::Initiator => {
                self.send_batch(stream, peer, &batch, BatchKind::Conflict, report)?;
                self.receive_batch(stream, peer, id, BatchKind::Conflict, report)?;
            }
            Side::Responder => {
                self.receive_batch(stream, peer, id, BatchKind::Conflict, report)?;
                self.send_batch(stream, peer, &batch, BatchKind::Conflict, report)?;
            }
        }
        Ok(true)
    }

    fn table(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        side: Side,
        id: Digest,
        repair: &mut Repair,
        report: &mut Report,
    ) -> Result<()> {
        self.active_table.set(Some(id));
        if !self.conflicts(stream, peer, side, id, repair, report)? {
            return Ok(());
        }
        // Reopen after negative evidence is durably applied in both directions.
        let local = self.inventory(peer, id)?;
        let remote = exchange_inventory(
            stream,
            side,
            local.as_ref().map(|inventory| &inventory.known),
            MAX_RECORDS,
            || self.local(peer, |_| Ok(())),
        )?;
        let (Some(local), Some(remote)) = (local, remote) else {
            report.deferred += 1;
            return Ok(());
        };
        report.retained = local.known.len();
        let ids: Vec<_> = cycle(&local.offered, repair.records.get(&id).copied())
            .filter(|v| !remote.contains(*v))
            .take(BATCH)
            .copied()
            .collect();
        if let Some(last) = ids.last() {
            repair.records.insert(id, *last);
        }
        let batch = self.local(peer, |session| {
            let store = self.store(session, id)?;
            ids.iter()
                .map(|id| {
                    store
                        .wire(session, id)?
                        .map(ToOwned::to_owned)
                        .ok_or_else(|| Error::new("table record disappeared"))
                })
                .collect::<Result<Vec<_>>>()
        })?;
        match side {
            Side::Initiator => {
                self.send_batch(stream, peer, &batch, BatchKind::Operation, report)?;
                self.receive_batch(stream, peer, id, BatchKind::Operation, report)?;
            }
            Side::Responder => {
                self.receive_batch(stream, peer, id, BatchKind::Operation, report)?;
                self.send_batch(stream, peer, &batch, BatchKind::Operation, report)?;
            }
        }
        Ok(())
    }
    fn send_batch(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        batch: &[Vec<u8>],
        kind: BatchKind,
        report: &mut Report,
    ) -> Result<()> {
        self.local(peer, |_| Ok(()))?;
        stream.write_all(&(batch.len() as u32).to_le_bytes())?;
        for wire in batch {
            control::send_frame(stream, wire)?;
        }
        stream.flush()?;
        for wire in batch {
            let mut receipt = [0; 33];
            stream.read_exact(&mut receipt)?;
            if receipt[..32] != Sha256::digest(wire) || receipt[32] > 1 {
                return Err(Error::new("invalid table receipt"));
            }
            if receipt[32] == 1 {
                match kind {
                    BatchKind::Operation => report.sent += 1,
                    BatchKind::Conflict => report.conflicts_sent += 1,
                }
            } else {
                report.deferred += 1;
            }
        }
        Ok(())
    }
    fn receive_batch(
        &self,
        stream: &mut (impl Read + Write),
        peer: Actor,
        id: Digest,
        kind: BatchKind,
        report: &mut Report,
    ) -> Result<()> {
        let n = control::read_u32(stream)? as usize;
        if n > BATCH {
            return Err(Error::new("table batch exceeds its bound"));
        }
        let mut wires = Vec::with_capacity(n);
        let mut ids = BTreeSet::new();
        for _ in 0..n {
            let max = match kind {
                BatchKind::Operation => crate::crypto::MAX_OPERATION,
                BatchKind::Conflict => crate::table::conflict::MAX_CONFLICT,
            };
            let wire = control::receive_frame(stream, max)?;
            if !ids.insert(Sha256::digest(&wire)) {
                return Err(Error::new("duplicate table batch record"));
            }
            wires.push(wire);
        }
        let verdicts = self.local(peer, |session| {
            let Ok(mut store) = self.store(session, id) else {
                return Ok(vec![false; n]);
            };
            Ok(wires
                .iter()
                .map(|wire| {
                    let receipt = match kind {
                        BatchKind::Operation => store.ingest(session, wire),
                        BatchKind::Conflict => store.ingest_conflict(session, wire),
                    };
                    receipt.is_ok_and(|r| r.id == Sha256::digest(wire))
                })
                .collect::<Vec<_>>())
        })?;
        self.local(peer, |_| Ok(()))?;
        for (wire, accepted) in wires.iter().zip(verdicts) {
            stream.write_all(&Sha256::digest(wire))?;
            stream.write_all(&[u8::from(accepted)])?;
            if accepted {
                match kind {
                    BatchKind::Operation => report.received += 1,
                    BatchKind::Conflict => report.conflicts_received += 1,
                }
            } else {
                report.deferred += 1;
            }
        }
        stream.flush()?;
        Ok(())
    }
}

fn cycle<const N: usize>(
    ids: &BTreeSet<[u8; N]>,
    after: Option<[u8; N]>,
) -> impl Iterator<Item = &[u8; N]> {
    ids.iter()
        .filter(move |id| after.is_none_or(|after| **id > after))
        .chain(
            ids.iter()
                .filter(move |id| after.is_some_and(|after| **id <= after)),
        )
}
fn exchange_ids<const N: usize>(
    stream: &mut (impl Read + Write),
    side: Side,
    ids: &BTreeSet<[u8; N]>,
    max: usize,
    authorize: impl Fn() -> Result<()>,
) -> Result<BTreeSet<[u8; N]>> {
    match side {
        Side::Initiator => {
            authorize()?;
            send_ids(stream, ids, max)?;
            receive_ids(stream, max)
        }
        Side::Responder => {
            let remote = receive_ids(stream, max)?;
            authorize()?;
            send_ids(stream, ids, max)?;
            Ok(remote)
        }
    }
}
fn send_ids<const N: usize>(
    stream: &mut impl Write,
    ids: &BTreeSet<[u8; N]>,
    max: usize,
) -> Result<()> {
    if ids.len() > max {
        return Err(Error::new("application inventory exceeds its bound"));
    }
    stream.write_all(&(ids.len() as u32).to_le_bytes())?;
    for id in ids {
        stream.write_all(id)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_ids<const N: usize>(stream: &mut impl Read, max: usize) -> Result<BTreeSet<[u8; N]>> {
    let n = control::read_u32(stream)? as usize;
    if n > max {
        return Err(Error::new("application inventory exceeds its bound"));
    }
    let mut ids = BTreeSet::new();
    for _ in 0..n {
        let mut id = [0; N];
        stream.read_exact(&mut id)?;
        if ids.last().is_some_and(|last| last >= &id) {
            return Err(Error::new("application inventory is not sorted and unique"));
        }
        ids.insert(id);
    }
    Ok(ids)
}
fn exchange_inventory<const N: usize>(
    stream: &mut (impl Read + Write),
    side: Side,
    local: Option<&BTreeSet<[u8; N]>>,
    maximum: usize,
    authorize: impl Fn() -> Result<()>,
) -> Result<Option<BTreeSet<[u8; N]>>> {
    match side {
        Side::Initiator => {
            authorize()?;
            send_inventory(stream, local, maximum)?;
            receive_inventory(stream, maximum)
        }
        Side::Responder => {
            let remote = receive_inventory(stream, maximum)?;
            authorize()?;
            send_inventory(stream, local, maximum)?;
            Ok(remote)
        }
    }
}

fn send_inventory<const N: usize>(
    stream: &mut impl Write,
    ids: Option<&BTreeSet<[u8; N]>>,
    maximum: usize,
) -> Result<()> {
    stream.write_all(&[u8::from(ids.is_some())])?;
    if let Some(ids) = ids {
        send_ids(stream, ids, maximum)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_inventory<const N: usize>(
    stream: &mut impl Read,
    maximum: usize,
) -> Result<Option<BTreeSet<[u8; N]>>> {
    let mut ready = [0];
    stream.read_exact(&mut ready)?;
    match ready[0] {
        0 => Ok(None),
        1 => Ok(Some(receive_ids(stream, maximum)?)),
        _ => Err(Error::new("invalid table inventory state")),
    }
}

#[cfg(any(test, debug_assertions))]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "NextPolicy",
        reason = "The conformance fixture selects one of eight independent config edits before admission; Round::local observes the resulting policy but does not choose the writer's edit. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises every selection."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "Receiving",
        reason = "The conformance fixture selects an outgoing or incoming batch; the shared Round::local admission does not select its caller's transfer direction. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises both directions and their retained storage."
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::spec_unmodeled(
        machine = "DbRepairPolicy",
        action = "AfterOpen",
        reason = "The conformance fixture selects whether the independent config edit runs before or after Session::open; this selector is not a production admission step. policy_admission_conforms_and_stale_policy_and_false_receipts_are_detected exercises both boundaries, whose hook fires only at its selected boundary."
    )
)]
#[doc(hidden)]
pub fn db_repair_policy_environment() {}

#[cfg(test)]
pub(crate) mod tests;
