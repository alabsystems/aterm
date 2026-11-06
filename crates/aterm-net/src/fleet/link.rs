// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mesh's persistent neighbour connection (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.4,
//! §11 B5): one [`ALPN_MESH`](super::ALPN_MESH) connection between two members, opened with the
//! [`FleetOp::Neighbour`] op, that carries every other op as a stream of frames tagged by op,
//! and re-proves its peer over the connection's exporter every [`REPROVE_EVERY`].
//!
//! # Frames
//!
//! After the handshake's `OK` each end writes frames of a 9-byte head (kind, stream id as a
//! big-endian `u32`, payload length as a big-endian `u32`) and at most [`FRAME_MAX`] bytes of
//! payload. A stream is opened by either end: the dialer's ids are odd, the listener's even, each
//! larger than the last it opened, so an id is never reused on one link.
//!
//! - `Open` names the op (its [`FleetOp::word`]) and, for [`FleetOp::Bus`] and
//!   [`FleetOp::Inventory`], one argument word (the log, [`BusLog`]). The other end answers
//!   `Accept`, or `Reset` with why it refused.
//! - `Data` carries bytes, within the send credit the receiver granted: [`WINDOW`] at open, then
//!   whatever it hands back by `Credit` as its reader takes them. A receiver never buffers more
//!   than [`WINDOW`] for a stream, so one slow stream holds up no other one, the re-proof
//!   included, and the reading thread never waits on a consumer.
//! - `End` closes the sender's half; `Reset` closes both halves with a reason.
//! - On stream 0: `Reprove` carries a fresh 32-byte nonce, `Proven` the nonce again and the
//!   prover's Ed25519 signature, and `Close` the reason the link ends.
//!
//! # The re-proof (M2)
//!
//! The checker sends a nonce from the system's random source. The prover exports keying material
//! from this TLS session under the label `EXPORTER-aterm-mesh-reproof-v1` with the nonce as its
//! context, and signs `role ‖ 0 ‖ that material ‖ node id` with its node key, the role being its
//! end's re-proof role, which no handshake proof uses. The checker verifies the signature with the
//! key the handshake proved, refuses a signature only the checker's own role verifies (a
//! reflection), judges the key again by its membership log read now ([`super::Denied::seat`]),
//! and hands each re-proof of a member to the link's [`NoteProven`]. A key the log no longer
//! seats ends the link. A re-proof unanswered within [`REPROVE_WITHIN`] ends it too, so a
//! neighbour that went away without a word is dropped by its next round.
//!
//! The timer reads an injected [`LinkClock`]: [`Neighbours::tick`] starts each round that is due
//! and ends each one that is overdue, and [`Neighbours::spawn_timer`] runs it whenever the
//! clock reaches the next deadline.

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use rustls::StreamOwned;

use super::{
    DenyFn, FleetError, FleetOp, NodeIdentity, NodeProof, SIG_LEN, Seat, Signer, Undialed,
};

/// How often each end re-proves its peer: half of home evidence's freshness
/// (`aterm_secrets::home_lan::FRESH`, 10 minutes), so a quiet link always holds a proof younger
/// than it.
pub const REPROVE_EVERY: Duration = Duration::from_secs(5 * 60);

/// How long, on the link's clock, a peer has to answer a re-proof before the link ends.
pub const REPROVE_WITHIN: Duration = Duration::from_secs(60);

/// The maximum time a local Close may drain before its socket is shut down.
pub const CLOSE_DRAIN: Duration = Duration::from_millis(250);

/// The most a receiver buffers for one stream, and the send credit each stream starts with.
pub const WINDOW: u32 = 256 * 1024;

/// The most payload one frame carries.
pub const FRAME_MAX: usize = 16 * 1024;

/// The most streams one link carries at once; an `Open` past it is refused `busy`.
pub const STREAMS_MAX: usize = 64;

/// The most links a [`Neighbours`] table holds; one past it is closed at once. Each Mac keeps
/// four neighbours of its own (§4.6) and is the neighbour of about four others, so this leaves
/// room for on-demand links and a lab's churn.
pub const LINKS_MAX: usize = 32;

/// The most stream data queued for the writer; a writer past it waits (its write timeout).
const QUEUED_MAX: usize = 1024 * 1024;

const HEAD: usize = 9;
const NONCE_LEN: usize = 32;
const REPROOF_LABEL: &[u8] = b"EXPORTER-aterm-mesh-reproof-v1";
/// The longest reason a `Reset` or `Close` carries.
const REASON_MAX: usize = 256;
/// The longest `Open` payload: an op word, a space and one argument word.
const OPEN_MAX: usize = 2 * 32 + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Kind {
    Open = 1,
    Accept = 2,
    Data = 3,
    End = 4,
    Reset = 5,
    Credit = 6,
    Reprove = 7,
    Proven = 8,
    Close = 9,
}

impl Kind {
    const fn of(byte: u8) -> Option<Self> {
        Some(match byte {
            1 => Self::Open,
            2 => Self::Accept,
            3 => Self::Data,
            4 => Self::End,
            5 => Self::Reset,
            6 => Self::Credit,
            7 => Self::Reprove,
            8 => Self::Proven,
            9 => Self::Close,
            _ => return None,
        })
    }
}

fn frame(kind: Kind, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEAD + payload.len());
    out.push(kind as u8);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(
        &u32::try_from(payload.len())
            .unwrap_or(u32::MAX)
            .to_be_bytes(),
    );
    out.extend_from_slice(payload);
    out
}

fn reason(why: &str) -> Vec<u8> {
    let mut end = why.len().min(REASON_MAX);
    while !why.is_char_boundary(end) {
        end -= 1;
    }
    why.as_bytes()[..end].to_vec()
}

/// The log a [`FleetOp::Bus`] or [`FleetOp::Inventory`] stream is for: the first frame of such a
/// stream names it (§4.4), and the receiver serves it with that log's broker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BusLog {
    /// `tables.log`: the membership log and every lab table.
    Tables,
    /// `metrics.log`: each Mac's own measurements.
    Metrics,
    /// `bus.log`: the fabric's bus.
    Bus,
}

impl BusLog {
    /// Every log, in wire order.
    pub const ALL: [Self; 3] = [Self::Tables, Self::Metrics, Self::Bus];

    /// Wire word for this log.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            Self::Tables => "tables",
            Self::Metrics => "metrics",
            Self::Bus => "bus",
        }
    }

    /// Parse a wire word into a [`BusLog`].
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|log| log.word() == word)
    }
}

/// Whether `op` travels as a stream of a link: every op [`ALPN_MESH`](super::ALPN_MESH) carries
/// but the link's own.
const fn carried(op: FleetOp) -> bool {
    op.meshed() && !matches!(op, FleetOp::Neighbour)
}

/// Whether `op` takes the argument word of its `Open` (the log of a [`BusLog`] stream).
const fn takes_log(op: FleetOp) -> bool {
    matches!(op, FleetOp::Bus | FleetOp::Inventory)
}

/// A wake-up a link's timer parks on ([`LinkClock::park`]): rung when the clock moves or a link
/// joins or leaves.
#[derive(Debug, Default)]
pub struct Bell {
    rung: Mutex<bool>,
    cv: Condvar,
}

impl Bell {
    /// Wake the timer parked on this bell, or the next one to park.
    pub fn ring(&self) {
        *self.rung.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.cv.notify_all();
    }

    /// Wait until rung (or `within` passed, where given), then clear the ring.
    pub fn wait(&self, within: Option<Duration>) {
        let mut rung = self.rung.lock().unwrap_or_else(PoisonError::into_inner);
        match within {
            None => {
                while !*rung {
                    rung = self.cv.wait(rung).unwrap_or_else(PoisonError::into_inner);
                }
            }
            Some(within) => {
                let until = Instant::now().checked_add(within);
                while !*rung {
                    let left = until.map_or(within, |until| {
                        until.saturating_duration_since(Instant::now())
                    });
                    if left.is_zero() {
                        break;
                    }
                    rung = self
                        .cv
                        .wait_timeout(rung, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
            }
        }
        *rung = false;
    }
}

/// The clock a link's re-proof timer reads (§11 B5: "The timer reads an injected clock").
pub trait LinkClock: Send + Sync {
    /// Time since an origin of the clock's own choosing; it never goes back.
    fn now(&self) -> Duration;

    /// Park the calling thread until [`Self::now`] reaches `until`, or `bell` rings.
    fn park(&self, until: Duration, bell: &Arc<Bell>);
}

/// The production clock: the system's monotonic clock.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// A clock whose origin is now.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl LinkClock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }

    fn park(&self, until: Duration, bell: &Arc<Bell>) {
        let left = until.saturating_sub(self.now());
        if !left.is_zero() {
            bell.wait(Some(left));
        }
    }
}

/// A clock that moves only when told to ([`Self::advance`]), for timing laws.
#[cfg(any(test, debug_assertions))]
#[derive(Debug, Default)]
pub struct ManualClock {
    held: Mutex<(Duration, Vec<std::sync::Weak<Bell>>)>,
}

#[cfg(any(test, debug_assertions))]
impl ManualClock {
    /// A clock at zero.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Move the clock `by` and wake every timer parked on it.
    pub fn advance(&self, by: Duration) {
        let bells = {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            held.0 = held.0.saturating_add(by);
            std::mem::take(&mut held.1)
        };
        for bell in bells.iter().filter_map(std::sync::Weak::upgrade) {
            bell.ring();
        }
    }
}

#[cfg(any(test, debug_assertions))]
impl LinkClock for ManualClock {
    fn now(&self) -> Duration {
        self.held.lock().unwrap_or_else(PoisonError::into_inner).0
    }

    fn park(&self, until: Duration, bell: &Arc<Bell>) {
        {
            let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
            if held.0 >= until {
                return;
            }
            held.1.push(Arc::downgrade(bell));
        }
        bell.wait(None);
    }
}

/// How a [`Proven`] peer proved its key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProvenBy {
    /// The link's [`ALPN_MESH`](super::ALPN_MESH) handshake.
    Handshake,
    /// A re-proof over the link's exporter.
    Reproof,
}

/// A node-proven answer from a link's peer: what the heard ledger's noting call (B6) takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proven {
    /// The node and key the peer proved; the membership log seats a member under the key.
    pub peer: NodeProof,
    /// The peer's address on this link.
    pub addr: SocketAddr,
    /// This end's address on this link: the interface the proof came in on.
    pub local: SocketAddr,
    /// When, on the link's clock.
    pub at: Duration,
    /// By handshake or by re-proof.
    pub by: ProvenBy,
}

/// Where a link hands each node-proven answer: B6 notes it in the heard ledger.
pub type NoteProven = Arc<dyn Fn(&Proven) + Send + Sync>;

/// Where a link hands each stream its peer opens: served on a thread of its own.
pub type ServeFn = Arc<dyn Fn(Incoming) + Send + Sync>;

/// What a link is configured with: the verdict it judges its peer by at each re-proof, how it
/// serves its peer's streams, where it notes each proof, and the clock its re-proof reads.
#[derive(Clone)]
pub struct LinkConfig {
    /// This Mac's membership verdict, read at each re-proof.
    pub verdict: DenyFn,
    /// Serves each stream the peer opens.
    pub serve: ServeFn,
    /// Notes each node-proven answer.
    pub note: NoteProven,
    /// The re-proof timer's clock.
    pub clock: Arc<dyn LinkClock>,
}

struct Pending {
    nonce: [u8; NONCE_LEN],
    overdue_at: Duration,
}

struct Reproof {
    next_at: Duration,
    pending: Option<Pending>,
    sent: u64,
    proven: u64,
}

struct Stream {
    /// Opened by this end.
    ours: bool,
    /// The peer accepted it (an opener's stream), or this end did (a peer's).
    accepted: bool,
    inbound: VecDeque<u8>,
    /// Bytes read and not yet handed back as credit.
    unacked: u32,
    credit: u32,
    peer_end: bool,
    sent_end: bool,
    reset: Option<String>,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
}

impl Stream {
    const fn new(ours: bool) -> Self {
        Self {
            ours,
            accepted: !ours,
            inbound: VecDeque::new(),
            unacked: 0,
            credit: WINDOW,
            peer_end: false,
            sent_end: false,
            reset: None,
            read_timeout: None,
            write_timeout: None,
        }
    }
}

struct State {
    /// Why the link ended; nothing moves on it once set.
    closed: Option<String>,
    /// A `Close` is queued: no stream opens or writes, and the writer ends after it.
    close_reason: Option<String>,
    streams: BTreeMap<u32, Stream>,
    next_id: u32,
    /// The highest id the peer opened.
    peer_max: u32,
    control: VecDeque<Vec<u8>>,
    data: VecDeque<Vec<u8>>,
    queued: usize,
    /// The TLS session has records to write that no frame queued (a key update).
    tls_due: bool,
    reproof: Reproof,
    /// What runs once the link ends ([`Link::on_end`]).
    ends: Vec<Box<dyn FnOnce() + Send>>,
}

struct Shared {
    me: NodeIdentity,
    peer: NodeProof,
    dialed: bool,
    addr: SocketAddr,
    local: SocketAddr,
    state: Mutex<State>,
    changed: Condvar,
    close_bell: Arc<Bell>,
    tls: Mutex<rustls::Connection>,
    sock: TcpStream,
    cfg: LinkConfig,
    guard: Mutex<Option<Box<dyn Send>>>,
    /// A negative control's: this end answers no re-proof.
    #[cfg(any(test, debug_assertions))]
    mute: AtomicBool,
}

/// A persistent [`ALPN_MESH`](super::ALPN_MESH) connection to one neighbour, carrying ops as
/// streams ([`Self::open`]) and re-proving its peer ([`Self::tick`]). Every stream ends with it.
pub struct Link {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link")
            .field("peer", &self.shared.peer.node)
            .field("addr", &self.shared.addr)
            .field("dialed", &self.shared.dialed)
            .finish_non_exhaustive()
    }
}

fn closed_error(why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        format!("the neighbour link ended: {why}"),
    )
}

impl Shared {
    /// Queue the first local close; its caller must enforce the returned drain deadline.
    fn queue_close(&self, why: &str) -> Option<Duration> {
        let mut state = self.link_state();
        if state.closed.is_some() || state.close_reason.is_some() {
            return None;
        }
        state.close_reason = Some(why.to_owned());
        let until = self.cfg.clock.now().saturating_add(CLOSE_DRAIN);
        state.control.push_back(frame(Kind::Close, 0, &reason(why)));
        drop(state);
        self.changed.notify_all();
        Some(until)
    }

    fn link_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The link's state while the link is live: a frame handled once it ended moves nothing
    /// (`State::closed`), whichever thread ended it.
    fn live_state(&self) -> Option<MutexGuard<'_, State>> {
        Some(self.link_state()).filter(|state| state.closed.is_none())
    }

    /// End the link now: every stream wakes to its end, both threads stop, the socket closes.
    fn fail(&self, why: &str) {
        self.fail_held(self.link_state(), why);
    }

    /// [`Self::fail`] from under the state lock `state` holds, so what its holder judged of the
    /// state is still true when the link ends.
    fn fail_held(&self, mut state: MutexGuard<'_, State>, why: &str) {
        if state.closed.is_some() {
            return;
        }
        state.closed = Some(state.close_reason.take().unwrap_or_else(|| why.to_owned()));
        state.control.clear();
        state.data.clear();
        state.queued = 0;
        let ends = std::mem::take(&mut state.ends);
        drop(state);
        self.close_bell.ring();
        self.changed.notify_all();
        let _ = self.sock.shutdown(Shutdown::Both);
        for end in ends {
            end();
        }
        drop(
            self.guard
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
    }

    fn queue_control(state: &mut State, frame: Vec<u8>) {
        if state.closed.is_none() {
            state.control.push_back(frame);
        }
    }

    const fn reprove_signer(&self, mine: bool) -> Signer {
        if self.dialed == mine {
            Signer::ReproveDialer
        } else {
            Signer::ReproveListener
        }
    }

    fn reproof_material(&self, nonce: &[u8]) -> Option<[u8; 32]> {
        self.tls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .export_keying_material([0u8; 32], REPROOF_LABEL, Some(nonce))
            .ok()
    }

    fn note(&self, by: ProvenBy) {
        (self.cfg.note)(&Proven {
            peer: self.peer.clone(),
            addr: self.addr,
            local: self.local,
            at: self.cfg.clock.now(),
            by,
        });
    }

    /// The peer as the membership log seats it now, ending the link for a key it no longer
    /// seats. `true` where it seats a member.
    fn judged(&self) -> bool {
        match (self.cfg.verdict)().seat(&self.peer) {
            Seat::Member => true,
            Seat::Unread => false,
            Seat::Out => {
                self.fail("the membership log took the neighbour's key out");
                false
            }
            Seat::Unknown => {
                self.fail("the membership log seats no member under the neighbour's key");
                false
            }
        }
    }

    fn handle(self: &Arc<Self>, kind: Kind, id: u32, payload: &[u8]) -> Result<(), String> {
        match kind {
            Kind::Open => self.opened(id, payload),
            Kind::Accept => {
                let Some(mut state) = self.live_state() else {
                    return Ok(());
                };
                if let Some(stream) = state.streams.get_mut(&id).filter(|s| s.ours) {
                    stream.accepted = true;
                }
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::Data => {
                if payload.len() > FRAME_MAX {
                    return Err("a frame past its size".to_owned());
                }
                let Some(mut state) = self.live_state() else {
                    return Ok(());
                };
                if let Some(stream) = state.streams.get_mut(&id) {
                    if stream.inbound.len().saturating_add(payload.len()) > WINDOW as usize {
                        return Err("a stream's data past the window it was granted".to_owned());
                    }
                    if stream.reset.is_none() {
                        stream.inbound.extend(payload);
                    }
                }
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::End => {
                let Some(mut state) = self.live_state() else {
                    return Ok(());
                };
                if let Some(stream) = state.streams.get_mut(&id) {
                    stream.peer_end = true;
                }
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::Reset => {
                let why = String::from_utf8_lossy(payload).into_owned();
                let Some(mut state) = self.live_state() else {
                    return Ok(());
                };
                if let Some(stream) = state.streams.get_mut(&id) {
                    stream.reset.get_or_insert(why);
                    // What arrived before the peer's `End` is still read; a reset past it says
                    // only that the peer reads no more.
                    if !stream.peer_end {
                        stream.inbound.clear();
                    }
                }
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::Credit => {
                let granted = <[u8; 4]>::try_from(payload)
                    .map(u32::from_be_bytes)
                    .map_err(|_| "a credit of no length".to_owned())?;
                let Some(mut state) = self.live_state() else {
                    return Ok(());
                };
                if let Some(stream) = state.streams.get_mut(&id) {
                    stream.credit = stream
                        .credit
                        .checked_add(granted)
                        .filter(|credit| *credit <= WINDOW)
                        .ok_or_else(|| "credit past the window".to_owned())?;
                }
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::Reprove => {
                #[cfg(any(test, debug_assertions))]
                if self.mute.load(Ordering::Acquire) {
                    return Ok(());
                }
                let nonce = <[u8; NONCE_LEN]>::try_from(payload)
                    .map_err(|_| "a re-proof nonce of the wrong length".to_owned())?;
                let material = self
                    .reproof_material(&nonce)
                    .ok_or_else(|| "this session exports no re-proof material".to_owned())?;
                let sig = self.me.sign(self.reprove_signer(true), &material);
                let mut answer = nonce.to_vec();
                answer.extend_from_slice(&sig);
                let mut state = self.link_state();
                Self::queue_control(&mut state, frame(Kind::Proven, 0, &answer));
                drop(state);
                self.changed.notify_all();
                Ok(())
            }
            Kind::Proven => self.proven(payload),
            Kind::Close => Err(format!(
                "the neighbour closed it: {}",
                String::from_utf8_lossy(payload)
            )),
        }
    }

    fn opened(self: &Arc<Self>, id: u32, payload: &[u8]) -> Result<(), String> {
        // The peer's ids: odd where it dialed, even where this end did.
        if id == 0 || (id % 2 == 1) == self.dialed {
            return Err("a stream id of this end's".to_owned());
        }
        let text =
            std::str::from_utf8(payload).map_err(|_| "an open that is no text".to_owned())?;
        if payload.len() > OPEN_MAX {
            return Err("an open past its size".to_owned());
        }
        let Some(mut state) = self.live_state() else {
            return Ok(());
        };
        if id <= state.peer_max {
            return Err("a stream id the peer used before".to_owned());
        }
        state.peer_max = id;
        let (word, arg) = text
            .split_once(' ')
            .map_or((text, None), |(word, arg)| (word, Some(arg)));
        let op = FleetOp::from_word(word).filter(|op| carried(*op));
        let refusal = match op {
            None => Some("op"),
            Some(op) if takes_log(op) != arg.is_some() => Some("op"),
            Some(op) if takes_log(op) && arg.and_then(BusLog::from_word).is_none() => Some("log"),
            Some(_) if state.close_reason.is_some() => Some("closing"),
            Some(_) if state.streams.len() >= STREAMS_MAX => Some("busy"),
            Some(_) => None,
        };
        let (Some(op), None) = (op, refusal) else {
            Self::queue_control(
                &mut state,
                frame(Kind::Reset, id, refusal.unwrap_or("op").as_bytes()),
            );
            drop(state);
            self.changed.notify_all();
            return Ok(());
        };
        state.streams.insert(id, Stream::new(false));
        drop(state);
        let incoming = Incoming {
            link: Arc::clone(self),
            id,
            op,
            log: arg.and_then(BusLog::from_word),
            answered: false,
        };
        serve_spawned(Arc::clone(&self.cfg.serve), incoming, |op| {
            let _ = std::thread::Builder::new()
                .name("aterm-mesh-op".to_owned())
                .spawn(op);
        });
        Ok(())
    }

    fn proven(&self, payload: &[u8]) -> Result<(), String> {
        let (nonce, sig) = payload
            .split_at_checked(NONCE_LEN)
            .filter(|(_, sig)| sig.len() == SIG_LEN)
            .ok_or_else(|| "a re-proof answer of the wrong length".to_owned())?;
        {
            let state = self.link_state();
            if state
                .reproof
                .pending
                .as_ref()
                .is_none_or(|pending| pending.nonce != nonce)
            {
                return Err("a re-proof answer to no question this end asked".to_owned());
            }
        }
        let material = self
            .reproof_material(nonce)
            .ok_or_else(|| "this session exports no re-proof material".to_owned())?;
        let signed_as = |end: Signer| {
            self.peer
                .key
                .verifies(&super::node_message(end, &material, &self.peer.node), sig)
        };
        if !signed_as(self.reprove_signer(false)) {
            return Err(if signed_as(self.reprove_signer(true)) {
                "a reflected re-proof".to_owned()
            } else {
                "a re-proof that does not verify under the neighbour's key".to_owned()
            });
        }
        {
            let Some(mut state) = self.live_state() else {
                return Ok(());
            };
            state.reproof.pending = None;
            state.reproof.proven = state.reproof.proven.saturating_add(1);
        }
        if self.judged() {
            self.note(ProvenBy::Reproof);
        }
        Ok(())
    }

    /// Start the re-proof that is due, end the link for one overdue; the next deadline.
    fn tick(&self) -> Option<Duration> {
        let now = self.cfg.clock.now();
        // A round's nonce is drawn before the state lock, so no system call runs under it.
        let mut nonce = [0u8; NONCE_LEN];
        let drawn =
            ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).is_ok();
        let mut state = self.link_state();
        if state.closed.is_some() {
            return None;
        }
        if let Some(overdue_at) = state.reproof.pending.as_ref().map(|p| p.overdue_at) {
            if now >= overdue_at {
                // Under the lock that saw it pending: an answer the reader counts first is not
                // ended as unanswered.
                self.fail_held(state, "the neighbour left a re-proof unanswered");
                return None;
            }
            return Some(overdue_at);
        }
        if now < state.reproof.next_at {
            return Some(state.reproof.next_at);
        }
        if !drawn {
            self.fail_held(state, "the system's random source failed");
            return None;
        }
        let overdue_at = now.saturating_add(REPROVE_WITHIN);
        state.reproof.pending = Some(Pending { nonce, overdue_at });
        state.reproof.next_at = now.saturating_add(REPROVE_EVERY);
        state.reproof.sent = state.reproof.sent.saturating_add(1);
        Self::queue_control(&mut state, frame(Kind::Reprove, 0, &nonce));
        drop(state);
        self.changed.notify_all();
        Some(overdue_at)
    }

    fn read_loop(self: &Arc<Self>, mut sock: TcpStream) {
        let mut raw = vec![0u8; 64 * 1024];
        let mut plain: Vec<u8> = Vec::new();
        let mut taken = vec![0u8; 64 * 1024];
        let why = 'read: loop {
            let n = match sock.read(&mut raw) {
                Ok(0) => break 'read "the neighbour hung up".to_owned(),
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => break 'read format!("the link's socket failed: {e}"),
            };
            // The peer's TLS close can arrive in the read that carries its last frames (a
            // `Close` is written with it), so the frames read before it are handled first.
            let mut tls_closed = false;
            {
                let mut tls = self.tls.lock().unwrap_or_else(PoisonError::into_inner);
                let mut rest = &raw[..n];
                while !rest.is_empty() && !tls_closed {
                    if let Err(e) = tls.read_tls(&mut rest) {
                        break 'read format!("the link's TLS took no bytes: {e}");
                    }
                    if let Err(e) = tls.process_new_packets() {
                        break 'read format!("the link's TLS failed: {e}");
                    }
                    loop {
                        match tls.reader().read(&mut taken) {
                            Ok(0) => {
                                tls_closed = true;
                                break;
                            }
                            Ok(k) => plain.extend_from_slice(&taken[..k]),
                            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                            Err(e) => break 'read format!("the link's TLS failed: {e}"),
                        }
                    }
                }
                if tls.wants_write() {
                    drop(tls);
                    self.link_state().tls_due = true;
                    self.changed.notify_all();
                }
            }
            let mut at = 0;
            while let Some(head) = plain.get(at..at + HEAD) {
                let Some(kind) = Kind::of(head[0]) else {
                    break 'read "a frame of no kind this build knows".to_owned();
                };
                let id = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
                let len = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) as usize;
                if len > FRAME_MAX {
                    break 'read "a frame past its size".to_owned();
                }
                let Some(payload) = plain.get(at + HEAD..at + HEAD + len) else {
                    break;
                };
                if let Err(why) = self.handle(kind, id, payload) {
                    break 'read why;
                }
                at += HEAD + len;
            }
            plain.drain(..at);
            if tls_closed {
                break 'read "the neighbour closed its TLS".to_owned();
            }
        };
        self.fail(&why);
    }

    fn write_loop(self: &Arc<Self>, mut sock: TcpStream) {
        let mut out = Vec::with_capacity(64 * 1024);
        loop {
            let (next, last) = {
                let mut state = self.link_state();
                loop {
                    if state.closed.is_some() {
                        return;
                    }
                    if let Some(next) = state.control.pop_front() {
                        let last = next.first() == Some(&(Kind::Close as u8));
                        break (Some(next), last);
                    }
                    if let Some(next) = state.data.pop_front() {
                        state.queued = state.queued.saturating_sub(next.len());
                        break (Some(next), false);
                    }
                    if std::mem::take(&mut state.tls_due) {
                        break (None, false);
                    }
                    state = self
                        .changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
            };
            self.changed.notify_all();
            out.clear();
            let written = {
                let mut tls = self.tls.lock().unwrap_or_else(PoisonError::into_inner);
                let wrote = next.map_or(Ok(()), |next| tls.writer().write_all(&next));
                if last {
                    tls.send_close_notify();
                }
                wrote.and_then(|()| {
                    while tls.wants_write() {
                        tls.write_tls(&mut out)?;
                    }
                    Ok(())
                })
            };
            if let Err(e) = written.and_then(|()| sock.write_all(&out)) {
                self.fail(&format!("the link's socket failed: {e}"));
                return;
            }
            if last {
                self.fail("this end closed it");
                return;
            }
        }
    }
}

impl Link {
    /// Run a link on a completed [`ALPN_MESH`](super::ALPN_MESH) connection that proved `peer`,
    /// holding `guard` until it ends. It notes the handshake's proof at once.
    pub(super) fn start<C>(
        stream: StreamOwned<C, TcpStream>,
        me: &NodeIdentity,
        (peer, dialed): (NodeProof, bool),
        cfg: &LinkConfig,
        guard: Box<dyn Send>,
    ) -> io::Result<Arc<Self>>
    where
        C: Into<rustls::Connection>,
    {
        let StreamOwned { conn, sock } = stream;
        let addr = sock.peer_addr()?;
        let local = sock.local_addr()?;
        let _ = aterm_uds::keep_out_of_children(&sock);
        let reader = aterm_uds::kept_out(|| sock.try_clone())?;
        let writer = aterm_uds::kept_out(|| sock.try_clone())?;
        sock.set_read_timeout(None)?;
        sock.set_write_timeout(None)?;
        let shared = Arc::new(Shared {
            me: me.clone(),
            peer,
            dialed,
            addr,
            local,
            state: Mutex::new(State {
                closed: None,
                close_reason: None,
                streams: BTreeMap::new(),
                next_id: if dialed { 1 } else { 2 },
                peer_max: 0,
                control: VecDeque::new(),
                data: VecDeque::new(),
                queued: 0,
                tls_due: false,
                reproof: Reproof {
                    next_at: cfg.clock.now().saturating_add(REPROVE_EVERY),
                    pending: None,
                    sent: 0,
                    proven: 0,
                },
                ends: Vec::new(),
            }),
            changed: Condvar::new(),
            close_bell: Arc::new(Bell::default()),
            tls: Mutex::new(conn.into()),
            sock,
            cfg: cfg.clone(),
            guard: Mutex::new(Some(guard)),
            #[cfg(any(test, debug_assertions))]
            mute: AtomicBool::new(false),
        });
        let reading = Arc::clone(&shared);
        let read = std::thread::Builder::new()
            .name("aterm-mesh-read".to_owned())
            .spawn(move || reading.read_loop(reader));
        let writing = Arc::clone(&shared);
        let wrote = read.and_then(|_| {
            std::thread::Builder::new()
                .name("aterm-mesh-write".to_owned())
                .spawn(move || writing.write_loop(writer))
        });
        if let Err(e) = wrote {
            shared.fail(&format!("its thread could not start: {e}"));
            return Err(e);
        }
        shared.note(ProvenBy::Handshake);
        Ok(Arc::new(Self { shared }))
    }

    /// The node and key the peer proved.
    #[must_use]
    pub fn peer(&self) -> &NodeProof {
        &self.shared.peer
    }

    /// The peer's address.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.shared.addr
    }

    /// Whether this end dialed the link.
    #[must_use]
    pub fn dialed(&self) -> bool {
        self.shared.dialed
    }

    /// The node id that dialed the link: which of two links between one pair both ends keep
    /// ([`Neighbours::adopt`]).
    fn dialer(&self) -> &str {
        if self.shared.dialed {
            self.shared.me.node()
        } else {
            &self.shared.peer.node
        }
    }

    /// Why the link ended, once it has.
    #[must_use]
    pub fn closed(&self) -> Option<String> {
        self.shared.link_state().closed.clone()
    }

    /// Run `then` once, on the thread that ends the link, after [`Self::closed`] reads the end
    /// (at once, on this thread, where the link has ended): how a holder that waits on something
    /// else (a clock, another stream) learns of the end. `then` must not block.
    pub fn on_end(&self, then: Box<dyn FnOnce() + Send>) {
        let mut state = self.shared.link_state();
        if state.closed.is_some() {
            drop(state);
            then();
            return;
        }
        state.ends.push(then);
    }

    /// The streams it carries now.
    #[must_use]
    pub fn streams(&self) -> usize {
        self.shared.link_state().streams.len()
    }

    /// Re-proofs this end asked of its peer, and those the peer answered with a valid proof.
    #[must_use]
    pub fn reproofs(&self) -> (u64, u64) {
        let state = self.shared.link_state();
        (state.reproof.sent, state.reproof.proven)
    }

    /// Open a stream for `op`, with the log for a [`FleetOp::Bus`] or [`FleetOp::Inventory`]
    /// stream, and wait at most `within` for the peer to accept it.
    /// # Errors
    /// [`io::ErrorKind::InvalidInput`] for an op links do not carry or a log it does not take;
    /// [`io::ErrorKind::PermissionDenied`] with the peer's reason where it refused the stream (a
    /// `Reset` with no `Accept` before it);
    /// [`io::ErrorKind::WouldBlock`] where the link already carries [`STREAMS_MAX`] streams;
    /// [`io::ErrorKind::TimedOut`] where it did not answer within `within`;
    /// [`io::ErrorKind::ConnectionAborted`] where the link ended before the peer answered the
    /// stream (an answer that came before the end is taken), while this end is closing it, once
    /// its stream ids ran out, or where the stream was dropped.
    pub fn open(
        &self,
        op: FleetOp,
        log: Option<BusLog>,
        within: Duration,
    ) -> io::Result<LinkStream> {
        if !carried(op) || takes_log(op) != log.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a neighbour link carries no `{}` stream so", op.word()),
            ));
        }
        let shared = &self.shared;
        let id = {
            let mut state = shared.link_state();
            if let Some(why) = &state.closed {
                return Err(closed_error(why));
            }
            if state.close_reason.is_some() {
                return Err(closed_error("this end is closing it"));
            }
            if state.streams.len() >= STREAMS_MAX {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "the neighbour link carries all the streams it may",
                ));
            }
            let id = state.next_id;
            state.next_id = id
                .checked_add(2)
                .ok_or_else(|| closed_error("it opened every stream id it has"))?;
            state.streams.insert(id, Stream::new(true));
            let named = log.map_or_else(
                || op.word().to_owned(),
                |log| format!("{} {}", op.word(), log.word()),
            );
            Shared::queue_control(&mut state, frame(Kind::Open, id, named.as_bytes()));
            id
        };
        shared.changed.notify_all();
        Self::answer(
            LinkStream {
                link: Arc::clone(shared),
                id,
                op,
            },
            within,
        )
    }

    /// `stream`, opened by this end, once its peer accepted it, waiting at most `within`.
    ///
    /// The peer's `Accept` goes before anything it sends on the stream, so an accepted stream is
    /// taken whatever arrived behind it: a peer that answers and drops its end before this end's
    /// `End` sends its data, its `End` and a `Reset`, and the link may end behind them, all
    /// before this wait wakes; the stream's reads then give that data and its end. Only a `Reset`
    /// with no `Accept` before it refuses the stream.
    /// # Errors
    /// [`io::ErrorKind::PermissionDenied`] with the peer's reason where it refused the stream;
    /// [`io::ErrorKind::TimedOut`] where it did not answer within `within`;
    /// [`io::ErrorKind::ConnectionAborted`] where the link ended before the peer answered, or
    /// where the stream was dropped.
    fn answer(stream: LinkStream, within: Duration) -> io::Result<LinkStream> {
        let (shared, id, op) = (Arc::clone(&stream.link), stream.id, stream.op);
        let until = Instant::now().checked_add(within);
        let mut state = shared.link_state();
        loop {
            if let Some(opened) = state.streams.get(&id) {
                if opened.accepted {
                    drop(state);
                    return Ok(stream);
                }
                if let Some(why) = &opened.reset {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        format!("the neighbour refused the `{}` stream: {why}", op.word()),
                    ));
                }
            }
            if let Some(why) = &state.closed {
                return Err(closed_error(why));
            }
            if !state.streams.contains_key(&id) {
                return Err(closed_error("the stream was dropped"));
            }
            let left = until.map_or(within, |until| {
                until.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("the neighbour did not answer a `{}` stream", op.word()),
                ));
            }
            state = shared
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    /// Re-prove the peer if a round is due on the link's clock, or end the link if its peer left
    /// one unanswered past [`REPROVE_WITHIN`]. The clock's next deadline for this link, or `None`
    /// once it ended.
    pub fn tick(&self) -> Option<Duration> {
        self.shared.tick()
    }

    /// End the link: try to drain a `Close` and TLS close for [`CLOSE_DRAIN`], then shut down
    /// the socket even if the peer stopped reading. The first local reason remains in
    /// [`Self::closed`] and is available to [`Self::on_end`] on either path.
    pub fn close(&self, why: &str) {
        let Some(until) = self.shared.queue_close(why) else {
            return;
        };
        let shared = Arc::clone(&self.shared);
        if std::thread::Builder::new()
            .name("aterm-mesh-close".to_owned())
            .spawn(move || {
                while shared.link_state().closed.is_none() {
                    if shared.cfg.clock.now() >= until {
                        shared.fail("the close drain deadline elapsed");
                        break;
                    }
                    shared.cfg.clock.park(until, &shared.close_bell);
                }
            })
            .is_err()
        {
            self.shared.fail("the close drain timer could not start");
        }
    }

    /// A negative control's: from now on this end answers no re-proof its peer asks.
    #[cfg(any(test, debug_assertions))]
    pub fn mute_reproofs(&self) {
        self.shared.mute.store(true, Ordering::Release);
    }

    /// End the link at once, sending nothing.
    pub fn abort(&self, why: &str) {
        self.shared.fail(why);
    }
}

impl Drop for Link {
    /// The last handle let go: the link closes. Its threads and streams hold its shared state,
    /// never a [`Link`], so this runs once no holder (a [`Neighbours`] table, a dialer) is left.
    fn drop(&mut self) {
        self.close("its last holder let it go");
    }
}

/// A stream the peer opened on a link, waiting for this end's answer: [`Self::accept`] or
/// [`Self::refuse`]. Dropped unanswered, it is refused `unserved`.
pub struct Incoming {
    link: Arc<Shared>,
    id: u32,
    op: FleetOp,
    log: Option<BusLog>,
    answered: bool,
}

impl Incoming {
    /// The op the peer opened it for.
    #[must_use]
    pub const fn op(&self) -> FleetOp {
        self.op
    }

    /// The log a [`FleetOp::Bus`] or [`FleetOp::Inventory`] stream names.
    #[must_use]
    pub const fn log(&self) -> Option<BusLog> {
        self.log
    }

    /// The node and key the link's peer proved.
    #[must_use]
    pub fn peer(&self) -> &NodeProof {
        &self.link.peer
    }

    /// The link peer's address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.link.addr
    }

    /// Take the stream.
    #[must_use]
    pub fn accept(mut self) -> LinkStream {
        self.answered = true;
        let mut state = self.link.link_state();
        Shared::queue_control(&mut state, frame(Kind::Accept, self.id, &[]));
        drop(state);
        self.link.changed.notify_all();
        LinkStream {
            link: Arc::clone(&self.link),
            id: self.id,
            op: self.op,
        }
    }

    /// Refuse the stream with `why`, which the opener reads.
    pub fn refuse(mut self, why: &str) {
        self.answered = true;
        self.reset(why);
    }

    fn reset(&self, why: &str) {
        let mut state = self.link.link_state();
        if state.streams.remove(&self.id).is_some() {
            Shared::queue_control(&mut state, frame(Kind::Reset, self.id, &reason(why)));
        }
        drop(state);
        self.link.changed.notify_all();
    }
}

/// A peer's stream on its way to the op thread that serves it: dropped before that thread took it
/// (the thread could not start), it refuses the stream `busy`.
struct Unstarted(Option<Incoming>);

/// Serve `incoming` with `serve` on the thread `spawn` starts for the op it is given. A spawner
/// that cannot start one drops that op, as `std::thread::Builder::spawn` does, and the stream is
/// then refused `busy`, which its opener may try again, not `unserved`.
fn serve_spawned(serve: ServeFn, incoming: Incoming, spawn: impl FnOnce(Box<dyn FnOnce() + Send>)) {
    let mut unstarted = Unstarted(Some(incoming));
    spawn(Box::new(move || {
        if let Some(incoming) = unstarted.0.take() {
            serve(incoming);
        }
    }));
}

impl Drop for Unstarted {
    fn drop(&mut self) {
        if let Some(incoming) = self.0.take() {
            incoming.refuse("busy");
        }
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        if !self.answered {
            self.reset("unserved");
        }
    }
}

/// One op's stream on a [`Link`]: bytes both ways, each direction closed by itself
/// ([`Self::finish`]). Dropping it closes both: its write half with an `End`, and, while the peer
/// still sends, its read half with a `Reset`.
pub struct LinkStream {
    link: Arc<Shared>,
    id: u32,
    op: FleetOp,
}

impl std::fmt::Debug for LinkStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinkStream")
            .field("op", &self.op)
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl LinkStream {
    /// The op it carries.
    #[must_use]
    pub const fn op(&self) -> FleetOp {
        self.op
    }

    /// The node and key the link's peer proved.
    #[must_use]
    pub fn peer(&self) -> &NodeProof {
        &self.link.peer
    }

    /// The link peer's address.
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.link.addr
    }

    /// Bound each read (`None`: no bound).
    pub fn set_read_timeout(&self, within: Option<Duration>) {
        if let Some(stream) = self.link.link_state().streams.get_mut(&self.id) {
            stream.read_timeout = within;
        }
    }

    /// Bound each write's wait for credit (`None`: no bound).
    pub fn set_write_timeout(&self, within: Option<Duration>) {
        if let Some(stream) = self.link.link_state().streams.get_mut(&self.id) {
            stream.write_timeout = within;
        }
    }

    /// Wait on the link's state until `ready` gives an answer, or `within` passes.
    fn wait<T>(
        &self,
        within: impl Fn(&Stream) -> Option<Duration>,
        mut ready: impl FnMut(&mut State, u32) -> Option<io::Result<T>>,
    ) -> io::Result<T> {
        let link = &self.link;
        let mut state = link.link_state();
        let until = state
            .streams
            .get(&self.id)
            .and_then(within)
            .and_then(|within| Instant::now().checked_add(within));
        loop {
            if let Some(answer) = ready(&mut state, self.id) {
                return answer;
            }
            if let Some(why) = &state.closed {
                return Err(closed_error(why));
            }
            if !state.streams.contains_key(&self.id) {
                return Err(closed_error("the stream was dropped"));
            }
            let left = until.map(|until| until.saturating_duration_since(Instant::now()));
            state = match left {
                None => link
                    .changed
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
                Some(left) if left.is_zero() => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the neighbour stream did not move in time",
                    ));
                }
                Some(left) => {
                    link.changed
                        .wait_timeout(state, left)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
            };
        }
    }

    /// Read what has arrived, waiting for at least one byte, the peer's `End` (0), or its end.
    /// # Errors
    /// [`io::ErrorKind::ConnectionReset`] once the peer reset the stream,
    /// [`io::ErrorKind::ConnectionAborted`] once the link ended,
    /// [`io::ErrorKind::TimedOut`] past the read timeout.
    pub fn read_some(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut credit = None;
        let read = self.wait(
            |stream| stream.read_timeout,
            |state, id| {
                let stream = state.streams.get_mut(&id)?;
                if !stream.inbound.is_empty() {
                    let n = buf.len().min(stream.inbound.len());
                    for (slot, byte) in buf.iter_mut().zip(stream.inbound.drain(..n)) {
                        *slot = byte;
                    }
                    stream.unacked = stream
                        .unacked
                        .saturating_add(u32::try_from(n).unwrap_or(u32::MAX));
                    if stream.unacked >= WINDOW / 4 && !stream.peer_end {
                        credit = Some(std::mem::take(&mut stream.unacked));
                    }
                    return Some(Ok(n));
                }
                if stream.peer_end {
                    return Some(Ok(0));
                }
                stream.reset.as_ref().map(|why| {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionReset,
                        format!("the neighbour reset the stream: {why}"),
                    ))
                })
            },
        )?;
        if let Some(credit) = credit {
            let mut state = self.link.link_state();
            Shared::queue_control(
                &mut state,
                frame(Kind::Credit, self.id, &credit.to_be_bytes()),
            );
            drop(state);
            self.link.changed.notify_all();
        }
        Ok(read)
    }

    /// Write up to [`FRAME_MAX`] bytes of `buf` within the stream's credit, waiting for credit
    /// and for room in the writer's queue.
    /// # Errors
    /// [`io::ErrorKind::BrokenPipe`] after [`Self::finish`], this end's reset ([`Self::reset`], or
    /// [`Self::shutdown`] of the read half) or the peer's (a peer that answers before it reads
    /// may reset its end behind its answer, so a write racing that reset can meet it, and the
    /// answer is still read),
    /// [`io::ErrorKind::ConnectionAborted`] once the link ended or while this end is closing it,
    /// [`io::ErrorKind::TimedOut`] past the write timeout.
    pub fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let written = self.wait(
            |stream| stream.write_timeout,
            |state, id| {
                if let Some(why) = &state.closed {
                    return Some(Err(closed_error(why)));
                }
                if state.close_reason.is_some() {
                    return Some(Err(closed_error("this end is closing it")));
                }
                let queued = state.queued;
                let stream = state.streams.get_mut(&id)?;
                if stream.sent_end || stream.reset.is_some() {
                    return Some(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "the neighbour stream is closed for writing",
                    )));
                }
                if stream.credit == 0 || queued >= QUEUED_MAX {
                    return None;
                }
                let n = buf.len().min(FRAME_MAX).min(stream.credit as usize);
                stream.credit -= u32::try_from(n).unwrap_or(u32::MAX);
                let data = frame(Kind::Data, id, &buf[..n]);
                state.queued = state.queued.saturating_add(data.len());
                state.data.push_back(data);
                Some(Ok(n))
            },
        )?;
        self.link.changed.notify_all();
        Ok(written)
    }

    /// Close this end's half: the peer reads to its end once what was written arrives.
    /// # Errors
    /// [`io::ErrorKind::ConnectionAborted`] once the link ended.
    pub fn finish(&self) -> io::Result<()> {
        let mut state = self.link.link_state();
        if let Some(why) = &state.closed {
            return Err(closed_error(why));
        }
        let State {
            streams,
            data,
            queued,
            ..
        } = &mut *state;
        if let Some(stream) = streams.get_mut(&self.id)
            && !std::mem::replace(&mut stream.sent_end, true)
            && stream.reset.is_none()
        {
            // Behind the stream's own data, so the peer reads it all before its end.
            let end = frame(Kind::End, self.id, &[]);
            *queued = queued.saturating_add(end.len());
            data.push_back(end);
        }
        drop(state);
        self.link.changed.notify_all();
        Ok(())
    }

    /// Close one half or both: [`Shutdown::Write`] is [`Self::finish`]; a read half closes with a
    /// `Reset`, which closes the write half too.
    /// # Errors
    /// As [`Self::finish`].
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match how {
            Shutdown::Write => self.finish(),
            Shutdown::Read | Shutdown::Both => self.reset_live("closed"),
        }
    }

    /// Close both halves with `why`, which the peer reads unless the link ends before it is sent
    /// (a `Close` this end queued goes first). On a link that ended it does nothing: the stream's
    /// reads give what arrived before the end, then the end.
    pub fn reset(&self, why: &str) {
        let _ = self.reset_live(why);
    }

    /// [`Self::reset`], judged under the one lock that resets the stream.
    /// # Errors
    /// [`io::ErrorKind::ConnectionAborted`] once the link ended, resetting nothing.
    fn reset_live(&self, why: &str) -> io::Result<()> {
        let mut state = self.link.link_state();
        if let Some(ended) = &state.closed {
            return Err(closed_error(ended));
        }
        if let Some(stream) = state.streams.get_mut(&self.id)
            && stream.reset.is_none()
        {
            stream.reset = Some(why.to_owned());
            stream.inbound.clear();
            Shared::queue_control(&mut state, frame(Kind::Reset, self.id, &reason(why)));
        }
        drop(state);
        self.link.changed.notify_all();
        Ok(())
    }
}

impl Drop for LinkStream {
    fn drop(&mut self) {
        let _ = self.finish();
        let mut state = self.link.link_state();
        if let Some(stream) = state.streams.remove(&self.id)
            && !stream.peer_end
            && stream.reset.is_none()
            && state.closed.is_none()
        {
            // Behind the stream's data and its `End`, so the peer reads them all first.
            let reset = frame(Kind::Reset, self.id, b"closed");
            state.queued = state.queued.saturating_add(reset.len());
            state.data.push_back(reset);
        }
        drop(state);
        self.link.changed.notify_all();
    }
}

impl Read for LinkStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl Read for &LinkStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_some(buf)
    }
}

impl Write for LinkStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_some(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for &LinkStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_some(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Dial a neighbour link on `tcp` as `me`, within `within`: an [`ALPN_MESH`](super::ALPN_MESH)
/// handshake for [`FleetOp::Neighbour`] alone, judging the listener by `me`'s verdict
/// ([`NodeIdentity::dialing_mesh`]), then a link run as `cfg` says.
/// # Errors
/// The handshake's failure, or [`FleetError::Io`] where the link could not start. A node that
/// does not dial the mesh is [`FleetError::Alpn`]: it cannot judge a mesh listener.
pub fn dial(
    tcp: TcpStream,
    me: &NodeIdentity,
    within: Duration,
    cfg: &LinkConfig,
) -> Result<Arc<Link>, Undialed> {
    let dialed = super::dial_mesh_op(tcp, me, FleetOp::Neighbour, within)?;
    let undialed = |error| Undialed {
        error,
        refused_by: None,
    };
    let peer = dialed
        .peer
        .filter(|_| dialed.mesh)
        .ok_or_else(|| undialed(FleetError::Alpn))?;
    Link::start(
        dialed.transport.into_stream(),
        me,
        (peer, true),
        cfg,
        Box::new(()),
    )
    .map_err(|e| undialed(FleetError::Io(e)))
}

/// This Mac's neighbour links, one per peer node: the links it dialed and the ones its listener
/// took ([`super::Served::into_link`]), each re-proved on one timer.
pub struct Neighbours {
    me: NodeIdentity,
    cfg: LinkConfig,
    links: Mutex<Vec<Arc<Link>>>,
    bell: Arc<Bell>,
    stopped: AtomicBool,
    ticks: AtomicU64,
}

impl std::fmt::Debug for Neighbours {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Neighbours")
            .field("me", &self.me.node())
            .field(
                "links",
                &self
                    .links
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .len(),
            )
            .finish_non_exhaustive()
    }
}

impl Neighbours {
    /// An empty table for `me`, whose links run as `cfg` says.
    #[must_use]
    pub fn new(me: NodeIdentity, cfg: LinkConfig) -> Arc<Self> {
        Arc::new(Self {
            me,
            cfg,
            links: Mutex::new(Vec::new()),
            bell: Arc::new(Bell::default()),
            stopped: AtomicBool::new(false),
            ticks: AtomicU64::new(0),
        })
    }

    /// This Mac's node.
    #[must_use]
    pub const fn me(&self) -> &NodeIdentity {
        &self.me
    }

    /// What its links run as.
    #[must_use]
    pub const fn config(&self) -> &LinkConfig {
        &self.cfg
    }

    /// Hold `link`, the one link to its peer: where a live link to the same node is held
    /// already, both ends keep the one the lower node id dialed (an older one where one end
    /// dialed both), and the other is closed. A table at [`LINKS_MAX`], or one stopped, closes
    /// it. The link kept, or `None` where `link` was closed.
    pub fn adopt(&self, link: Arc<Link>) -> Option<Arc<Link>> {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        links.retain(|held| held.closed().is_none());
        if self.stopped.load(Ordering::Acquire) {
            drop(links);
            link.close("this Mac's node service stopped");
            return None;
        }
        if let Some(at) = links
            .iter()
            .position(|held| held.peer().node == link.peer().node)
        {
            let held = &links[at];
            let replaces = held.dialer() != link.dialer() && link.dialer() < held.dialer();
            if !replaces {
                let kept = Arc::clone(held);
                drop(links);
                link.close("a duplicate of a link this pair holds");
                return Some(kept);
            }
            let old = std::mem::replace(&mut links[at], Arc::clone(&link));
            drop(links);
            old.close("a duplicate of a link this pair holds");
            self.bell.ring();
            return Some(link);
        }
        if links.len() >= LINKS_MAX {
            drop(links);
            link.close("this Mac holds all the neighbour links it may");
            return None;
        }
        links.push(Arc::clone(&link));
        drop(links);
        self.bell.ring();
        Some(link)
    }

    /// Dial `addr` and hold the link ([`Self::adopt`]).
    /// # Errors
    /// As [`dial`]; [`FleetError::Io`] where the table closed it.
    pub fn dial(&self, addr: SocketAddr, within: Duration) -> Result<Arc<Link>, Undialed> {
        let undialed = |error| Undialed {
            error,
            refused_by: None,
        };
        let tcp = aterm_uds::connect_tcp(&addr, within).map_err(|e| undialed(FleetError::Io(e)))?;
        let link = dial(tcp, &self.me, within, &self.cfg)?;
        self.adopt(link).ok_or_else(|| {
            undialed(FleetError::Io(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "this Mac's neighbour table took no more links",
            )))
        })
    }

    /// The live link to `node`, if one is held.
    #[must_use]
    pub fn link_to(&self, node: &str) -> Option<Arc<Link>> {
        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|link| link.peer().node == node && link.closed().is_none())
            .cloned()
    }

    /// Every live link held.
    #[must_use]
    pub fn links(&self) -> Vec<Arc<Link>> {
        let mut links = self.links.lock().unwrap_or_else(PoisonError::into_inner);
        links.retain(|link| link.closed().is_none());
        links.clone()
    }

    /// Run each link's re-proof timer once ([`Link::tick`]) and let go of the links that ended;
    /// the clock's next deadline.
    pub fn tick(&self) -> Duration {
        self.ticks.fetch_add(1, Ordering::AcqRel);
        let links = self
            .links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let next = links
            .iter()
            .filter_map(|link| link.tick())
            .min()
            .unwrap_or_else(|| self.cfg.clock.now().saturating_add(REPROVE_EVERY));
        self.links
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|link| link.closed().is_none());
        next
    }

    /// How many times the table's timer ran ([`Self::tick`]).
    #[must_use]
    pub fn ticks(&self) -> u64 {
        self.ticks.load(Ordering::Acquire)
    }

    /// End every link whose peer this Mac's membership log no longer seats (its verdict read now).
    pub fn judge(&self) {
        let verdict = (self.cfg.verdict)();
        for link in self.links() {
            match verdict.seat(link.peer()) {
                Seat::Member | Seat::Unread => {}
                Seat::Out | Seat::Unknown => {
                    link.close("the membership log no longer seats the neighbour's key");
                }
            }
        }
    }

    /// Run [`Self::tick`] whenever the clock reaches the next deadline, until [`Self::stop`].
    /// # Errors
    /// Where the thread could not start.
    pub fn spawn_timer(self: &Arc<Self>) -> io::Result<std::thread::JoinHandle<()>> {
        let table = Arc::clone(self);
        std::thread::Builder::new()
            .name("aterm-mesh-reprove".to_owned())
            .spawn(move || {
                while !table.stopped.load(Ordering::Acquire) {
                    let next = table.tick();
                    if table.stopped.load(Ordering::Acquire) {
                        break;
                    }
                    table.cfg.clock.park(next, &table.bell);
                }
            })
    }

    /// Close every link and stop the timer; no link joins after.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        for link in std::mem::take(&mut *self.links.lock().unwrap_or_else(PoisonError::into_inner))
        {
            link.close("this Mac's node service stopped");
        }
        self.bell.ring();
    }
}

#[cfg(all(test, unix))]
#[path = "link_tests.rs"]
mod tests;
