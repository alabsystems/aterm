// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Repair the independent Commons control journal over exporter-bound TLS.
//! Historical authority evidence grants only sealed control-history exchange;
//! it grants no current roster, table, vault, administrator or terminal access.
//! No network epoch key is needed, so a retired/offline device can learn newer
//! restrictions. This module neither dials nor listens: it accepts connections
//! admitted by the native LAN policy and rechecks their bound route observations.

use crate::{Error, Result};
use astream_wire::sync::{Actor, Digest};
use aterm_digest::Sha256;
use aterm_net::fleet::{self, FleetOp, FleetScope, FleetSecret, NodeIdentity, NodeProof};
use aterm_secrets::commons::{
    binding::{REQUEST_LEN, Request},
    constraints::Fact,
    journal::{Journal, MAX_NODES, Trust},
    node::{Kind, MAX_NODE, Node},
};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

mod peers;

/// At most 4.5 MiB of sealed nodes are buffered per direction. The durable
/// inventory, rather than an acknowledgement cursor, determines missing data.
pub const RECORDS_PER_ROUND: usize = 16;
const HELLO_FIXED: usize = 4 + 16 + 32 + 32 + REQUEST_LEN;
const MAX_HELLO: usize = HELLO_FIXED + MAX_NODE;

fn failure(_: impl std::fmt::Display) -> Error {
    Error::new("Commons control journal refused the operation")
}

/// The connection's fixed ordering. This avoids simultaneous large writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Initiator,
    Responder,
}

/// Durable acknowledgements in one bounded round. A zero report does not prove
/// global convergence: other peers or previously unknown branches may exist.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub sent: usize,
    pub received: usize,
    /// Offered nodes without a durable acknowledgement, including local refusal.
    pub deferred: usize,
}

/// Volatile scheduling hint, never a claim of delivery. Keep one per peer.
/// Advancing past a deferred node prevents it from starving later restrictions;
/// wraparound retries it even if the caller does not maintain a timer.
#[derive(Clone, Default, Debug)]
pub struct Repair {
    peer: Option<(Trust, Actor)>,
    cursor: Option<Digest>,
    data: crate::replica::Repair,
    application: Option<crate::replica::Report>,
}
impl Repair {
    /// The most recently channel-authenticated peer; the round may have failed.
    pub fn actor(&self) -> Option<Actor> {
        self.peer.map(|(_, actor)| actor)
    }
    /// Only a fully completed application round has a report. None also covers
    /// legacy peers and deferred/failed upgrades, never a claim of no data.
    pub const fn application(&self) -> Option<crate::replica::Report> {
        self.application
    }
    fn prepare(&mut self, trust: Trust, actor: Actor) {
        if self.peer != Some((trust, actor)) {
            self.cursor = None;
            self.peer = Some((trust, actor));
            self.data = crate::replica::Repair::default();
        }
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "RefusedFirst",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "DeliveredMiddle",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "DbControlRepair",
            action = "DeliveredLast",
            project = "aterm_db::control::tests::repair_transition"
        )
    )]
    fn next(&mut self, local: &BTreeSet<Digest>, remote: &BTreeSet<Digest>) -> Vec<Digest> {
        let ids = select(local, remote, self.cursor);
        if let Some(last) = ids.last() {
            self.cursor = Some(*last);
        }
        ids
    }
}

/// Locally pinned journal plus an authority-approved device signing identity.
/// Construction uses an already enrolled persistent device; it never creates a
/// network, age identity, writer incarnation, epoch key or plaintext key file.
pub struct Endpoint {
    dir: PathBuf,
    trust: Trust,
    actor: Actor,
    identity: NodeIdentity,
    hello: Vec<u8>,
    application: Option<crate::replica::Replica>,
    inbound: peers::Cache,
    /// The absolute deadline of one exchange ([`crate::network::WITHIN`]).
    within: Duration,
}
impl Endpoint {
    /// This endpoint's exchanges wait `within` before their deadline cuts them: a test that
    /// needs every round to complete waits it out as a one-minute hang detector.
    #[cfg(test)]
    pub(crate) const fn within(mut self, within: Duration) -> Self {
        self.within = within;
        self
    }

    /// Attach an explicit local table set. Discovery/subscription is a higher
    /// layer; no table path or admission can come from a peer's packet.
    pub fn with_tables(
        mut self,
        paths: aterm_secrets::client::Paths,
        tables: Vec<(crate::table::Scope, PathBuf)>,
    ) -> Result<Self> {
        let application = crate::replica::Replica::new(paths, tables)?;
        if !application.matches(self.trust, self.actor) {
            return Err(Error::new(
                "application repair has another control identity",
            ));
        }
        self.application = Some(application);
        Ok(self)
    }
    /// Build the control-only endpoint from encrypted secrets custody. Pending
    /// enrollment has no historical witness and cannot open an endpoint.
    pub fn from_bootstrap(
        bootstrap: &aterm_secrets::commons::bootstrap::Bootstrap,
    ) -> Result<Option<Self>> {
        let Some(witness) = bootstrap.witness().map_err(failure)? else {
            return Ok(None);
        };
        let request = bootstrap.request().map_err(failure)?;
        bootstrap
            .device()
            .with_seed(|seed| {
                let endpoint = Self::configured(
                    bootstrap.journal_dir(),
                    bootstrap.trust(),
                    &request,
                    witness.bytes(),
                    seed,
                )?;
                drop(
                    Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust())
                        .map_err(failure)?,
                );
                Ok(endpoint)
            })
            .map(Some)
    }

    /// `witness` is a signed ACN3 node containing this request's exact Bind fact.
    /// A genesis witness must equal the independent pin. Other signed witnesses
    /// establish historical approval only, even before their ancestry arrives.
    /// No journal lock is retained by this handle.
    pub fn new(
        dir: &Path,
        trust: Trust,
        request: &Request,
        witness: &[u8],
        seed: &[u8; 32],
    ) -> Result<Self> {
        let endpoint = Self::configured(dir, trust, request, witness, seed)?;
        drop(Journal::open(dir, trust).map_err(failure)?);
        Ok(endpoint)
    }

    fn configured(
        dir: &Path,
        trust: Trust,
        request: &Request,
        witness: &[u8],
        seed: &[u8; 32],
    ) -> Result<Self> {
        check_witness(trust, request, witness)?;
        let identity = NodeIdentity::from_seed(
            &aterm_codec::hex::encode_lower(request.binding().incarnation()),
            seed,
        )
        .ok_or_else(|| Error::new("invalid Commons signing identity"))?;
        if identity.key().bytes() != request.binding().writer() {
            return Err(Error::new(
                "Commons writer differs from its persistent identity",
            ));
        }
        let mut hello = b"ACX1".to_vec();
        hello.extend_from_slice(&trust.network);
        hello.extend_from_slice(&trust.authority);
        hello.extend_from_slice(&trust.genesis);
        hello.extend_from_slice(request.bytes());
        hello.extend_from_slice(witness);
        Ok(Self {
            dir: dir.into(),
            trust,
            actor: request.binding().actor(),
            identity,
            hello,
            application: None,
            inbound: peers::Cache::default(),
            within: crate::network::WITHIN,
        })
    }

    /// Exchange over a connection admitted by the native LAN/loopback policy.
    /// One absolute 30-second deadline covers TLS, authentication and repair.
    /// Route evidence is rechecked after TLS and after the bounded exchange.
    /// This does not start any automatic network service.
    pub fn exchange(
        &self,
        connection: aterm_net::lan::Connection,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        repair.application = None;
        let (tcp, locality) = connection.into_parts();
        locality.recheck()?;
        let _deadline = crate::network::Deadline::arm_within(&tcp, self.within)?;
        // This PUBLIC discriminator separates networks/protocols. It is not an
        // admission secret. Authority witness + exporter-bound identity below
        // are mandatory before any inventory or control repair is exposed.
        let mut domain = b"aterm/commons/control-transport/1\0".to_vec();
        domain.extend_from_slice(&self.trust.network);
        domain.extend_from_slice(&self.trust.authority);
        domain.extend_from_slice(&self.trust.genesis);
        let discriminator = FleetSecret::new(&Sha256::digest(&domain))
            .map_err(|_| Error::new("invalid Commons protocol discriminator"))?;
        match side {
            Side::Initiator => {
                let mut channel = fleet::client_handshake_as(
                    tcp,
                    &discriminator,
                    Some(&self.identity),
                    FleetOp::Database,
                    Duration::from_secs(30),
                )
                .map_err(|_| Error::new("Commons control TLS handshake failed"))?;
                locality.recheck()?;
                let result = self.connected(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                );
                locality.finish()?;
                result
            }
            Side::Responder => {
                let mut channel = fleet::server_handshake_as(
                    tcp,
                    &discriminator,
                    Some(&self.identity),
                    FleetScope::Facts,
                    &[FleetOp::Database],
                    Duration::from_secs(30),
                )
                .map_err(|_| Error::new("Commons control TLS handshake failed"))?;
                locality.recheck()?;
                let result = self.connected(
                    channel.transport.stream(),
                    channel.peer.as_ref(),
                    side,
                    repair,
                );
                locality.finish()?;
                result
            }
        }
    }

    fn connected(
        &self,
        stream: &mut (impl Read + Write),
        proof: Option<&NodeProof>,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        if proof.is_none() {
            return Err(Error::new("Commons requires a channel-bound node proof"));
        }
        if side == Side::Initiator {
            send_frame(stream, &self.hello)?;
        }
        let hello = receive_frame(stream, MAX_HELLO)?;
        let actor = authenticate(self.trust, self.actor, proof, &hello)?;
        repair.prepare(self.trust, actor);
        repair.application = None;
        if side == Side::Responder {
            let mut lease = self.inbound.acquire(actor, repair)?;
            lease.prepare(self.trust, actor);
            lease.application = None;
            send_frame(stream, &self.hello)?;
            self.repair_connected(stream, actor, side, &mut lease)
        } else {
            self.repair_connected(stream, actor, side, repair)
        }
    }

    fn repair_connected(
        &self,
        stream: &mut (impl Read + Write),
        actor: Actor,
        side: Side,
        repair: &mut Repair,
    ) -> Result<Report> {
        let report = exchange(
            stream,
            &Disk {
                dir: &self.dir,
                trust: self.trust,
            },
            side,
            repair,
        )?;
        if let Some(application) = &self.application {
            repair.application = application
                .exchange(stream, actor, side, &mut repair.data)
                .ok();
        }
        Ok(report)
    }
}

fn check_witness(trust: Trust, request: &Request, wire: &[u8]) -> Result<()> {
    let binding = request.binding();
    if trust.genesis == [0; 32]
        || *binding.network() != trust.network
        || *binding.authority() != trust.authority
    {
        return Err(Error::new("Commons device belongs to another network"));
    }
    let node = Node::verify(wire, trust.network, trust.authority).map_err(failure)?;
    if (node.kind() == Kind::Genesis && node.id() != trust.genesis)
        || !node
            .facts()
            .iter()
            .any(|fact| matches!(fact, Fact::Bind { binding: approved } if approved == binding))
    {
        return Err(Error::new(
            "Commons peer lacks historical authority approval",
        ));
    }
    Ok(())
}

#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "Approved",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "MissingProof",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "CopiedWitness",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ForeignPins",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ForeignGenesis",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "Unapproved",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
#[cfg_attr(
    any(test, debug_assertions),
    aterm_spec_anchor::refines(
        machine = "DbControlAdmission",
        action = "ReflectedWriter",
        project = "aterm_db::control::tests::admission_transition"
    )
)]
fn authenticate(
    trust: Trust,
    own: Actor,
    proof: Option<&NodeProof>,
    hello: &[u8],
) -> Result<Actor> {
    if hello.len() <= HELLO_FIXED
        || hello.len() > MAX_HELLO
        || &hello[..4] != b"ACX1"
        || hello[4..20] != trust.network
        || hello[20..52] != trust.authority
        || hello[52..84] != trust.genesis
    {
        return Err(Error::new("Commons peer protocol or trust pins differ"));
    }
    let request = Request::decode(&hello[84..HELLO_FIXED]).map_err(failure)?;
    check_witness(trust, &request, &hello[HELLO_FIXED..])?;
    let peer = proof.ok_or_else(|| Error::new("Commons requires a channel-bound node proof"))?;
    let actor = request.binding().actor();
    if peer.key.bytes().as_slice() != &actor[..32]
        || peer.node != aterm_codec::hex::encode_lower(&actor[32..])
        || actor[..32] == own[..32]
    {
        return Err(Error::new(
            "Commons peer proof differs from its approved device",
        ));
    }
    Ok(actor)
}

// Each call acquires and releases one local lock. No Disk method performs IO
// on the network. Even a slow remote cannot own the local journal's writer lock.
trait Store {
    fn inventory(&self) -> Result<BTreeSet<Digest>>;
    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>>;
    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>>;
}
struct Disk<'a> {
    dir: &'a Path,
    trust: Trust,
}
impl Store for Disk<'_> {
    fn inventory(&self) -> Result<BTreeSet<Digest>> {
        let journal = Journal::open_existing(self.dir, self.trust).map_err(failure)?;
        Ok(journal.inventory().copied().collect())
    }
    fn batch(&self, ids: &[Digest]) -> Result<Vec<Vec<u8>>> {
        let journal = Journal::open_existing(self.dir, self.trust).map_err(failure)?;
        ids.iter()
            .map(|id| {
                journal
                    .node(id)
                    .map(|node| node.bytes().to_vec())
                    .ok_or_else(|| Error::new("Commons control record disappeared"))
            })
            .collect()
    }
    fn retain(&self, wires: &[Vec<u8>]) -> Result<Vec<bool>> {
        // Reject garbage before taking the writer lock or replaying a large
        // journal. Authenticated legacy evidence is still retained: omitting it
        // could hide restrictions that this version cannot safely interpret.
        let valid: Vec<bool> = wires
            .iter()
            .map(|wire| {
                Node::verify(wire, self.trust.network, self.trust.authority).is_ok_and(|node| {
                    node.kind() != Kind::Genesis || node.id() == self.trust.genesis
                })
            })
            .collect();
        if !valid.iter().any(|valid| *valid) {
            return Ok(valid);
        }
        let mut journal = Journal::open_existing(self.dir, self.trust).map_err(failure)?;
        Ok(wires
            .iter()
            .zip(valid)
            .map(|(wire, valid)| {
                // Journal verifies signatures and pins and commits its checkpoint
                // before returning a receipt. Uncertain appends yield no positive ack.
                valid
                    && journal
                        .ingest(wire)
                        .is_ok_and(|receipt| receipt.id == Sha256::digest(wire))
            })
            .collect())
    }
}

fn exchange(
    stream: &mut (impl Read + Write),
    store: &impl Store,
    side: Side,
    repair: &mut Repair,
) -> Result<Report> {
    let local = store.inventory()?;
    let remote = match side {
        Side::Initiator => {
            send_inventory(stream, &local)?;
            receive_inventory(stream)?
        }
        Side::Responder => {
            let remote = receive_inventory(stream)?;
            send_inventory(stream, &local)?;
            remote
        }
    };
    let ids = repair.next(&local, &remote);
    let batch = store.batch(&ids)?;
    let (sent, received, deferred) = match side {
        Side::Initiator => {
            let (sent, out) = send_batch(stream, &batch)?;
            let (received, incoming) = receive_batch(stream, store)?;
            (sent, received, out + incoming)
        }
        Side::Responder => {
            let (received, incoming) = receive_batch(stream, store)?;
            let (sent, out) = send_batch(stream, &batch)?;
            (sent, received, out + incoming)
        }
    };
    Ok(Report {
        sent,
        received,
        deferred,
    })
}

fn select(
    local: &BTreeSet<Digest>,
    remote: &BTreeSet<Digest>,
    cursor: Option<Digest>,
) -> Vec<Digest> {
    local
        .iter()
        .filter(|id| cursor.is_none_or(|cursor| **id > cursor))
        .chain(
            local
                .iter()
                .filter(|id| cursor.is_some_and(|cursor| **id <= cursor)),
        )
        .filter(|id| !remote.contains(*id))
        .take(RECORDS_PER_ROUND)
        .copied()
        .collect()
}
fn send_inventory(stream: &mut impl Write, ids: &BTreeSet<Digest>) -> Result<()> {
    if ids.len() > MAX_NODES {
        return Err(Error::new("Commons inventory exceeds its bound"));
    }
    stream.write_all(&(ids.len() as u32).to_le_bytes())?;
    for id in ids {
        stream.write_all(id)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_inventory(stream: &mut impl Read) -> Result<BTreeSet<Digest>> {
    let n = read_u32(stream)? as usize;
    if n > MAX_NODES {
        return Err(Error::new("Commons inventory exceeds its bound"));
    }
    let mut ids = BTreeSet::new();
    for _ in 0..n {
        let mut id = [0; 32];
        stream.read_exact(&mut id)?;
        if ids.last().is_some_and(|previous| previous >= &id) {
            return Err(Error::new("Commons inventory is not sorted and unique"));
        }
        ids.insert(id);
    }
    Ok(ids)
}
fn send_batch(stream: &mut (impl Read + Write), batch: &[Vec<u8>]) -> Result<(usize, usize)> {
    stream.write_all(&(batch.len() as u32).to_le_bytes())?;
    for wire in batch {
        send_frame(stream, wire)?;
    }
    stream.flush()?;
    let mut retained = 0;
    for wire in batch {
        let mut ack = [0; 33];
        stream.read_exact(&mut ack)?;
        if ack[..32] != Sha256::digest(wire) || ack[32] > 1 {
            return Err(Error::new("Commons peer returned an invalid receipt"));
        }
        retained += usize::from(ack[32]);
    }
    Ok((retained, batch.len() - retained))
}
fn receive_batch(stream: &mut (impl Read + Write), store: &impl Store) -> Result<(usize, usize)> {
    let n = read_u32(stream)? as usize;
    if n > RECORDS_PER_ROUND {
        return Err(Error::new("Commons batch exceeds its bound"));
    }
    let mut wires = Vec::with_capacity(n);
    let mut ids = BTreeSet::new();
    for _ in 0..n {
        let wire = receive_frame(stream, MAX_NODE)?;
        if !ids.insert(Sha256::digest(&wire)) {
            return Err(Error::new("duplicate Commons batch record"));
        }
        wires.push(wire);
    }
    // Acquire the local journal only after the entire bounded batch has arrived.
    let verdicts = store.retain(&wires).unwrap_or_else(|_| vec![false; n]);
    if verdicts.len() != n {
        return Err(Error::new("invalid local Commons receipt count"));
    }
    let mut retained = 0;
    for (wire, durable) in wires.iter().zip(verdicts) {
        stream.write_all(&Sha256::digest(wire))?;
        stream.write_all(&[u8::from(durable)])?;
        retained += usize::from(durable);
    }
    stream.flush()?;
    Ok((retained, n - retained))
}
pub(crate) fn send_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}
pub(crate) fn receive_frame(stream: &mut impl Read, max: usize) -> Result<Vec<u8>> {
    let n = read_u32(stream)? as usize;
    if n > max {
        return Err(Error::new("Commons frame exceeds its bound"));
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
pub(crate) fn read_u32(stream: &mut impl Read) -> Result<u32> {
    let mut bytes = [0; 4];
    stream.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests;
