// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LAN anti-entropy over aterm-net's exporter-bound authenticated TLS. A dedicated
//! operation and dataset-derived transport key grant no terminal capabilities.
//! Exact digest inventories include pending records and fork evidence. All received
//! operations pass signature/schema checks and strict append before acknowledgement.

use crate::{
    Error, Result,
    crypto::{Capability, MAX_OPERATION},
    database::MAX_RECORDS,
    local::{Credentials, Database},
};
use astream_wire::sync::{Actor, Digest};
use aterm_net::fleet::{self, FleetOp, FleetScope, FleetSecret, NodeIdentity, NodeProof};
use aterm_net::lan::{Connection, Listener, Scope};
use aterm_uds::stop::Stop;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{Duration, Instant},
};

/// Bounded repair work per connection; subsequent rounds resume from durable inventory.
pub const RECORDS_PER_ROUND: usize = 256;
/// One exchange's absolute deadline.
#[cfg(not(test))]
pub(crate) const WITHIN: Duration = Duration::from_secs(30);
/// A unit test's peer is its own listener, which serves: a round a loaded machine slows waits the
/// one-minute hang detector rather than being cut (as [`crate::control`]'s exchange does).
#[cfg(test)]
pub(crate) const WITHIN: Duration = aterm_spec::wait::MUST_HAPPEN;
/// How long [`serve_until`]'s listener gives a connection's TLS and both proofs: aterm-net's
/// [`fleet::HANDSHAKE_WITHIN`].
#[cfg(not(test))]
const HANDSHAKE: Duration = fleet::HANDSHAKE_WITHIN;
/// A unit test's dialer is its own runtime, which finishes the handshake: the hang detector.
#[cfg(test)]
const HANDSHAKE: Duration = aterm_spec::wait::MUST_HAPPEN;

/// Durably exchanged operation counts for this peer and round. Zero counts do
/// not prove global convergence or the absence of another peer's unseen history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Records this side sent and the peer durably acknowledged.
    pub sent: usize,
    /// Newly retained records this side acknowledged.
    pub received: usize,
    /// Offered records not retained, or temporarily skipped after deferral.
    pub deferred: usize,
}

/// Bounded per-peer retry hints. Deferral is never a durable acknowledgement and
/// expires so increased capacity or repaired local state is retried automatically.
#[derive(Clone, Debug, Default)]
pub struct Repair {
    peer: Option<Actor>,
    since: Option<Instant>,
    deferred: BTreeSet<Digest>,
    cursor: Option<Digest>,
}
impl Repair {
    fn prepare(&mut self, actor: Actor) {
        self.prepare_at(actor, Instant::now());
    }
    fn prepare_at(&mut self, actor: Actor, now: Instant) {
        if self.peer != Some(actor) {
            self.cursor = None;
        }
        if self.peer != Some(actor)
            || self
                .since
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_secs(60))
        {
            self.peer = Some(actor);
            self.since = Some(now);
            self.deferred.clear();
        }
    }
}
type Repairs = Mutex<BTreeMap<Actor, Repair>>;

/// Retry `attempt` while another local command holds the database. Each holds it for one edit or
/// exchange, so a command waits its turn rather than failing beside a neighbour.
pub(crate) fn while_busy<T>(mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    let deadline = Instant::now() + WITHIN;
    loop {
        match attempt() {
            Err(error) if error.is_busy() && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            outcome => return outcome,
        }
    }
}

pub(crate) struct Deadline {
    done: mpsc::Sender<Instant>,
    join: Option<thread::JoinHandle<()>>,
}
impl Deadline {
    pub(crate) fn arm(tcp: &TcpStream) -> Result<Self> {
        Self::arm_within(tcp, WITHIN)
    }

    /// [`Self::arm`] with its own deadline.
    pub(crate) fn arm_within(tcp: &TcpStream, within: Duration) -> Result<Self> {
        Self::arm_with_clock(tcp, within, Instant::now)
    }

    fn arm_with_clock(
        tcp: &TcpStream,
        within: Duration,
        now: impl Fn() -> Instant + Send + 'static,
    ) -> Result<Self> {
        // Socket preparation and thread scheduling consume the original budget.
        let started = now();
        let socket = aterm_uds::kept_out(|| tcp.try_clone())?;
        let (done, wait) = mpsc::channel::<Instant>();
        let join = thread::Builder::new()
            .name("db-exchange-deadline".into())
            .spawn(move || {
                let remaining = within.saturating_sub(now().saturating_duration_since(started));
                // A queued cancellation keeps its own completion time: a late
                // worker must honor an early finish, but not a late one.
                let finished = wait
                    .recv_timeout(remaining)
                    .is_ok_and(|completed| completed.saturating_duration_since(started) < within);
                if !finished {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            })?;
        Ok(Self {
            done,
            join: Some(join),
        })
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        let _ = self.done.send(Instant::now());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn transport(credentials: &Credentials) -> Result<(FleetSecret, NodeIdentity)> {
    let prk = zeroize::Zeroizing::new(aterm_digest::hkdf_extract(
        b"aterm-db/transport/1",
        &credentials.key,
    ));
    let key = zeroize::Zeroizing::new(aterm_digest::hkdf_expand_32(&prk, &credentials.manifest.id));
    let secret =
        FleetSecret::new(key.as_ref()).map_err(|_| Error::new("invalid dataset transport key"))?;
    let node = aterm_codec::hex::encode_lower(&credentials.capability.actor[32..]);
    let identity = NodeIdentity::from_seed(&node, &credentials.seed)
        .ok_or_else(|| Error::new("invalid database channel identity"))?;
    Ok((secret, identity))
}
fn authenticate(
    credentials: &Credentials,
    peer: Option<&NodeProof>,
    capability: &[u8],
) -> Result<Actor> {
    let capability = Capability::decode(&credentials.manifest, capability)?;
    if capability.control {
        return Err(Error::new(
            "administrative certificates do not grant peer transport access",
        ));
    }
    let peer = peer.ok_or_else(|| Error::new("database requires a node-bound TLS proof"))?;
    if peer.key.bytes().as_slice() != &capability.actor[..32]
        || crate::database::decode_hex::<16>(&peer.node)
            .ok()
            .is_none_or(|incarnation| incarnation != capability.actor[32..])
        || capability.actor == credentials.capability.actor
    {
        return Err(Error::new(
            "database channel proof differs from its certified installation",
        ));
    }
    Ok(capability.actor)
}
pub(crate) fn send_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_OPERATION {
        return Err(Error::new("outgoing database frame too large"));
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    frame.extend_from_slice(bytes);
    stream.write_all(&frame)?;
    stream.flush()?;
    Ok(())
}
pub(crate) fn receive_frame(stream: &mut impl Read, max: usize) -> Result<Vec<u8>> {
    let n = read_u32(stream)? as usize;
    if n > max {
        return Err(Error::new("peer database frame exceeds bound"));
    }
    let mut bytes = vec![0; n];
    stream.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn read_u32(stream: &mut impl Read) -> Result<u32> {
    let mut n = [0; 4];
    stream.read_exact(&mut n)?;
    Ok(u32::from_le_bytes(n))
}
fn send_inventory(stream: &mut impl Write, ids: &BTreeSet<Digest>) -> Result<()> {
    let mut frame = Vec::with_capacity(4 + ids.len() * 32);
    frame.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    for id in ids {
        frame.extend_from_slice(id);
    }
    stream.write_all(&frame)?;
    stream.flush()?;
    Ok(())
}
fn receive_inventory(stream: &mut impl Read) -> Result<BTreeSet<Digest>> {
    let n = read_u32(stream)? as usize;
    if n > MAX_RECORDS {
        return Err(Error::new("peer inventory exceeds database capacity"));
    }
    let mut wire = vec![0; n * 32];
    stream.read_exact(&mut wire)?;
    let mut ids = BTreeSet::new();
    let mut previous = None;
    for &id in wire.as_chunks::<32>().0 {
        if previous.is_some_and(|p| p >= id) {
            return Err(Error::new("peer inventory is not sorted and unique"));
        }
        previous = Some(id);
        ids.insert(id);
    }
    Ok(ids)
}
fn send_operations(
    stream: &mut (impl Read + Write),
    dir: &Path,
    remote: &BTreeSet<Digest>,
    repair: &mut Repair,
) -> Result<(usize, usize)> {
    repair.deferred.retain(|id| !remote.contains(id));
    let records = {
        let db = while_busy(|| Database::open(dir))?;
        let cursor = repair.cursor;
        // Sweep past deferred prefixes even when retry hints expired. A peer
        // returning infrequently must not offer the same rejected page forever.
        db.history
            .index
            .inventory()
            .skip_while(|id| cursor.is_some_and(|cursor| **id <= cursor))
            .chain(
                db.history
                    .index
                    .inventory()
                    .take_while(|id| cursor.is_some_and(|cursor| **id <= cursor)),
            )
            .filter(|id| !remote.contains(*id) && !repair.deferred.contains(*id))
            .take(RECORDS_PER_ROUND)
            .map(|id| {
                db.history
                    .wire(id)
                    .map(|wire| (*id, wire.to_vec()))
                    .ok_or_else(|| Error::new("missing local operation bytes"))
            })
            .collect::<Result<Vec<_>>>()?
    };
    stream.write_all(&(records.len() as u32).to_le_bytes())?;
    let mut sent = 0;
    for (id, wire) in &records {
        send_frame(stream, wire)?;
        let mut ack = [0u8; 33];
        stream.read_exact(&mut ack)?;
        if ack[..32] != *id {
            return Err(Error::new(
                "peer did not acknowledge the exact durable operation",
            ));
        }
        match ack[32] {
            0 | 1 => {
                sent += 1;
            }
            2 => {
                if repair.deferred.len() < RECORDS_PER_ROUND {
                    repair.deferred.insert(*id);
                }
            }
            _ => return Err(Error::new("unknown database retention verdict")),
        }
        repair.cursor = Some(*id);
    }
    stream.flush()?;
    Ok((sent, repair.deferred.len()))
}
fn receive_operations(stream: &mut (impl Read + Write), dir: &Path) -> Result<(usize, usize)> {
    let n = read_u32(stream)? as usize;
    if n > RECORDS_PER_ROUND {
        return Err(Error::new("peer repair batch exceeds bound"));
    }
    let mut received = 0;
    let mut deferred = 0;
    for _ in 0..n {
        let wire = receive_frame(stream, MAX_OPERATION)?;
        let id = aterm_digest::Sha256::digest(&wire);
        // All bytes arrive before taking the writer lock. A silent or slow peer
        // can consume a connection slot but cannot hold up a local edit. A neighbour's
        // turn at the lock is no reason to defer: the peer would not offer it again for a while.
        let retained = while_busy(|| {
            Database::open(dir).and_then(|mut db| {
                let fresh = db.ingest(&wire)?;
                let refused = db.history.is_refused(&id);
                Ok((fresh, refused))
            })
        });
        let verdict = match retained {
            Ok((fresh, refused)) => {
                received += usize::from(fresh);
                u8::from(refused)
            }
            Err(_) => {
                deferred += 1;
                2
            }
        };
        let mut receipt = [0u8; 33];
        receipt[..32].copy_from_slice(&id);
        receipt[32] = verdict;
        stream.write_all(&receipt)?;
        stream.flush()?;
    }
    Ok((received, deferred))
}
/// How many of the peer's operations this replica still does not hold. A peer stops offering one
/// it was refused for its retry window, so a round in which it offers nothing proves nothing.
pub(crate) fn unretained(dir: &Path, remote: &BTreeSet<Digest>) -> Result<usize> {
    let db = while_busy(|| Database::open(dir))?;
    Ok(remote
        .iter()
        .filter(|id| db.history.wire(id).is_none())
        .count())
}

/// Receive one connection with native link evidence. No journal lock is held
/// before peer authentication, and locality is checked around the exchange.
pub fn serve_connection(dir: &Path, connection: Connection) -> Result<Report> {
    let (tcp, locality) = connection.into_parts();
    locality.recheck()?;
    let _deadline = Deadline::arm(&tcp)?;
    let credentials = Credentials::load(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let mut accepted = fleet::server_handshake_as(
        tcp,
        &secret,
        Some(&identity),
        FleetScope::Facts,
        &[FleetOp::Database],
        WITHIN,
    )
    .map_err(|_| Error::new("database TLS peer authentication failed"))?;
    locality.recheck()?;
    let stream = accepted.transport.stream();
    let result = server_exchange(
        dir,
        &credentials,
        accepted.peer.as_ref(),
        stream,
        &Mutex::new(BTreeMap::new()),
    );
    locality.recheck()?;
    result
}
fn server_exchange(
    dir: &Path,
    credentials: &Credentials,
    peer: Option<&NodeProof>,
    stream: &mut (impl Read + Write),
    repairs: &Repairs,
) -> Result<Report> {
    crate::fleet_db::refuse_unfinished(dir)?;
    let capability = receive_frame(stream, 256)?;
    let actor = authenticate(credentials, peer, &capability)?;
    let mut repair = repairs
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(&actor)
        .cloned()
        .unwrap_or_default();
    repair.prepare(actor);
    send_frame(stream, credentials.capability.bytes())?;
    let remote = receive_inventory(stream)?;
    let local = while_busy(|| Database::open(dir))?
        .history
        .index
        .inventory()
        .copied()
        .collect();
    send_inventory(stream, &local)?;
    let (received, incoming_deferred) = receive_operations(stream, dir)?;
    let (sent, outgoing_deferred) = send_operations(stream, dir, &remote, &mut repair)?;
    let mut repairs = repairs.lock().unwrap_or_else(|p| p.into_inner());
    if repairs.len() < 64 {
        repairs.insert(actor, repair);
    } else if let std::collections::btree_map::Entry::Occupied(mut entry) = repairs.entry(actor) {
        entry.insert(repair);
    }
    Ok(Report {
        sent,
        received,
        deferred: incoming_deferred + outgoing_deferred,
    })
}
/// Exchange one bounded round with a certified LAN peer. Neither side is a leader.
pub fn sync_round(dir: &Path, peer: SocketAddr) -> Result<Report> {
    sync_round_with(dir, peer, &mut Repair::default())
}
/// Continue repair past a peer's temporary deferrals without claiming they synced.
pub fn sync_round_with(dir: &Path, peer: SocketAddr, repair: &mut Repair) -> Result<Report> {
    let credentials = Credentials::load(dir)?;
    crate::fleet_db::refuse_unfinished(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let (tcp, locality) =
        Connection::connect(peer, scope(peer), Duration::from_secs(3))?.into_parts();
    locality.recheck()?;
    let _deadline = Deadline::arm(&tcp)?;
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .map_err(|_| Error::new("database TLS peer authentication failed"))?;
    locality.recheck()?;
    let stream = dialed.transport.stream();
    send_frame(stream, credentials.capability.bytes())?;
    let capability = receive_frame(stream, 256)?;
    let actor = authenticate(&credentials, dialed.peer.as_ref(), &capability)?;
    repair.prepare(actor);
    let local = while_busy(|| Database::open(dir))?
        .history
        .index
        .inventory()
        .copied()
        .collect();
    send_inventory(stream, &local)?;
    let remote = receive_inventory(stream)?;
    let (sent, outgoing_deferred) = send_operations(stream, dir, &remote, repair)?;
    let (received, incoming_deferred) = receive_operations(stream, dir)?;
    locality.recheck()?;
    Ok(Report {
        sent,
        received,
        deferred: incoming_deferred.max(unretained(dir, &remote)?) + outgoing_deferred,
    })
}
fn scope(address: SocketAddr) -> Scope {
    if address.ip().is_loopback() {
        Scope::Loopback
    } else {
        Scope::Lan
    }
}
/// Serve on an explicitly selected LAN address. Optional peers are retried in a
/// separate thread with random jitter, so partitions do not stop local editing.
pub fn serve(dir: &Path, listener: Listener, peers: Vec<SocketAddr>) -> Result<()> {
    serve_until(
        dir,
        listener,
        Arc::new(Stop::new()),
        Arc::new(Mutex::new(peers)),
    )
}
/// A bounded listener and repair worker owned by the session runtime. Discovery
/// may update dial hints; every candidate still proves its certified TLS identity.
pub fn serve_until(
    dir: &Path,
    listener: Listener,
    stop: Arc<Stop>,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
) -> Result<()> {
    let credentials = Credentials::load(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let outbound = {
        let dir = dir.to_path_buf();
        let stop = stop.clone();
        thread::Builder::new()
            .name("db-anti-entropy".into())
            .spawn(move || {
                let mut repairs = BTreeMap::new();
                let mut peer_list = Vec::new();
                while !stop.is_set() {
                    peer_list.clone_from(&peers.lock().unwrap_or_else(|p| p.into_inner()));
                    for peer in &peer_list {
                        if stop.is_set() {
                            break;
                        }
                        // Bound cached retry hints, not the set of machines we
                        // actually contact. A sixty-fifth peer must still repair.
                        let mut repair = repairs.remove(peer).unwrap_or_default();
                        let _ = sync_round_with(&dir, *peer, &mut repair);
                        if repairs.len() >= 64 {
                            repairs.pop_first();
                        }
                        repairs.insert(*peer, repair);
                    }
                    let jitter = crate::crypto::random::<1>().map_or(0, |b| u64::from(b[0]));
                    stop.sleep(Duration::from_millis(500 * (10 + jitter / 40)));
                }
            })?
    };
    let stopped = Arc::new(Mutex::new(None));
    let error = stopped.clone();
    let dir = dir.to_path_buf();
    let repairs = Arc::new(Mutex::new(BTreeMap::new()));
    fleet::serve_lan_within(
        listener,
        &fleet::Listening {
            secret,
            scope: FleetScope::Facts,
            node: Some(identity),
            serves: &[FleetOp::Database],
            slots: 16,
            per_source: 4,
            lifetime: WITHIN,
        },
        &stop,
        HANDSHAKE,
        move |served, _| {
            let peer = served.peer_node().cloned();
            let credentials = Credentials::load(&dir).map_err(std::io::Error::other)?;
            served
                .answer(|stream| {
                    server_exchange(&dir, &credentials, peer.as_ref(), stream, &repairs)
                        .map_err(std::io::Error::other)
                })
                .map(|_| ())
        },
        move |event| {
            if let fleet::FleetEvent::Stopped(reason) = event {
                *error.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
            }
        },
    );
    stop.set();
    let _ = outbound.join();
    let error = stopped.lock().unwrap_or_else(|p| p.into_inner()).take();
    error.map_or(Ok(()), |reason| Err(Error::new(&reason)))
}

/// Exchange one bounded round with a fleet peer over exporter-bound TLS (`FleetOp::Database`),
/// allowing normal OS routing across multi-homed fleet interfaces.
pub fn sync_fleet_round_with(dir: &Path, peer: SocketAddr, repair: &mut Repair) -> Result<Report> {
    let credentials = Credentials::load(dir)?;
    crate::fleet_db::refuse_unfinished(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let tcp = aterm_uds::connect_tcp(&peer, Duration::from_secs(3))?;
    let _deadline = Deadline::arm(&tcp)?;
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .map_err(|_| Error::new("database TLS peer authentication failed"))?;
    let stream = dialed.transport.stream();
    send_frame(stream, credentials.capability.bytes())?;
    let capability = receive_frame(stream, 256)?;
    let actor = authenticate(&credentials, dialed.peer.as_ref(), &capability)?;
    repair.prepare(actor);
    let local = while_busy(|| Database::open(dir))?
        .history
        .index
        .inventory()
        .copied()
        .collect();
    send_inventory(stream, &local)?;
    let remote = receive_inventory(stream)?;
    let (sent, outgoing_deferred) = send_operations(stream, dir, &remote, repair)?;
    let (received, incoming_deferred) = receive_operations(stream, dir)?;
    Ok(Report {
        sent,
        received,
        deferred: incoming_deferred.max(unretained(dir, &remote)?) + outgoing_deferred,
    })
}

/// Serve the fleet database over a standard `TcpListener` (supporting `0.0.0.0:7485` and
/// dual-homed fleet Macs) with full `FleetOp::Database` TLS and capability authentication.
pub fn serve_fleet_until(
    dir: &Path,
    listener: std::net::TcpListener,
    stop: Arc<Stop>,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
) -> Result<()> {
    let credentials = Credentials::load(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let outbound = {
        let dir = dir.to_path_buf();
        let stop = stop.clone();
        thread::Builder::new()
            .name("db-fleet-anti-entropy".into())
            .spawn(move || {
                let mut repairs = BTreeMap::new();
                let mut peer_list = Vec::new();
                while !stop.is_set() {
                    peer_list.clone_from(&peers.lock().unwrap_or_else(|p| p.into_inner()));
                    for peer in &peer_list {
                        if stop.is_set() {
                            break;
                        }
                        let mut repair = repairs.remove(peer).unwrap_or_default();
                        let _ = sync_fleet_round_with(&dir, *peer, &mut repair);
                        if repairs.len() >= 64 {
                            repairs.pop_first();
                        }
                        repairs.insert(*peer, repair);
                    }
                    let jitter = crate::crypto::random::<1>().map_or(0, |b| u64::from(b[0]));
                    stop.sleep(Duration::from_millis(500 * (10 + jitter / 40)));
                }
            })?
    };
    let stopped = Arc::new(Mutex::new(None));
    let error = stopped.clone();
    let dir = dir.to_path_buf();
    let repairs = Arc::new(Mutex::new(BTreeMap::new()));
    let listening = fleet::Listening {
        secret,
        scope: FleetScope::Facts,
        node: Some(identity),
        serves: &[FleetOp::Database],
        slots: 16,
        per_source: 4,
        lifetime: WITHIN,
    };
    const SERVED_OPS: &[FleetOp] = &[FleetOp::Database];
    fleet::serve_offering(
        &listener,
        &listening,
        Arc::new(|| (FleetScope::Facts, SERVED_OPS)),
        &stop,
        move |served, _| {
            let peer = served.peer_node().cloned();
            let credentials = Credentials::load(&dir).map_err(std::io::Error::other)?;
            served
                .answer(|stream| {
                    server_exchange(&dir, &credentials, peer.as_ref(), stream, &repairs)
                        .map_err(std::io::Error::other)
                })
                .map(|_| ())
        },
        move |event| {
            if let fleet::FleetEvent::Stopped(reason) = event {
                *error.lock().unwrap_or_else(|p| p.into_inner()) = Some(reason);
            }
        },
    );
    stop.set();
    let _ = outbound.join();
    let error = stopped.lock().unwrap_or_else(|p| p.into_inner()).take();
    error.map_or(Ok(()), |reason| Err(Error::new(&reason)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{Change, Command};
    use aterm_json::json;
    use std::net::TcpListener;

    #[test]
    fn deadline_worker_does_not_restart_an_expired_exchange() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = aterm_uds::accept(
            &listener,
            aterm_uds::AcceptWait::Forever,
            aterm_uds::AcceptedMode::Blocking,
        )
        .unwrap();
        tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
        let creator = thread::current().id();
        let started = Instant::now();
        let budget = aterm_spec::wait::MUST_HAPPEN * 10;
        let deadline = Deadline::arm_with_clock(&tcp, budget, move || {
            if thread::current().id() == creator {
                started
            } else {
                started + budget * 2
            }
        })
        .unwrap();
        let mut byte = [0];
        let read = (&tcp).read(&mut byte);
        assert!(
            matches!(&read, Ok(0))
                || read.as_ref().is_err_and(|error| !matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )),
            "an expired exchange must close without a new timeout: {read:?}"
        );
        drop(deadline);
    }

    #[test]
    fn deadline_worker_judges_queued_completion_at_the_original_deadline() {
        let budget = aterm_spec::wait::MUST_HAPPEN * 10;
        for completed_after in [budget - Duration::from_nanos(1), budget] {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let tcp = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
            let (mut peer, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            tcp.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
                .unwrap();
            let creator = thread::current().id();
            let started = Instant::now();
            let (release_worker, released) = mpsc::channel::<()>();
            let deadline = Deadline::arm_with_clock(&tcp, budget, move || {
                if thread::current().id() == creator {
                    started
                } else {
                    released
                        .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
                        .expect("the queued completion releases its deadline worker");
                    started + budget * 2
                }
            })
            .unwrap();
            // Queue the completion before the delayed worker can read it.
            deadline.done.send(started + completed_after).unwrap();
            release_worker.send(()).unwrap();
            drop(deadline);
            let mut byte = [0];
            if completed_after < budget {
                peer.write_all(b"x").unwrap();
                assert_eq!((&tcp).read(&mut byte).unwrap(), 1);
                assert_eq!(byte, [b'x']);
            } else {
                let read = (&tcp).read(&mut byte);
                assert!(
                    matches!(&read, Ok(0))
                        || read.as_ref().is_err_and(|error| !matches!(
                            error.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        )),
                    "completion at the deadline must not cancel expiry: {read:?}"
                );
            }
        }
    }

    struct DeferredPeer {
        answers: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }
    impl Read for DeferredPeer {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.answers.read(bytes)
        }
    }
    impl Write for DeferredPeer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    fn refusing(ids: impl IntoIterator<Item = Digest>) -> DeferredPeer {
        let mut answers = Vec::new();
        for id in ids {
            answers.extend_from_slice(&id);
            answers.push(2);
        }
        DeferredPeer {
            answers: std::io::Cursor::new(answers),
            written: Vec::new(),
        }
    }
    #[test]
    fn repair_sweeps_past_a_deferred_page_after_retry_hints_expire() {
        let root = crate::tests::tmp();
        let dir = root.path().join("db");
        crate::local::init(&dir, "test").unwrap();
        let mut db = Database::open(&dir).unwrap();
        for n in 0..=(RECORDS_PER_ROUND as u128) {
            db.commit(Command::Write {
                request: n.to_le_bytes(),
                changes: vec![Change {
                    collection: "example".into(),
                    key: format!("r{n}"),
                    value: Some(json!(n as u64)),
                }],
            })
            .unwrap();
        }
        let ids: Vec<_> = db.history.index.inventory().copied().collect();
        drop(db);
        let now = Instant::now();
        let actor = [1; 48];
        let mut repair = Repair::default();
        repair.prepare_at(actor, now);
        let mut first = refusing(ids[..RECORDS_PER_ROUND].iter().copied());
        assert_eq!(
            send_operations(&mut first, &dir, &BTreeSet::new(), &mut repair).unwrap(),
            (0, RECORDS_PER_ROUND)
        );
        repair.prepare_at(actor, now + Duration::from_secs(61));
        assert!(repair.deferred.is_empty());
        let expected = ids[RECORDS_PER_ROUND..]
            .iter()
            .chain(ids[..RECORDS_PER_ROUND - 1].iter())
            .copied();
        let mut second = refusing(expected);
        send_operations(&mut second, &dir, &BTreeSet::new(), &mut repair).unwrap();
        let frame_len = u32::from_le_bytes(second.written[4..8].try_into().unwrap()) as usize;
        assert_eq!(
            aterm_digest::Sha256::digest(&second.written[8..8 + frame_len]),
            ids[RECORDS_PER_ROUND],
            "a deferred prefix cannot starve the record after it"
        );
    }

    #[test]
    fn bounded_listener_repairs_both_replicas_and_stops_without_a_peer() {
        let (_root, a, b) = crate::tests::pair();
        for (dir, request, key) in [(&a, 1, "a"), (&b, 2, "b")] {
            Database::open(dir)
                .unwrap()
                .commit(Command::Write {
                    request: [request; 16],
                    changes: vec![Change {
                        collection: "example".into(),
                        key: key.into(),
                        value: Some(json!(request)),
                    }],
                })
                .unwrap();
        }
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(Stop::new());
        let server = {
            let a = a.clone();
            let stop = stop.clone();
            thread::spawn(move || serve_until(&a, listener, stop, Arc::new(Mutex::new(Vec::new()))))
        };
        let first = sync_round(&b, address);
        let second = first.as_ref().ok().map(|_| sync_round(&b, address));
        stop.set();
        server.join().unwrap().unwrap();
        let first = first.unwrap();
        assert_eq!((first.sent, first.received, first.deferred), (1, 1, 0));
        assert_eq!(second.unwrap().unwrap(), Report::default());
        let inventory = |dir: &Path| {
            Database::open(dir)
                .unwrap()
                .history
                .index
                .inventory()
                .copied()
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(inventory(&a), inventory(&b));
    }

    #[test]
    fn an_authenticated_peer_waiting_for_its_next_frame_does_not_hold_the_writer_lock() {
        let (_root, a, b) = crate::tests::pair();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = a.clone();
        let server = thread::spawn(move || {
            let connection = Connection::admit(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                Scope::Loopback,
            )
            .unwrap();
            serve_connection(&served, connection).unwrap()
        });
        let credentials = Credentials::load(&b).unwrap();
        let (secret, identity) = transport(&credentials).unwrap();
        let tcp = TcpStream::connect(address).unwrap();
        let _deadline = Deadline::arm(&tcp).unwrap();
        let mut dialed =
            fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
                .unwrap();
        let stream = dialed.transport.stream();
        send_frame(stream, credentials.capability.bytes()).unwrap();
        let certificate = receive_frame(stream, 256).unwrap();
        authenticate(&credentials, dialed.peer.as_ref(), &certificate).unwrap();
        send_inventory(stream, &BTreeSet::new()).unwrap();
        assert!(receive_inventory(stream).unwrap().is_empty());
        Database::open(&a)
            .expect("a peer waiting on network IO must leave the local writer available")
            .commit(Command::Write {
                request: [1; 16],
                changes: vec![Change {
                    collection: "example".into(),
                    key: "local".into(),
                    value: Some(json!("works")),
                }],
            })
            .unwrap();
        stream.write_all(&0u32.to_le_bytes()).unwrap();
        stream.flush().unwrap();
        assert_eq!(receive_operations(stream, &b).unwrap(), (1, 0));
        assert_eq!(server.join().unwrap().sent, 1);
    }

    #[test]
    fn a_refused_frame_is_not_a_durable_ack_and_does_not_stop_the_next_operation() {
        let (_root, a, b) = crate::tests::pair();
        Database::open(&b)
            .unwrap()
            .commit(Command::Write {
                request: [1; 16],
                changes: vec![Change {
                    collection: "example".into(),
                    key: "valid".into(),
                    value: Some(json!(true)),
                }],
            })
            .unwrap();
        let wire = crate::tests::wires(&b).pop().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let served = a.clone();
        let server = thread::spawn(move || {
            let connection = Connection::admit(
                aterm_uds::accept(
                    &listener,
                    aterm_uds::AcceptWait::Forever,
                    aterm_uds::AcceptedMode::Blocking,
                )
                .unwrap()
                .0,
                Scope::Loopback,
            )
            .unwrap();
            serve_connection(&served, connection).unwrap()
        });
        let credentials = Credentials::load(&b).unwrap();
        let (secret, identity) = transport(&credentials).unwrap();
        let tcp = TcpStream::connect(address).unwrap();
        let _deadline = Deadline::arm(&tcp).unwrap();
        let mut dialed =
            fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
                .unwrap();
        let stream = dialed.transport.stream();
        send_frame(stream, credentials.capability.bytes()).unwrap();
        receive_frame(stream, 256).unwrap();
        send_inventory(
            stream,
            &BTreeSet::from([aterm_digest::Sha256::digest(&wire)]),
        )
        .unwrap();
        receive_inventory(stream).unwrap();
        stream.write_all(&2u32.to_le_bytes()).unwrap();
        for (bytes, expected) in [(&[0u8; 32][..], 2u8), (wire.as_slice(), 0)] {
            send_frame(stream, bytes).unwrap();
            let mut digest = [0; 32];
            stream.read_exact(&mut digest).unwrap();
            assert_eq!(digest, aterm_digest::Sha256::digest(bytes));
            let mut verdict = [0];
            stream.read_exact(&mut verdict).unwrap();
            assert_eq!(verdict, [expected]);
        }
        assert_eq!(receive_operations(stream, &b).unwrap(), (0, 0));
        let report = server.join().unwrap();
        assert_eq!(report.received, 1);
        assert_eq!(report.deferred, 1);
        assert_eq!(
            Database::open(&a)
                .unwrap()
                .history
                .index
                .inventory()
                .count(),
            1
        );
    }
}
