//! A blocking client for the broker: connect, publish (awaiting the ack), and
//! subscribe (blocking on each delivery). The blocking calls are what make tests
//! deterministic — a publish round-trips before the next step, and a subscriber's
//! `recv` returns exactly when the next delivery arrives, with no reliance on sleeps
//! or wall-clock.

use crate::logid::{Lineage, LogIdentity};
use crate::proto::{
    append_frame, append_publish_frame, decode_response, encode_request, foreign_protocol,
    read_frame, Request, Response, CAP_EXPIRED_PREFIX, ERR_BAD_FILTER, ERR_BAD_SUBJECT,
    ERR_PROTOCOL, ERR_RECORD_REFUSED, ERR_REFUSED, ERR_SUBJECT_BOUND, ERR_UNAUTHORIZED,
    LOG_POISONED_PREFIX, PROTO_VERSION,
};
use crate::proto::{NonceExt, ServedInfo};
use astream_wire::{Frame, FrameError};
use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::Path;
use std::time::{Duration, Instant};

#[cfg(clean_islands)]
#[path = "client_island.rs"]
mod client_island;
#[cfg(not(clean_islands))]
pub use crate::twins::client_walk_page_advance;
#[cfg(clean_islands)]
pub use client_island::client_walk_page_advance;

contract_fn! {
    /// Verified check that a `Last` walk page count lies within the liveness limit
    /// (`pages <= max_pages`), linked via `clean-kernel-defeq` to `client_walk_within_limit_isl`.
    #[must_use]
    pub fn client_walk_within_limit(pages: usize, max_pages: usize) -> bool
    clauses [ensures result == (pages <= max_pages)]
    {
        pages <= max_pages
    }
}

/// One record as a client sees it: `(offset, subject, body)` — what a `Delivery`
/// carries.
pub type Record = (u64, String, Vec<u8>);

/// The answer to a bounded, NON-TERMINAL read ([`Client::last`], [`Client::fetch`]):
/// the page of records, then the `(next, head)` its closing `Mark` carried. Also the
/// shape of [`Client::last_all`]'s whole-face answer, whose mark is its FIRST page's.
pub type Page = (Vec<Record>, (u64, u64));

/// A producer's DISTINCT-SUBJECT BUDGET as one broker log counts it: the answer to
/// [`Client::subject_budget`].
///
/// `subjects` is how many entries of the last-value index are charged to the
/// producer (a subject is charged to the producer whose record first put it in the
/// index — updating someone else's subject costs nothing), `reserved` how many more
/// its registered wills hold for subjects they have not landed yet, and `max` the
/// budget both count against
/// ([`MAX_SUBJECTS_PER_PRODUCER`](crate::store::MAX_SUBJECTS_PER_PRODUCER)). A publish
/// that would add a subject past it is refused with
/// [`ERR_SUBJECT_BOUND`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubjectBudget {
    /// Index entries charged to the producer.
    pub subjects: u64,
    /// Subjects the producer's registered, unlanded wills hold in reserve.
    pub reserved: u64,
    /// The budget `subjects + reserved` counts against.
    pub max: u64,
}

impl SubjectBudget {
    /// How many NEW subjects the producer may still introduce: `max - subjects -
    /// reserved`, saturating at 0. A snapshot — another connection publishing as the
    /// same producer spends from it at once.
    pub const fn remaining(&self) -> u64 {
        self.max
            .saturating_sub(self.subjects)
            .saturating_sub(self.reserved)
    }
}

/// The most `Last` requests one [`Client::last_walk`] may make before the walk is
/// called a FAILURE rather than an answer.
///
/// A liveness bound, not a size one. Every page moves the resume cursor strictly
/// forward (it is the last subject the broker VISITED, after the one it was sent), so
/// against a conforming broker a walk ends; one that has not ended in this many
/// requests has met something pathological — a peer whose cursor never runs out, an
/// index grown faster than it is walked — and the honest answer to a caller that must
/// not read absence as evidence is an error, not a short list. At the broker's
/// `LAST_PAGE_MAX` rows a page the ceiling is 4096 × 4096 = 16 777 216 rows; pages the
/// index-scan bound cuts short reach it sooner.
pub const LAST_WALK_PAGES_MAX: usize = 4096;

/// What a [`Client::last_walk`] callback answers after each row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// Deliver the next row, paging on as the answer needs.
    Continue,
    /// That row was the last one wanted: the walk returns at once — the rest of the
    /// page already read is dropped and no further page is asked for — reporting the
    /// row's subject as the cursor that continues it (or no cursor at all, when that
    /// row was the last of an answer the broker said was complete).
    Stop,
}

/// Credit-windowed pipelined flow-control state machine, backed directly by the
/// Clean-origin transition functions [`crate::proto::credit_horizon_monotone`],
/// [`crate::proto::replica_ack_monotone`], [`crate::proto::committed_watermark`], and
/// the `#[ensures]`-verified guard [`crate::proto::can_send_in_window`] from
/// `Astream.Broker.TlaCreditFlowWindow`.
///
/// Maintains the machine-checked invariant `CreditWindowInv`:
/// `acked_seq <= next_send && next_send <= credit_horizon`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditWindow {
    acked_seq: u64,
    next_send: u64,
    credit_horizon: u64,
}

impl CreditWindow {
    /// Construct a fresh credit window starting at `(0, 0, initial_credit)` (`Init` in
    /// `Astream.Broker.TlaCreditFlowWindow`).
    #[must_use]
    pub const fn new(initial_credit: u64) -> Self {
        Self {
            acked_seq: 0,
            next_send: 0,
            credit_horizon: initial_credit,
        }
    }

    /// Monotone cumulative sequence acknowledged so far.
    #[must_use]
    pub const fn acked_seq(&self) -> u64 {
        self.acked_seq
    }

    /// Next sequence index to be sent.
    #[must_use]
    pub const fn next_send(&self) -> u64 {
        self.next_send
    }

    /// Current credit horizon (`next_send <= credit_horizon`).
    #[must_use]
    pub const fn credit_horizon(&self) -> u64 {
        self.credit_horizon
    }

    /// Number of unacknowledged frames currently in flight (`next_send - acked_seq`).
    #[must_use]
    pub const fn in_flight(&self) -> u64 {
        self.next_send.saturating_sub(self.acked_seq)
    }

    /// Whether a send is permitted under the current credit horizon
    /// (`can_send_in_window(self.credit_horizon, self.next_send)`).
    #[must_use]
    pub const fn can_send(&self) -> bool {
        crate::proto::can_send_in_window(self.credit_horizon, self.next_send)
    }

    /// Attempt to consume one unit of send credit (`SendBatch` in
    /// `Astream.Broker.TlaCreditFlowWindow`) via the 4-parameter Clean-origin
    /// [`crate::proto::windowed_send_advance`] kernel. Returns `true` and advances
    /// `next_send` by 1 if `self.can_send()` holds, or `false` if the credit window is full.
    pub const fn try_send(&mut self) -> bool {
        if !self.can_send() {
            return false;
        }
        let cand = self.next_send.saturating_add(1);
        let next = crate::proto::windowed_send_advance(
            self.next_send,
            self.acked_seq,
            self.credit_horizon,
            cand,
        );
        if next > self.next_send {
            self.next_send = next;
            true
        } else {
            false
        }
    }

    /// Advance `next_send` toward `cand_seq` clamped by `credit_horizon` via the
    /// 4-parameter Clean-origin [`crate::proto::windowed_send_advance`] kernel.
    pub const fn advance_send_to(&mut self, cand_seq: u64) -> u64 {
        self.next_send = crate::proto::windowed_send_advance(
            self.next_send,
            self.acked_seq,
            self.credit_horizon,
            cand_seq,
        );
        self.next_send
    }

    /// Record a cumulative acknowledgment up to `ack` (`AckStep` in
    /// `Astream.Broker.TlaCreditFlowWindow`), clamped to `self.next_send` via
    /// [`crate::proto::committed_watermark`] and advanced monotonically via
    /// [`crate::proto::replica_ack_monotone`].
    pub const fn record_ack(&mut self, ack: u64) -> u64 {
        let bounded = crate::proto::committed_watermark(self.next_send, ack);
        self.acked_seq = crate::proto::replica_ack_monotone(self.acked_seq, bounded);
        self.acked_seq
    }

    /// Advance the credit horizon monotonically (`GrantCreditStep` in
    /// `Astream.Broker.TlaCreditFlowWindow`) via Clean-origin
    /// [`crate::proto::credit_horizon_monotone`].
    pub const fn grant_credit(&mut self, new_horizon: u64) -> u64 {
        self.credit_horizon =
            crate::proto::credit_horizon_monotone(self.credit_horizon, new_horizon);
        self.credit_horizon
    }

    /// Grant sliding-window credit so the horizon covers `acked_seq + window`.
    pub const fn grant_window(&mut self, window: u64) -> u64 {
        self.grant_credit(self.acked_seq.saturating_add(window))
    }
}

const ATTACH_WINDOW: usize = 16;
const MAX_PIPELINE_WINDOW: usize = 512;

/// A connection to a broker over a byte stream. Three transports:
///
/// - [`connect`](Client::connect) — a Unix socket, same machine, unsealed (the
///   kernel scopes who can reach the socket path).
/// - [`connect_tcp`](Client::connect_tcp) — PLAINTEXT TCP, multi-machine on a
///   trusted network only: no confidentiality, no peer authentication.
/// - `connect_tcp_sealed` (behind the `aead` feature) — TCP inside an
///   XChaCha20-Poly1305 sealed record layer under a pre-shared key: confidential
///   and authenticated across an untrusted network. `connect_tcp_handshake`
///   (`handshake`) and `connect_tcp_identity` (`identity`) put a forward-secret key
///   agreement in front of the same record layer.
///
/// `S` is the byte stream; any `Read + Write` works (see
/// [`from_stream`](Client::from_stream)), and the Frame protocol is identical on
/// every transport. A caller that picks the transport at RUNTIME uses the free
/// function [`connect`] instead: one [`AnyClient`] for every wire, plus the
/// [`Closer`] that ends it and bounds its I/O.
///
/// A request/reply client keeps itself IN STEP with its replies. A request that fails
/// part-way through its exchange — a write error, a read that timed out or hit EOF, a
/// reply that does not decode or is not the one the request is owed, a page longer
/// than it asked for — leaves the stream at an unknown point (a late reply may still
/// arrive), so the client latches: every later request on it fails at once with
/// `ErrorKind::BrokenPipe`, writing nothing, rather than reading the reply owed to
/// the request before it. Reconnect. A broker REFUSAL is a whole reply and leaves the
/// client in step ([`BrokerRefusal`]). A broker in ANOTHER protocol is neither: its
/// reply is a [`ProtocolMismatch`], and it latches the client like any other failure.
#[cfg(unix)]
pub struct Client<S = UnixStream> {
    stream: S,
    /// Why this connection is out of step with its replies, once it is.
    lost: Option<String>,
    /// The log identity this connection's latest `Hello` answer carried.
    identity: Option<LogIdentity>,
    /// The extension block beside it (the served tier, the lineage).
    ext: NonceExt,
    /// When this client last wrote a request (or was made): what
    /// [`keepalive_if_idle`](Client::keepalive_if_idle) measures idleness from.
    sent_at: Instant,
    /// The broker answered a `Keepalive` as a verb it does not know.
    keepalive_unknown: bool,
    /// The durable head the newest `Mark` this connection read carried
    /// ([`seen_durable`](Client::seen_durable)).
    durable: Option<u64>,
}

/// A connection to a broker over a byte stream. Windows twin: std has no
/// Unix-domain sockets there, so TCP (`connect_tcp`) is the default (and only)
/// transport.
#[cfg(not(unix))]
pub struct Client<S = TcpStream> {
    stream: S,
    /// Why this connection is out of step with its replies, once it is.
    lost: Option<String>,
    /// The log identity this connection's latest `Hello` answer carried.
    identity: Option<LogIdentity>,
    /// The extension block beside it (the served tier, the lineage).
    ext: NonceExt,
    /// When this client last wrote a request (or was made): what
    /// [`keepalive_if_idle`](Client::keepalive_if_idle) measures idleness from.
    sent_at: Instant,
    /// The broker answered a `Keepalive` as a verb it does not know.
    keepalive_unknown: bool,
    /// The durable head the newest `Mark` this connection read carried
    /// ([`seen_durable`](Client::seen_durable)).
    durable: Option<u64>,
}

#[cfg(unix)]
impl Client<UnixStream> {
    /// Connect to the broker's Unix socket at `socket_path` (same machine,
    /// unsealed — reachability is scoped by the socket path's permissions).
    pub fn connect(socket_path: impl AsRef<Path>) -> io::Result<Client<UnixStream>> {
        Ok(Client::from_stream(UnixStream::connect(socket_path)?))
    }
}

impl Client<TcpStream> {
    /// Connect to the broker over PLAINTEXT TCP: multi-machine, but for a trusted
    /// network only — the bytes are neither encrypted nor authenticated. For an
    /// untrusted network use `connect_tcp_sealed` (the `aead` feature), which
    /// carries the same protocol inside an authenticated-encryption record layer.
    pub fn connect_tcp(addr: impl ToSocketAddrs) -> io::Result<Client<TcpStream>> {
        Ok(Client::from_stream(dial_tcp(addr, None)?))
    }

    /// [`connect_tcp`](Self::connect_tcp) with a bound on the CONNECT: each address
    /// `addr` resolves to is tried in turn with `TcpStream::connect_timeout`, so a
    /// black-holed address fails `TimedOut` after `timeout` instead of after the OS's
    /// SYN-retry schedule (minutes). The bound is per address, so the worst case is
    /// `timeout` × addresses; the error is the LAST address's. Name resolution itself
    /// is not bounded (std's resolver has no timeout) — an IP-literal `addr` makes
    /// this a hard bound. The stream comes back blocking: this bounds the connect,
    /// not the requests (see [`Closer`] for those).
    ///
    /// # Errors
    /// Every address failed (the last one's error), `addr` resolved to nothing
    /// (`InvalidInput`), or a zero `timeout` (`InvalidInput`, std's own refusal).
    pub fn connect_tcp_timeout(
        addr: impl ToSocketAddrs,
        timeout: Duration,
    ) -> io::Result<Client<TcpStream>> {
        Ok(Client::from_stream(dial_tcp(addr, Some(timeout))?))
    }
}

/// Dial TCP with `TCP_NODELAY` — every request is one small frame that waits for its
/// answer, so Nagle only adds latency. `None` is `TcpStream::connect` exactly (each
/// resolved address under the OS's own connect timeout); `Some(t)` tries each
/// resolved address with `TcpStream::connect_timeout(addr, t)` and answers the first
/// that connects, else the last error.
fn dial_tcp(addr: impl ToSocketAddrs, timeout: Option<Duration>) -> io::Result<TcpStream> {
    let stream = match timeout {
        None => TcpStream::connect(addr)?,
        Some(t) => {
            let mut last = None;
            let mut connected = None;
            for a in addr.to_socket_addrs()? {
                match TcpStream::connect_timeout(&a, t) {
                    Ok(s) => {
                        connected = Some(s);
                        break;
                    }
                    Err(e) => last = Some(e),
                }
            }
            match (connected, last) {
                (Some(s), _) => s,
                (None, Some(e)) => return Err(e),
                (None, None) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "could not resolve to any addresses",
                    ))
                }
            }
        }
    };
    stream.set_nodelay(true)?;
    Ok(stream)
}

/// How long the broker may take to complete the SEALED handshake when the caller gave
/// no connect timeout; lifted after it.
#[cfg(feature = "aead")]
const SEALED_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the client half of the sealed handshake over an already-dialed socket, each
/// read bounded by `deadline` so an unresponsive peer cannot hang the connect; the
/// socket is blocking again afterwards.
#[cfg(feature = "aead")]
fn seal_tcp(
    stream: TcpStream,
    key: [u8; astream_aead::KEY_LEN],
    deadline: Duration,
) -> io::Result<astream_aead::SealedStream<TcpStream>> {
    stream.set_read_timeout(Some(deadline))?;
    let stream = astream_aead::SealedStream::handshake_client(stream, key)?;
    stream.get_ref().set_read_timeout(None)?;
    Ok(stream)
}

/// How long the Rung-7 key agreement may take when the caller gave no connect
/// timeout; lifted after it.
#[cfg(feature = "handshake")]
const KEY_AGREEMENT_TIMEOUT: Duration = Duration::from_secs(10);

/// Run the client half of the Rung-7 forward-secret handshake over an already-dialed
/// socket. Its reads AND writes are bounded by `deadline`, so a broken or hostile
/// server cannot hang the client on the key-agreement I/O; reset to blocking after.
#[cfg(feature = "handshake")]
fn agree_tcp(
    stream: TcpStream,
    psk: &[u8; astream_aead::KEY_LEN],
    deadline: Duration,
) -> io::Result<astream_aead::SealedStream<TcpStream>> {
    stream.set_read_timeout(Some(deadline))?;
    stream.set_write_timeout(Some(deadline))?;
    let sealed = astream_aead::client_handshake(stream, psk)?;
    sealed.get_ref().set_read_timeout(None)?;
    sealed.get_ref().set_write_timeout(None)?;
    Ok(sealed)
}

#[cfg(feature = "aead")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_sealed`](crate::Broker::serve_tcp_sealed)
    /// over an XChaCha20-Poly1305-sealed transport under the pre-shared `key`. Runs
    /// the sealed handshake before returning — a fresh hello from each side, bound
    /// with the direction and the record sequence into every record's AAD, and a
    /// key-confirming record each way — so a broker that lacks the key is refused
    /// HERE (the connect fails; nothing but an empty confirm was sent under the
    /// key), and a record recorded from any other connection or direction cannot
    /// be replayed into this one. The Frame protocol is unchanged; it just rides
    /// inside an encrypted, ordered record layer. The handshake has a deadline, so
    /// an unresponsive peer cannot hang the connect.
    pub fn connect_tcp_sealed(
        addr: impl ToSocketAddrs,
        key: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        let stream = seal_tcp(dial_tcp(addr, None)?, key, SEALED_HANDSHAKE_TIMEOUT)?;
        Ok(Client::from_stream(stream))
    }
}

#[cfg(feature = "handshake")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_handshake`](crate::Broker::serve_tcp_handshake),
    /// running the Rung-7 forward-secret handshake (ephemeral X25519 authenticated
    /// by the pre-shared `psk`) before the Frame protocol. Each connection derives a
    /// fresh session key, so a later `psk` compromise cannot decrypt this session's
    /// recorded traffic. A wrong `psk`, or a peer not speaking the handshake, fails
    /// the connect.
    pub fn connect_tcp_handshake(
        addr: impl ToSocketAddrs,
        psk: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        let sealed = agree_tcp(dial_tcp(addr, None)?, &psk, KEY_AGREEMENT_TIMEOUT)?;
        Ok(Client::from_stream(sealed))
    }
}

#[cfg(feature = "identity")]
impl Client<astream_aead::SealedStream<TcpStream>> {
    /// Connect to a broker served by [`serve_tcp_identity`](crate::Broker::serve_tcp_identity)
    /// with **static public-key identity, no shared secret** (Rung 8). Runs a
    /// mutual signed-DH (SIGMA/Ed25519) handshake: the broker must present exactly
    /// `expected_server` (its pinned host key), and we prove our own `identity`
    /// (which the broker must have allow-listed). The session key comes from the
    /// ephemeral DH alone (forward-secret under identity-key compromise). A wrong
    /// host key, an unauthorized identity, or a bad signature fails the connect.
    pub fn connect_tcp_identity(
        addr: impl ToSocketAddrs,
        identity: astream_aead::IdentityKeypair,
        expected_server: [u8; astream_aead::ID_PUB_LEN],
    ) -> io::Result<Client<astream_aead::SealedStream<TcpStream>>> {
        let stream = TcpStream::connect(addr)?;
        stream.set_nodelay(true)?;
        let hs_timeout = std::time::Duration::from_secs(10);
        stream.set_read_timeout(Some(hs_timeout))?;
        stream.set_write_timeout(Some(hs_timeout))?;
        let sealed = astream_aead::client_identity_handshake(stream, &identity, &expected_server)?;
        sealed.get_ref().set_read_timeout(None)?;
        sealed.get_ref().set_write_timeout(None)?;
        Ok(Client::from_stream(sealed))
    }
}

impl<S: Read + Write> Client<S> {
    /// Wrap an already-connected byte stream (any `Read + Write`, e.g. a boxed
    /// `dyn` stream chosen at runtime) as a client. The caller has already done
    /// whatever transport setup (nodelay, sealing) the stream needs; this is the
    /// inverse of [`into_stream`](Self::into_stream).
    pub fn from_stream(stream: S) -> Client<S> {
        Client {
            stream,
            lost: None,
            identity: None,
            ext: NonceExt::default(),
            sent_at: Instant::now(),
            keepalive_unknown: false,
            durable: None,
        }
    }

    /// Give the underlying byte stream back (e.g. to box it behind a `dyn` trait so
    /// one code path serves every transport). The connection stays open. A client
    /// that was out of step hands back a stream that still is.
    pub fn into_stream(self) -> S {
        self.stream
    }

    /// Ask the broker for this connection's NONCE — the value an [`attach`](Self::attach)
    /// proof is computed over. Answered by a guarded and an unguarded broker alike, so
    /// one client works against either. The log identity the answer carries is kept
    /// ([`seen_log_identity`](Self::seen_log_identity)).
    pub fn hello(&mut self) -> io::Result<[u8; 32]> {
        let (nonce, _) = self.hello_identity()?;
        Ok(nonce)
    }

    /// [`hello`](Self::hello), returning beside the nonce the [`LogIdentity`] of the
    /// log the broker serves — `None` from a broker that predates log identity. Kept,
    /// like `hello`'s, as [`seen_log_identity`](Self::seen_log_identity).
    pub fn hello_identity(&mut self) -> io::Result<([u8; 32], Option<LogIdentity>)> {
        self.send(&Request::Hello)?;
        match self.read_response()? {
            Response::Nonce { nonce, log, ext } => match nonce.as_slice().try_into() {
                Ok(nonce) => {
                    self.identity = log;
                    self.ext = ext;
                    Ok((nonce, log))
                }
                Err(_) => Err(self.lose(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("broker nonce is {} bytes, expected 32", nonce.len()),
                ))),
            },
            other => Err(self.reply_error(other)),
        }
    }

    /// The [`LogIdentity`] of the log the broker serves NOW: one `Hello` round trip.
    /// `Ok(None)` is a broker that predates log identity, which says nothing either
    /// way.
    ///
    /// Check it before using any offset persisted from an EARLIER connection — a
    /// delivered-up-to watermark, a cursor, an offset already answered. If it is not
    /// the identity that offset was read under (a different `id` OR `generation`),
    /// the offset names nothing in this log: drop it and start over from this log, as
    /// a first run would (the [`LogIdentity`] docs say how). The broker's own consumer
    /// groups need no such check: their cursors live inside the log.
    ///
    /// It IS a `Hello`: the connection's nonce is replaced, so an attach proof
    /// computed over an earlier one no longer verifies ([`attach`](Self::attach) and
    /// [`attach_all`](Self::attach_all) fetch their own).
    pub fn log_identity(&mut self) -> io::Result<Option<LogIdentity>> {
        let (_, log) = self.hello_identity()?;
        Ok(log)
    }

    /// The [`LogIdentity`] this connection's latest `Hello` answer carried — the one
    /// [`attach`](Self::attach) and [`attach_all`](Self::attach_all) send — so a client
    /// that attached learns it with no further round trip. `None` before any `Hello`
    /// on this connection, or from a broker that predates log identity
    /// ([`log_identity`](Self::log_identity) asks). A connection to a REPLICA can see
    /// it change, when the replica adopts its leader's: ask again for the current one.
    pub const fn seen_log_identity(&self) -> Option<LogIdentity> {
        self.identity
    }

    /// The [`Lineage`] the latest `Hello` answer carried beside the identity: where the
    /// log's generations agree, so offsets kept under an earlier generation resume at
    /// [`LogIdentity::continues`] instead of being forgotten. `None` before any
    /// `Hello`, or from a broker that predates lineage (which says nothing either way:
    /// forget, as before).
    pub const fn log_lineage(&self) -> Option<&Lineage> {
        self.ext.lineage.as_ref()
    }

    /// The tier the broker serves, as the latest `Hello` answer told it
    /// ([`ServedInfo`]) — read from the broker itself, so a status line never infers it
    /// from how the broker was started. `None` before any `Hello`, or from a broker that
    /// predates the extension block.
    pub const fn served_durability(&self) -> Option<ServedInfo> {
        self.ext.durability
    }

    /// The broker's DURABLE head as the newest `Mark` this connection read told it —
    /// every record below it is on stable media by a full flush. `None` before any
    /// `Mark` (an `Attach`'s, a `Fetch`'s or a `Last` page's), or from a broker that
    /// predates the field. To ask about an offset now, a `Fetch` head query (`max = 0`)
    /// answers with a fresh `Mark`.
    pub const fn seen_durable(&self) -> Option<u64> {
        self.durable
    }

    /// Keep what a `Mark` said about durability (an older broker's says nothing, which
    /// leaves the last word standing).
    const fn saw_durable(&mut self, durable: Option<u64>) {
        if durable.is_some() {
            self.durable = durable;
        }
    }

    /// Add a grant to this connection's keyring by presenting a PROOF the caller has
    /// already computed over the nonce from [`hello`](Self::hello) — the primitive
    /// under [`attach`](Self::attach), for a caller that holds its tag somewhere this
    /// process cannot reach it (an agent, a signer). Blocks for the acknowledgement
    /// and returns its `(next, head)`; a refused attach is an error HERE, not a
    /// surprise at the next request.
    pub fn attach_with_proof(&mut self, grant: &str, proof: &[u8]) -> io::Result<(u64, u64)> {
        self.send(&Request::Attach {
            grant: grant.to_string(),
            proof: proof.to_vec(),
        })?;
        self.read_mark()
    }

    /// Add the capability `grant`/`tag` to this connection's keyring: `Hello` for the
    /// connection's nonce, then `Attach` carrying `HMAC-SHA256(tag, nonce ‖ grant)` —
    /// a PROOF OF POSSESSION, so the tag never crosses the wire and a captured attach
    /// cannot be replayed onto another connection. Blocks for the acknowledgement and
    /// returns its `(next, head)`. [`attach_all`](Self::attach_all) with one grant.
    ///
    /// `Attach` APPENDS: several narrow grants on one connection are the intended
    /// shape (read one subtree, commit under another, publish as a bound principal in
    /// a third), and each request is authorized existentially over the ring. A
    /// repeated grant string replaces rather than growing it. A connection that holds
    /// several grants attaches them with [`attach_all`](Self::attach_all), which pays
    /// one `Hello` for the lot.
    ///
    /// # Errors
    /// The socket errors, or the broker refuses the attach (a proof that does not
    /// verify, a producer-id collision in the binding table, or a full keyring).
    pub fn attach(&mut self, grant: &str, tag: &[u8]) -> io::Result<(u64, u64)> {
        match self.attach_all([(grant, tag)])? {
            Some(mark) => Ok(mark),
            None => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "no attach was acknowledged",
            )),
        }
    }

    /// Add every capability in `ring` — `(grant, tag)` pairs, in order — to this
    /// connection's keyring in TWO round trips however many there are: ONE `Hello`
    /// (the broker keeps a connection's nonce until the next `Hello`, and verifies any
    /// number of `Attach`es against it), then every `Attach` pipelined, at most
    /// [`ATTACH_WINDOW`](Self::ATTACH_WINDOW) in flight, each acknowledged in order.
    /// Returns the LAST acknowledgement's `(next, head)`; `None` for an empty ring,
    /// which sends nothing at all (an unguarded setup with no capability files).
    ///
    /// A REFUSED grant stops the sending: the attaches already in flight are still
    /// answered and read — so the connection stays in step for the next request —
    /// and the FIRST refusal is returned ([`BrokerRefusal`] inside, in ring order).
    /// Each grant is decided on its own: on an `Err` the connection holds the grants
    /// ahead of the refused one, and perhaps some that were in flight behind it, so a
    /// caller that needs the whole ring drops the connection.
    ///
    /// # Errors
    /// A tag that is not 32 bytes (`InvalidInput`, before anything is sent); the
    /// socket; or the broker's first refusal.
    #[cfg(feature = "cap")]
    pub fn attach_all<'a>(
        &mut self,
        ring: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> io::Result<Option<(u64, u64)>> {
        let mut parsed_ring = Vec::new();
        for (grant, tag) in ring {
            let Ok(tag_arr): Result<[u8; 32], _> = tag.try_into() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("capability tag is {} bytes, expected 32", tag.len()),
                ));
            };
            parsed_ring.push((grant, tag_arr));
        }
        let ring = parsed_ring;
        if ring.is_empty() {
            return Ok(None);
        }
        let nonce = self.hello()?;
        let mut mark = None;
        let mut sent = 0usize;
        let mut received = 0usize;
        let mut failure: Option<io::Error> = None;
        loop {
            while failure.is_none()
                && sent < ring.len()
                && sent.saturating_sub(received) < ATTACH_WINDOW
            {
                let (grant, tag) = &ring[sent];
                let proof = astream_cap::attach_proof(tag, &nonce, grant);
                match self.send(&Request::Attach {
                    grant: grant.to_string(),
                    proof: proof.to_vec(),
                }) {
                    Ok(()) => sent = sent.saturating_add(1),
                    Err(e) => failure = Some(e),
                }
            }
            if received == sent {
                break;
            }
            received = received.saturating_add(1);
            match self.read_mark() {
                Ok(m) => mark = Some(m),
                Err(e) if broker_refusal(&e).is_some() => {
                    failure.get_or_insert(e);
                }
                // A transport failure: the client is out of step now, and says so.
                Err(e) => return Err(failure.unwrap_or(e)),
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(mark),
        }
    }

    /// Without the `cap` feature there is no way to compute an attach PROOF: the tag
    /// stays on the client and the wire carries `HMAC-SHA256(tag, nonce ‖ grant)`
    /// instead, which needs the same vetted MAC a guarded broker uses. Build with
    /// `cap` (or compute the proof yourself and call
    /// [`attach_with_proof`](Self::attach_with_proof)). An EMPTY ring needs no proof:
    /// it is `Ok(None)`, as with `cap`, and sends nothing.
    #[cfg(not(feature = "cap"))]
    pub fn attach_all<'a>(
        &mut self,
        ring: impl IntoIterator<Item = (&'a str, &'a [u8])>,
    ) -> io::Result<Option<(u64, u64)>> {
        if ring.into_iter().next().is_none() {
            return Ok(None);
        }
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "presenting a capability needs the `cap` feature: an Attach carries a proof of \
             possession (HMAC-SHA256 over the broker's per-connection nonce), never the tag, \
             so the client must be able to compute that MAC",
        ))
    }

    /// The most `Attach`es [`attach_all`](Self::attach_all) keeps in flight before
    /// reading an acknowledgement — a keyring's worth (`MAX_KEYRING`, past which the
    /// broker refuses anyway), so a ring of any length cannot fill both directions'
    /// socket buffers and wedge the connection.
    pub const ATTACH_WINDOW: usize = ATTACH_WINDOW;

    /// Publish `body` to `subject`, deduped by `(producer_id, producer_seq)`. Blocks
    /// for the ack; returns `(offset, deduped)` — `deduped` true means a re-send
    /// returning the original offset.
    pub fn publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, bool)> {
        self.send_publish(producer_id, producer_seq, subject, body)?;
        self.recv_publish_ack()
    }

    /// Durably retire `subject` from the broker's last-value index (`Last`) and
    /// decrement `producer_id`'s distinct-subject count (`MAX_SUBJECTS_PER_PRODUCER`),
    /// sending a [`crate::store::RETIRE_SUBJECT`] (`/a/retire`) request over the wire.
    ///
    /// Returns `(offset, already_absent)`: `already_absent` is `false` when `subject`
    /// was in `Last` and a `/a/retire` record was appended at `offset`, or `true` when
    /// `subject` was already absent (idempotent no-op). Refuses if `subject` is owned
    /// in `Last` by a different producer or if the connection's capability keyring does
    /// not grant read-write publish authority on `subject` for `producer_id`.
    pub fn retire_subject(&mut self, producer_id: u64, subject: &str) -> io::Result<(u64, bool)> {
        self.send_publish(
            producer_id,
            0,
            crate::store::RETIRE_SUBJECT,
            subject.as_bytes(),
        )?;
        self.recv_publish_ack()
    }

    /// PIPELINED publish (send side): write a publish WITHOUT waiting for its ack, so a
    /// single connection can keep many in flight (they coalesce into one group-commit
    /// fsync at the broker). Pair each `send_publish` with a later
    /// [`recv_publish_ack`](Self::recv_publish_ack) in the SAME order — the broker
    /// streams acks back in request order. Interleave sends and receives within a
    /// bounded window to avoid filling the socket buffers.
    pub fn send_publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<()> {
        self.in_step()?;
        let mut out = Vec::new();
        append_publish_frame(&mut out, producer_id, producer_seq, subject, body)?;
        if let Err(e) = self.stream.write_all(&out) {
            return Err(self.lose(e));
        }
        self.sent_at = Instant::now();
        Ok(())
    }

    /// PIPELINED publish (receive side): read the next ack in order, returning
    /// `(offset, deduped)`. See [`send_publish`](Self::send_publish).
    pub fn recv_publish_ack(&mut self) -> io::Result<(u64, bool)> {
        match self.read_response()? {
            Response::PublishAck { offset, deduped } => Ok((offset, deduped)),
            other => Err(self.reply_error(other)),
        }
    }

    /// The largest in-flight window [`publish_pipelined`](Self::publish_pipelined) will
    /// use, regardless of the requested window. Kept well under the broker's
    /// `PIPELINE_DEPTH` so the interleaved send/recv loop can never wedge the connection
    /// (a window at/above the broker's bound + non-draining could deadlock until the
    /// broker's ack write-timeout).
    pub const MAX_PIPELINE_WINDOW: usize = 512;

    /// Convenience windowed pipeline: publish `bodies` under `producer_id` (seqs
    /// `1..=len`), keeping at most `window` acks outstanding, and return the acks in
    /// order. Far faster than calling [`publish`](Self::publish) in a loop because the
    /// in-flight publishes share group-commit fsyncs — one connection, many in flight.
    /// The effective window is clamped to `[1, MAX_PIPELINE_WINDOW]` so it cannot
    /// out-run the broker's bounded ack queue.
    ///
    /// The first publish that fails — refused by the broker, or unframeable here —
    /// stops the sending, and the error is returned only after the responses of the
    /// publishes already in flight have been read, so the connection stays in step
    /// for the next call. Those publishes may have landed; re-sending the same bodies
    /// under the same `producer_id` dedups them.
    pub fn publish_pipelined(
        &mut self,
        producer_id: u64,
        subject: &str,
        bodies: &[&[u8]],
        window: usize,
    ) -> io::Result<Vec<(u64, bool)>> {
        self.in_step()?;
        let window = window.clamp(1, MAX_PIPELINE_WINDOW);
        let mut credit = CreditWindow::new(window as u64);
        let mut acks = Vec::new();
        let mut tx = Vec::new();
        let mut failure: Option<io::Error> = None;
        loop {
            // Top the in-flight window back up using the TLA+-verified `CreditWindow`
            // (`Astream.Broker.TlaCreditFlowWindow`), coalescing all frames that fit
            // into one `write_all` syscall so the broker's reader drains a whole batch
            // into one group-commit fsync.
            credit.grant_window(window as u64);
            tx.clear();
            let mut staged = credit;
            let mut batch_count = 0usize;
            while failure.is_none() && staged.can_send() {
                let idx = staged.next_send() as usize;
                let Some(&body) = bodies.get(idx) else {
                    break;
                };
                match append_publish_frame(
                    &mut tx,
                    producer_id,
                    (idx as u64).saturating_add(1),
                    subject,
                    body,
                ) {
                    Ok(()) => {
                        let _ = staged.try_send();
                        batch_count = batch_count.saturating_add(1);
                    }
                    Err(e) => failure = Some(e),
                }
            }
            if batch_count > 0 {
                match self.stream.write_all(&tx) {
                    Ok(()) => {
                        self.sent_at = Instant::now();
                        credit = staged;
                    }
                    Err(e) => {
                        let e = self.lose(e);
                        return Err(failure.unwrap_or(e));
                    }
                }
            }
            if credit.in_flight() == 0 {
                break;
            }
            // Drain up to half the window (or all remaining if sending is done) before
            // the next coalesced batch refill so each refill is a true batch.
            let in_flight = credit.in_flight() as usize;
            let drain_target = if failure.is_some() || (credit.next_send() as usize) == bodies.len()
            {
                in_flight
            } else {
                (in_flight / 2).max(1)
            };
            for _ in 0..drain_target {
                let resp = match self.read_response() {
                    Ok(resp) => resp,
                    Err(e) => return Err(failure.unwrap_or(e)),
                };
                credit.record_ack(credit.acked_seq().saturating_add(1));
                match resp {
                    Response::PublishAck { offset, deduped } => acks.push((offset, deduped)),
                    other => {
                        let e = self.reply_error(other);
                        failure.get_or_insert(e);
                    }
                }
            }
        }
        match failure {
            Some(e) => Err(e),
            None => Ok(acks),
        }
    }

    /// REGISTER A LAST WILL on this connection: the record the broker appends on your
    /// behalf when the connection ends, for any reason — a clean close, a crash, a
    /// cable pull the broker eventually notices. Blocks for the acknowledgement and
    /// returns its `(next, head)`.
    ///
    /// One per connection: a second call replaces the first. The will fires as an
    /// ordinary publish under `(producer_id, producer_seq)`, so publishing that key
    /// yourself first makes it a no-op (an exactly-once goodbye), and it is FENCED —
    /// it appends nothing if a record with a higher `producer_seq` from the same
    /// producer has landed since.
    ///
    /// The fence reads sequences BELOW [`ACK_SEQ_BASE`] only: an ack's sequence is
    /// derived from an offset rather than an incarnation, so acking under this producer
    /// id does not suppress this will. For the same reason a `producer_seq` in the
    /// reserved half is REFUSED here — the fence could not protect a will that lives
    /// where it does not look.
    pub fn will(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: &str,
        body: &[u8],
    ) -> io::Result<(u64, u64)> {
        self.send(&Request::Will {
            producer_id,
            producer_seq,
            subject: subject.to_string(),
            body: body.to_vec(),
        })?;
        self.read_mark()
    }

    /// Say this connection is alive: one [`Request::Keepalive`], answered with a
    /// `Mark` and appending nothing. Over TCP it also asks the broker to HOLD the
    /// connection to its keepalive deadline
    /// ([`KEEPALIVE_DEADLINE`](crate::broker::KEEPALIVE_DEADLINE), 60 s unless the
    /// broker set another) from now on: a connection that then goes that long with
    /// nothing read from it is ended exactly as a closed one is — its will fires, its
    /// slot is freed — which is how the broker notices a client that vanished without a
    /// FIN (a power loss, a NAT or Wi-Fi change, a cable pull) instead of waiting on it
    /// until the broker restarts. So a client that keeps alive once keeps alive for the
    /// connection's life: every
    /// [`KEEPALIVE_INTERVAL`](crate::broker::KEEPALIVE_INTERVAL) it has sent nothing
    /// else, which is [`keepalive_if_idle`](Self::keepalive_if_idle). Any request
    /// counts, so a busy connection never needs one. A Unix-socket connection is
    /// answered and never held: its peer cannot vanish without its kernel closing it.
    ///
    /// ARMING ON A KEEPALIVE IS THE VERSION CHECK, the twin of
    /// [`Subscription::recv_live`] arming on a `Mark`: a client that never sends one is
    /// never cut, so every client that predates the verb idles exactly as it always has.
    ///
    /// `Ok(true)`: the broker knows the verb. `Ok(false)`: it predates it — it answered
    /// `ERR_REFUSED` ("malformed request"), a whole reply, so this client is in step —
    /// and it will never cut this connection; `keepalive_if_idle` sends nothing more
    /// on it.
    ///
    /// PRECONDITION, as for [`fetch`](Self::fetch): no pipelined publish is un-acked on
    /// this connection.
    ///
    /// # Errors
    /// The socket; on a guarded broker, `ERR_UNAUTHORIZED` while no capability is
    /// attached (a keepalive grants nothing, but its answer names the log's head).
    pub fn keepalive(&mut self) -> io::Result<bool> {
        self.send(&Request::Keepalive)?;
        match self.read_mark() {
            Ok(_) => Ok(true),
            Err(e) if is_refused_error(&e) => {
                self.keepalive_unknown = true;
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    /// [`keepalive`](Self::keepalive) when this client has sent nothing for `interval`
    /// — pass [`KEEPALIVE_INTERVAL`](crate::broker::KEEPALIVE_INTERVAL) — and nothing at
    /// all otherwise, or once the broker has answered a keepalive as a verb it does not
    /// know. Call it from a loop that wakes at least that often: it reads the clock
    /// and, while the connection is busy, sends nothing. Returns whether a keepalive
    /// went out.
    ///
    /// Idleness runs from the last request WRITTEN (or the client's making), whatever
    /// it was; a blocking request's own wait for its answer is not idleness on the
    /// broker's side, which counts only the time it spends waiting to read.
    ///
    /// # Errors
    /// As [`keepalive`](Self::keepalive).
    pub fn keepalive_if_idle(&mut self, interval: Duration) -> io::Result<bool> {
        if self.keepalive_unknown || self.sent_at.elapsed() < interval {
            return Ok(false);
        }
        let _ = self.keepalive()?;
        Ok(true)
    }

    /// READ `producer_id`'s DISTINCT-SUBJECT BUDGET: one [`Request::Budget`], answered
    /// with what the broker's log counts against the producer and the budget it
    /// counts against ([`SubjectBudget`]). Appends nothing.
    ///
    /// It is the count a subject-bound refusal is decided by
    /// ([`BrokerRefusal::is_subject_bound`]), read after this connection's own pending
    /// acks, so a producer can keep headroom for the subjects it cannot do without
    /// from the broker's number rather than from a guess of its own.
    ///
    /// `Ok(None)`: the broker predates the verb — it answered `ERR_REFUSED` ("malformed
    /// request"), a whole reply, so this client is in step — and cannot say.
    ///
    /// PRECONDITION, as for [`fetch`](Self::fetch): no pipelined publish is un-acked on
    /// this connection.
    ///
    /// # Errors
    /// The socket; on a guarded broker, `ERR_UNAUTHORIZED` unless the ring holds a
    /// genuine, unexpired read-write grant that publishes as `producer_id` (bound to
    /// it, or unbound).
    pub fn subject_budget(&mut self, producer_id: u64) -> io::Result<Option<SubjectBudget>> {
        self.send(&Request::Budget { producer_id })?;
        match self.read_response()? {
            Response::Budget {
                subjects,
                reserved,
                max,
            } => Ok(Some(SubjectBudget {
                subjects,
                reserved,
                max,
            })),
            other => match self.reply_error(other) {
                e if is_refused_error(&e) => Ok(None),
                e => Err(e),
            },
        }
    }

    /// Ask for catch-up and idle marks on this connection's next subscription.
    /// `subscribe` and `subscribe_group` negotiate this before handing the socket
    /// over. A broker that does not know the verb returns `Ok(false)` and leaves
    /// the stream framed; the subscription retains that broker's behavior.
    /// This request arms no deadline and appends no record. A guarded broker
    /// requires an attached capability because the acknowledgement names the head.
    ///
    /// As for `fetch`, no pipelined publish may be awaiting its acknowledgement.
    pub fn idle_marks(&mut self) -> io::Result<bool> {
        self.send(&Request::IdleMarks)?;
        match self.read_mark() {
            Ok(_) => Ok(true),
            Err(e) if is_refused_error(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// LAST-VALUE query: the most recent record of every subject matching `filter`
    /// whose subject sorts strictly after `after` (`""` for the first page), at most
    /// `max`, in ascending SUBJECT order. Returns the page and the `(next, head)` of
    /// the closing `Mark`.
    ///
    /// `next` pairs with THIS PAGE, and with no other: it is the head this page was
    /// read at, so a `subscribe` from it tails gap-free and dup-free from THIS
    /// snapshot. A PAGED reader must therefore tail from the FIRST page's `next` and
    /// fold newest-wins across the pages — later pages are read at later heads, and
    /// subscribing from the LAST page's `next` silently skips any record that
    /// superseded a value an earlier page already reported.
    ///
    /// The broker CLAMPS `max` and bounds its scan of the subject index, so a short
    /// page — an EMPTY one included — does not mean the answer ended, and this method
    /// has already thrown away the cursor that says whether it did. It is ONE page, and
    /// nothing that must see the whole face should read it as more. Walk instead:
    /// [`last_walk`](Self::last_walk) (streaming, row-bounded, stoppable) or
    /// [`last_all`](Self::last_all) (collected) follow the cursor to the end, keep the
    /// FIRST page's mark, and turn a walk that does not end into an error.
    /// [`last_page`](Self::last_page) is the one-request primitive under them.
    ///
    /// Non-terminal: the connection is fully usable afterwards (this is the one read
    /// verb that does NOT consume the client).
    pub fn last(&mut self, filter: &str, after: &str, max: u32) -> io::Result<Page> {
        let (page, mark, _) = self.last_page(filter, after, max)?;
        Ok((page, mark))
    }

    /// [`last`](Self::last) with the RESUME CURSOR: `(page, (next, head), resume)`.
    ///
    /// `resume` is the `after` for the next page; an EMPTY `resume` means the broker's
    /// scan reached the end of the filter's range and this was the last page. Page
    /// until it is empty — a page shorter than `max` is NOT the end, because the
    /// broker bounds how many index entries one request may visit (matched or not) as
    /// well as how many rows it may return. [`last_walk`](Self::last_walk) is that loop.
    pub fn last_page(
        &mut self,
        filter: &str,
        after: &str,
        max: u32,
    ) -> io::Result<(Vec<Record>, (u64, u64), String)> {
        self.send(&Request::Last {
            filter: filter.to_string(),
            after: after.to_string(),
            max,
        })?;
        self.read_page_resumable(max)
    }

    /// WALK A LAST-VALUE FACE: [`last_page`](Self::last_page) repeated on its resume
    /// cursor until the broker says the filter's range is exhausted, each row handed to
    /// `on_row` in ascending subject order. This is the one loop that knows how a
    /// `Last` answer ENDS, so a reader that must see every subject — a roster, a halt
    /// list, an escalation view — calls it rather than restating it.
    ///
    /// THE END IS AN EMPTY `resume`, AND NOTHING ELSE. The broker clamps each request
    /// to `LAST_PAGE_MAX` rows and cuts its index scan after `LAST_SCAN_MAX` entries
    /// VISITED, matched or not, so a page shorter than asked — an EMPTY one included —
    /// is not the end of the answer. The walk pages on the cursor, never on the row
    /// count and never on the last row's subject, which an empty page does not have.
    ///
    /// THE MARK IS THE FIRST PAGE'S. Each request pins its own, later head, so the
    /// answer is a UNION of per-page snapshots, not one snapshot. The `(next, head)`
    /// returned is the first request's — the lowest pin, and so the only offset a
    /// `subscribe(next, filter)` can splice on at with no gap: from a later page's
    /// `next`, a record that superseded a value an earlier page already delivered would
    /// never arrive. The reader pays for the splice: a row from page 2..n may carry an
    /// offset AT OR ABOVE the returned `head`, and the tail may supersede a delivered
    /// row, so a reader that tails folds newest-wins and dedups on offset. Rows never
    /// repeat within one walk; the cursor is a strictly increasing subject.
    ///
    /// AND THE ROWS ALONE ARE NOT THE WHOLE FACE. A subject whose newest record lands
    /// at or above a request's pinned head before that request reaches it is OMITTED
    /// from its page rather than answered stale (see [`Request::Last`]); the record
    /// that displaced it arrives on the tail from `next`. The whole face is the rows
    /// PLUS that tail.
    ///
    /// `after` starts the walk strictly after that subject (`""` for the whole face).
    /// `max` bounds the ROWS the whole walk delivers, not one page's, and each request
    /// asks the broker for exactly the rows still owed, so it never works for rows the
    /// caller will not take; `u32::MAX` is the whole face. `max = 0` is the head query:
    /// one request, no rows, the mark.
    ///
    /// `on_row` runs once its row's page has been read to the closing `Mark`, so the
    /// connection is at a frame boundary whenever it runs and stays usable however the
    /// walk ends — [`Walk::Stop`], the row bound, an `on_row` error. Memory is one page,
    /// not the face.
    ///
    /// Returns the first page's `(next, head)` and the CONTINUATION:
    ///
    /// - `None` — the walk delivered the whole answer: the broker's cursor came back
    ///   empty and every row it sent reached `on_row` (a `Stop` on the very last row
    ///   included).
    /// - `Some(after)` — the walk ended early, on [`Walk::Stop`] or on `max`. Pass it
    ///   back as `after` to continue; it is the subject of the last row delivered (or
    ///   this walk's own `after`, if it delivered none), and continuing from it may
    ///   find nothing more. It is an `Option` where `last_page` has an
    ///   empty-means-done `String` because a walk that stopped before its first row
    ///   must be able to say "continue from the start", and that cursor IS `""`.
    ///
    /// # Errors
    ///
    /// A broker or transport failure, including the broker refusing a request; the
    /// first error `on_row` returns, which ends the walk and is returned as is; or a
    /// walk that has not ended within [`LAST_WALK_PAGES_MAX`] requests — because a
    /// caller that cannot tell "no rows" from "I stopped looking" reports an empty
    /// fleet while a node is escalating. Rows delivered before an error WERE
    /// delivered; the error says the answer is incomplete, not that nothing was seen.
    pub fn last_walk(
        &mut self,
        filter: &str,
        after: &str,
        max: u32,
        mut on_row: impl FnMut(Record) -> io::Result<Walk>,
    ) -> io::Result<((u64, u64), Option<String>)> {
        let mut cursor = after.to_string();
        // The continuation if the walk ends early: the last subject DELIVERED, which
        // starts as the walk's own `after` so a walk that delivered nothing hands that
        // back. `clone_from` reuses the buffer, so tracking it costs no allocation per
        // row once it has grown to the longest subject.
        let mut last = cursor.clone();
        let mut remaining = max;
        // The FIRST request's mark; every later page's is dropped (see above).
        let mut first: Option<(u64, u64)> = None;
        for _ in 0..LAST_WALK_PAGES_MAX {
            let ask = remaining;
            let (page, page_mark, resume) = self.last_page(filter, &cursor, ask)?;
            let mark = *first.get_or_insert(page_mark);
            let rows = page.len();
            let mut taken = 0usize;
            let mut stopped = false;
            for row in page {
                if remaining == 0 {
                    // Unreachable against `last_page`, which refuses a page longer
                    // than it asked for; kept so `max` holds whatever reads the page.
                    break;
                }
                remaining -= 1;
                taken = taken.saturating_add(1);
                last.clone_from(&row.1);
                if on_row(row)? == Walk::Stop {
                    stopped = true;
                    break;
                }
            }
            // `ask > 0`: a head query's empty cursor means "no page", not "complete".
            if resume.is_empty() && taken == rows && ask > 0 {
                return Ok((mark, None));
            }
            if stopped || remaining == 0 {
                return Ok((mark, Some(last)));
            }
            cursor = resume;
        }
        Err(io::Error::other(format!(
            "the last-value walk of {filter} did not end within {LAST_WALK_PAGES_MAX} pages"
        )))
    }

    /// The WHOLE last-value face of `filter`, collected: [`last_walk`](Self::last_walk)
    /// from the first subject with no row bound. Returns every row, in ascending subject
    /// order, and the FIRST page's `(next, head)` — the gap-free splice point for a
    /// `subscribe(next, filter)` that goes on watching the face (fold newest-wins across
    /// the seam; `last_walk` says why).
    ///
    /// `Ok` only for an answer the broker said was complete; a walk past
    /// [`LAST_WALK_PAGES_MAX`] pages is an error, never a short list. Memory is the
    /// whole face; a reader that can act row by row walks instead.
    pub fn last_all(&mut self, filter: &str) -> io::Result<Page> {
        let mut rows = Vec::new();
        let mut cursor = String::new();
        let mut remaining = u32::MAX;
        let mut first: Option<(u64, u64)> = None;
        for _ in 0..LAST_WALK_PAGES_MAX {
            let ask = remaining;
            let (page, page_mark, resume) = self.last_page(filter, &cursor, ask)?;
            let mark = *first.get_or_insert(page_mark);
            let page_rows = page.len();
            let mut taken = 0usize;
            for row in page {
                if remaining == 0 {
                    break;
                }
                remaining -= 1;
                taken = taken.saturating_add(1);
                rows.push(row);
            }
            if resume.is_empty() && taken == page_rows && ask > 0 {
                return Ok((rows, mark));
            }
            if remaining == 0 {
                return Err(io::Error::other(format!(
                    "the last-value face of {filter} did not end within {} rows",
                    u32::MAX
                )));
            }
            cursor = resume;
        }
        Err(io::Error::other(format!(
            "the last-value walk of {filter} did not end within {LAST_WALK_PAGES_MAX} pages"
        )))
    }

    /// BOUNDED READ: at most `max` records matching `filter` with offset >= `from`,
    /// scanning at most the broker's [`FETCH_SCAN_MAX`](crate::broker::FETCH_SCAN_MAX)
    /// records. Returns the page and the `(next, head)` of its closing `Mark`; `next`
    /// is the offset AFTER the last record the broker SCANNED, so paging with it
    /// advances even when a page matched nothing. `max = 0` is the head query.
    ///
    /// Non-terminal: unlike `subscribe` it never tails and does not consume the
    /// client — the connection is fully usable afterwards.
    pub fn fetch(&mut self, from: u64, filter: &str, max: u32) -> io::Result<Page> {
        self.send(&Request::Fetch {
            from_offset: from,
            filter: filter.to_string(),
            max,
        })?;
        let (page, mark, _) = self.read_page_resumable(max)?;
        Ok((page, mark))
    }

    /// Read `Delivery*` then the closing `Mark` of a bounded, non-terminal read,
    /// keeping the `Mark`'s subject cursor.
    ///
    /// The page is buffered before it is returned, so it is bounded by the `max` the
    /// request asked for (the broker never sends more): a peer that keeps sending
    /// deliveries past it is a protocol violation, not a longer page.
    ///
    /// PRECONDITION: no pipelined publish is un-acked on this connection. The broker
    /// flushes pending acks BEFORE the page (they arrive first, in order), so a caller
    /// that owes itself acks must drain them first; the blocking `publish` never does.
    fn read_page_resumable(&mut self, max: u32) -> io::Result<(Vec<Record>, (u64, u64), String)> {
        let mut page = Vec::new();
        loop {
            match self.read_response()? {
                // The rest of that page is still on the stream, unread: out of step.
                Response::Delivery { .. } if page.len() >= max as usize => {
                    return Err(self.lose(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("broker sent more than the {max} records the page asked for"),
                    )))
                }
                Response::Delivery {
                    offset,
                    subject,
                    body,
                } => page.push((offset, subject, body)),
                Response::Mark {
                    next,
                    head,
                    resume,
                    durable,
                } => {
                    self.saw_durable(durable);
                    return Ok((page, (next, head), resume));
                }
                other => return Err(self.reply_error(other)),
            }
        }
    }

    /// Subscribe to every record matching `filter` from `from_offset`, then live.
    /// Consumes the client (the connection becomes a delivery stream). Negotiates
    /// [`idle_marks`](Self::idle_marks) first, falling back to an older broker's
    /// existing tail behavior. The acknowledgement is consumed before any
    /// subscription event, so only a tail's own mark arms `recv_live`.
    pub fn subscribe(mut self, from_offset: u64, filter: &str) -> io::Result<Subscription<S>> {
        self.idle_marks()?;
        self.into_subscription(&Request::Subscribe {
            from_offset,
            filter: filter.to_string(),
        })
    }

    /// Durably advance consumer `group`'s committed offset to `upto` (a pure commit).
    /// Blocks for the ack; returns the commit record's offset.
    pub fn commit(&mut self, group: &str, upto: u64) -> io::Result<u64> {
        self.send(&Request::Commit {
            group: group.to_string(),
            upto,
        })?;
        let (offset, _) = self.recv_publish_ack()?;
        Ok(offset)
    }

    /// The read-process-write TRANSACTION: append `out_body` to `out_subject` AND
    /// commit `group` to `upto` atomically (one durable record), deduped by
    /// `(producer_id, producer_seq)`. Returns `(offset, deduped)`.
    #[allow(clippy::too_many_arguments)]
    pub fn process_and_produce(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        out_subject: &str,
        out_body: &[u8],
        group: &str,
        upto: u64,
    ) -> io::Result<(u64, bool)> {
        self.send(&Request::ProcessAndProduce {
            producer_id,
            producer_seq,
            out_subject: out_subject.to_string(),
            out_body: out_body.to_vec(),
            group: group.to_string(),
            upto,
        })?;
        self.recv_publish_ack()
    }

    /// Subscribe as consumer `group`: the broker resumes delivery from the group's
    /// DURABLE committed offset (no client-tracked cursor), then tails live.
    /// Negotiates [`idle_marks`](Self::idle_marks) as [`subscribe`](Self::subscribe)
    /// does; an older broker keeps its existing tail behavior.
    pub fn subscribe_group(mut self, group: &str, filter: &str) -> io::Result<Subscription<S>> {
        self.idle_marks()?;
        self.into_subscription(&Request::SubscribeGroup {
            group: group.to_string(),
            filter: filter.to_string(),
        })
    }

    /// COUNTERFACTUAL fork-delivery: stream the alternate timeline that results from
    /// forking the topic at `fork_at` and swapping that record for
    /// `(replacement_subject, replacement_body)`, filtered by `filter`. The live log
    /// is unchanged; the snapshot is delivered then the stream ends (`recv` → `None`).
    pub fn fork_subscribe(
        self,
        fork_at: u64,
        replacement_subject: &str,
        replacement_body: &[u8],
        filter: &str,
    ) -> io::Result<Subscription<S>> {
        self.into_subscription(&Request::ForkSubscribe {
            fork_at,
            replacement_subject: replacement_subject.to_string(),
            replacement_body: replacement_body.to_vec(),
            filter: filter.to_string(),
        })
    }

    /// Write one request frame. A request that cannot be FRAMED (over the frame cap)
    /// is refused before a byte is written, so the client stays in step; a failed
    /// WRITE may have sent part of a frame, so it latches the client out of step.
    fn send(&mut self, req: &Request) -> io::Result<()> {
        self.in_step()?;
        let mut out = Vec::new();
        append_frame(&mut out, &encode_request(req))?;
        if let Err(e) = self.stream.write_all(&out) {
            return Err(self.lose(e));
        }
        self.sent_at = Instant::now();
        Ok(())
    }

    /// Read the one response a request is owed. A close is `UnexpectedEof`, a broker in
    /// another protocol a [`ProtocolMismatch`] and an undecodable payload
    /// `InvalidData`; a broker `Error` is returned AS a response, because a refusal
    /// leaves the stream in step and a transport failure does not — every failure here
    /// latches the client out of step.
    fn read_response(&mut self) -> io::Result<Response> {
        self.in_step()?;
        let read = match read_frame(&mut self.stream) {
            Ok(Some(p)) => decode_reply(&p),
            Ok(None) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "broker closed",
            )),
            Err(e) => Err(e),
        };
        match read {
            Ok(resp) => Ok(resp),
            Err(e) => Err(self.lose(e)),
        }
    }

    /// Read the `Mark` that acknowledges an `Attach` or a `Will`: its `(next, head)`.
    fn read_mark(&mut self) -> io::Result<(u64, u64)> {
        match self.read_response()? {
            Response::Mark {
                next,
                head,
                durable,
                ..
            } => {
                self.saw_durable(durable);
                Ok((next, head))
            }
            other => Err(self.reply_error(other)),
        }
    }

    /// The error a reply other than the one a request is owed becomes: the broker's
    /// refusal, which leaves the client in step — or any other reply, which is the
    /// signature of a LATE one (owed to an earlier request), and latches it out.
    fn reply_error(&mut self, resp: Response) -> io::Error {
        match resp {
            Response::Error { code, msg } => BrokerRefusal { code, msg }.into(),
            _ => self.lose(unexpected_response()),
        }
    }

    /// Latch this client out of step with its replies because of `e`, and hand `e`
    /// back. The first cause is the one kept.
    fn lose(&mut self, e: io::Error) -> io::Error {
        if self.lost.is_none() {
            self.lost = Some(e.to_string());
        }
        e
    }

    /// `Ok` while the client is in step with its replies; once it is not, the
    /// `BrokenPipe` every later request fails with. Not `ErrorKind::Other`, which is
    /// what a broker refusal is.
    fn in_step(&self) -> io::Result<()> {
        match &self.lost {
            None => Ok(()),
            Some(why) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                format!(
                    "this connection is out of step with its replies since a request on it \
                     failed ({why}): reconnect"
                ),
            )),
        }
    }

    /// Send a streaming request and hand the connection over to its delivery stream.
    fn into_subscription(mut self, req: &Request) -> io::Result<Subscription<S>> {
        self.send(req)?;
        Ok(Subscription {
            stream: self.stream,
            rx: RecvBuf::default(),
            marked: false,
            durable: self.durable,
        })
    }
}

fn unexpected_response() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "unexpected response")
}

fn is_refused_error(e: &io::Error) -> bool {
    match broker_refusal(e) {
        Some(r) => r.code == ERR_REFUSED,
        None => false,
    }
}

/// A reply payload as the response it carries — or, from a broker that speaks another
/// protocol, the [`ProtocolMismatch`] it is. Asked FIRST, so a reply this build cannot
/// decode for that reason is never reported as a garbled one.
fn decode_reply(p: &[u8]) -> io::Result<Response> {
    if let Some(broker) = foreign_protocol(p) {
        return Err(ProtocolMismatch {
            broker,
            client: PROTO_VERSION,
        }
        .into());
    }
    match decode_response(p) {
        Some(resp) => Ok(resp),
        None => Err(unexpected_response()),
    }
}

/// The broker and this client speak DIFFERENT astream protocols (`PROTO_VERSION`s), so
/// nothing either says can be decoded by the other: the broker's reply came stamped with
/// another version — what an older broker's "malformed request" is — or was its
/// [`ERR_PROTOCOL`] refusal, which names its version. The
/// broker closes the connection after it, and the client is out of step for good.
/// The remedy is the same astream release on both ends; no retry on this pair succeeds.
///
/// It reaches the caller inside an `io::Error` of kind `Unsupported` — the kind of a
/// transport this build cannot speak, and for the same reason: two builds that do not
/// match. Read it back with [`protocol_mismatch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolMismatch {
    /// The protocol the broker speaks.
    pub broker: u8,
    /// The protocol this client speaks (this build's `PROTO_VERSION`).
    pub client: u8,
}

impl std::fmt::Display for ProtocolMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "protocol mismatch: broker speaks astream protocol {}, this client {} (run the \
             same astream release on both ends)",
            self.broker, self.client
        )
    }
}

impl std::error::Error for ProtocolMismatch {}

impl From<ProtocolMismatch> for io::Error {
    /// `ErrorKind::Unsupported`, the mismatch inside.
    fn from(m: ProtocolMismatch) -> io::Error {
        io::Error::new(io::ErrorKind::Unsupported, m)
    }
}

/// The protocol mismatch `e` carries, if it is one: `Some` exactly when a broker's reply
/// said it speaks another astream protocol (see [`ProtocolMismatch`]).
pub fn protocol_mismatch(e: &io::Error) -> Option<&ProtocolMismatch> {
    e.get_ref()?.downcast_ref()
}

/// The error a response other than the one a stream expects becomes: the broker's
/// own refusal ([`BrokerRefusal`]), or a protocol violation.
fn refused(resp: Response) -> io::Error {
    match resp {
        Response::Error { code, msg } => BrokerRefusal { code, msg }.into(),
        _ => unexpected_response(),
    }
}

/// A REFUSAL the broker sent — its `Response::Error` — as the client hands it back:
/// inside the `io::Error` a refused request returns, with `ErrorKind::Other`. Read it
/// back with [`broker_refusal`].
///
/// `code` is the refusal's class (the [`proto`](crate::proto) table of `ERR_*`
/// codes); `msg` is the broker's words, and is the error's `Display`, so a caller
/// that only prints or matches the message sees exactly what it always did.
///
/// A refusal is a whole reply: the client stays in step and the next request on the
/// connection is answered normally. Every `ErrorKind::Other` the request/reply verbs
/// return is one of these, except what a client decides for itself — the error a
/// [`Client::last_walk`] callback returns, a walk past [`LAST_WALK_PAGES_MAX`] pages,
/// [`Client::last_all`] past `u32::MAX` rows — which carries no refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerRefusal {
    /// The `proto::ERR_*` class.
    pub code: u8,
    /// The broker's message.
    pub msg: String,
}

impl BrokerRefusal {
    /// Whether the SAME request, resent unchanged on the same connection, is refused
    /// again — a record that can never land, a read the grants never cover — so a
    /// caller retrying it would loop: [`ERR_BAD_SUBJECT`], [`ERR_BAD_FILTER`],
    /// [`ERR_RECORD_REFUSED`], [`ERR_UNAUTHORIZED`], [`ERR_PROTOCOL`] (which this
    /// client reads as a [`ProtocolMismatch`] when the refusal names the broker's
    /// version).
    ///
    /// `false` is NOT "will succeed": it is every other code, [`ERR_REFUSED`] above
    /// all — a write that failed to append or commit, a quorum not reached — then
    /// [`ERR_SUBJECT_BOUND`] (see [`is_subject_bound`](Self::is_subject_bound)), and
    /// every code this client does not know. A broker older than the permanent codes
    /// answered every refusal but 2, 3 and 6 with `ERR_REFUSED`, so against it this is
    /// `false` for a permanent refusal too: the fail-safe reading, under which a
    /// caller retries rather than drops.
    pub const fn is_permanent(&self) -> bool {
        matches!(
            self.code,
            ERR_BAD_SUBJECT | ERR_BAD_FILTER | ERR_RECORD_REFUSED | ERR_UNAUTHORIZED | ERR_PROTOCOL
        )
    }

    /// Whether the producer is at its DISTINCT-SUBJECT BUDGET and the request named a
    /// subject new to the index ([`ERR_SUBJECT_BOUND`]): nothing was appended.
    ///
    /// Not [`is_permanent`](Self::is_permanent): the identical request lands once
    /// another producer introduces the subject (an update spends nothing) or the
    /// producer's own count falls. Whether to wait for that is the PRODUCER's
    /// question, because only it knows who else writes the subject: a producer that
    /// alone writes it — its own lanes — can drop the record for good, and one that
    /// shares it can hold the record. [`Client::subject_budget`] reads the count.
    ///
    /// `false` against a broker that predates the code, which sends [`ERR_REFUSED`]
    /// for this refusal: the fail-safe reading, under which a caller retries.
    pub const fn is_subject_bound(&self) -> bool {
        self.code == ERR_SUBJECT_BOUND
    }

    /// Whether the refusal is an EXPIRED capability: [`ERR_UNAUTHORIZED`] whose
    /// message starts [`CAP_EXPIRED_PREFIX`]. The broker sends that refusal for a
    /// genuine grant whose `exp=` has passed, whether at the `Attach`, on a request
    /// every covering grant has lapsed for, or when a stream ends at its grants'
    /// deadline. Its remedy is to mint the same grant again with a later `exp=`.
    ///
    /// `false` is every other refusal, every other code-8 one included: a proof that
    /// does not verify (a forged or tampered grant, or one minted under another
    /// secret), a request no attached grant ever covered, a full keyring, a producer
    /// id bound elsewhere, and a connection that stayed unattached too long. Neither
    /// half alone is enough. The words under another code are not an expiry, and code
    /// 8 under other words is not one either.
    ///
    /// Against a broker that predates [`CAP_EXPIRED_PREFIX`], this is `true` for an
    /// expired attach and for a stream ended at its deadline. It is `false` for a
    /// lone request whose grants lapsed, which such a broker refused as "does not
    /// grant".
    pub fn is_expired(&self) -> bool {
        self.code == ERR_UNAUTHORIZED && self.msg.starts_with(CAP_EXPIRED_PREFIX)
    }

    /// Whether the refusal is a POISONED log's: [`ERR_REFUSED`] whose message starts
    /// [`LOG_POISONED_PREFIX`]. The broker sends it for every write — publish, commit,
    /// will, binding, flush — while its log refuses every mutation until it is reopened
    /// ([`Broker::poisoned`](crate::Broker::poisoned)): a full flush failed with acked or
    /// held records no full flush covers, or a rollback could not put the file back.
    ///
    /// NOT [`is_permanent`](Self::is_permanent): the same record lands once the log is
    /// reopened. What it says is that nothing resent on THIS connection lands — the cure
    /// is a new open, which only the broker's embedder makes (a supervised broker exits
    /// and is started again), and the client sees it as the connection ending. A
    /// producer that reports whether its broker is working should report this broker
    /// as not, and wait for the reconnect, not retry into it.
    ///
    /// `false` is every other refusal, every other code-5 one included: a write that
    /// failed and was rolled back (the next may land on the same connection), a quorum
    /// not reached, a broker stopping. Against a broker that predates
    /// [`LOG_POISONED_PREFIX`] this is `false` for every refusal: such a broker answered
    /// the first op of a batch on a poisoned log `append failed: log poisoned (…)` and
    /// every later one `batch aborted (prior append I/O error)` — the fail-safe reading,
    /// under which a producer retries.
    pub fn is_poisoned(&self) -> bool {
        self.code == ERR_REFUSED && self.msg.starts_with(LOG_POISONED_PREFIX)
    }
}

impl std::fmt::Display for BrokerRefusal {
    /// The broker's message, exactly.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl std::error::Error for BrokerRefusal {}

impl From<BrokerRefusal> for io::Error {
    /// `ErrorKind::Other`, the refusal inside.
    fn from(r: BrokerRefusal) -> io::Error {
        io::Error::other(r)
    }
}

/// The broker refusal `e` carries, if it is one: `Some` exactly when a request was
/// answered with the broker's `Response::Error` (see [`BrokerRefusal`]). A transport
/// failure, a client-side bound and an out-of-step client are `None`.
pub fn broker_refusal(e: &io::Error) -> Option<&BrokerRefusal> {
    e.get_ref()?.downcast_ref()
}

/// How long [`take`] must have WAITED for a `Mark` to read it as the broker saying
/// "idle, now" rather than as one already queued on the socket. Under the broker's
/// liveness poll (200 ms), which is the closest two idle marks can come.
const IDLE_STOP_WAIT: Duration = Duration::from_millis(100);

/// Take at most `max` records from a group subscription WITHOUT committing anything.
///
/// Stops early on end-of-stream, or when a read TIMES OUT — so a caller that wants
/// "whatever has arrived within an idle window" sets that window with
/// [`Subscription::set_read_timeout`] first. That method exists on the three
/// transports whose socket is reachable — `Subscription<UnixStream>` (Unix builds),
/// `Subscription<TcpStream>` and (feature `aead`) the sealed
/// `Subscription<SealedStream<TcpStream>>`, which the `handshake` and `identity`
/// transports also produce. An erased [`AnySubscription`] sets it through the
/// [`Closer`] that [`connect`] returned with its client. On any OTHER stream type
/// there is no idle window: bound the socket yourself before subscribing (reach it
/// with [`Subscription::get_ref`]), or this parks in the read until the broker sends
/// something.
///
/// It also stops when the broker says the subscription is IDLE: a caught-up tail is
/// sent a `Mark` after every [`IDLE_MARK`](crate::broker::IDLE_MARK) with nothing to
/// deliver, and each one is a read that returned, so without this stop a window
/// longer than the broker's interval would never time out. The stop is on a mark
/// this take WAITED for (100 ms or longer). A mark that was already queued — the one
/// the tail sends as it first catches up, or marks that piled up while nobody was
/// taking — says nothing about now, and is passed over. So against a broker that
/// marks, a take parks for at most about two idle intervals; against one that does
/// not, it is exactly the read timeout.
///
/// The idle stop is frame-atomic: a timeout that lands part-way through a record
/// keeps that record's bytes on the subscription and resumes it on the next call
/// (see [`Subscription::recv_event`]), so the same `sub` can be taken from again.
/// The records are already durable and the group's cursor has NOT moved, so a
/// caller that dies here sees them again.
pub fn take<S: Read + Write>(sub: &mut Subscription<S>, max: usize) -> io::Result<Vec<Record>> {
    let mut out = Vec::new();
    while out.len() < max {
        let caught_up_before = sub.marked;
        let asked = Instant::now();
        match sub.recv_event() {
            Ok(Some(Event::Delivery {
                offset,
                subject,
                body,
            })) => out.push((offset, subject, body)),
            // An idle mark this take waited for: nothing more right now.
            Ok(Some(Event::Mark { .. }))
                if caught_up_before && asked.elapsed() >= IDLE_STOP_WAIT =>
            {
                break
            }
            // The catch-up mark, or one already queued: more may follow at once.
            Ok(Some(Event::Mark { .. })) => {}
            Ok(None) => break, // the broker closed
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break; // idle: nothing more right now
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// THE TWO-CONNECTION DRAIN: take at most `max` records from `sub` (a group
/// subscription) and then durably commit `group` through the last one on
/// `committer` — a SECOND connection, because `subscribe_group` consumes its client
/// and a group subscription's own connection is a one-way delivery stream. That price
/// is the reason this is a library helper rather than something every caller
/// re-invents.
///
/// The commit happens AFTER the take, so a failure between the two redelivers the
/// records rather than skipping them. But the group has moved past them by the time
/// `drain` returns: a caller that must PROCESS before committing (at-least-once into
/// an idempotent sink) calls [`take`] and commits itself.
///
/// A broker reaps a connection that sends no request within its first-frame timeout
/// ([`FIRST_FRAME_TIMEOUT`](crate::broker::FIRST_FRAME_TIMEOUT) by default), so a
/// FRESH `committer` whose first request is this commit fails if the take runs
/// longer than that. Make one request on it first — an attach, or
/// [`Client::hello`]; on a guarded broker the deadline runs until a capability is
/// accepted, so there only a successful attach does.
///
/// Returns the records; an empty result commits nothing.
pub fn drain<S: Read + Write, C: Read + Write>(
    sub: &mut Subscription<S>,
    committer: &mut Client<C>,
    group: &str,
    max: usize,
) -> io::Result<Vec<Record>> {
    let records = take(sub, max)?;
    if let Some((last, _, _)) = records.last() {
        committer.commit(group, *last)?;
    }
    Ok(records)
}

/// The top half of the producer sequence space, RESERVED for [`ack`].
///
/// Defined by the LOG ([`crate::store::ACK_SEQ_BASE`]) and re-exported here, so the
/// derivation below and the store's last-will fence read one constant: acks occupy
/// sequences at or above this value, ordinary publishes must stay below it, and the
/// fence — which asks whether a producer has been heard from since it registered its
/// will — ignores the reserved half entirely. `asb` enforces the lower half on `--seq`
/// and `--seq-file`.
pub use crate::store::ACK_SEQ_BASE;

/// THE READ-PROCESS-WRITE ACK: append the ack record AND advance `group` past
/// `offset` in ONE durable record, so the answer and the cursor can never disagree.
///
/// The `producer_seq` is [`ACK_SEQ_BASE`]` | offset`, which is what makes a retry
/// idempotent: a repeated ack for the same offset returns `deduped = true`, appends
/// nothing, and re-applies no commit — and it does so whether the retry came through
/// this helper or through `asb ack`, because both derive the key the same way.
///
/// `offset` must be below [`ACK_SEQ_BASE`] (any real log offset is, by many orders of
/// magnitude), and ordinary publishes by the same `producer_id` must stay below it too
/// — `asb` enforces that on `--seq`/`--seq-file`; a library caller must observe it
/// itself. Within those bounds an ack and an ordinary publish by one producer id never
/// share a dedup key.
///
/// An ack does not raise the producer's last-will fence: the reserved half is excluded
/// from `BrokerLog::producer_high_water`, because an ack's sequence is derived from an
/// offset rather than from an incarnation and is above every sequence a will can hold.
/// Acking under a producer id that also holds a will is therefore safe, and a will
/// registered at a sequence in the reserved half is refused.
///
/// Compatibility: an ack written by a version that keyed it on the BARE `offset` sits
/// under a different dedup key, so retrying it through this version appends a second
/// answer record (the monotone group commit is unaffected). Drain a producer's
/// in-flight acks before upgrading it.
pub fn ack<S: Read + Write>(
    client: &mut Client<S>,
    producer_id: u64,
    group: &str,
    offset: u64,
    out_subject: &str,
    out_body: &[u8],
) -> io::Result<(u64, bool)> {
    if offset >= ACK_SEQ_BASE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "an ack's input offset must stay in the lower half of the sequence space",
        ));
    }
    client.process_and_produce(
        producer_id,
        ACK_SEQ_BASE | offset,
        out_subject,
        out_body,
        group,
        offset,
    )
}

/// A live delivery stream from the broker.
#[cfg(unix)]
pub struct Subscription<S = UnixStream> {
    stream: S,
    /// What has been read off `stream` and not yet returned; see [`RecvBuf`].
    rx: RecvBuf,
    /// Whether a `Mark` has come down this stream yet (see [`take`]).
    marked: bool,
    /// The durable head the newest `Mark` on this stream carried
    /// ([`seen_durable`](Subscription::seen_durable)).
    durable: Option<u64>,
}

/// A live delivery stream from the broker (Windows twin: TCP default, as above).
#[cfg(not(unix))]
pub struct Subscription<S = TcpStream> {
    stream: S,
    /// What has been read off `stream` and not yet returned; see [`RecvBuf`].
    rx: RecvBuf,
    /// Whether a `Mark` has come down this stream yet (see [`take`]).
    marked: bool,
    /// The durable head the newest `Mark` on this stream carried
    /// ([`seen_durable`](Subscription::seen_durable)).
    durable: Option<u64>,
}

#[cfg(unix)]
impl Subscription<UnixStream> {
    /// A handle that can end this subscription from ANOTHER thread: `recv` blocks
    /// in a socket read with no timeout, so an owner that wants to stop a tailing
    /// thread (and release the broker-side connection) shuts the socket down through
    /// this closer, which makes the blocked `recv` return `Ok(None)`.
    ///
    /// The closer holds its own handle to the socket, so while it lives, dropping the
    /// subscription alone does NOT close the connection: call
    /// [`close`](SubscriptionCloser::close), or drop both.
    /// The TCP and sealed subscriptions have the same method, returning the same
    /// [`Closer`]; an erased [`AnySubscription`] has no socket left to reach, so its
    /// closer is the one [`connect`] handed back.
    pub fn closer(&self) -> io::Result<SubscriptionCloser> {
        Closer::unix(&self.stream)
    }
}

/// The name [`Closer`] had when only a Unix-socket [`Subscription`] could make one.
/// Kept so code written against it compiles unchanged: it IS [`Closer`], so it also
/// comes from a TCP or sealed subscription and from [`connect`].
pub type SubscriptionCloser = Closer;

/// What a subscription can carry: a record, or a `Mark`. [`Subscription::recv`] keeps
/// its signature and SKIPS marks; a caller that needs the cursor reads
/// [`recv_event`](Subscription::recv_event) instead, and
/// [`recv_live`](Subscription::recv_live) turns them into liveness.
///
/// On a `ForkSubscribe` stream the one `Mark` closes the snapshot. On a `Subscribe` or
/// `SubscribeGroup` stream that negotiated idle marks a `Mark` is the broker saying
/// it is there with nothing to deliver: once when the tail first CATCHES UP, then
/// again after each [`IDLE_MARK`](crate::broker::IDLE_MARK) with nothing written —
/// so a live idle subscription is never silent for long, and a silent one is a link
/// that has died.
#[derive(Debug, Clone, Eq)]
pub enum Event {
    /// One delivered record: `(offset, subject, body)`.
    Delivery {
        offset: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// Resume from `next`; `head` is the subscriber-visible head the broker read
    /// with — the end of a bounded read, or a tail that is caught up or idle.
    /// `durable` is the broker's durable head then: every record below it is on stable
    /// media. `None` from a broker that predates the field, which serves Strict or
    /// Relaxed, never Ordered: read its tier from `Hello`'s block
    /// ([`Client::served_durability`]) — on Strict the durable head is `head`; on Relaxed
    /// nothing acked since its open (or clean close) is durable, so `head` is not. A
    /// broker older still sends no block, and may serve Relaxed without saying so: there
    /// `head` is durable only for a reader that knows its broker strict. On a broker that
    /// serves Ordered, a tail that asked for idle marks also gets a `Mark` when a full
    /// flush makes records it was delivered durable.
    Mark {
        next: u64,
        head: u64,
        durable: Option<u64>,
    },
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Event::Delivery {
                    offset: a_o,
                    subject: a_s,
                    body: a_b,
                },
                Event::Delivery {
                    offset: b_o,
                    subject: b_s,
                    body: b_b,
                },
            ) => *a_o == *b_o && a_s.as_str() == b_s.as_str() && a_b.as_slice() == b_b.as_slice(),
            (
                Event::Mark {
                    next: a_n,
                    head: a_h,
                    durable: a_d,
                },
                Event::Mark {
                    next: b_n,
                    head: b_h,
                    durable: b_d,
                },
            ) => *a_n == *b_n && *a_h == *b_h && *a_d == *b_d,
            _ => false,
        }
    }
}

/// Bytes a subscription asks its stream for per read, and the size its receive
/// buffer returns to after an oversized frame.
const READ_CHUNK: usize = 64 * 1024;

/// A subscription's receive buffer: the bytes read off the stream that no `recv` has
/// returned yet, `buf[start..end]`.
///
/// Frames are parsed out of this buffer rather than read off the stream with
/// `read_exact`, for two reasons. A read can TIME OUT part-way through a frame
/// (`SO_RCVTIMEO`, under [`take`]'s idle window), and `read_exact` drops the bytes it
/// had already consumed when it does, so the next read would parse a body as a
/// header; here those bytes simply stay in the buffer and the next `recv` finishes
/// the frame. And one read takes every queued frame that fits, so a subscriber
/// catching up on a backlog pays one syscall per buffer rather than two per frame.
///
/// The buffer grows by at most [`READ_CHUNK`] per read — with what the peer has
/// actually SENT, never with the length a header declares — so a peer that announces
/// a 16 MiB frame and stalls pins only what it sent.
#[derive(Default)]
struct RecvBuf {
    buf: Vec<u8>,
    start: usize,
    end: usize,
}

impl RecvBuf {
    /// The payload of the next frame; `Ok(None)` when the stream closes at a frame
    /// boundary. A timeout keeps the unfinished frame for the next call; any other
    /// error discards it, since the stream cannot be resumed past one.
    fn next_frame(&mut self, stream: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
        loop {
            match Frame::decode(self.buf.get(self.start..self.end).unwrap_or(&[])) {
                Ok(Some(d)) => {
                    self.start = self.start.saturating_add(d.consumed);
                    if self.start == self.end {
                        self.reset();
                    }
                    return Ok(Some(d.frame.payload));
                }
                Ok(None) => {}
                Err(e) => {
                    self.reset();
                    let msg = match e {
                        FrameError::TooLarge => "frame exceeds cap",
                        _ => "corrupt frame",
                    };
                    return Err(io::Error::new(io::ErrorKind::InvalidData, msg));
                }
            }
            if self.end == self.buf.len() {
                // Full: slide the unfinished frame to the front, and grow only when it
                // already fills the whole buffer.
                let len = self.end.saturating_sub(self.start);
                if self.start > 0 {
                    let mut i = 0usize;
                    while i < len {
                        if let Some(&b) = self.buf.get(self.start.saturating_add(i)) {
                            if let Some(dst) = self.buf.get_mut(i) {
                                *dst = b;
                            }
                        }
                        i = i.saturating_add(1);
                    }
                }
                self.end = len;
                self.start = 0;
                if self.end == self.buf.len() {
                    for _ in 0..8 {
                        self.buf.extend_from_slice(&[0u8; 8192]);
                    }
                }
            }
            let Some(dst) = self.buf.get_mut(self.end..) else {
                self.reset();
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "receive buffer out of bounds",
                ));
            };
            match stream.read(dst) {
                Ok(0) => {
                    let partial = self.end.saturating_sub(self.start);
                    self.reset();
                    // As `proto::read_frame`: only a close with NOTHING of the next
                    // frame read is a close; once any byte of it — a header byte
                    // included — has arrived, the frame was cut short.
                    return if partial == 0 {
                        Ok(None)
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "broker closed mid-frame",
                        ))
                    };
                }
                Ok(n) => self.end = self.end.saturating_add(n),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => {
                    if !matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) {
                        self.reset();
                    }
                    return Err(e);
                }
            }
        }
    }

    /// Nothing is owed: empty the buffer, and give back whatever an oversized frame
    /// grew it by.
    fn reset(&mut self) {
        self.start = 0;
        self.end = 0;
        if self.buf.len() > READ_CHUNK {
            self.buf.truncate(READ_CHUNK);
            self.buf.shrink_to_fit();
        }
    }
}

impl<S: Read + Write> Subscription<S> {
    /// The underlying byte stream, for transport-level control — a timeout, a
    /// shutdown — that does not touch the record bytes. This is the escape hatch for
    /// a stream type that has no [`set_read_timeout`](Self::set_read_timeout) of its
    /// own: reach the socket through it and bound the read there. (A boxed
    /// [`AnyStream`] cannot be reached this way — erasing it hid the socket — which
    /// is why [`connect`] returns a [`Closer`] alongside it.) Do NOT read or write
    /// the stream itself — a subscription holds the bytes it has already read (the
    /// rest of a frame a timeout interrupted, and frames queued behind it), and
    /// bytes taken from underneath it are gone from the stream it resumes.
    pub const fn get_ref(&self) -> &S {
        &self.stream
    }

    /// The broker's durable head as the newest `Mark` on this stream told it (see
    /// [`Event::Mark`]) — or the client's before it subscribed. `None` before any.
    pub const fn seen_durable(&self) -> Option<u64> {
        self.durable
    }

    /// Block until the next matching delivery; `Ok(None)` if the broker closes.
    /// A `Mark` is SKIPPED (this method's signature is unchanged) — read
    /// [`recv_event`](Self::recv_event) to see it.
    pub fn recv(&mut self) -> io::Result<Option<Record>> {
        loop {
            match self.recv_event()? {
                None => return Ok(None),
                Some(Event::Mark { .. }) => continue,
                Some(Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => return Ok(Some((offset, subject, body))),
            }
        }
    }

    /// Block until the next event — a delivery OR a `Mark`; `Ok(None)` if the broker
    /// closes.
    ///
    /// A read that TIMES OUT part-way through a frame (see
    /// [`set_read_timeout`](Subscription::set_read_timeout)) is resumable, not a
    /// desync: the bytes that arrived stay on the subscription and the next call
    /// completes the frame from where the timeout left it, so the caller sees the
    /// whole record once — never a prefix dropped on the floor and the tail of a body
    /// parsed as the next header. Any other error discards the partial frame; the
    /// stream is not resumable past it.
    pub fn recv_event(&mut self) -> io::Result<Option<Event>> {
        let Some(p) = self.rx.next_frame(&mut self.stream)? else {
            return Ok(None);
        };
        match decode_reply(&p)? {
            Response::Delivery {
                offset,
                subject,
                body,
            } => Ok(Some(Event::Delivery {
                offset,
                subject,
                body,
            })),
            Response::Mark {
                next,
                head,
                durable,
                ..
            } => {
                self.marked = true;
                if durable.is_some() {
                    self.durable = durable;
                }
                Ok(Some(Event::Mark {
                    next,
                    head,
                    durable,
                }))
            }
            other => Err(refused(other)),
        }
    }

    /// [`recv`](Self::recv) that turns a DEAD LINK into an error instead of silence:
    /// the first `Mark` this subscription reads arms a read `deadline` on its socket
    /// through `closer` (the [`Closer`] from [`connect`], or this subscription's own
    /// `closer()`), and from then on `deadline` with no byte arriving fails the call
    /// `WouldBlock`/`TimedOut`. Every `Mark` re-arms it.
    ///
    /// A `Subscribe`/`SubscribeGroup` tail is sent a `Mark` when it catches up and
    /// after every [`IDLE_MARK`](crate::broker::IDLE_MARK) with nothing to deliver, so a
    /// healthy idle subscription keeps answering the deadline, and one whose peer
    /// vanished without a FIN — power loss, a NAT or Wi-Fi change, a frozen relay —
    /// times out instead of parking for good. Pass a deadline a few idle intervals
    /// long ([`IDLE_MARK_DEADLINE`](crate::broker::IDLE_MARK_DEADLINE)).
    ///
    /// ARMING ON A MARK IS THE VERSION CHECK. A broker that predates idle marks never
    /// marks a tail, so the deadline is never armed and this is exactly `recv` — no
    /// timeouts against a long-lived older broker. The other side of that: until the
    /// first `Mark` (a backlog still being delivered) the subscription is unbounded.
    ///
    /// A timeout leaves any partial frame resumable (see
    /// [`recv_event`](Self::recv_event)), but a caller that set this deadline means
    /// "the link is gone": drop the subscription and resubscribe.
    ///
    /// # Errors
    /// As `recv`; the timeout above; the `setsockopt` that arms it.
    pub fn recv_live(&mut self, closer: &Closer, deadline: Duration) -> io::Result<Option<Record>> {
        loop {
            match self.recv_event()? {
                None => return Ok(None),
                Some(Event::Mark { .. }) => closer.set_read_timeout(Some(deadline))?,
                Some(Event::Delivery {
                    offset,
                    subject,
                    body,
                }) => return Ok(Some((offset, subject, body))),
            }
        }
    }
}

#[cfg(unix)]
impl Subscription<UnixStream> {
    /// Bound how long [`recv`](Self::recv) / [`recv_event`](Self::recv_event) block
    /// with nothing arriving: past it the read returns `WouldBlock`/`TimedOut`, which
    /// is how [`drain`] stops on an idle stream instead of parking forever.
    ///
    /// The timeout is on the SOCKET, so it can land part-way through a record. That
    /// is not a desync: the partial frame stays on the subscription and the next
    /// `recv` finishes it (see [`recv_event`](Self::recv_event)), so the same `sub`
    /// is safe to drain from again.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }
}

impl Subscription<TcpStream> {
    /// The TCP twin of [`Subscription::set_read_timeout`].
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.set_read_timeout(d)
    }

    /// The TCP twin of [`Subscription::closer`]: a [`Closer`] on a duplicate of this
    /// subscription's socket, so another thread can end a `recv` parked on it.
    pub fn closer(&self) -> io::Result<Closer> {
        Closer::tcp(&self.stream)
    }
}

#[cfg(feature = "aead")]
impl Subscription<astream_aead::SealedStream<TcpStream>> {
    /// The SEALED twin of [`Subscription::set_read_timeout`] — the same idle window
    /// for a subscription from `connect_tcp_sealed`, `connect_tcp_handshake` or
    /// `connect_tcp_identity` (all three hand back a `SealedStream<TcpStream>`), so
    /// a consumer on the sealed transport can bound [`drain`] too.
    ///
    /// The timeout is set on the TCP socket UNDER the record layer, so it can land
    /// part-way through a sealed record as well as part-way through a frame. Both
    /// layers resume: the record layer keeps its partial record and its sequence
    /// counter (`astream_aead`'s `read_record`), and [`recv_event`](Self::recv_event)
    /// keeps the partial frame.
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.stream.get_ref().set_read_timeout(d)
    }

    /// The SEALED twin of [`Subscription::closer`]. The duplicate is of the TCP
    /// socket UNDER the record layer — never a second `SealedStream`, which would be a
    /// second record sequence over one socket — because shutting that socket down is
    /// what unparks the reader, and it is all the closer needs.
    pub fn closer(&self) -> io::Result<Closer> {
        Closer::tcp(self.stream.get_ref())
    }
}

/// Any byte stream a broker connection can ride, so a caller that picks its transport
/// at RUNTIME — a flag, a config file — has ONE code path per verb rather than one per
/// wire. Blanket-implemented for every `Read + Write + Send` type. `Send` is a
/// supertrait because a subscription is typically pumped by a thread of its own, and
/// every transport this crate builds is `Send`.
pub trait AnyStream: Read + Write + Send {}
impl<T: Read + Write + Send> AnyStream for T {}

/// A [`Client`] whose transport has been erased — what [`connect`] returns.
pub type AnyClient = Client<Box<dyn AnyStream>>;

/// A [`Subscription`] whose transport has been erased — what an [`AnyClient`]'s
/// `subscribe` / `subscribe_group` / `fork_subscribe` return. Its idle window and its
/// shutdown live on the [`Closer`] that came with the client.
pub type AnySubscription = Subscription<Box<dyn AnyStream>>;

/// How a client reaches the broker: the runtime choice [`connect`] dispatches on.
///
/// EVERY VARIANT EXISTS IN EVERY BUILD. The sealed and handshake wires need the `aead`
/// and `handshake` features, but their variants are not cfg-gated: Cargo unifies
/// features ADDITIVELY across a build graph, so a variant that appeared only when some
/// other crate in the graph switched `aead` on would break every exhaustive `match`
/// written against the default build. [`connect`] refuses a transport this build cannot
/// speak with `ErrorKind::Unsupported` naming the missing feature instead — so a config
/// that says "sealed" still parses and still redacts its key, and fails loudly at the
/// one place it matters, never by quietly speaking plaintext.
///
/// Deliberately NOT `#[non_exhaustive]`, for the opposite reason: a caller's exhaustive
/// match over transports (what to serve, what to name) is where a NEW wire should break
/// the build and be decided on, not fall through a wildcard arm into some other wire's
/// behaviour. Static-identity connections (`connect_tcp_identity`) are not a variant:
/// they carry a keypair and a pinned host key rather than a pre-shared key, and no
/// runtime-chosen caller of this enum serves them yet.
///
/// `Debug` NEVER prints a key: it names the variant and says the key is redacted, so a
/// config logged on a bad day does not put the fleet's pre-shared key in a file.
#[derive(Clone)]
pub enum Transport {
    /// A Unix-domain socket at a filesystem path: same machine, unsealed (the socket
    /// path's permissions scope who can reach it). Unix hosts only — elsewhere
    /// [`connect`] refuses it.
    Unix,
    /// PLAINTEXT TCP to `host:port` — a trusted network only (as
    /// [`Client::connect_tcp`]).
    Tcp,
    /// TCP inside the XChaCha20-Poly1305 sealed record layer under this 32-byte
    /// pre-shared key (as `Client::connect_tcp_sealed`; feature `aead`). Boxed so
    /// moving or cloning the config does not copy the key around the stack.
    Sealed(Box<[u8; 32]>),
    /// The sealed wire behind the Rung-7 forward-secret handshake: an ephemeral X25519
    /// key agreement authenticated by this 32-byte pre-shared key (as
    /// `Client::connect_tcp_handshake`; feature `handshake`).
    Handshake(Box<[u8; 32]>),
}

impl std::fmt::Debug for Transport {
    /// The variant, and never the key.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Transport::Unix => "Unix",
            Transport::Tcp => "Tcp",
            Transport::Sealed(_) => "Sealed(<key redacted>)",
            Transport::Handshake(_) => "Handshake(<key redacted>)",
        })
    }
}

/// Ends a connection from outside the thread using it, and bounds how long that
/// thread's I/O may park — the one handle on the SOCKET under a client or subscription
/// whose stream has been erased ([`connect`]) or wrapped (a sealed record layer).
///
/// It holds a DUPLICATE descriptor of the socket. For a sealed or handshake connection
/// that is the TCP socket UNDER the record layer — never a second `SealedStream`, which
/// would be a second record sequence over one socket. Shutdown and the socket timeouts
/// are properties of the socket rather than of a descriptor, so everything done through
/// the closer applies to the connection it came from, and keeps applying after
/// `subscribe` consumes the client. Dropping a closer does nothing to the connection;
/// [`close`](Self::close) is explicit. `Send + Sync`: one closer can be shared (an
/// `Arc`) with a watchdog.
#[derive(Debug)]
pub struct Closer {
    socket: Socket,
}

/// The socket a [`Closer`] duplicated.
#[derive(Debug)]
enum Socket {
    #[cfg(unix)]
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl Closer {
    #[cfg(unix)]
    fn unix(s: &UnixStream) -> io::Result<Closer> {
        Ok(Closer {
            socket: Socket::Unix(s.try_clone()?),
        })
    }

    fn tcp(s: &TcpStream) -> io::Result<Closer> {
        Ok(Closer {
            socket: Socket::Tcp(s.try_clone()?),
        })
    }

    /// Shut the socket down in BOTH directions: a `recv` (or a request's read) parked
    /// on it returns — `Ok(None)` at a frame boundary, an error part-way through one —
    /// and the broker sees the peer go away, which releases its side of the connection
    /// (and fires a registered last will). Idempotent: closing an already-closed socket
    /// is not an error, and nothing is reported.
    pub fn close(&self) {
        let _ = match &self.socket {
            #[cfg(unix)]
            Socket::Unix(s) => s.shutdown(std::net::Shutdown::Both),
            Socket::Tcp(s) => s.shutdown(std::net::Shutdown::Both),
        };
    }

    /// Bound how long a read on this connection parks with nothing arriving:
    /// `SO_RCVTIMEO` on the socket, so it applies to every read the client or
    /// subscription makes from now on (`None` clears it). Past it the read fails
    /// `WouldBlock`/`TimedOut`.
    ///
    /// On a SUBSCRIPTION that is an idle window, and a resumable one: the partial frame
    /// stays on the subscription ([`Subscription::recv_event`]) and the record layer
    /// keeps its partial record, so [`take`] / [`drain`] can stop and come back. On a
    /// request/reply CLIENT it is a deadline and nothing more: a request that timed out
    /// may still be answered late, so the connection is no longer in step with its
    /// replies, and the client says so — every later request on it fails `BrokenPipe`
    /// without being sent (see [`Client`]). Reconnect. That is what turns a broker that
    /// accepts and never answers from a caller parked forever into an error.
    ///
    /// # Errors
    /// The `setsockopt`; a zero duration is `InvalidInput` (std's own refusal).
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match &self.socket {
            #[cfg(unix)]
            Socket::Unix(s) => s.set_read_timeout(d),
            Socket::Tcp(s) => s.set_read_timeout(d),
        }
    }

    /// Bound how long a write on this connection parks on a full send buffer (a broker
    /// that stopped reading): `SO_SNDTIMEO` on the socket, `None` clears it. A write
    /// that times out may have sent PART of a frame, so the connection is no longer
    /// framed: a client refuses every later request on it (`BrokenPipe`); reconnect.
    ///
    /// # Errors
    /// The `setsockopt`; a zero duration is `InvalidInput` (std's own refusal).
    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match &self.socket {
            #[cfg(unix)]
            Socket::Unix(s) => s.set_write_timeout(d),
            Socket::Tcp(s) => s.set_write_timeout(d),
        }
    }
}

/// Open ONE connection to `endpoint` over `transport` — a socket path for
/// [`Transport::Unix`], `host:port` for the rest — and return it with its transport
/// erased, together with the [`Closer`] that ends it and bounds its I/O.
///
/// The closer is made HERE, at connect time, from a duplicate of the socket under
/// whatever wrapper the transport adds, because once the stream is boxed nothing can
/// reach that socket again — and it outlives the `subscribe` that consumes the client,
/// which is when a caller needs it most (to end a `recv` parked on another thread, or
/// to give a group subscription its idle window).
///
/// `connect_timeout` bounds ESTABLISHING the connection, and nothing after it:
///
/// - **TCP** (all three TCP transports): each address the endpoint resolves to is tried
///   in turn with `TcpStream::connect_timeout` (see
///   [`Client::connect_tcp_timeout`]), so a black-holed address fails `TimedOut` after
///   the bound instead of after the OS's SYN-retry schedule. The bound is per address,
///   and name resolution is not bounded (std's resolver has no timeout): an IP-literal
///   endpoint makes it a hard bound.
/// - **Sealed / handshake**: the bound also REPLACES the built-in deadline on the
///   transport's handshake I/O (5 s sealed, 10 s key agreement), so a peer that accepts
///   and never answers the hello costs `connect_timeout`, not the default.
/// - **Unix**: not applied. std has no bounded Unix-socket connect; a local connect
///   fails at once when nothing listens, and can park only while a LIVE listener's
///   accept backlog is full.
///
/// `None` is exactly each transport's own constructor — [`Client::connect`],
/// [`Client::connect_tcp`], `Client::connect_tcp_sealed`,
/// `Client::connect_tcp_handshake` — including their built-in handshake deadlines.
/// Either way every stream comes back BLOCKING: bound requests and deliveries with
/// [`Closer::set_read_timeout`] / [`Closer::set_write_timeout`]. Every TCP transport
/// gets `TCP_NODELAY`, as every TCP constructor here does.
///
/// # Errors
/// `Unsupported`, naming what is missing, for a transport this build cannot speak
/// ([`Transport::Sealed`] without the `aead` feature, [`Transport::Handshake`] without
/// `handshake`, [`Transport::Unix`] off a unix host); `InvalidInput` for a zero
/// `connect_timeout`; otherwise the connect, the handshake (a broker without the key is
/// refused HERE, not at the first verb), or the descriptor duplication the closer needs.
pub fn connect(
    transport: &Transport,
    endpoint: &str,
    connect_timeout: Option<Duration>,
) -> io::Result<(AnyClient, Closer)> {
    if connect_timeout == Some(Duration::ZERO) {
        // Refused up front so every transport says the same thing: std refuses a zero
        // TCP connect timeout itself, but a Unix connect would silently ignore it.
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a zero connect timeout bounds nothing: pass None for the OS default, or a positive duration",
        ));
    }
    match transport {
        Transport::Unix => connect_unix(endpoint),
        Transport::Tcp => {
            let s = dial_tcp(endpoint, connect_timeout)?;
            let closer = Closer::tcp(&s)?;
            Ok(erase(s, closer))
        }
        Transport::Sealed(key) => connect_sealed(endpoint, key.as_ref(), connect_timeout),
        Transport::Handshake(key) => connect_handshake(endpoint, key.as_ref(), connect_timeout),
    }
}

/// Box a connected stream behind [`AnyStream`], beside its closer.
fn erase<S: AnyStream + 'static>(stream: S, closer: Closer) -> (AnyClient, Closer) {
    (Client::from_stream(Box::new(stream)), closer)
}

#[cfg(unix)]
fn connect_unix(endpoint: &str) -> io::Result<(AnyClient, Closer)> {
    let s = UnixStream::connect(endpoint)?;
    let closer = Closer::unix(&s)?;
    Ok(erase(s, closer))
}

#[cfg(not(unix))]
fn connect_unix(_endpoint: &str) -> io::Result<(AnyClient, Closer)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Transport::Unix needs a unix host (std has no Unix-domain sockets here); use Transport::Tcp",
    ))
}

#[cfg(feature = "aead")]
fn connect_sealed(
    endpoint: &str,
    key: &[u8; 32],
    connect_timeout: Option<Duration>,
) -> io::Result<(AnyClient, Closer)> {
    let stream = dial_tcp(endpoint, connect_timeout)?;
    let sealed = seal_tcp(
        stream,
        *key,
        connect_timeout.unwrap_or(SEALED_HANDSHAKE_TIMEOUT),
    )?;
    let closer = Closer::tcp(sealed.get_ref())?;
    Ok(erase(sealed, closer))
}

/// Without `aead` there is no record layer to seal with, and a sealed transport must
/// never quietly become a plaintext one.
#[cfg(not(feature = "aead"))]
fn connect_sealed(
    _endpoint: &str,
    _key: &[u8; 32],
    _connect_timeout: Option<Duration>,
) -> io::Result<(AnyClient, Closer)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Transport::Sealed needs astream-broker built with the `aead` feature",
    ))
}

#[cfg(feature = "handshake")]
fn connect_handshake(
    endpoint: &str,
    psk: &[u8; 32],
    connect_timeout: Option<Duration>,
) -> io::Result<(AnyClient, Closer)> {
    let stream = dial_tcp(endpoint, connect_timeout)?;
    let sealed = agree_tcp(
        stream,
        psk,
        connect_timeout.unwrap_or(KEY_AGREEMENT_TIMEOUT),
    )?;
    let closer = Closer::tcp(sealed.get_ref())?;
    Ok(erase(sealed, closer))
}

/// Without `handshake` there is no key agreement, and falling back to the plain
/// sealed wire would silently drop the forward secrecy the caller asked for.
#[cfg(not(feature = "handshake"))]
fn connect_handshake(
    _endpoint: &str,
    _psk: &[u8; 32],
    _connect_timeout: Option<Duration>,
) -> io::Result<(AnyClient, Closer)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Transport::Handshake needs astream-broker built with the `handshake` feature",
    ))
}

#[cfg(test)]
mod tests {
    use super::{RecvBuf, READ_CHUNK};
    use crate::proto::read_frame;
    use astream_wire::{Frame, HEADER_SIZE, MAX_PAYLOAD_LEN};
    use std::io::{self, Read};

    /// A stream that plays `script` one read at a time: `Some(n)` hands out the next
    /// `n` bytes (fewer at the end), `None` is a read timeout. Past the script it is
    /// at EOF.
    struct Scripted {
        bytes: Vec<u8>,
        pos: usize,
        script: Vec<Option<usize>>,
        step: usize,
    }

    impl Scripted {
        fn new(bytes: Vec<u8>, script: Vec<Option<usize>>) -> Scripted {
            Scripted {
                bytes,
                pos: 0,
                script,
                step: 0,
            }
        }
    }

    impl Read for Scripted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(&step) = self.script.get(self.step) else {
                return Ok(0);
            };
            self.step += 1;
            let Some(n) = step else {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "scripted"));
            };
            let n = n.min(buf.len()).min(self.bytes.len() - self.pos);
            buf[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    fn framed(payloads: &[Vec<u8>]) -> Vec<u8> {
        payloads
            .iter()
            .flat_map(|p| Frame::new(p.clone()).encode().unwrap())
            .collect()
    }

    /// Every frame comes back whole and in order however the bytes are split — with a
    /// timeout after every single byte, or with every frame in one read — and a frame
    /// bigger than the resting buffer gives its growth back.
    #[test]
    fn frames_survive_any_split_and_a_timeout_at_every_byte() {
        let payloads = vec![b"a".to_vec(), vec![], vec![7u8; 70_000], b"z".to_vec()];
        let bytes = framed(&payloads);
        let byte_by_byte = (0..bytes.len()).flat_map(|_| [Some(1), None]).collect();
        let all_at_once = vec![Some(usize::MAX); bytes.len()];
        for script in [byte_by_byte, all_at_once] {
            let mut s = Scripted::new(bytes.clone(), script);
            let mut rx = RecvBuf::default();
            let mut got = Vec::new();
            loop {
                match rx.next_frame(&mut s) {
                    Ok(Some(p)) => got.push(p),
                    Ok(None) => break,
                    Err(e) if e.kind() == io::ErrorKind::TimedOut => continue,
                    Err(e) => panic!("{e}"),
                }
            }
            assert_eq!(got, payloads);
            assert!(rx.buf.len() <= READ_CHUNK);
        }
    }

    /// A stream cut at EVERY byte of a frame is classified as `proto::read_frame`
    /// classifies it: only a cut before the first byte is a close (`Ok(None)`); a cut
    /// after any of its bytes — inside the header as well as inside the payload — is
    /// a frame cut short (`UnexpectedEof`).
    #[test]
    fn a_close_mid_frame_is_classified_like_read_frame() {
        let bytes = framed(&[b"payload".to_vec()]);
        for cut in 0..bytes.len() {
            let mut s = Scripted::new(bytes[..cut].to_vec(), vec![Some(usize::MAX)]);
            let ours = RecvBuf::default().next_frame(&mut s).map_err(|e| e.kind());
            let theirs = read_frame(&mut &bytes[..cut]).map_err(|e| e.kind());
            assert_eq!(ours, theirs, "cut at {cut}");
            let expected = if cut == 0 {
                Ok(None)
            } else {
                Err(io::ErrorKind::UnexpectedEof)
            };
            assert_eq!(
                ours, expected,
                "cut at {cut} (header is {HEADER_SIZE} bytes)"
            );
        }
    }

    /// A broker that answers `Hello` with a nonce, and holds every `Attach`
    /// acknowledgement back until `ring` of them have arrived — reading earlier gets
    /// `WouldBlock`, as a socket whose peer has not answered yet would.
    #[cfg(feature = "cap")]
    struct RingBroker {
        written: Vec<u8>,
        ring: usize,
        nonce: [u8; 32],
        pending: Vec<u8>,
        answered: usize,
    }

    #[cfg(feature = "cap")]
    impl RingBroker {
        fn requests(&self) -> Vec<crate::proto::Request> {
            let mut at = &self.written[..];
            let mut out = Vec::new();
            while let Some(p) = read_frame(&mut at).unwrap() {
                out.push(crate::proto::decode_request(&p).unwrap());
            }
            out
        }
    }

    #[cfg(feature = "cap")]
    impl io::Write for RingBroker {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(feature = "cap")]
    impl Read for RingBroker {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            use crate::proto::{encode_response, Request, Response};
            if self.pending.is_empty() {
                let reqs = self.requests();
                let reply = |r: &Response| Frame::new(encode_response(r)).encode().unwrap();
                if self.answered == 0 && reqs == [Request::Hello] {
                    self.pending = reply(&Response::Nonce {
                        ext: crate::proto::NonceExt::default(),
                        log: None,
                        nonce: self.nonce.to_vec(),
                    });
                } else if self.answered == 1 && reqs.len() == 1 + self.ring {
                    for _ in 0..self.ring {
                        self.pending.extend(reply(&Response::Mark {
                            next: 3,
                            head: 3,
                            resume: String::new(),
                            durable: None,
                        }));
                    }
                } else {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "not yet"));
                }
                self.answered += 1;
            }
            let n = self.pending.len().min(buf.len());
            buf[..n].copy_from_slice(&self.pending[..n]);
            self.pending.drain(..n);
            Ok(n)
        }
    }

    /// `attach_all` sends ONE `Hello` and then every `Attach` before it reads an
    /// acknowledgement — each proof over that one nonce — so a ring costs two round
    /// trips. A client that waited for each acknowledgement (or said `Hello` per
    /// grant, as `attach` once did) reads before the broker answers and fails here.
    #[cfg(feature = "cap")]
    #[test]
    fn attach_all_pipelines_a_ring_under_one_hello() {
        use crate::proto::Request;
        let tags = [[1u8; 32], [2u8; 32], [3u8; 32]];
        let grants = ["ro:/a/>", "ro:/b/>", "rw:/c/>"];
        let mut c = super::Client::from_stream(RingBroker {
            written: Vec::new(),
            ring: grants.len(),
            nonce: [9u8; 32],
            pending: Vec::new(),
            answered: 0,
        });
        let mark = c
            .attach_all(grants.iter().zip(&tags).map(|(g, t)| (*g, &t[..])))
            .unwrap();
        assert_eq!(mark, Some((3, 3)));
        let wire = c.into_stream();
        let reqs = wire.requests();
        assert_eq!(reqs[0], Request::Hello);
        assert_eq!(reqs.len(), 1 + grants.len(), "one Hello for the whole ring");
        for ((req, grant), tag) in reqs[1..].iter().zip(grants).zip(&tags) {
            let proof = astream_cap::attach_proof(tag, &[9u8; 32], grant);
            assert_eq!(
                req,
                &Request::Attach {
                    grant: grant.to_string(),
                    proof: proof.to_vec(),
                }
            );
        }
    }

    /// A header declaring more than the cap is refused on the header alone, before a
    /// byte of its payload is buffered.
    #[test]
    fn an_oversized_header_is_refused_before_its_payload_is_read() {
        let mut hdr = Frame::new(vec![]).encode().unwrap();
        hdr[4..8].copy_from_slice(&((MAX_PAYLOAD_LEN + 1) as u32).to_le_bytes());
        let mut s = Scripted::new(hdr, vec![Some(usize::MAX), Some(usize::MAX)]);
        let e = RecvBuf::default().next_frame(&mut s).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert_eq!(s.step, 1, "one read: the header's");
    }

    #[test]
    fn credit_window_state_machine_preserves_invariant() {
        let mut cw = super::CreditWindow::new(4);
        assert_eq!(cw.acked_seq(), 0);
        assert_eq!(cw.next_send(), 0);
        assert_eq!(cw.credit_horizon(), 4);
        assert!(cw.can_send());

        for expected_next in 1..=4 {
            assert!(cw.try_send());
            assert_eq!(cw.next_send(), expected_next);
            assert!(cw.acked_seq() <= cw.next_send() && cw.next_send() <= cw.credit_horizon());
        }
        // Window is now exhausted (`next_send == 4 == credit_horizon`).
        assert!(!cw.can_send());
        assert!(!cw.try_send());
        assert_eq!(cw.in_flight(), 4);

        // Acknowledge 2 frames; stale ack (1) does not regress; overshooting ack (99)
        // is clamped to `next_send == 4`.
        assert_eq!(cw.record_ack(2), 2);
        assert_eq!(cw.record_ack(1), 2);
        assert_eq!(cw.in_flight(), 2);

        // Granting sliding-window credit of 4 moves horizon from 4 to `2 + 4 = 6`.
        assert_eq!(cw.grant_window(4), 6);
        // Stale credit grant (3 < 6) never regresses horizon.
        assert_eq!(cw.grant_credit(3), 6);
        assert!(cw.can_send());
        assert!(cw.try_send());
        assert_eq!(cw.next_send(), 5);
        assert_eq!(cw.record_ack(99), 5);
        assert_eq!(cw.in_flight(), 0);
    }
}
