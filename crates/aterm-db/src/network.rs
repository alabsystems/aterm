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
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    net::{Shutdown, SocketAddr, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

/// Bounded repair work per connection; subsequent rounds resume from durable inventory.
pub const RECORDS_PER_ROUND: usize = 256;
const WITHIN: Duration = Duration::from_secs(30);

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

pub(crate) struct Deadline {
    done: mpsc::Sender<()>,
    join: Option<thread::JoinHandle<()>>,
}
impl Deadline {
    pub(crate) fn arm(tcp: &TcpStream) -> Result<Self> {
        let socket = tcp.try_clone()?;
        aterm_uds::keep_out_of_children(&socket)?;
        let (done, wait) = mpsc::channel();
        let join = thread::Builder::new()
            .name("db-exchange-deadline".into())
            .spawn(move || {
                if wait.recv_timeout(WITHIN).is_err() {
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
        let _ = self.done.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn transport(credentials: &Credentials) -> Result<(FleetSecret, NodeIdentity)> {
    let prk = aterm_digest::hkdf_extract(b"aterm-db/transport/1", &credentials.key);
    let key = aterm_digest::hkdf_expand_32(&prk, &credentials.manifest.id);
    let secret = FleetSecret::new(&key).map_err(|_| Error::new("invalid dataset transport key"))?;
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
        || peer.node != aterm_codec::hex::encode_lower(&capability.actor[32..])
        || capability.actor == credentials.capability.actor
    {
        return Err(Error::new(
            "database channel proof differs from its certified installation",
        ));
    }
    Ok(capability.actor)
}
fn send_frame(stream: &mut impl Write, bytes: &[u8]) -> Result<()> {
    if bytes.len() > MAX_OPERATION {
        return Err(Error::new("outgoing database frame too large"));
    }
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}
fn receive_frame(stream: &mut impl Read, max: usize) -> Result<Vec<u8>> {
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
    stream.write_all(&(ids.len() as u32).to_le_bytes())?;
    for id in ids {
        stream.write_all(id)?;
    }
    stream.flush()?;
    Ok(())
}
fn receive_inventory(stream: &mut impl Read) -> Result<BTreeSet<Digest>> {
    let n = read_u32(stream)? as usize;
    if n > MAX_RECORDS {
        return Err(Error::new("peer inventory exceeds database capacity"));
    }
    let mut ids = BTreeSet::new();
    let mut previous = None;
    for _ in 0..n {
        let mut id = [0; 32];
        stream.read_exact(&mut id)?;
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
        let db = Database::open(dir)?;
        let cursor = repair.cursor;
        // Sweep past deferred prefixes even when retry hints expired. A peer
        // returning infrequently must not offer the same rejected page forever.
        db.history
            .index
            .inventory()
            .filter(|id| cursor.is_none_or(|cursor| **id > cursor))
            .chain(
                db.history
                    .index
                    .inventory()
                    .filter(|id| cursor.is_some_and(|cursor| **id <= cursor)),
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
        let mut ack = [0; 32];
        stream.read_exact(&mut ack)?;
        if ack != *id {
            return Err(Error::new(
                "peer did not acknowledge the exact durable operation",
            ));
        }
        let mut verdict = [0];
        stream.read_exact(&mut verdict)?;
        match verdict[0] {
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
        // can consume a connection slot but cannot hold up a local edit.
        let retained = Database::open(dir).and_then(|mut db| {
            let fresh = db.ingest(&wire)?;
            let refused = db.history.refusals().any(|(retained, _)| retained == &id);
            Ok((fresh, refused))
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
        stream.write_all(&id)?;
        stream.write_all(&[verdict])?;
        stream.flush()?;
    }
    Ok((received, deferred))
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
    let local = Database::open(dir)?
        .history
        .index
        .inventory()
        .copied()
        .collect();
    send_inventory(stream, &local)?;
    let (received, incoming_deferred) = receive_operations(stream, dir)?;
    let (sent, outgoing_deferred) = send_operations(stream, dir, &remote, &mut repair)?;
    let mut repairs = repairs.lock().unwrap_or_else(|p| p.into_inner());
    if repairs.len() < 64 || repairs.contains_key(&actor) {
        repairs.insert(actor, repair);
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
    let local = Database::open(dir)?
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
        deferred: incoming_deferred + outgoing_deferred,
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
        Arc::new(AtomicBool::new(true)),
        Arc::new(Mutex::new(peers)),
    )
}
/// A bounded listener and repair worker owned by the session runtime. Discovery
/// may update dial hints; every candidate still proves its certified TLS identity.
pub fn serve_until(
    dir: &Path,
    listener: Listener,
    running: Arc<AtomicBool>,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
) -> Result<()> {
    let credentials = Credentials::load(dir)?;
    let (secret, identity) = transport(&credentials)?;
    let outbound = {
        let dir = dir.to_path_buf();
        let running = running.clone();
        thread::Builder::new()
            .name("db-anti-entropy".into())
            .spawn(move || {
                let mut repairs = BTreeMap::new();
                while running.load(Ordering::Acquire) {
                    let peers = peers.lock().unwrap_or_else(|p| p.into_inner()).clone();
                    for peer in &peers {
                        if !running.load(Ordering::Acquire) {
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
                    for _ in 0..(10 + jitter / 40) {
                        if !running.load(Ordering::Acquire) {
                            break;
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            })?
    };
    let stopped = Arc::new(Mutex::new(None));
    let error = stopped.clone();
    let dir = dir.to_path_buf();
    let repairs = Arc::new(Mutex::new(BTreeMap::new()));
    fleet::serve_lan(
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
        &running,
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
    running.store(false, Ordering::Release);
    let _ = outbound.join();
    let error = stopped.lock().unwrap_or_else(|p| p.into_inner()).take();
    error.map_or(Ok(()), |reason| Err(Error(reason)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{Change, Command};
    use aterm_json::json;
    use std::net::TcpListener;

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
        let running = Arc::new(AtomicBool::new(true));
        let server = {
            let a = a.clone();
            let running = running.clone();
            thread::spawn(move || {
                serve_until(&a, listener, running, Arc::new(Mutex::new(Vec::new())))
            })
        };
        let first = sync_round(&b, address);
        let second = first.as_ref().ok().map(|_| sync_round(&b, address));
        running.store(false, Ordering::Release);
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
            let connection =
                Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap();
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
            let connection =
                Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap();
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
