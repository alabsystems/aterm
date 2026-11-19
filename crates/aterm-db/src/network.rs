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
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
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

/// How long an exchange may go on: `window` with no byte moved on its stream, and `guard` in
/// all, each counted from when its [`Deadline`] was armed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Patience {
    /// The longest the exchange may go with no byte moved.
    pub(crate) window: Duration,
    /// The longest the exchange may go, moving or not.
    pub(crate) guard: Duration,
}
impl Patience {
    /// A wall-clock deadline of `within`, whatever moves: the window is the guard.
    pub(crate) const fn absolute(within: Duration) -> Self {
        Self {
            window: within,
            guard: within,
        }
    }

    /// A test's hang detector: an exchange that moves no byte for one
    /// [`aterm_spec::wait::MUST_HAPPEN`] is cut, and one that keeps moving is cut at
    /// [`aterm_spec::wait::WORKING_GUARD`]. A loaded machine stretches a working round's wall
    /// time without bound, and does not stop it moving.
    #[cfg(test)]
    pub(crate) const HANG: Self = Self {
        window: aterm_spec::wait::MUST_HAPPEN,
        guard: aterm_spec::wait::WORKING_GUARD,
    };

    /// The last instant an exchange armed at `from` may run to: its guard.
    pub(crate) fn horizon(self, from: Instant) -> Instant {
        from + self.guard
    }

    /// The longest a watcher waits between two looks at the exchange's progress. Where the window
    /// is the guard, progress changes nothing: one wait to the deadline.
    fn look(self) -> Duration {
        if self.window >= self.guard {
            self.guard
        } else {
            self.window / 4
        }
    }
}

/// One exchange's patience: the product's absolute [`WITHIN`]; a unit test's
/// [`Patience::HANG`].
#[cfg(not(test))]
pub(crate) const PATIENCE: Patience = Patience::absolute(WITHIN);
/// One exchange's patience: the product's absolute [`WITHIN`]; a unit test's
/// [`Patience::HANG`].
#[cfg(test)]
pub(crate) const PATIENCE: Patience = Patience::HANG;

/// How long a watcher of an exchange armed at `started`, whose stream last moved at `moved`,
/// waits from `now` before it looks again; `None` where the exchange has run out of patience
/// and is cut now. It expires at the earlier of a window after its last move and its guard.
fn watch(patience: Patience, started: Instant, moved: Instant, now: Instant) -> Option<Duration> {
    let expires = (moved + patience.window).min(patience.horizon(started));
    (now < expires).then(|| (expires - now).min(patience.look()))
}

/// The watcher's loop for an exchange armed at `started`: whether it finished within its
/// patience. Each look reads the clock (`now`) and the stream's moves (`moves`), and waits
/// (`wait`) as long as [`watch`] says for the exchange's completion: when it finished, and its
/// moves then. A completion is judged at its own time, so a late watcher honours an early
/// finish, not a late one; one whose moves are newer than the last look moved after it.
fn finished(
    patience: Patience,
    started: Instant,
    now: impl Fn() -> Instant,
    moves: impl Fn() -> u64,
    mut wait: impl FnMut(Duration) -> std::result::Result<(Instant, u64), mpsc::RecvTimeoutError>,
) -> bool {
    let (mut moved, mut seen) = (started, 0);
    loop {
        let at = now();
        let count = moves();
        if count != seen {
            (moved, seen) = (at, count);
        }
        let next = watch(patience, started, moved, at);
        match wait(next.unwrap_or(Duration::ZERO)) {
            Ok((completed, count)) => {
                let moved = if count == seen { moved } else { completed };
                return watch(patience, started, moved, completed).is_some();
            }
            Err(mpsc::RecvTimeoutError::Timeout) if next.is_some() => {}
            Err(_) => return false,
        }
    }
}

#[cfg(test)]
thread_local! {
    /// TEST SEAM: the moves the last [`Deadline`] dropped on this thread had counted on its
    /// stream ([`Deadline::progressing`]).
    pub(crate) static LAST_MOVES: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

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

/// An exchange's watcher: it shuts the exchange's socket once the exchange runs out of its
/// [`Patience`], and lets it be once this drops first.
pub(crate) struct Deadline {
    done: mpsc::Sender<(Instant, u64)>,
    /// How many reads and writes of the exchange's stream moved a byte ([`Self::progressing`]).
    moves: Arc<AtomicU64>,
    join: Option<thread::JoinHandle<()>>,
}
impl Deadline {
    /// [`Self::arm_with`] at the exchange's [`PATIENCE`].
    pub(crate) fn arm(tcp: &TcpStream) -> Result<Self> {
        Self::arm_with(tcp, PATIENCE)
    }

    /// Watch `tcp`'s exchange with its own patience.
    pub(crate) fn arm_with(tcp: &TcpStream, patience: Patience) -> Result<Self> {
        Self::arm_with_clock(tcp, patience, Instant::now)
    }

    fn arm_with_clock(
        tcp: &TcpStream,
        patience: Patience,
        now: impl Fn() -> Instant + Send + 'static,
    ) -> Result<Self> {
        // Socket preparation and thread scheduling consume the original budget.
        let started = now();
        let socket = aterm_uds::kept_out(|| tcp.try_clone())?;
        let (done, completion) = mpsc::channel::<(Instant, u64)>();
        let moves = Arc::new(AtomicU64::new(0));
        let read = Arc::clone(&moves);
        let join = thread::Builder::new()
            .name("db-exchange-deadline".into())
            .spawn(move || {
                let in_time = finished(
                    patience,
                    started,
                    now,
                    || read.load(Ordering::Relaxed),
                    |within| completion.recv_timeout(within),
                );
                if !in_time {
                    let _ = socket.shutdown(Shutdown::Both);
                }
            })?;
        crate::runtime::census::count(crate::runtime::census::Event::DeadlineSpawn);
        Ok(Self {
            done,
            moves,
            join: Some(join),
        })
    }

    /// `stream`, each of whose reads and writes that moves a byte this deadline counts as the
    /// exchange's progress.
    pub(crate) fn progressing<'a, S>(&'a self, stream: &'a mut S) -> Progressing<'a, S> {
        Progressing {
            stream,
            moves: &self.moves,
        }
    }
}
impl Drop for Deadline {
    fn drop(&mut self) {
        let moves = self.moves.load(Ordering::Relaxed);
        #[cfg(test)]
        LAST_MOVES.with(|last| last.set(Some(moves)));
        let _ = self.done.send((Instant::now(), moves));
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

/// An exchange's stream whose moves its [`Deadline`] counts ([`Deadline::progressing`]).
pub(crate) struct Progressing<'a, S> {
    stream: &'a mut S,
    moves: &'a AtomicU64,
}
impl<S> Progressing<'_, S> {
    fn moved(&self, bytes: usize) {
        if bytes > 0 {
            self.moves.fetch_add(1, Ordering::Relaxed);
        }
    }
}
impl<S: Read> Read for Progressing<'_, S> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        let read = self.stream.read(bytes)?;
        self.moved(read);
        Ok(read)
    }
}
impl<S: Write> Write for Progressing<'_, S> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.stream.write(bytes)?;
        self.moved(written);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
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
            .index()
            .inventory()
            .skip_while(|id| cursor.is_some_and(|cursor| **id <= cursor))
            .chain(
                db.history
                    .index()
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
    let deadline = Deadline::arm(&tcp)?;
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
    let stream = &mut deadline.progressing(accepted.transport.stream());
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
        .index()
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
    let deadline = Deadline::arm(&tcp)?;
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .map_err(|_| Error::new("database TLS peer authentication failed"))?;
    locality.recheck()?;
    let stream = &mut deadline.progressing(dialed.transport.stream());
    send_frame(stream, credentials.capability.bytes())?;
    let capability = receive_frame(stream, 256)?;
    let actor = authenticate(&credentials, dialed.peer.as_ref(), &capability)?;
    repair.prepare(actor);
    let local = while_busy(|| Database::open(dir))?
        .history
        .index()
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
    let deadline = Deadline::arm(&tcp)?;
    let mut dialed =
        fleet::client_handshake_as(tcp, &secret, Some(&identity), FleetOp::Database, WITHIN)
            .map_err(|_| Error::new("database TLS peer authentication failed"))?;
    let stream = &mut deadline.progressing(dialed.transport.stream());
    send_frame(stream, credentials.capability.bytes())?;
    let capability = receive_frame(stream, 256)?;
    let actor = authenticate(&credentials, dialed.peer.as_ref(), &capability)?;
    repair.prepare(actor);
    let local = while_busy(|| Database::open(dir))?
        .history
        .index()
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
#[path = "network_tests.rs"]
mod tests;
