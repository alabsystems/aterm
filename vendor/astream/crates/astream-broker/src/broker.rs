//! The broker daemon: a thread-per-connection server over a Unix-domain socket or TCP
//! (plaintext; AEAD-sealed with the `aead` feature; capability-guarded with the `cap`
//! feature), one `Mutex<BrokerLog>` + `Condvar` for the single-writer log and live
//! tail, and a group-commit writer thread that every durable mutation funnels through.
//!
//! Lock discipline (the crux): the log `Mutex` is held ONLY for the memory half of a
//! batch — staging it and beginning its commit (the seal, the flush its tier owes), then
//! ending it (the promotion, the head) — or a short catch-up pointer copy (at most
//! [`TAIL_CHUNK`] records per hold) — NEVER across a socket write, a follower
//! round-trip, a `Condvar` wait, or the device. The writer RELEASES it for the batch's
//! `write` and fsync and takes it again to promote (`BrokerLog::commit_begin`, then
//! `BatchWrite::run`, then `BrokerLog::commit_end`), so a reader beside a busy Strict
//! writer never waits out an fsync. That is safe because nothing is promoted until the
//! end: while the lock is free the batch's records are only staged — the head has not
//! moved, so no reader can see them and no ack has gone — and a staged batch refuses
//! every other mutation of the log until it ends. The one caller that would rewrite the
//! file under it, retention, first takes the writer's BATCH lock (`Shared::batch`, held
//! by the writer from staging to promotion; lock order batch → log, and no reader takes
//! it), so it waits for the batch instead of being refused — for its begin and for its
//! finish; the bulk of its copy runs between them with neither lock held. So a slow/dead consumer (or
//! follower) blocks only its own thread, never other consumers, and a busy writer
//! blocks no reader for longer than it takes to stage or promote (and, on an
//! anti-rollback log, to advance its watermark sidecar). The tail wakes via
//! `Condvar` (no sleeps, no poll); the wait predicate reads the visible `head` under the
//! same lock the writer advances it under, so a commit landing between "caught up" and
//! "wait" is never missed. The writer wakes the tails only after a batch that made a
//! deliverable record visible: a parked tail sleeps through a batch that made nothing
//! visible or only hidden records, and reads past those offsets when it next wakes.
//!
//! The connection handshake reads nothing under the log lock. `Hello`'s log identity
//! (with the tier served and the lineage beside it) and the durable producer bindings
//! an `Attach` checks are copies kept beside the log, each behind its own short
//! `Mutex` (`Shared::hello`, `Shared::bindings`), which
//! the writer refreshes while it holds the log lock and every reader takes ALONE. So
//! the order is log → copy, never the reverse, and a guarded connect whose principals
//! are ALREADY bound does not queue behind another writer's fsync. A principal's FIRST
//! bind is still the writer's to append — as is a binding not yet visible, or a
//! different principal on a bound id, for the writer to refuse — so that attach waits
//! out whatever batch the writer is in, fsync included, once per connection.
//!
//! Lifecycle: the acceptor blocks in `accept()` — no poll, so a new connection is taken
//! the instant it arrives and an idle listener never wakes. Shutdown flips the
//! `running` flags, force-closes every registered connection socket (a thread parked in
//! a socket read/write returns at once), DIALS the listener until its acceptor reports
//! that it left (a Unix socket through `<socket>.wake`, a name the broker owns for the
//! same inode, so an embedder that unlinked the socket path first does not wedge it;
//! TCP through the bound address), joins the acceptor, queues the writer's stop behind
//! the work already queued and joins the writer — which sleeps on its queue with no
//! timer, so the stop wakes it at once — then releases the log's file lock. A dial
//! counts only when that listener's own acceptor answers it: after
//! `ACCEPT_WAKE_ATTEMPTS` unanswered rounds — no name left, or another listener taking
//! the dials — the acceptor is abandoned and counted, so shutdown always returns. A
//! listener that cannot get that name polls instead (`ACCEPT_POLL`,
//! [`BrokerHandle::blocks_in_accept`] `false`). A `Broker` dropped without ever serving
//! stops its own writer thread the same way.

use crate::brecord::{BrokerRecord, GroupCommit};
use crate::proto::{
    append_delivery_frame, append_delivery_frame_with_crc, append_frame, compute_delivery_crc,
    decode_request, decode_response, encode_protocol_refusal, encode_replicate, encode_response,
    read_frame, write_frame, Request, Response, CAP_EXPIRED_PREFIX, ERR_BAD_FILTER,
    ERR_BAD_SUBJECT, ERR_RECORD_REFUSED, ERR_REFUSED, ERR_RETIRE_REFUSED, ERR_SUBJECT_BOUND,
    ERR_TOO_MANY_CONNS, ERR_UNAUTHORIZED, LOG_POISONED_PREFIX, PROTO_VERSION,
};
use crate::store::{
    is_hidden_subject, replicated_reserved_msg, reserved_msg, Begun, BrokerLog, Durability,
    FlushCounts, InjectedFaults, LogStats, OpenOptions, Prepared, StageErr, WillRecord,
    COMMIT_SUBJECT, MAX_SUBJECTS_PER_PRODUCER, RETIRE_SUBJECT,
};
use astream_wire::{Filter, Offset, Subject};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::net::{
    IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs,
};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// An object-safe handle that can force-close a connection's socket (both directions),
/// so [`BrokerHandle::shutdown`] can unblock a connection thread parked in a socket
/// read/write and let it (and its ack-writer thread + fds) be reaped promptly. Kept
/// separate from [`Stream`] because `Stream::try_clone -> Self` makes `Stream` itself
/// not object-safe.
trait ConnShutdown: Send + Sync {
    fn shutdown_both(&self);
    /// Shut only the write direction: the peer reads EOF, and may still write.
    fn shutdown_write(&self);
}
#[cfg(unix)]
impl ConnShutdown for UnixStream {
    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
    fn shutdown_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}
impl ConnShutdown for TcpStream {
    fn shutdown_both(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
    fn shutdown_write(&self) {
        let _ = self.shutdown(Shutdown::Write);
    }
}

#[cfg(clean_islands)]
#[path = "broker_island.rs"]
mod broker_island;
#[cfg(not(clean_islands))]
pub use crate::twins::broker_batch_clamp;
#[cfg(clean_islands)]
pub use broker_island::broker_batch_clamp;

contract_fn! {
    /// Verified check that unanswered acceptor wake rounds lie within the 10-attempt cap
    /// (`unanswered <= 10`), linked via `clean-kernel-defeq` to `broker_wake_attempts_within_cap_isl`.
    #[must_use]
    pub fn broker_wake_attempts_within_cap(unanswered: u32) -> bool
    clauses [ensures result == (unanswered <= 10)]
    {
        unanswered <= 10
    }
}

/// One durable mutation queued to the single writer thread. The connection thread
/// blocks on `ack` until the writer's batch fsync makes the record durable (or fails).
struct WriteOp {
    kind: WriteKind,
    ack: Sender<AckResult>,
}

/// What the writer's queue carries: a durable mutation, or the STOP [`stop_writer`]
/// queues behind every one already queued, which is how the writer learns of shutdown
/// at once ([`writer_loop`]).
enum Queued {
    Op(WriteOp),
    Stop,
}

/// `Ok((offset, deduped))` once durable, or `Err(refusal)` if the op or its batch
/// failed (e.g. the batch fsync errored — the record is then NOT durable).
type AckResult = Result<(u64, bool), Refusal>;

/// A refusal on its way to the wire: the `Response::Error` code (the
/// `proto::ERR_*` table: whether resending can ever land) and the message.
type Refusal = (u8, String);

/// The [`Refusal`] for `code` and `msg`.
fn refusal(code: u8, msg: impl Into<String>) -> Refusal {
    (code, msg.into())
}

enum WriteKind {
    Publish {
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    },
    Commit {
        group: String,
        upto: u64,
    },
    ProcessAndProduce {
        producer_id: u64,
        producer_seq: u64,
        out_subject: String,
        out_body: Vec<u8>,
        group: String,
        upto: u64,
    },
    /// A record shipped by a leader, to be appended at exactly its leader offset
    /// (see [`Request::Replicate`]), with the leader's log identity when it sent one.
    Replicate {
        rec: BrokerRecord,
        leader: Option<crate::LogIdentity>,
    },
    /// Bind a producer id to the principal a capability names, durably (a hidden
    /// `/a/bind` record). Checked and appended under the writer's lock, so two
    /// connections racing to bind colliding ids can never both succeed.
    #[cfg_attr(not(feature = "cap"), allow(dead_code))]
    Bind {
        producer_id: u64,
        principal: String,
    },
    /// Persist a connection's registered last will (a hidden `/a/will` record).
    WillRegister {
        conn_id: Option<u64>,
        will: WillRecord,
    },
    /// Fire a registered will as an ordinary, FENCED publish — when its connection
    /// ends, or on broker open for every will the log still holds.
    WillFire {
        conn_id: Option<u64>,
        will: WillRecord,
    },
    /// Drop a closed connection's live will slot when its capability lapsed or its peer
    /// identity was revoked before firing.
    WillDrop {
        conn_id: u64,
    },
    /// Durably retire `target_subject` from `Last` and decrement `producer_id`'s
    /// distinct-subject count (a hidden `/a/retire` record).
    Retire {
        producer_id: u64,
        target_subject: String,
    },
    /// Conditional subject retirement (`Request::Retire`, `DESIGN-aterm-fabric` §5.2).
    RetireConditional {
        producer_id: u64,
        subject: String,
        offset: u64,
        log: crate::LogIdentity,
    },
    /// Flush the log in full ([`Broker::flush_durable`]): stages nothing; the writer
    /// issues the flush outside the log lock once this batch is committed (skipped when a
    /// full flush already covers the head) and answers with the durable head.
    Flush,
}

/// Upper bound on records folded into one group-commit fsync. The batch self-tunes to
/// offered concurrency (everything queued while the previous fsync ran is drained at
/// once); this only caps a single fsync's fan-in so one batch cannot grow unbounded.
const BATCH_MAX: usize = 4096;

/// How long a POLLING acceptor sleeps between non-blocking `accept()` calls, and how
/// long any acceptor backs off after an `accept()` error that is not one connection's
/// own (EMFILE/ENFILE: retried at once, they would spin).
///
/// Only the fallback polls. An acceptor normally BLOCKS in `accept()`: the poll it
/// replaced cost every new connection the rest of the sleep it landed in — 20 ms asked,
/// about 25 ms slept on macOS, so a connect + `Hello` took p50 24.4 ms back to back and
/// 12.1 ms at random arrivals against 0.03-0.07 ms blocking — and woke an idle broker
/// 42.75 times a second, forever. An aterm attach dials three or four connections in
/// series and paid it on each.
///
/// The blocking accept needs a wakeup at shutdown, and a ONE-SHOT self-connect races:
/// the acceptor could consume the wakeup while still observing a stale `running` and
/// re-block forever. [`BrokerHandle::shutdown`] therefore keeps dialing, one connection
/// per [`ACCEPT_WAKE_RETRY`], until the acceptor's exit signal fires — every `accept()`
/// return re-reads the flags, so each dial is another chance and none can be lost. The
/// dial needs a NAME for the listener, and an embedder may unlink the socket path
/// before it shuts down (astream's own `tests/suite/any_connection.rs` does): a Unix
/// listener gets a second name the broker owns, `<socket>.wake`, a hard link to the
/// same inode. A listener that cannot get one — `<socket>.wake` too long for a socket
/// address, a hard link the filesystem refuses, or a foreign file already at that name
/// (never removed) — polls at this interval, as every listener did before.
const ACCEPT_POLL: Duration = Duration::from_millis(20);

/// How long shutdown waits for a blocking acceptor's exit signal after each wakeup
/// dial before dialing again (and a TCP dial's connect timeout). The first dial
/// normally ends the wait within microseconds; another is needed only when the
/// acceptor was still busy (handing off a connection, draining a full backlog) or the
/// dial failed.
const ACCEPT_WAKE_RETRY: Duration = Duration::from_millis(100);

/// Consecutive wakeup rounds — one dial, then one [`ACCEPT_WAKE_RETRY`] wait — that the
/// acceptor never ANSWERS, after which shutdown stops waiting and abandons it (counted
/// in [`BrokerStats::acceptors_abandoned`]). A round is answered when this listener's
/// own acceptor returned from `accept()` during it; whether the dial connected does
/// not count. A connect proves only that SOME listener took it: on macOS/BSD a socket
/// bound to `127.0.0.1:P` beside this broker's `0.0.0.0:P` (a legal bind; std, OpenSSH
/// `-L` and most servers set `SO_REUSEADDR`) takes every loopback connection to P —
/// and loopback is what a wildcard bind is dialed through — while a `serve` that a
/// racing `serve` beat past its bind records the winner's inode and so dials the
/// winner. Counting connects as progress redialed such a stranger ten times a second,
/// forever, and the shutdown never returned.
///
/// So this bound covers every way the dials can miss: an embedder that removed both
/// the socket path and `<socket>.wake`, a TCP address no longer routable from this
/// host, or a name another listener answers. The parked thread keeps its listener fd
/// until a connection reaches it (none may, without a name) or the process exits.
/// Answered rounds cannot repeat without end: an `accept()` return after the flags are
/// cleared leaves the loop, so a round answered without the exit signal is one in which
/// the acceptor was already on its way out (the next round sees the signal) or read a
/// stale flag (the next dial gets it out).
const ACCEPT_WAKE_ATTEMPTS: u32 = 10;

/// Deadline for a connection to complete the Rung-7 handshake. Bounds the
/// pre-handshake read/write so a silent or slow peer that TCP-connects and stalls
/// cannot park a connection thread + fd indefinitely (a slowloris DoS on the one
/// read no key gates yet). Cleared to blocking once the handshake completes.
#[cfg(feature = "handshake")]
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Max un-acked publishes allowed in flight on ONE pipelined connection. Bounds
/// per-connection memory and provides flow control: when this many acks are pending,
/// the read half blocks (which back-pressures the client via the socket buffer)
/// instead of buffering unboundedly. A well-behaved client keeps its window well under
/// this (see [`crate::client::Client::publish_pipelined`]).
const PIPELINE_DEPTH: usize = 1024;

/// DEFAULT write timeout for a served connection — the request thread's writes and the
/// pipelined ack-writer's alike (they share one socket, so they share one bound), until
/// a streaming verb raises it to [`STREAM_WRITE_TIMEOUT`]. A client that pipelines
/// without reading its acks can wedge its OWN connection (its socket buffers fill, the
/// ack-writer blocks, the bounded ack queue fills, the read half blocks) — purely
/// self-inflicted, affecting no other connection. This bound reaps such a wedged
/// connection in bounded time rather than hanging it indefinitely; it is also generous
/// enough for a legitimately slow consumer. (A graceful broker shutdown reaps it
/// immediately via the conn registry.) Overridable per broker with
/// [`Broker::set_write_timeout`].
pub const ACK_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Write timeout for a STREAMING verb, once it has taken the socket over
/// (`Subscribe`/`SubscribeGroup` via `tail_loop`, and `ForkSubscribe`, which delivers
/// its snapshot on the request thread). Deliberately looser than the ack bound: the
/// peer is a reader folding records as they arrive, not a client that owes an ack, so
/// a slow-but-healthy consumer must not be torn down. Still bounded, so a wedged
/// delivery errors out instead of hanging the thread forever.
///
/// This is a FLOOR, not an override: a streaming verb runs under the larger of this and
/// whatever [`Broker::set_write_timeout`] configured, so it only ever RAISES the bound
/// (a configured 60s is never narrowed to 30s on the path that needs the most room).
pub const STREAM_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// The write bound a streaming verb runs under: the connection's configured bound, or
/// [`STREAM_WRITE_TIMEOUT`] if that is larger. Never less than either.
fn stream_write_timeout(shared: &Shared) -> Duration {
    Duration::from_millis(shared.write_timeout_ms.load(Ordering::Relaxed)).max(STREAM_WRITE_TIMEOUT)
}

/// How long a freshly accepted connection may take to deliver its FIRST complete
/// frame — and, on a GUARDED broker (`Broker::open_guarded`, feature `cap`), to have
/// a capability ACCEPTED. Until a request has arrived nothing is known about an
/// UNAUTHENTICATED peer, so a socket that connects and then idles — or trickles a
/// length prefix and stops — is reaped instead of pinning a thread and a frame buffer
/// indefinitely.
///
/// On a guarded broker a request proves nothing either: `Hello`, a refused `Attach`
/// and every request an empty keyring refuses are free for anyone to send. There the
/// deadline runs from accept until an `Attach` succeeds, however many frames arrive
/// before it, and a connection still unattached at the deadline is refused (an
/// `unauthorized` error) and closed; otherwise a peer could say `Hello` and hold one of
/// the [`MAX_CONNS`] slots for good. That deadline is absolute: each read is bounded
/// by the time left, so neither a stream of frames nor a frame sent a byte at a time
/// extends it. (On the sealed transports the time left is re-armed once per sealed
/// record rather than per byte, so only a peer holding the transport key can stretch
/// a record past it.)
///
/// The bound is applied on every transport, including the sealed one whose handshake
/// has already proved the peer holds the key: an idle keyed client is reaped too.
/// Lifted once the first frame is in (guarded: once a capability is accepted). From
/// then on an established, idle connection is read with no deadline — unless its
/// client KEEPS ALIVE ([`KEEPALIVE_DEADLINE`]); one that never does is reaped only by
/// shutdown's force-close. Tunable per broker via [`Broker::set_first_frame_timeout`].
pub const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// How many capabilities one connection's KEYRING may hold. A connection presents
/// several narrow grants (read one subtree, commit under another, publish as a bound
/// principal in a third) rather than one wide one, and each request is authorized
/// EXISTENTIALLY over the ring. The bound keeps an attacker from making the per-request
/// check (linear in the ring) expensive.
pub const MAX_KEYRING: usize = 16;

/// The longest subject, filter, consumer-group name, `Last` cursor or capability
/// grant a request may carry, in bytes. The frame cap alone would admit a 16 MiB name,
/// and parsing and matching one costs many times its size (a filter's segment
/// vectors; a subject's segment vector per keyring entry), so a request over this is
/// refused before anything parses it. Far above any real subject.
pub const MAX_NAME_LEN: usize = 4096;

/// Monotone floor for [`now_unix_ms`] (`REQUIREMENTS` R3): ensures the broker's
/// millisecond clock never steps backwards across NTP adjustments, so a capability
/// expiry or fabric deadline that has lapsed can never become un-expired.
static LAST_NOW_MS: AtomicU64 = AtomicU64::new(0);

/// Unix milliseconds, for the capability-expiry gate.
///
/// The wall clock is read HERE, at the connection boundary, and never inside
/// `astream-cap` — that crate is on the deterministic side of the effect seam,
/// where a clock read would make a replay depend on when it ran. A clock that
/// cannot be read at all yields the last monotone reading (or 0 if never read),
/// and `LAST_NOW_MS.fetch_max` guarantees strict non-decreasing monotonicity.
fn now_unix_ms() -> u64 {
    let raw = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    };
    LAST_NOW_MS.fetch_max(raw, Ordering::AcqRel).max(raw)
}

/// `Some(now)` once the authority that admitted a streaming verb has lapsed:
/// `until` is the unix-ms instant from `cap_authorized_until`, and `u64::MAX` —
/// the value on an unguarded broker, and for grants without an expiry — never
/// lapses (and reads no clock).
fn lapsed(until: u64) -> Option<u64> {
    if until == u64::MAX {
        return None;
    }
    let now = now_unix_ms();
    (now >= until).then_some(now)
}

/// End a streaming verb whose authority lapsed at `until`: flush what `out` already
/// gathered (it was read while authorized), then say why the stream ends.
fn end_lapsed<S: Write>(stream: &mut S, out: &mut Vec<u8>, until: u64, now: u64) -> io::Result<()> {
    append_frame(
        out,
        &encode_response(&Response::Error {
            code: ERR_UNAUTHORIZED,
            msg: format!("{CAP_EXPIRED_PREFIX} at {until} (now {now}, unix ms)"),
        }),
    )?;
    flush_egress(stream, out)
}

/// Independent sources of an effective revocation. A file replacement changes only
/// the file source; an explicit public un-revoke clears both sources.
struct RevocationSet<T> {
    manual: HashSet<T>,
    file: HashSet<T>,
}

impl<T: Eq + std::hash::Hash> RevocationSet<T> {
    fn new() -> Self {
        Self {
            manual: HashSet::new(),
            file: HashSet::new(),
        }
    }

    fn contains(&self, value: &T) -> bool {
        self.manual.contains(value) || self.file.contains(value)
    }

    #[cfg(any(feature = "cap", test))]
    fn is_empty(&self) -> bool {
        self.manual.is_empty() && self.file.is_empty()
    }

    /// Record a manual source even when the file already revokes this value.
    /// Return whether the effective set gained it, matching the public API.
    fn insert(&mut self, value: T) -> bool {
        let added = !self.contains(&value);
        self.manual.insert(value);
        added
    }

    fn remove(&mut self, value: &T) -> bool {
        let manual = self.manual.remove(value);
        let file = self.file.remove(value);
        manual || file
    }

    fn replace_file(&mut self, file: HashSet<T>) {
        self.file = file;
    }
}

/// Return the public identity-added answer separately from any effective change:
/// the identity can already be revoked while its derived principal was cleared.
fn insert_identity_revocation(
    identities: &mut RevocationSet<[u8; 32]>,
    principals: &mut RevocationSet<String>,
    pubkey: [u8; 32],
) -> (bool, bool) {
    let added = identities.insert(pubkey);
    let principal_added = principals.insert(identity_principal(&pubkey));
    (added, added || principal_added)
}

/// State of the revocation file loaded via [`Broker::load_revocation_file`]. On
/// Apple targets it reloads when `SIGHUP` advances [`astream_sys::sighup_generation`].
struct RevocationFileState {
    path: std::path::PathBuf,
}

const REVOCATION_FILE_MAX: u64 = 1024 * 1024;

#[allow(clippy::type_complexity)]
fn parse_revocation_file(
    path: &Path,
) -> io::Result<(HashSet<[u8; 32]>, HashSet<String>, usize)> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(REVOCATION_FILE_MAX + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > REVOCATION_FILE_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "revocation file {}: larger than {REVOCATION_FILE_MAX} bytes",
                path.display()
            ),
        ));
    }
    let text = match core::str::from_utf8(&bytes) {
        Ok(t) => t,
        Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
    };
    let mut identities = HashSet::new();
    let mut principals = HashSet::new();
    let mut count = 0usize;
    for (idx, raw_line) in text.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut is_hex64 = line.len() == 64;
        if is_hex64 {
            for &b in line.as_bytes() {
                if !b.is_ascii_hexdigit() {
                    is_hex64 = false;
                    break;
                }
            }
        }
        if is_hex64 {
            let mut pk = [0u8; 32];
            for (dst, [b0, b1]) in pk.iter_mut().zip(line.as_bytes().as_chunks::<2>().0) {
                *dst = pack_hex_byte(*b0, *b1);
            }
            identities.insert(pk);
            principals.insert(identity_principal(&pk));
            count = count.saturating_add(1);
            continue;
        }
        let mut is_principal = true;
        for &b in line.as_bytes() {
            if !(b.is_ascii_alphanumeric() || b == b'-') {
                is_principal = false;
                break;
            }
        }
        if is_principal {
            principals.insert(line.to_ascii_lowercase());
            count = count.saturating_add(1);
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "revocation file {}:{} invalid entry {line:?}: expected 64-hex Ed25519 pubkey or principal",
                    path.display(),
                    idx.saturating_add(1)
                ),
            ));
        }
    }
    Ok((identities, principals, count))
}

fn apply_revocation_file(shared: &Shared, path: &Path) -> io::Result<usize> {
    #[cfg(target_vendor = "apple")]
    astream_sys::install_sighup_handler()?;
    let seen = astream_sys::sighup_generation();
    let (new_identities, new_principals, count) = parse_revocation_file(path)?;
    {
        let mut file_state = shared
            .revocation_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut ids = shared
            .revoked_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut princs = shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ids.replace_file(new_identities);
        princs.replace_file(new_principals);
        *file_state = Some(RevocationFileState {
            path: path.to_path_buf(),
        });
    }
    shared.sighup_seen.store(seen, Ordering::Release);
    shared.has_revocation_file.store(true, Ordering::Release);
    shared.revocation_epoch.fetch_add(1, Ordering::AcqRel);
    shared.tail.notify_all();
    Ok(count)
}

fn maybe_reload_revocation_file(shared: &Shared) {
    if !shared.has_revocation_file.load(Ordering::Acquire) {
        return;
    }
    let cur = astream_sys::sighup_generation();
    if cur == shared.sighup_seen.load(Ordering::Acquire) {
        return;
    }
    let path = {
        let guard = shared
            .revocation_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.as_ref().map(|st| st.path.clone())
    };
    let Some(path) = path else {
        shared.sighup_seen.store(cur, Ordering::Release);
        return;
    };
    if let Err(e) = apply_revocation_file(shared, &path) {
        shared.sighup_seen.store(cur, Ordering::Release);
        let _ = writeln!(
            io::stderr(),
            "astream-broker: warn: SIGHUP revocation reload of {} failed ({e}); keeping existing revocations",
            path.display()
        );
    }
}

/// Whether `peer_id` is in `shared.revoked_identities` without triggering file I/O
/// (safe to call while holding `shared.log`).
fn is_peer_revoked_raw(shared: &Shared, peer_id: Option<[u8; 32]>) -> bool {
    match peer_id {
        Some(ref pk) => shared
            .revoked_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(pk),
        None => false,
    }
}

/// Whether `peer_id` (an Ed25519 identity key from `serve_tcp_identity`) has been
/// revoked via [`Broker::revoke_identity`] or a `SIGHUP` revocation-file reload.
fn is_peer_revoked(shared: &Shared, peer_id: Option<[u8; 32]>) -> bool {
    maybe_reload_revocation_file(shared);
    is_peer_revoked_raw(shared, peer_id)
}

/// Whether the capability principal bound to `producer_id` on a guarded broker has been
/// revoked (suppressing a disconnecting connection's pending last will).
fn is_will_principal_revoked(shared: &Shared, producer_id: u64) -> bool {
    maybe_reload_revocation_file(shared);
    if shared.cap_secret.is_none() {
        return false;
    }
    let principal = shared
        .bindings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&producer_id)
        .map(|(p, _)| p.to_ascii_lowercase());
    let Some(p) = principal else {
        return false;
    };
    shared
        .revoked_principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&p)
}

#[derive(Debug, PartialEq, Eq)]
enum StreamFailure {
    Revoked(&'static str),
    Expired { until: u64, now: u64 },
}

struct StreamCapCheck {
    #[cfg(feature = "cap")]
    keyring: Vec<astream_cap::Capability>,
    #[cfg(feature = "cap")]
    req: Option<Request>,
    // None forces an initial check even if revocation completed after the request
    // gate but before construction. No epoch value is reserved as a sentinel.
    seen_epoch: Option<u64>,
}

impl StreamCapCheck {
    #[cfg(feature = "cap")]
    fn new(
        shared: &Shared,
        keyring: &mut Vec<astream_cap::Capability>,
        req: Request,
    ) -> StreamCapCheck {
        if shared.cap_secret.is_some() {
            StreamCapCheck {
                keyring: std::mem::take(keyring),
                req: Some(req),
                seen_epoch: None,
            }
        } else {
            StreamCapCheck {
                keyring: Vec::new(),
                req: None,
                seen_epoch: None,
            }
        }
    }

    #[cfg(not(feature = "cap"))]
    fn new(_shared: &Shared) -> StreamCapCheck {
        StreamCapCheck { seen_epoch: None }
    }

    // The caller prunes revoked grants before a new epoch's validation. Captured
    // `now` makes the authority/deadline state transition independently testable.
    fn validate(
        &mut self,
        epoch: u64,
        now: u64,
        until: &mut u64,
        #[cfg(feature = "cap")] secret: Option<&[u8]>,
    ) -> Option<StreamFailure> {
        #[cfg(feature = "cap")]
        if let (Some(secret), Some(req)) = (secret, self.req.as_ref()) {
            if self.seen_epoch != Some(epoch) {
                match cap_authorized_until(secret, &self.keyring, req, now) {
                    Some(current_until) => *until = (*until).min(current_until),
                    None => {
                        // Time zero is diagnostic only. An expired grant must never
                        // authorize a delivery; preserve the typed expiry refusal.
                        if let Some(expired_until) =
                            cap_authorized_until(secret, &self.keyring, req, 0)
                        {
                            *until = (*until).min(expired_until);
                            return Some(StreamFailure::Expired { until: *until, now });
                        }
                        return Some(StreamFailure::Revoked(
                            "unauthorized: capability principal has been revoked",
                        ));
                    }
                }
            }
        }
        // Refresh accepted checks even without a capability guard, so an unrelated
        // epoch change cannot keep the tail's park predicate permanently awake.
        self.seen_epoch = Some(epoch);
        if *until != u64::MAX && now >= *until {
            return Some(StreamFailure::Expired { until: *until, now });
        }
        None
    }
}

fn stream_failure(
    shared: &Shared,
    peer_id: Option<[u8; 32]>,
    cap_check: &mut StreamCapCheck,
    until: &mut u64,
) -> Option<StreamFailure> {
    maybe_reload_revocation_file(shared);
    if is_peer_revoked_raw(shared, peer_id) {
        return Some(StreamFailure::Revoked(
            "unauthorized: peer identity key has been revoked",
        ));
    }
    // Read before pruning. A concurrent later epoch remains different from this
    // accepted snapshot and is rechecked, rather than being silently consumed.
    let epoch = shared.revocation_epoch.load(Ordering::Acquire);
    let needs_time = *until != u64::MAX;
    #[cfg(feature = "cap")]
    let needs_time = {
        let guarded = shared.cap_secret.is_some() && cap_check.req.is_some();
        let validate = guarded && cap_check.seen_epoch != Some(epoch);
        if validate {
            prune_revoked_capabilities(shared, &mut cap_check.keyring);
        }
        needs_time || validate
    };
    // Preserve the unguarded infinite-deadline fast path's lack of wall-clock reads.
    let now = if needs_time { now_unix_ms() } else { 0 };
    cap_check.validate(
        epoch,
        now,
        until,
        #[cfg(feature = "cap")]
        shared.cap_secret.as_deref(),
    )
}

fn end_stream_failure<S: Write>(
    stream: &mut S,
    out: &mut Vec<u8>,
    failure: StreamFailure,
) -> io::Result<()> {
    match failure {
        StreamFailure::Revoked(msg) => end_revoked(stream, out, msg),
        StreamFailure::Expired { until, now } => end_lapsed(stream, out, until, now),
    }
}

/// End a streaming verb whose peer identity key or capability principal was revoked:
/// flush what `out` already gathered, then emit the revocation error frame.
fn end_revoked<S: Write>(stream: &mut S, out: &mut Vec<u8>, msg: &str) -> io::Result<()> {
    append_frame(
        out,
        &encode_response(&Response::Error {
            code: ERR_UNAUTHORIZED,
            msg: msg.to_string(),
        }),
    )?;
    flush_egress(stream, out)
}

/// A fresh 32-byte nonce for one connection's capability handshake.
///
/// HONEST BOUNDARY: what the protocol needs from this value is FRESHNESS — an
/// `Attach`'s proof is `HMAC-SHA256(tag, nonce ‖ grant)`, so a proof captured from
/// another connection is refused because that connection's nonce was a different
/// value. It is built from std alone: four independent `RandomState`s (each seeded
/// from the OS once per thread, then incremented per construction), a process-wide
/// counter and the wall clock. That is not an OS CSPRNG read per nonce — std exposes
/// none on the stable toolchain, and the DEFAULT broker has no dependency that does
/// (adding one would break the zero-third-party rule this crate keeps). It is a
/// distinct, hard-to-predict value per connection; it is not claimed to be
/// cryptographically random.
fn fresh_nonce() -> [u8; 32] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    static NONCE_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = NONCE_SEQ.fetch_add(1, Ordering::Relaxed);
    let now = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as u64,
        Err(_) => 0,
    };
    let mut out = [0u8; 32];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(seq);
        h.write_u64(i as u64);
        h.write_u64(now);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// Records a streaming verb copies out of the log under ONE hold of its lock: a
/// subscriber catching up from an old cursor, or a `ForkSubscribe` snapshot, reads the
/// log this many records at a time and releases the lock between them. The copy is of
/// shared `Arc` handles (no payload), but it is one per record, so without this bound
/// a reader starting at offset 0 of a long log would hold every publisher off for a
/// copy of the whole log. Delivery is unchanged by it: in order, exactly once, across
/// every chunk boundary.
pub const TAIL_CHUNK: usize = 4096;

/// Records ONE [`Request::Fetch`] may scan, whatever its filter matches. A bounded
/// read is bounded in WORK, not only in output: without this a sparse filter over a
/// long log would hold the log lock for a whole-history walk, which a read-only
/// holder could loop to stall ingest. A page that scans this many records and matches
/// none still advances the client's cursor (`Mark.next` counts scanned records), so
/// paging always makes progress.
pub const FETCH_SCAN_MAX: usize = 65536;

/// Index entries ONE [`Request::Last`] may visit, matched or not — `Fetch`'s bound,
/// worn by the sibling verb. The last-value index is ordered by subject, so a page
/// starts with an `O(log n)` seek to the filter's literal prefix; but a filter whose
/// matches are SPARSE inside that prefix (`/f/F/pub/*/*/ack/<B>` over an index holding
/// one `ack` subject per member per barrier ever issued) pays a parse and a match test
/// for every entry it skips, with the log lock held — the whole-index walk a read-only
/// holder could loop to stall ingest. A page cut off here still advances the client's
/// cursor, so paging always progresses; `last_page` re-takes the lock and continues
/// the walk when the entry it stopped on is one the caller's filter does not cover, so
/// what reaches the wire as `Mark.resume` is never a subject outside that filter.
pub const LAST_SCAN_MAX: usize = 65536;

/// Rows ONE [`Request::Last`] may DELIVER, whatever the client asked for. `max` is a
/// client-chosen `u32` on the wire; without this clamp a single request could ask the
/// broker to clone and frame every subject it has ever seen. The client pages instead,
/// through `Mark.resume`.
pub const LAST_PAGE_MAX: u32 = 4096;

/// How many `LAST_SCAN_MAX`-bounded rounds ONE [`Request::Last`] may take while it
/// looks for a resume position the caller's own filter covers. Each round takes and
/// releases the log lock, so this bounds a request's total CPU, never a lock hold. See
/// `last_page`: past it the request is refused rather than answered with a cursor
/// naming a subject outside the filter (and outside the grant that contained it).
pub const LAST_RESUME_ROUNDS: usize = 64;

/// Cap on simultaneously open connections (each is a thread, plus an ack-writer thread
/// once it writes). Beyond it a new connection is answered with an error and closed,
/// so an unauthenticated flood cannot exhaust threads/fds — the broker keeps serving
/// the connections it has. Generous for a fleet of agents; a hard bound, not tuning.
/// Tunable per broker via [`Broker::set_max_conns`].
pub const MAX_CONNS: usize = 1024;

/// How long a connection refused at [`MAX_CONNS`] stays open after its refusal,
/// reading and discarding what the client sends (see `linger_refused`).
const REFUSAL_LINGER: Duration = Duration::from_millis(500);

/// How many refused connections may linger at once. Past it a refusal closes at once,
/// so a flood cannot turn the lingering into a thread sink.
const MAX_LINGERING_REFUSALS: usize = 64;

/// Socket timeout (connect / read / write) on a leader's link to a follower, and the
/// bound on how long a batch can wait on a follower that never answers: a timed-out
/// follower is a FAILED confirmation for that batch (its link is dropped and
/// re-established on the next batch), never an indefinite stall of the writer.
/// [`Broker::open_replicated_with`] takes an explicit value.
pub const REPLICA_IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Records the leader keeps in flight to a follower before draining their acks —
/// pipelined shipping (the follower's ack-writer streams them back in order), bounded
/// well under the follower's `PIPELINE_DEPTH` so the two sides can never wedge.
const REPLICA_WINDOW: usize = 256;

/// The connection abstraction the broker serves over: any byte stream with the two
/// socket timeouts the tail loop needs. Implemented for a local Unix socket
/// (same-machine), plaintext TCP (multi-machine on a TRUSTED network), and — with
/// the `aead` feature — an XChaCha20-Poly1305 `SealedStream` over TCP (confidential
/// and authenticated: [`Broker::serve_tcp_sealed`]). The same Frame protocol rides
/// all three; capability enforcement (the `cap` feature, [`Broker::open_guarded`])
/// is orthogonal to the transport.
trait Stream: Read + Write + Send + Sized + ConnShutdown + 'static {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()>;
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()>;
    /// Switch the socket's non-blocking mode (`O_NONBLOCK`, a property of the open
    /// socket that every handle to it shares) — only for the liveness probe's one
    /// read, by the one thread using the socket (see `peer_gone`).
    fn set_nonblocking(&self, on: bool) -> io::Result<()>;
    /// A second handle to the same socket, so the read half (draining requests) and
    /// the ack-writer half (streaming acks back) run concurrently — single-connection
    /// pipelining. Read and write are independent directions on the same fd.
    fn try_clone(&self) -> io::Result<Self>;
}
#[cfg(unix)]
impl Stream for UnixStream {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        UnixStream::set_read_timeout(self, d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        UnixStream::set_write_timeout(self, d)
    }
    fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        UnixStream::set_nonblocking(self, on)
    }
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
}
impl Stream for TcpStream {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_read_timeout(self, d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        TcpStream::set_write_timeout(self, d)
    }
    fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        TcpStream::set_nonblocking(self, on)
    }
    fn try_clone(&self) -> io::Result<Self> {
        TcpStream::try_clone(self)
    }
}

/// Where the broker listens, so shutdown can wake a blocking acceptor (dial the
/// listener) and clean up the endpoint (unlink a Unix socket), and
/// [`BrokerHandle::tcp_addr`] can report a bound TCP address.
/// The Unix-socket endpoint is cfg(unix): std has no Unix-domain sockets on
/// Windows, where TCP ([`Broker::serve_tcp`]) is the only transport.
enum Endpoint {
    #[cfg(unix)]
    Unix {
        /// The socket path `serve` bound.
        path: PathBuf,
        /// The listener's inode, read right after the bind — `None` if that stat
        /// failed (the listener then polls, and shutdown unlinks `path` unchecked, as
        /// it always did). Shutdown dials and unlinks only names that still carry it:
        /// an embedder may have unlinked `path`, and another broker may since have
        /// bound its own socket there.
        node: Option<SocketNode>,
        /// `<path>.wake`, the hard link to the listener this broker created and owns
        /// (`None`: the listener polls).
        wake: Option<PathBuf>,
    },
    Tcp {
        /// The connectable form of the bound address ([`connectable_addr`]): what
        /// [`BrokerHandle::tcp_addr`] reports and what shutdown dials.
        dial: SocketAddr,
        /// `dial`, rendered once for [`BrokerHandle::tcp_addr`].
        shown: String,
    },
}

/// A filesystem object's identity, device and inode: what makes "this name still
/// points at OUR listener" checkable after an embedder or another broker may have
/// replaced the name.
#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SocketNode {
    dev: u64,
    ino: u64,
}

/// The identity of whatever `path` names now, WITHOUT following a symlink (a symlink
/// planted at the name is not the listener).
#[cfg(unix)]
fn node_at(path: &Path) -> Option<SocketNode> {
    use std::os::unix::fs::MetadataExt;
    let Ok(m) = std::fs::symlink_metadata(path) else {
        return None;
    };
    Some(SocketNode {
        dev: m.dev(),
        ino: m.ino(),
    })
}

/// Unlink `path` only while it still names the listener `node`. Narrow, not atomic: a
/// replacement between the check and the unlink loses — the same window the
/// unconditional unlink it replaced held open for the whole of shutdown.
#[cfg(unix)]
fn unlink_if_listener(path: &Path, node: SocketNode) {
    if node_at(path) == Some(node) {
        let _ = crate::store::remove_file_path(path);
    }
}

/// Give the listener at `socket` (identity `node`) a second name this broker owns,
/// `<socket>.wake`, so shutdown can dial the blocking acceptor out of `accept()` even
/// after the embedder unlinked `socket`. A hard link: the same inode, so the same
/// permissions — it lets in nobody the socket path does not (a peer could reach it
/// only through the same directory), except after an embedder unlinks the path to
/// hide a broker it means to keep running, which nothing in astream or aterm does.
///
/// `None` — and the listener polls — when that name cannot be had safely:
/// - `<socket>.wake` is too long for a socket address (`SUN_LEN`), so it could never
///   be dialed;
/// - something that is not a socket is there (a user's file: never removed, exactly
///   as `serve` treats its own path);
/// - a LIVE socket is there: another broker's wake name (its embedder unlinked the
///   socket path and this broker bound a new one), which that broker's shutdown needs;
/// - the filesystem refuses the link, or the link does not land on `node` (the socket
///   path changed between the bind and the link).
///
/// A STALE socket there (nobody answers: a broker that crashed or was killed) is
/// removed, as `serve` removes a stale socket at its own path.
#[cfg(unix)]
fn link_wake_name(socket: &Path, node: SocketNode) -> Option<PathBuf> {
    use std::os::unix::fs::FileTypeExt;
    let mut wake = socket.to_path_buf();
    wake.as_mut_os_string().push(".wake");
    if std::os::unix::net::SocketAddr::from_pathname(&wake).is_err() {
        return None;
    }
    if let Ok(meta) = std::fs::symlink_metadata(&wake) {
        if !meta.file_type().is_socket() || UnixStream::connect(&wake).is_ok() {
            return None;
        }
        let _ = crate::store::remove_file_path(&wake); // stale: nobody answers
    }
    if std::fs::hard_link(socket, &wake).is_err() {
        return None;
    }
    if node_at(&wake) != Some(node) {
        // The link succeeded, so the name is ours — but not our listener's.
        let _ = crate::store::remove_file_path(&wake);
        return None;
    }
    Some(wake)
}

/// Map a bound socket address to one that can be *connected to*, for
/// [`BrokerHandle::tcp_addr`] and for shutdown's wakeup dial: an unspecified host
/// (`0.0.0.0` / `::`) becomes loopback, keeping the port, so a port-0 bind reports an
/// address a local client can dial. A concrete host is returned unchanged.
/// (`SocketAddr`'s `Display` brackets IPv6 for us.)
fn connectable_addr(addr: SocketAddr) -> SocketAddr {
    if addr.ip().is_unspecified() {
        let loopback = match addr.ip() {
            IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
        };
        SocketAddr::new(loopback, addr.port())
    } else {
        addr
    }
}

/// How [`BrokerHandle::shutdown`] gets a listener's acceptor out of `accept()`.
enum AcceptWake {
    /// The acceptor polls (the fallback, [`ACCEPT_POLL`]): it observes the cleared
    /// flags on its own within one poll, so shutdown just joins it. Unix only: a TCP
    /// address needs no name to stay dialable.
    #[cfg_attr(not(unix), allow(dead_code))]
    Polls,
    /// The acceptor blocks in `accept()`: shutdown dials the listener until `exited`
    /// disconnects — the acceptor thread holds the sender and drops it as it returns,
    /// panic included, so a disconnect means "left", never "still parked". Behind a
    /// `Mutex` only because a `Receiver` is not `Sync` and `BrokerHandle` must stay
    /// so (aterm shares one across threads); shutdown, which runs once, is its only
    /// locker, and takes no other lock while it holds it.
    ///
    /// `turns` counts this listener's `accept()` returns (the broker-wide
    /// [`BrokerStats::acceptor_wakeups`] cannot tell its listeners apart): it is how
    /// shutdown tells a dial THIS acceptor took from one some other listener answered
    /// ([`ACCEPT_WAKE_ATTEMPTS`]).
    Blocks {
        exited: Mutex<Receiver<()>>,
        turns: Arc<AtomicU64>,
    },
}

/// A wakeup dial's client socket, held until shutdown's wait for the exit signal is
/// over: the acceptor takes it off the backlog whatever its state, but holding it
/// keeps "the peer already closed" out of the question.
enum WakeDial {
    #[cfg(unix)]
    Unix(#[allow(dead_code)] UnixStream),
    Tcp(#[allow(dead_code)] TcpStream),
}

/// One listener's accept loop, Unix and TCP alike: take connections and hand each to
/// `hand_off` until shutdown clears `running` (the broker's) or `listening` (this
/// listener's). Every `accept()` return, connection or error, re-reads both flags, so
/// a stopping broker closes what the backlog hands it instead of spawning a handler
/// shutdown could miss — and each of shutdown's wakeup dials is a fresh chance to see
/// the flags, which is why a stale read cannot strand a blocking acceptor. The flags
/// are read only after an `accept()` returns, never before the first: an acceptor
/// that starts after shutdown began still parks in `accept()`, where shutdown's dials
/// find it (or, when none can, abandon it), instead of leaving by itself on a timing
/// that shutdown cannot see.
///
/// `polls`: the listener is non-blocking (no wake name, [`ACCEPT_POLL`]). Counted on
/// [`Shared`] for [`BrokerStats`]: every resumption of the thread from a blocking wait
/// (an `accept()` that returned in blocking mode, a sleep that ended), every sleep, and
/// every connection handed off. Counted on `turns`, this listener's own: every
/// `accept()` return, which is what shutdown reads to know its wakeup dial reached THIS
/// acceptor ([`AcceptWake::Blocks`]).
fn accept_loop<S>(
    shared: &Shared,
    listening: &AtomicBool,
    turns: &AtomicU64,
    polls: bool,
    mut accept: impl FnMut() -> io::Result<S>,
    mut hand_off: impl FnMut(S),
) {
    loop {
        let got = accept();
        turns.fetch_add(1, Ordering::Relaxed);
        if !polls {
            shared.acceptor_wakeups.fetch_add(1, Ordering::Relaxed);
        }
        let is_stopping =
            !shared.running.load(Ordering::Relaxed) || !listening.load(Ordering::Relaxed);
        match got {
            Ok(s) => {
                // Stopping: close what the backlog handed us (a wakeup dial among them)
                // instead of spawning a handler shutdown could miss.
                if is_stopping {
                    break;
                }
                shared.accepted.fetch_add(1, Ordering::Relaxed);
                hand_off(s);
            }
            Err(ref e) if polls && e.kind() == io::ErrorKind::WouldBlock => {
                if is_stopping {
                    break;
                }
                shared.acceptor_sleeps.fetch_add(1, Ordering::Relaxed);
                thread::sleep(ACCEPT_POLL);
                shared.acceptor_wakeups.fetch_add(1, Ordering::Relaxed);
            }
            // ONE connection's failure (reset while it sat in the backlog): the next
            // connection is not to wait out a back-off for it. (A signal never gets
            // here: std's `accept()` retries EINTR itself.)
            Err(ref e) if e.kind() == io::ErrorKind::ConnectionAborted => {}
            // A transient accept() error (EMFILE/ENFILE) must NOT kill the listener —
            // back off briefly instead of spinning on it. Only shutdown stops it.
            Err(_) => {
                if is_stopping {
                    break;
                }
                shared.acceptor_sleeps.fetch_add(1, Ordering::Relaxed);
                thread::sleep(ACCEPT_POLL);
                shared.acceptor_wakeups.fetch_add(1, Ordering::Relaxed);
            }
        }
        if !shared.running.load(Ordering::Relaxed) || !listening.load(Ordering::Relaxed) {
            break;
        }
    }
}

/// How often a parked subscriber wakes to probe whether its peer disconnected (a
/// never-matching subscriber never writes, so it would otherwise never notice a
/// dead client). This is a liveness heartbeat, NOT a delivery poll — a publish
/// still wakes the tail immediately via the `Condvar`. Overridable per broker with
/// [`Broker::set_liveness_poll`].
pub const LIVENESS_POLL: Duration = Duration::from_millis(200);

/// How long a CAUGHT-UP subscription (`Subscribe`, `SubscribeGroup`) that opted in
/// with [`Request::IdleMarks`] may go with
/// nothing written to it before the broker writes a `Mark { next, head }` — the
/// existing "resume from `next`; `head` is the visible head" frame, which
/// `Subscription::recv` skips — so a live idle subscription is never silent for
/// longer than this, and a subscriber that hears nothing for several of these is on
/// a dead link (`Subscription::recv_live` turns that into an error). The tail also
/// marks once the moment it first catches up. A wire FRAME, never a log record: it
/// costs the log nothing, and a write that fails reaps a vanished subscriber the
/// read probe alone could not. Overridable per broker with [`Broker::set_idle_mark`].
pub const IDLE_MARK: Duration = Duration::from_secs(10);

/// The read deadline a subscriber arms on the broker's idle marks
/// (`Subscription::recv_live`): three [`IDLE_MARK`]s, so a mark delayed by a loaded
/// broker is not a dead link, and a link that IS dead is an error inside half a
/// minute rather than never.
pub const IDLE_MARK_DEADLINE: Duration = Duration::from_secs(30);

/// How long a REQUEST/REPLY connection over TCP that keeps alive may go with nothing
/// read from it before the broker ends it — the request-side twin of
/// [`IDLE_MARK_DEADLINE`], and the only bound on a peer that vanished without a FIN.
///
/// The broker writes to a request/reply connection only to answer it, so a peer that
/// lost power, or whose path died while the link was idle (a NAT or Wi-Fi change, a
/// cable pull, a laptop asleep past its NAT mapping), leaves the connection thread
/// parked in a read that nothing ends: no OS timer runs on an idle socket with no
/// keepalive, and std offers none to set. Until the broker restarted, its will never
/// fired and its [`MAX_CONNS`] slot, thread and fd were never freed. A client that
/// sends [`Request::Keepalive`] asks to be held to this deadline instead: from its
/// first one, every read on the connection is bounded by it, and a connection silent
/// that long is ended exactly as a closed one is — `handle_conn` fires its will
/// (fenced as ever: a later incarnation's record still suppresses it) and its slot is
/// freed.
///
/// OPT-IN BY CONSTRUCTION. The deadline is armed by the client's first `Keepalive`,
/// so a client that never sends one — every client that predates the verb, idling
/// for hours on a quiet link — is never cut. A Unix-socket connection is never held
/// to it: its peer cannot vanish without its kernel closing the socket (a dead
/// process is an EOF, and a dead host took this broker with it), so a deadline there
/// could only ever cut a live peer that stalled.
///
/// Three [`KEEPALIVE_INTERVAL`]s, so a keepalive delayed by a loaded client is not a
/// dead link. Overridable per broker with [`Broker::set_keepalive_deadline`].
pub const KEEPALIVE_DEADLINE: Duration = Duration::from_secs(60);

/// How often a client that keeps a TCP connection alive sends a [`Request::Keepalive`]
/// when it has sent nothing else (`Client::keepalive_if_idle`): a third of
/// [`KEEPALIVE_DEADLINE`]. A wire frame, never a log record — it costs the log nothing
/// and reaches no subscriber, so it is not the heartbeat the fabric design rules out.
pub const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(20);

/// How long ONE liveness probe may block reading the subscriber's socket should the
/// socket refuse to go non-blocking for it — the fallback bound, never the normal
/// path (see `peer_gone`). While a tail sits in a blocking probe a publish's `Condvar`
/// wakeup finds nobody waiting and the record waits out the probe instead, so keep it
/// the shortest the socket accepts.
const PEER_PROBE_TIMEOUT: Duration = Duration::from_millis(1);

#[derive(Clone, Copy)]
struct CachedDeliveryCrc {
    offset: u64,
    crc: u32,
}

/// What `Hello` reports from the log — its identity and the extension block beside it
/// — copied out of the log so the answer never takes the log lock (`Shared::hello`).
#[derive(Clone)]
struct HelloCopy {
    identity: crate::LogIdentity,
    ext: crate::proto::NonceExt,
}

impl HelloCopy {
    /// The log's own answer now: its identity, the tier this open serves, and its
    /// lineage. Taken under the log lock (the caller holds the log).
    fn of(log: &BrokerLog) -> HelloCopy {
        HelloCopy {
            identity: log.identity(),
            ext: crate::proto::NonceExt {
                durability: Some(log.served()),
                lineage: Some(log.lineage().clone()),
                requests: Some(crate::proto::REQUEST_RETIRE),
                // Never copied: the answer reads it fresh from the vitals
                // (`hello_health`), since it moves with every batch's flush.
                health: None,
            },
        }
    }
}

/// The `Hello` answer's health entry (its block's tag 0x04,
/// [`HelloHealth`](crate::HelloHealth)): the poison and the flush counts as they stand,
/// read from the vitals without the log lock.
fn hello_health(vitals: &crate::store::LogVitals) -> crate::proto::HelloHealth {
    crate::proto::HelloHealth {
        poisoned: match vitals.poisoned() {
            Some(why) => Some(why.to_string()),
            None => None,
        },
        flushes: vitals.flush_counts(),
    }
}

/// THE ANSWER TO `Hello` NOW, from `told` (the copy beside the log, [`Shared::hello`])
/// and the vitals: the health entry (tag 0x04), then the tier served (tag 0x01) with any
/// latch to Strict the vitals hold that the copy does not yet. In that order: the copy
/// is refreshed only when the writer publishes the batch's state, after the poison of a
/// failure that latched it — so the tier is read from the vitals AFTER the poison, and
/// an answer that names the poison names the tier the log serves with it
/// ([`LogVitals::served`](crate::store::LogVitals::served)).
fn hello_answer(told: HelloCopy, vitals: &crate::store::LogVitals) -> HelloCopy {
    let HelloCopy { identity, mut ext } = told;
    ext.health = Some(hello_health(vitals));
    ext.durability = match ext.durability {
        Some(s) => Some(vitals.served(s)),
        None => None,
    };
    HelloCopy { identity, ext }
}

struct Shared {
    log: Mutex<BrokerLog>,
    tail: Condvar,
    /// The SUBSCRIBER-VISIBLE head: subscribers are delivered records below it, `Fetch`
    /// reads below it, `Last` pins it. In the Strict/Ordered/Relaxed tiers it is the
    /// log's head as each batch commits (advanced under the log lock) — HELD at the first
    /// escalated record of a batch until the full flush it owes has succeeded, so an
    /// escalated record is never visible before it is durable, and since every ack waits
    /// for visibility, never acked before either (the held head; see `process_batch`) —
    /// but a will registration's, whose hidden record no subscriber is ever delivered and
    /// whose ack answers for its commit alone. Nor does its effect leak: a group commit a
    /// held batch carries steers no group reader until then (`BrokerLog::group_start`).
    /// In the Replicated tier it is the QUORUM WATERMARK — the offset below which a
    /// quorum of followers holds every record — so a leader-side consumer never reads a
    /// record no follower has yet.
    head: AtomicU64,
    /// The DURABLE head ([`BrokerLog::durable_head`]): every record below it is on stable
    /// media by a full flush the writer saw succeed. Written under the log lock, beside
    /// `head`, wherever the log's moves (every Strict batch; a full flush on the weaker
    /// tiers), so [`Broker::durable_head`], every `Mark`'s `durable` and a sampler read it
    /// without the lock, and a tail parked on the lock's condition sees it move.
    durable: AtomicU64,
    /// Whether this open owes idle-marks subscribers DURABLE MARKS (`tail_loop`): the log
    /// served Ordered at its open. Fixed for the open, and so KEPT when the log latches
    /// to Strict. A Strict-from-open log sends none — its `durable` is the visible head
    /// after every batch, so a mark per delivery would tell nobody anything — and a
    /// Relaxed one's does not move between opens. But a tail that attached while the log
    /// served Ordered, like a connection whose `Hello` said Ordered, may hold back what it
    /// was delivered until a durable mark covers it (aterm's notifier pages only those),
    /// and nothing on its stream tells it the tier changed: cleared at the latch, it would
    /// wait for the idle mark an interval later, which a busy subscription never reaches.
    /// After a latch every batch is flushed in full, so each delivered batch costs such a
    /// tail one mark.
    durable_marks: bool,
    /// The Ordered horizon the writer keeps ([`Durability::Ordered`]), fixed at open:
    /// `None` on every other tier, and for `Ordered { horizon: None }`.
    horizon: Option<Duration>,
    running: AtomicBool,
    /// Producers enqueue durable mutations here; the single writer thread drains and
    /// group-commits them. Keeping all writes on one thread means dedup + the offset
    /// spine advance in order with no cross-thread append race. Shutdown queues the
    /// writer's stop here too ([`stop_writer`]).
    writes: Sender<Queued>,
    /// Live connection sockets (a clone per accepted connection), so shutdown can
    /// force-close any thread parked in a blocking socket read/write and reap it.
    conns: Mutex<HashMap<u64, Box<dyn ConnShutdown>>>,
    conn_seq: AtomicU64,
    /// `Some` in the Replicated durability tier: after each batch commits locally, the
    /// writer ships every not-yet-confirmed record to the followers and an `ack` is
    /// gated on the quorum watermark.
    replicator: Option<Replicator>,
    /// `Some` when the broker enforces capabilities on the accept path (built with
    /// the `cap` feature via `open_guarded`): every non-`Attach` request must be
    /// authorized by the connection's presented capability. `None` = no enforcement.
    #[cfg_attr(not(feature = "cap"), allow(dead_code))]
    cap_secret: Option<Vec<u8>>,
    /// [`FIRST_FRAME_TIMEOUT`], in milliseconds ([`Broker::set_first_frame_timeout`]).
    first_frame_timeout_ms: AtomicU64,
    /// How long ONE response write on a served connection may block before the
    /// connection is torn down, in milliseconds ([`Broker::set_write_timeout`]).
    write_timeout_ms: AtomicU64,
    /// [`MAX_CONNS`] ([`Broker::set_max_conns`]).
    max_conns: AtomicUsize,
    /// [`IDLE_MARK`], in milliseconds ([`Broker::set_idle_mark`]).
    idle_mark_ms: AtomicU64,
    /// [`LIVENESS_POLL`], in milliseconds ([`Broker::set_liveness_poll`]).
    liveness_poll_ms: AtomicU64,
    /// [`KEEPALIVE_DEADLINE`], in milliseconds ([`Broker::set_keepalive_deadline`]).
    keepalive_deadline_ms: AtomicU64,
    /// Connections refused at the cap that are still lingering (`linger_refused`).
    refusing: AtomicUsize,
    /// Number of currently active [`BrokerHandle`] listeners serving this broker.
    /// Dropping or shutting down a single listener stops only its own acceptor;
    /// the shared writer thread and log lock are torn down when the LAST listener
    /// handle shuts down (or when an unserved `Broker` drops).
    active_listeners: AtomicUsize,
    /// The group-commit writer thread, shared across all [`BrokerHandle`]s on this
    /// broker and joined when the last listener shuts down.
    writer: Mutex<Option<JoinHandle<()>>>,
    /// [`BrokerStats::accepted`].
    accepted: AtomicU64,
    /// [`BrokerStats::acceptor_wakeups`].
    acceptor_wakeups: AtomicU64,
    /// [`BrokerStats::acceptor_sleeps`].
    acceptor_sleeps: AtomicU64,
    /// [`BrokerStats::acceptors_abandoned`].
    acceptors_abandoned: AtomicU64,
    /// Revoked Ed25519 peer identity public keys (`REQUIREMENTS` R6). Any new
    /// connection on [`Broker::serve_tcp_identity`] whose peer key is in this set is
    /// refused at the handshake, and any already-open connection is terminated with an
    /// `unauthorized` error on its next request or tail delivery.
    revoked_identities: Mutex<RevocationSet<[u8; 32]>>,
    /// Revoked capability principals (`p=<principal>`, including `n-<16 hex>` node
    /// principals derived from revoked Ed25519 keys via [`identity_principal`]).
    revoked_principals: Mutex<RevocationSet<String>>,
    /// Revocation file tracked for automatic Apple `SIGHUP` reload (`REQUIREMENTS` R6).
    revocation_file: Mutex<Option<RevocationFileState>>,
    /// Fast atomic flag set once [`Broker::load_revocation_file`] has been called.
    has_revocation_file: AtomicBool,
    /// Last [`astream_sys::sighup_generation`] observed by this broker.
    sighup_seen: AtomicU64,
    /// Monotone counter incremented whenever the live revocation set changes.
    revocation_epoch: AtomicU64,
    /// [`Broker::pause_last_scan_round`]: the test seam taken at one `last_page` round
    /// boundary. `None` — and one uncontended lock per boundary, never per record —
    /// unless a test installs one.
    ///
    /// A closed rendezvous, not a caller-supplied `dyn Fn`: a `dyn` callback stored here
    /// is an open principal for Trust's thread-spawn census (an out-of-sight caller
    /// could install a closure that spawns), which marked every indirect-call pool in
    /// the crate "may spawn" — `ConnGuard::drop` included.
    last_round_pause: Mutex<Option<RoundPause>>,
    /// Direct-mapped cache only for immutable live `Response::Delivery` payloads.
    /// Each slot keeps the full 64-bit offset and CRC under one short lock; a fork's
    /// counterfactual delivery never enters this cache. No offset bit is a valid flag.
    delivery_crc_cache: Box<[Mutex<Option<CachedDeliveryCrc>>; 4096]>,
    /// The log's [`LogIdentity`](crate::LogIdentity), and the extension block beside it
    /// (the tier served, the lineage: [`HelloCopy`]), as `Hello` reports them, kept
    /// beside the log so the answer never waits on the log lock — which the writer
    /// holds while it stages and promotes a batch (it held it across every batch's write
    /// and fsync too, before those left the lock). Without it a guarded connect beside
    /// one busy Strict publisher waited out most of an fsync before its very first
    /// reply (`Hello` p50 3.3-3.5 ms against 0.008 ms idle, cost study B2/B5).
    ///
    /// Set from the log at open. The identity changes in a running broker in exactly
    /// one place, a replica adopting its leader's in `process_batch` — which drops the
    /// replica's own lineage with it — and the writer copies both here right there,
    /// still under the log lock (lock order: log → copy). A `Hello` that reads it a
    /// moment before that copy reports what a `Hello` holding the log lock just before
    /// the batch would have: the adopting batch's records are not visible until the
    /// head moves, after the copy. The tier served changes only when the log latches to
    /// Strict: the writer copies it here when it publishes the batch's state, and every
    /// reader adds the latch from the log's vitals before that ([`hello_answer`]), so the
    /// poison of the failure that latched it is never told beside the tier before it.
    /// Anything else `Hello` comes to report from the log belongs here with it.
    hello: Mutex<HelloCopy>,
    /// The log's DURABLE producer bindings, `producer id → (principal, offset of its
    /// /a/bind record)`, kept beside the log so an `Attach` whose principal is already
    /// bound decides that without the log lock and without a writer round trip.
    ///
    /// Without it every read-write grant naming a principal queued a `Bind` to the
    /// writer, even for a principal bound long ago: the writer found the binding,
    /// appended nothing, and ran an empty batch anyway — behind whatever batch, fsync
    /// included, it was already in. aterm's ring carries four `rw,p=<node>` grants,
    /// so every guarded connection paid four serial writer round trips and four
    /// empty batches for nothing (`Hello` + attach p50 12.7-18.9 ms beside one busy
    /// Strict publisher, 0.107 ms idle; cost study B5, checker's numbers).
    ///
    /// WHY A COPY CAN BE TRUSTED. While this broker has the log open, a binding the log
    /// has promoted never stops being one: `/a/bind` records are appended and never
    /// rewritten, promotion keeps the FIRST binding of an id (`or_insert`), a running
    /// broker never cuts back its committed tail (a rollback cuts only the staged
    /// batch), and retention, which drops from the front, keeps every binding —
    /// `retain_before` refuses to drop a binding's record, `retain_before_carrying_state`
    /// carries it in `<log>.state`, and neither touches the in-memory table. So an
    /// entry copied here is the log's answer for as long as this copy lives, and the
    /// copy only ever has to GROW. It holds only what the log has promoted (durable at
    /// the log's tier), never the in-flight batch's.
    ///
    /// NOT ACROSS AN OPEN, and it need not be: the copy lives and dies with this
    /// `Broker`, and every open seeds a fresh one from what THAT open recovered. A
    /// Relaxed log that a power cut shortened (`broker.guarded-durability`), or one an
    /// explicit repair cut back to a corrupt record, can come back without a binding
    /// the last open held; the new copy simply lacks it, and the principal's next
    /// attach binds it again through the writer.
    ///
    /// Seeded from the log at open; the writer adds what each batch promoted, under
    /// the log lock (lock order: log → bindings), after publishing the head the batch
    /// reached. Readers take it alone. A MISS — an unbound id, a different principal,
    /// or a binding not yet below the visible head — is answered by the writer exactly
    /// as before, which is where a first bind is appended and a collision refused.
    /// Kept by a guarded broker only (an unguarded one checks no binding); empty
    /// otherwise.
    bindings: Mutex<HashMap<u64, (String, u64)>>,
    /// [`BrokerStats::writer_round_trips`].
    writer_round_trips: AtomicU64,
    /// [`BrokerStats::writer_batches`].
    writer_batches: AtomicU64,
    /// [`BrokerStats::writer_wakeups`].
    writer_wakeups: AtomicU64,
    /// [`BrokerStats::empty_batches`].
    empty_batches: AtomicU64,
    /// [`BrokerStats::binds_settled`].
    binds_settled: AtomicU64,
    /// [`BrokerStats::wills_settled`].
    wills_settled: AtomicU64,
    /// [`BrokerStats::tail_broadcasts`].
    tail_broadcasts: AtomicU64,
    /// [`BrokerStats::tail_wakeups`].
    tail_wakeups: AtomicU64,
    /// [`BrokerStats::peer_probes`].
    peer_probes: AtomicU64,
    /// [`BrokerStats::parked_tails`].
    parked_tails: AtomicUsize,
    /// THE BATCH LOCK: held by the writer thread for a batch's whole life — staging, the
    /// write and flush it runs with the LOG lock released, the promotion — and taken by
    /// nothing else but retention, before the log lock (lock order: batch → log). So a
    /// `retain_before` that arrives while a batch is being written waits for that batch
    /// to commit or roll back, exactly as it waited on the log lock when the writer held
    /// that across the fsync, instead of meeting the batch staged and being refused
    /// ("a batch is being written"). No reader ever takes it, so it costs a reader
    /// nothing; the writer takes it uncontended once per batch.
    batch: Mutex<()>,
    /// [`BrokerStats::retention_waits`].
    retention_waits: AtomicU64,
    /// One compaction at a time: held by a retention call across its begin, its unlocked
    /// copy and its finish (lock order: compaction → batch → log). The copy writes the one
    /// `<log>.kept`, and the log is replaced by the finish of the compaction that copied
    /// it, so two may never overlap. Nothing else takes it.
    #[cfg(feature = "retention")]
    compaction: Mutex<()>,
    /// [`Broker::pause_next_batch`] and [`Broker::pause_next_acks`]: the test seam
    /// taken at one point of one batch. Empty unless a test arms it;
    /// `batch_pause_armed` keeps an unarmed broker's writer from taking even this lock
    /// per batch.
    batch_pause: Mutex<BatchPauseSeam>,
    batch_pause_armed: AtomicBool,
    /// [`Broker::arm_unclean_stop`]: the test seam under which this broker's stop skips
    /// the clean close ([`stop_log`]). False unless a test arms it.
    unclean_stop: AtomicBool,
    /// The log's poison and flush counts, shared with the log that writes them
    /// ([`LogVitals`](crate::store::LogVitals)): what [`Broker::poisoned`],
    /// [`Broker::flush_counts`], [`Broker::wait_poisoned`], every `Hello` answer (its
    /// block's tag 0x04) and the health sidecar read WITHOUT the log lock — which the
    /// writer holds across a rollback's truncate and the identity I/O of a failed flush,
    /// the slow I/O of exactly the failure they report.
    vitals: Arc<crate::store::LogVitals>,
    /// The path the log was opened from: the stem `<log>.health` is named from.
    log_path: std::path::PathBuf,
    /// `<log>.health` once this broker keeps it ([`Broker::write_health`],
    /// [`Broker::keep_health`]): the file it last published, held under its lock until
    /// the log closes ([`stop_health`]). Taken alone, never under the log lock.
    health: Mutex<Option<crate::health::HealthFile>>,
    /// The thread that rewrites `<log>.health` every period ([`Broker::keep_health`]),
    /// joined when the log closes.
    health_thread: Mutex<Option<JoinHandle<()>>>,
    /// The log's retention base (`BrokerLog::base`), updated under the log lock
    /// whenever retention advances it so [`Broker::base`] and `tail_loop`'s caught-up
    /// fast path can read it without locking `shared.log`.
    base: AtomicU64,
    /// The log's committed head (`BrokerLog::head`), updated under the log lock
    /// whenever a batch commits so [`Broker::head`] and `serve_replicate`'s hidden-subject
    /// pre-check can read it without locking `shared.log`.
    log_head: AtomicU64,
    /// Whether the log is a replica (`BrokerLog::is_replica`), updated under the log
    /// lock so `serve_replicate` can check it without locking `shared.log`.
    is_replica: AtomicBool,
    /// Automatic state-carrying compaction record cap (`0` = unlimited, default `0`).
    #[cfg_attr(not(feature = "retention"), allow(dead_code))]
    max_retained_records: AtomicUsize,
    /// Automatic state-carrying compaction byte cap (`0` = unlimited, default `0`).
    #[cfg_attr(not(feature = "retention"), allow(dead_code))]
    max_retained_bytes: AtomicUsize,
}

/// Where in a batch a [`BatchPauseSlot`] parks the writer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchPoint {
    /// Staged, the log lock HELD, nothing written yet: [`Broker::pause_next_batch`].
    Staged,
    /// Begun — sealed, its flush chosen, its bytes taken — the log lock RELEASED,
    /// nothing written or flushed yet: [`Broker::pause_next_write`].
    Writing,
    /// Committed, its head published and its bindings copied, the log lock released,
    /// no ack sent yet: [`Broker::pause_next_acks`].
    BeforeAcks,
    /// A batch that owes a full flush (the held head): every op it does not keep back
    /// answered, the log lock released, the flush not yet issued:
    /// [`Broker::pause_next_flush`].
    BeforeFlush,
}

/// The broker side of the batch-pause seam.
#[derive(Default)]
struct BatchPauseSeam {
    /// The armed rendezvous, if any.
    slot: Option<BatchPauseSlot>,
    /// Set, under this lock, as the writer thread exits. After that no batch reaches a
    /// pause point again, so the armed slot is dropped — its `reached` sender with it,
    /// which is what lets [`BatchPause::wait`] answer `false` instead of blocking for
    /// as long as the `Broker` lives — and a pause armed later is never stored. Under
    /// the one lock, an arm racing the exit lands on one side of it or the other.
    writer_stopped: bool,
}

/// The broker side of a batch-pause rendezvous.
struct BatchPauseSlot {
    /// The point of the batch that takes the pause.
    at: BatchPoint,
    /// Signalled when the writer is there.
    reached: Sender<()>,
    /// The batch goes on when this yields (or its sender is gone).
    resume: Receiver<()>,
}

/// The test side of [`Broker::pause_next_batch`] and [`Broker::pause_next_acks`]: wait
/// for the writer to reach the point in a batch, act in that window, then
/// [`resume`](Self::resume) it. Dropping the handle also resumes the batch, so a
/// panicking test never wedges the broker.
pub struct BatchPause {
    reached: Receiver<()>,
    resume: Sender<()>,
}

impl BatchPause {
    /// Block until the writer is paused at the point. `false` once no batch can reach
    /// it any more: the writer thread stopped (the broker shut down) first, or the
    /// pause was armed after it had.
    pub fn wait(&self) -> bool {
        self.reached.recv().is_ok()
    }

    /// [`wait`](Self::wait), giving up after `limit` (`false`): a test's HANG DETECTOR
    /// for a point a batch must reach — never a latency budget.
    pub fn wait_at_most(&self, limit: Duration) -> bool {
        self.reached.recv_timeout(limit).is_ok()
    }

    /// Let the paused batch go on.
    pub fn resume(self) {
        let _ = self.resume.send(());
    }
}

/// Run on the writer thread as it exits, by returning or by unwinding: see
/// [`BatchPauseSeam::writer_stopped`].
struct WriterExit<'a>(&'a Shared);

impl Drop for WriterExit<'_> {
    fn drop(&mut self) {
        // Never a second panic while unwinding: nothing that holds this lock panics,
        // and if something ever did, a pause left armed is a test's hang detector away.
        if let Ok(mut seam) = self.0.batch_pause.lock() {
            seam.writer_stopped = true;
            seam.slot = None;
            self.0.batch_pause_armed.store(false, Ordering::Release);
        }
    }
}

/// The broker side of a [`Broker::pause_last_scan_round`] rendezvous.
struct RoundPause {
    /// The round whose trailing boundary pauses.
    round: usize,
    /// Signalled when the walk reaches that boundary.
    reached: Sender<()>,
    /// The walk resumes when this yields (or its sender is gone).
    resume: Receiver<()>,
}

/// The test side of [`Broker::pause_last_scan_round`]: wait for the `Last` walk to reach
/// the round boundary, act in the window, then [`resume`](Self::resume) it. Dropping the
/// handle also resumes the walk, so a panicking test never wedges the broker.
pub struct LastRoundPause {
    reached: Receiver<()>,
    resume: Sender<()>,
}

impl LastRoundPause {
    /// Block until a `Last` walk is paused at the boundary. `false` if the broker went
    /// away without ever reaching it.
    pub fn wait(&self) -> bool {
        self.reached.recv().is_ok()
    }

    /// Let the paused walk continue.
    pub fn resume(self) {
        let _ = self.resume.send(());
    }
}

/// Leader-side replication for the Replicated durability tier. Each follower link
/// tracks the follower's CONFIRMED PREFIX (`upto`); every batch re-ships each follower
/// every committed record from its own prefix to the head (followers append at the
/// leader's exact offset and dedup what they already hold, so this is idempotent and
/// catches a follower up after a link drop), and the batch's acks are gated on the
/// QUORUM WATERMARK — the `quorum`-th highest prefix. A Replicated `ack` therefore
/// means a quorum of followers holds the record in memory: it survives the loss of
/// the leader node, but NOT a full-cluster power loss; the dial's middle setting
/// (Kafka's default posture). The local write is Relaxed (page cache), so durability
/// comes from the replicas, not fsync. Nothing here is vacuous: a deduped retry, a
/// pure commit, or an op in a batch whose replication failed is acked ONLY once its
/// offset is below the watermark.
struct Replicator {
    links: Mutex<Vec<FollowerLink>>,
    /// Per-link socket clones so [`BrokerHandle::shutdown`] can force-close a follower
    /// link the writer is parked on (a follower that never answers) without waiting
    /// for the I/O timeout.
    kill: Mutex<Vec<Option<TcpStream>>>,
    quorum: usize,
    io_timeout: Duration,
}

/// One follower's replication link, driven only by the writer thread.
struct FollowerLink {
    /// The leader's own [`LogIdentity`](crate::LogIdentity), sent beside every record
    /// so the follower — a replica of exactly these offsets — reports it too. Fixed
    /// for the leader's life: a log a broker owns changes identity only at an open.
    leader: crate::LogIdentity,
    addr: String,
    /// `None` while the link is down (dropped after an I/O error / timeout); it is
    /// re-established on the next batch. A per-record REFUSAL from the follower (a
    /// `Response::Error`) does not drop the link — the stream stays in sync.
    stream: Option<TcpStream>,
    /// This follower's CONFIRMED PREFIX: it has acknowledged every leader record with
    /// offset `< upto`, in order. What a follower confirmed, it holds, even if the link
    /// later drops — and the point re-shipping resumes from. It starts at the leader
    /// log's retention BASE: nothing below it exists to ship. Reset to 0 only when the
    /// follower reports a GAP (it restarted having lost its tail, or never held the
    /// records below the base): shipping then starts over from 0 on the next batch and
    /// the follower dedups what it still holds — unless retention has pruned the
    /// leader's log above 0, in which case the records it lacks are gone and it is LEFT
    /// BEHIND, shipped nothing more (see [`Replicator::replicate`]).
    upto: u64,
    /// `Some(reason)` once the follower REFUSED a record in a way SHIPPING CANNOT
    /// REPAIR — a different record already at that offset (its log diverged from the
    /// leader's), or unauthorized — so its prefix can never grow: no more is shipped
    /// to it. Its stuck `upto` still counts toward the quorum for the prefix it really
    /// did confirm — an honest count, never a silent desync — but it can never advance
    /// the watermark past that point, and it is not counted at all when deciding where
    /// a batch's re-ship starts ([`Replicator::min_upto`]). Cleared only by reopening
    /// the leader.
    ///
    /// A TRANSIENT refusal (the follower is at its connection cap, its disk errored,
    /// its log is poisoned) is NOT a fence: it merely ends the confirmed prefix for
    /// that batch, and shipping resumes from the same point on the next one.
    fenced: Option<String>,
    /// Set when `Replicator::redial` has just dialed `stream` for this batch: a fresh
    /// link that fails is down for the rest of the batch, where a stale one (the
    /// follower may have closed an idle connection) is re-dialed once.
    fresh: bool,
    /// Consecutive timed-out redial attempts on a blackholed/unresponsive follower,
    /// used to compute exponential redial backoff so a dead WAN host does not stall
    /// the leader's writer thread for `io_timeout` on every batch.
    timeout_failures: u32,
    /// Earliest instant at which a timed-out follower link should be redialed.
    /// Immediate `ConnectionRefused` (local process restart) leaves this `None` so
    /// fast-restarting followers catch up on the very next batch.
    next_dial_at: Option<Instant>,
    /// Optional replication capability attached (`Hello` -> `Nonce` -> `Attach` -> `Mark`)
    /// on initial dial and on every reconnect when replicating to guarded followers.
    #[cfg(feature = "cap")]
    cap: Option<astream_cap::Capability>,
}

#[cfg(feature = "cap")]
fn attach_follower_cap(
    stream: &mut TcpStream,
    cap: &astream_cap::Capability,
    timeout: Duration,
) -> Result<(), (bool, io::Error)> {
    let deadline = Instant::now().checked_add(timeout).ok_or_else(|| {
        (
            false,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "follower handshake deadline is too far away",
            ),
        )
    })?;
    // One deadline covers the complete Hello/Attach exchange, not each socket read.
    let mut bounded = Deadlined { stream, deadline };
    write_frame(&mut bounded, &crate::proto::encode_request(&Request::Hello))
        .map_err(|e| (false, e))?;
    let Some(hello_reply) = read_frame(&mut bounded).map_err(|e| (false, e))? else {
        return Err((
            false,
            io::Error::new(io::ErrorKind::UnexpectedEof, "follower closed before Nonce"),
        ));
    };
    let Some(Response::Nonce { nonce, .. }) = decode_response(&hello_reply) else {
        return Err((
            false,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected follower Hello response",
            ),
        ));
    };
    let proof = astream_cap::attach_proof(&cap.tag, &nonce, &cap.filter);
    let req = Request::Attach {
        grant: cap.filter.clone(),
        proof: proof.to_vec(),
    };
    write_frame(&mut bounded, &crate::proto::encode_request(&req)).map_err(|e| (false, e))?;
    let Some(attach_reply) = read_frame(&mut bounded).map_err(|e| (false, e))? else {
        return Err((
            false,
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "follower closed before Attach Mark",
            ),
        ));
    };
    match decode_response(&attach_reply) {
        Some(Response::Mark { .. }) => {
            // Restore the ordinary per-record I/O timeout after the bounded handshake.
            bounded
                .stream
                .set_read_timeout(Some(timeout))
                .map_err(|e| (false, e))?;
            bounded
                .stream
                .set_write_timeout(Some(timeout))
                .map_err(|e| (false, e))?;
            Ok(())
        }
        Some(Response::Error { code, msg }) => {
            let is_unauth = code == ERR_UNAUTHORIZED || msg.starts_with("unauthorized");
            Err((
                is_unauth,
                io::Error::new(
                    if is_unauth {
                        io::ErrorKind::PermissionDenied
                    } else {
                        io::ErrorKind::Other
                    },
                    msg,
                ),
            ))
        }
        _ => Err((
            false,
            io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected follower Attach response",
            ),
        )),
    }
}

fn encode_replica_record(r: &BrokerRecord, leader: &crate::LogIdentity) -> Vec<u8> {
    let commit = match &r.commit {
        Some(c) => Some((c.group.as_str(), c.upto)),
        None if r.is_marked_retire() => Some((crate::proto::REPLICATE_RETIRE_GROUP, 0)),
        None => None,
    };
    encode_replicate(
        r.seq.0,
        r.producer_id,
        r.producer_seq,
        &r.subject,
        &r.body,
        commit,
        Some(leader),
    )
}

/// Dial a follower with the replication timeouts applied to the socket.
fn connect_follower(addr: &str, timeout: Duration) -> io::Result<TcpStream> {
    let mut last = io::Error::new(
        io::ErrorKind::NotFound,
        format!("follower {addr}: no address"),
    );
    for sa in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&sa, timeout) {
            Ok(s) => {
                s.set_nodelay(true)?;
                s.set_read_timeout(Some(timeout))?;
                s.set_write_timeout(Some(timeout))?;
                return Ok(s);
            }
            Err(e) => last = e,
        }
    }
    Err(last)
}

impl FollowerLink {
    /// Write one window of `records` (at most [`REPLICA_WINDOW`] `Replicate` frames)
    /// into `stream`, flushing in [`EGRESS_CHUNK`] slices.
    fn write_window_frames(
        leader: &crate::LogIdentity,
        stream: &mut TcpStream,
        window: &[Arc<BrokerRecord>],
    ) -> io::Result<()> {
        let mut out = Vec::new();
        for r in window {
            let payload = encode_replica_record(r, leader);
            append_frame(&mut out, &payload)?;
            if out.len() >= EGRESS_CHUNK {
                flush_egress(stream, &mut out)?;
            }
        }
        flush_egress(stream, &mut out)
    }

    /// Read the acks for `window` in order, advancing `self.upto` past each record the
    /// follower confirms in order. Returns the number of records confirmed (`confirmed == window.len()`
    /// means the full window was confirmed; `< window.len()` means a record was refused
    /// or acked out of order, with the remaining acks in the window drained so `stream`
    /// stays in sync).
    fn read_window_acks(
        &mut self,
        stream: &mut TcpStream,
        window: &[Arc<BrokerRecord>],
    ) -> io::Result<usize> {
        let mut confirmed = 0usize;
        let mut stopped = false;
        for r in window {
            let Some(payload) = read_frame(stream)? else {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "follower closed",
                ));
            };
            if stopped {
                continue; // drain, so the stream stays in sync for the next batch
            }
            self.apply_replica_ack(&payload, r.seq.0, &mut confirmed, &mut stopped)?;
        }
        Ok(confirmed)
    }

    fn clear_kill_slot(i: usize, kill: &Mutex<Vec<Option<TcpStream>>>) {
        if let Some(slot) = kill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(i)
        {
            *slot = None;
        }
    }

    /// Pass 1 of pipelined multi-follower replication: write `window` to this
    /// follower's socket without waiting for its acks yet. If a stale link fails on
    /// write, re-dial once and retry writing `window`. Returns `true` if `window` is
    /// now in flight on `self.stream`.
    fn write_window_with_retry(
        &mut self,
        i: usize,
        window: &[Arc<BrokerRecord>],
        fresh_flags: &mut [bool],
        timeout: Duration,
        running: &AtomicBool,
        kill: &Mutex<Vec<Option<TcpStream>>>,
    ) -> bool {
        if !running.load(Ordering::Relaxed) {
            return false;
        }
        let Some(mut stream) = self.stream.take() else {
            return false;
        };
        if Self::write_window_frames(&self.leader, &mut stream, window).is_ok() {
            self.stream = Some(stream);
            return true;
        }
        Self::clear_kill_slot(i, kill);
        drop(stream);
        let was_fresh = fresh_flags.get(i).copied().unwrap_or(true);
        if was_fresh {
            return false;
        }
        if let Some(slot) = fresh_flags.get_mut(i) {
            *slot = true;
        }
        let Some(mut retry_stream) = self.reconnect(i, timeout, running, kill) else {
            return false;
        };
        if Self::write_window_frames(&self.leader, &mut retry_stream, window).is_ok() {
            self.stream = Some(retry_stream);
            true
        } else {
            Self::clear_kill_slot(i, kill);
            drop(retry_stream);
            false
        }
    }

    /// Pass 2 of pipelined multi-follower replication: read the acks for the `window`
    /// written in Pass 1. If a stale link fails mid-read, re-dial once so the next
    /// round resumes from the confirmed `self.upto`. Returns `true` if this follower
    /// can continue to the next window in this batch.
    fn read_window_with_retry(
        &mut self,
        i: usize,
        window: &[Arc<BrokerRecord>],
        fresh_flags: &mut [bool],
        timeout: Duration,
        running: &AtomicBool,
        kill: &Mutex<Vec<Option<TcpStream>>>,
    ) -> bool {
        let Some(mut stream) = self.stream.take() else {
            return false;
        };
        match self.read_window_acks(&mut stream, window) {
            Ok(confirmed) => {
                self.stream = Some(stream);
                confirmed == window.len()
            }
            Err(_) => {
                Self::clear_kill_slot(i, kill);
                drop(stream);
                let was_fresh = fresh_flags.get(i).copied().unwrap_or(true);
                if was_fresh {
                    return false;
                }
                if let Some(slot) = fresh_flags.get_mut(i) {
                    *slot = true;
                }
                match self.reconnect(i, timeout, running, kill) {
                    Some(s) => {
                        self.stream = Some(s);
                        true
                    }
                    None => false,
                }
            }
        }
    }

    fn apply_replica_ack(
        &mut self,
        payload: &[u8],
        seq: u64,
        confirmed: &mut usize,
        stopped: &mut bool,
    ) -> io::Result<()> {
        let Some(resp) = decode_response(payload) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected follower response",
            ));
        };
        match resp {
            Response::PublishAck { offset, .. } => {
                if offset == seq && self.upto == seq {
                    self.upto =
                        crate::proto::replica_ack_monotone(self.upto, seq.saturating_add(1));
                    *confirmed = confirmed.saturating_add(1);
                } else {
                    // Acked out of order: the confirmed prefix ends here. The rest of the
                    // window is still drained below, so the stream stays in sync and is
                    // reused on the next batch.
                    *stopped = true;
                }
                Ok(())
            }
            // Refused: the confirmed prefix ends here and nothing after it counts.
            // A gap means the follower's log is SHORTER than the prefix this link
            // confirmed (it restarted having lost its tail): start over from 0
            // next batch. A DIVERGENCE (a different record already at that offset)
            // or an authorization refusal cannot be repaired by shipping, ever:
            // fence the link. ANYTHING ELSE is TRANSIENT — the follower is at its
            // connection cap, its disk errored, its log is poisoned — and must NOT
            // fence a follower the leader could still catch up: the prefix simply
            // ends here for this batch and shipping resumes from it on the next.
            Response::Error { msg, .. } => {
                if msg.starts_with("replica gap") {
                    self.upto = 0;
                } else if msg.starts_with("replica diverged") || msg.starts_with("unauthorized") {
                    self.fenced = Some(msg);
                }
                *stopped = true;
                Ok(())
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unexpected follower response",
            )),
        }
    }

    fn reconnect(
        &mut self,
        i: usize,
        timeout: Duration,
        running: &AtomicBool,
        kill: &Mutex<Vec<Option<TcpStream>>>,
    ) -> Option<TcpStream> {
        if !running.load(Ordering::Relaxed) {
            return None;
        }
        match connect_follower(&self.addr, timeout) {
            #[allow(unused_mut)]
            Ok(mut s) => {
                let shutdown = s.try_clone().ok()?;
                {
                    let mut slots = kill
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if !running.load(Ordering::Relaxed) {
                        let _ = s.shutdown(Shutdown::Both);
                        return None;
                    }
                    let Some(slot) = slots.get_mut(i) else {
                        let _ = s.shutdown(Shutdown::Both);
                        return None;
                    };
                    *slot = Some(shutdown);
                }
                #[cfg(feature = "cap")]
                if let Some(ref cap) = self.cap {
                    if let Err((is_unauth, e)) = attach_follower_cap(&mut s, cap, timeout) {
                        Self::clear_kill_slot(i, kill);
                        if is_unauth {
                            self.fenced = Some(e.to_string());
                            self.timeout_failures = 0;
                            self.next_dial_at = None;
                        } else if matches!(
                            e.kind(),
                            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                        ) {
                            let backoff =
                                Duration::from_millis(follower_backoff_ms(self.timeout_failures));
                            self.timeout_failures = self.timeout_failures.saturating_add(1);
                            self.next_dial_at = Some(Instant::now() + backoff);
                        } else {
                            self.timeout_failures = 0;
                            self.next_dial_at = None;
                        }
                        return None;
                    }
                }
                self.timeout_failures = 0;
                self.next_dial_at = None;
                if !running.load(Ordering::Relaxed) {
                    Self::clear_kill_slot(i, kill);
                    let _ = s.shutdown(Shutdown::Both);
                    return None;
                }
                Some(s)
            }
            Err(e) => {
                if matches!(
                    e.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) {
                    let backoff = Duration::from_millis(follower_backoff_ms(self.timeout_failures));
                    self.timeout_failures = self.timeout_failures.saturating_add(1);
                    self.next_dial_at = Some(Instant::now() + backoff);
                } else {
                    self.timeout_failures = 0;
                    self.next_dial_at = None;
                }
                None
            }
        }
    }
}

impl Replicator {
    /// Re-dial every follower whose link is down (and not fenced) — at the start of a
    /// batch, before it reads the log for them. One that answers is caught up in this
    /// same batch; one still down is left out of the re-ship base (`min_upto`).
    ///
    /// A follower whose prior redial timed out (blackholed host) is skipped until its
    /// exponential backoff window elapses, preventing per-batch `io_timeout` stalls on
    /// the writer thread.
    fn redial(&self, running: &AtomicBool) {
        let mut links = self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        for (i, link) in links.iter_mut().enumerate() {
            link.fresh = false; // fresh only for the batch that dialed it
            let dial_ready = match link.next_dial_at {
                Some(t) => now >= t,
                None => true,
            };
            if link.fenced.is_none() && link.stream.is_none() && dial_ready {
                link.stream = link.reconnect(i, self.io_timeout, running, &self.kill);
                link.fresh = link.stream.is_some();
            }
        }
    }

    /// The lowest confirmed prefix over the followers a batch can still ship to:
    /// where the re-ship must start so every LIVE follower is caught up from ITS OWN
    /// prefix. `floor` is the leader log's retention base.
    ///
    /// DOWN LINKS ARE NOT COUNTED either: nothing reaches a follower whose re-dial just
    /// failed, and the slice below its prefix — cloned under the log lock, one `Arc`
    /// per record — would grow with every batch for as long as it stays down.
    ///
    /// FENCED LINKS ARE NOT COUNTED, because `replicate` skips them: nothing is ever
    /// shipped to a diverged or unauthorized follower, so its `upto` is frozen for the
    /// life of the leader. Counting it would pin this base at that frozen offset, and
    /// the base is the start of a slice the writer clones UNDER THE LOG LOCK — one
    /// `Arc` per record, `head - upto` of them, growing without bound, on every batch,
    /// for a copy the healthy followers then skip in full. `fallback` (the head) is the
    /// answer when no link can be shipped to at all, so that slice is empty rather than
    /// the whole log.
    ///
    /// LINKS BEHIND `floor` ARE NOT COUNTED, for the same reason: retention pruned the
    /// records they lack, `replicate` never ships to them again, and their `upto` is as
    /// frozen as a fenced link's.
    fn min_upto(&self, floor: u64, fallback: u64) -> u64 {
        let links = self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut min_val: Option<u64> = None;
        for l in links.iter() {
            if l.fenced.is_none() && l.stream.is_some() && l.upto >= floor {
                min_val = Some(match min_val {
                    Some(m) => m.min(l.upto),
                    None => l.upto,
                });
            }
        }
        min_val.unwrap_or(fallback)
    }

    /// Ship every committed-but-unconfirmed record to each follower (from that
    /// follower's own confirmed prefix; `records` is contiguous from its first
    /// record's offset), then return the QUORUM WATERMARK: the offset below which at
    /// least `quorum` followers hold every record. Never vacuous — with nothing to
    /// ship, or every follower down or fenced, the watermark simply does not advance.
    ///
    /// The slice's base is its FIRST RECORD's offset, not the offset the caller asked
    /// `read_from` for: retention may have pruned the log above that, and `read_from`
    /// clamps up to the first retained record. A follower whose prefix is below the
    /// slice lacks records the leader no longer holds: it is left behind, shipped
    /// nothing, counted in the watermark only for the prefix it confirmed.
    fn replicate(&self, records: &[Arc<BrokerRecord>], running: &AtomicBool) -> u64 {
        let mut links = self
            .links
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(first) = records.first() else {
            return Self::watermark(&links, self.quorum);
        };
        let base = first.seq.0;
        let mut fresh_flags: Vec<bool> = Vec::new();
        let mut done: Vec<bool> = Vec::new();
        for link in links.iter_mut() {
            let is_done = link.fenced.is_some() || link.stream.is_none() || link.upto < base;
            fresh_flags.push(std::mem::take(&mut link.fresh));
            done.push(is_done);
        }
        // Two-phase pipelined shipping across all followers in lockstep windows:
        // Pass 1 writes up to REPLICA_WINDOW frames to every active follower's TCP
        // socket before Pass 2 blocks reading any follower's acks, overlapping network
        // RTT and follower processing across all replicas in parallel.
        let mut in_flight: Vec<Option<(usize, usize)>> = Vec::new();
        while running.load(Ordering::Relaxed) {
            in_flight.clear();
            let mut any_sent = false;
            for (i, link) in links.iter_mut().enumerate() {
                if done.get(i).copied().unwrap_or(true) || link.fenced.is_some() {
                    in_flight.push(None);
                    continue;
                }
                let Some(skip) = link.upto.checked_sub(base) else {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    in_flight.push(None);
                    continue;
                };
                let pos = usize::try_from(skip)
                    .unwrap_or(usize::MAX)
                    .min(records.len());
                let rem = records.len().saturating_sub(pos);
                if rem == 0 {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    in_flight.push(None);
                    continue;
                }
                let want = rem.min(REPLICA_WINDOW);
                let Some(window) = records.get(pos..pos.saturating_add(want)) else {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    in_flight.push(None);
                    continue;
                };
                if link.write_window_with_retry(
                    i,
                    window,
                    &mut fresh_flags,
                    self.io_timeout,
                    running,
                    &self.kill,
                ) {
                    any_sent = true;
                    in_flight.push(Some((pos, want)));
                } else {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    in_flight.push(None);
                }
            }
            if !any_sent {
                break;
            }
            for (i, link) in links.iter_mut().enumerate() {
                let Some((pos, want)) = in_flight.get(i).copied().flatten() else {
                    continue;
                };
                let Some(window) = records.get(pos..pos.saturating_add(want)) else {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                    continue;
                };
                if !link.read_window_with_retry(
                    i,
                    window,
                    &mut fresh_flags,
                    self.io_timeout,
                    running,
                    &self.kill,
                ) {
                    if let Some(d) = done.get_mut(i) {
                        *d = true;
                    }
                }
            }
        }
        Self::watermark(&links, self.quorum)
    }

    fn watermark(links: &[FollowerLink], quorum: usize) -> u64 {
        let mut uptos: Vec<u64> = Vec::new();
        for l in links {
            uptos.push(l.upto);
        }
        uptos.sort_unstable();
        uptos.reverse();
        uptos.get(quorum.saturating_sub(1)).copied().unwrap_or(0)
    }

    /// Force-close every live follower socket so a writer parked on a follower that
    /// never answers returns immediately (shutdown must not wait out the I/O timeout).
    fn shutdown_links(&self) {
        for k in self
            .kill
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter_mut()
        {
            if let Some(s) = k.take() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

/// Deregisters a connection from [`Shared::conns`] on drop, so every `handle_conn`
/// return path (clean EOF, error, or a streaming verb taking over) cleans up.
struct ConnGuard<'a> {
    shared: &'a Arc<Shared>,
    id: u64,
}
impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        self.shared
            .conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// Operational snapshot of a running [`Broker`] (`REQUIREMENTS` R5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokerStats {
    /// Snapshot of the underlying durable [`BrokerLog`].
    pub log: LogStats,
    /// Subscriber-visible head offset (the durable head in Strict/Relaxed, or the
    /// quorum watermark in Replicated).
    pub visible_head: u64,
    /// Number of currently open client connections.
    pub active_conns: usize,
    /// Number of currently active [`BrokerHandle`] listeners.
    pub active_listeners: usize,
    /// Connections this broker's acceptors have handed to a connection thread, over
    /// the broker's life and every listener.
    pub accepted: u64,
    /// Times an acceptor thread resumed from a blocking wait: an `accept()` that
    /// returned, in the normal blocking mode, or a poll sleep that ended. A blocking
    /// acceptor wakes once per connection and never while idle (shutdown's wakeup dial
    /// is one more), so this tracks [`accepted`](Self::accepted); a polling one
    /// ([`BrokerHandle::blocks_in_accept`] `false`) wakes per sleep.
    pub acceptor_wakeups: u64,
    /// Times an acceptor slept: a polling listener's sleep between `accept()` calls,
    /// or any listener's back-off after an `accept()` error such as fd exhaustion.
    /// Zero for blocking listeners that met no such error — no connection waits out a
    /// sleep.
    pub acceptor_sleeps: u64,
    /// Blocking acceptors that shutdown could not dial out of `accept()` and left
    /// parked: no name reached the listener any more (an embedder removed both the
    /// socket path and `<socket>.wake`), its TCP address no longer connects, or some
    /// OTHER listener answered every dial (on macOS a socket bound to `127.0.0.1:P`
    /// beside a `0.0.0.0:P` broker; the winner of a `serve` that raced this one past its
    /// bind). Zero in any embedding that leaves the broker's names and port alone.
    pub acceptors_abandoned: u64,
    /// Durable mutations a connection thread queued to the writer and then WAITED on
    /// before answering its request: a principal's first `Bind` at attach, and a
    /// `Will` registration. (Pipelined publishes are not counted: their acks stream
    /// back without the connection thread waiting on each.) Each one queues behind
    /// whatever batch the writer is in, fsync included. An attach whose principal is
    /// already durably bound adds nothing here (see
    /// [`binds_settled`](Self::binds_settled)).
    pub writer_round_trips: u64,
    /// Batches the writer thread has processed, committed or refused, empty or not.
    pub writer_batches: u64,
    /// Times the writer thread resumed from its wait with nothing queued: only the
    /// Ordered horizon falling due, which owes a full flush
    /// ([`Durability::Ordered`]). Otherwise an idle writer sleeps until work, or the
    /// stop shutdown queues, arrives, so on every other tier this stays 0 for the
    /// broker's life, its shutdown included: shutdown never waits out a timer.
    pub writer_wakeups: u64,
    /// Writer batches that appended no record: every op in them deduplicated, was
    /// refused, or (an attach re-binding a principal the writer found bound) had
    /// nothing to append. In the Strict and Relaxed tiers none wakes a subscriber tail.
    /// In the Replicated tier one can: appending nothing is not shipping nothing, and
    /// an empty batch still ships whatever its followers lack, so a batch that only
    /// re-sends a publish refused for want of a quorum, and confirms it, makes that
    /// record visible and wakes the tails (see
    /// [`tail_broadcasts`](Self::tail_broadcasts)).
    pub empty_batches: u64,
    /// Read-write grants naming a principal whose attach found that principal already
    /// durably bound to its producer id, below the visible head, and so made no writer
    /// round trip: the second and later `rw,p=` grants of one principal on a
    /// connection, and every one of them on a later connection or a later open.
    pub binds_settled: u64,
    /// Wills the writer has SETTLED. A connection that ends holding a registered will
    /// queues the will's firing (or, when the capability behind it lapsed or its peer's
    /// identity was revoked, its drop), and an open queues a firing for each will the
    /// log holds. Each is counted here once the batch that staged it has ended and
    /// published its head, whatever the firing came to: the goodbye appended, deduped
    /// against a record already under its key, fenced by a later record of its
    /// producer, refused on a replica, or rolled back with a failed batch (the next open
    /// fires it again).
    ///
    /// A will that comes to NOTHING leaves no record to wait for, so this is how a
    /// reader knows it has come to nothing. The writer takes its queue in order: once
    /// this reads N, the first N wills queued have been staged, whatever they appended
    /// is in the log, and a write queued afterwards lands above it.
    /// [`active_conns`](Self::active_conns) cannot say the same: a connection leaves
    /// that set when its requests end, BEFORE it queues its will.
    pub wills_settled: u64,
    /// Times the writer woke every parked subscriber tail at once (one `notify_all` on
    /// the log's `Condvar`) after a batch: only after a batch that made a record some
    /// subscriber could be delivered visible. A batch that made nothing visible or only
    /// hidden records (a group commit, a will registration, a binding, a retirement)
    /// wakes nobody, so `writer_batches - tail_broadcasts` counts those. What a batch
    /// makes visible depends on the tier. In Strict and Relaxed it is the batch's own
    /// appends, so an empty batch ([`empty_batches`](Self::empty_batches)) never
    /// broadcasts. In Replicated it is what the quorum watermark the batch publishes
    /// newly passed, whichever batch appended it: an empty or hidden-only batch whose
    /// ship confirms an earlier publish broadcasts for it, and a batch whose own
    /// publish no quorum confirmed does not. Revocation and shutdown wake the tails
    /// too, and are not counted here.
    pub tail_broadcasts: u64,
    /// Times a parked subscriber tail resumed from its wait: woken by a writer
    /// broadcast, a revocation or shutdown, or at the end of its liveness poll
    /// ([`Broker::set_liveness_poll`]). A broadcast resumes every tail parked at that
    /// moment, whatever its filter, so with S parked tails each broadcast adds S.
    pub tail_wakeups: u64,
    /// Liveness probes subscriber tails made: a one-byte read of the subscriber's own
    /// socket that answers whether its peer has gone. A tail that writes nothing — its
    /// filter matched nothing it read — has no other way to learn its subscriber left.
    pub peer_probes: u64,
    /// Subscriber tails parked right now, waiting for the visible head to pass their
    /// cursor. Each is counted under the log lock as it goes to wait — the lock the
    /// writer publishes the head under — so a batch whose head is published after a
    /// read of N here finds those N tails waiting.
    pub parked_tails: usize,
    /// Retention calls (`Broker::retain_before` and
    /// `Broker::retain_before_carrying_state`, the `retention` feature's) that found the
    /// writer's batch lock (`Shared::batch`) taken and waited for it, at the compaction's
    /// begin or at its finish — counted once per call, not once per wait. The writer
    /// writes and flushes a batch with the log lock released, so retention, which
    /// rewrites the file, waits on that lock instead — for the batch the writer is in to
    /// commit or roll back. Two retention calls made at once queue on a lock of their own
    /// (`Shared::compaction`), which this does not count, so what it counts is retention
    /// that waited for a batch.
    pub retention_waits: u64,
}

const fn follower_backoff_ms(timeout_failures: u32) -> u64 {
    let shift = if timeout_failures < 5 {
        timeout_failures
    } else {
        5
    };
    100u64 << shift
}

const fn hex_byte_chars(b: u8) -> (char, char) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    (
        HEX[(b >> 4) as usize] as char,
        HEX[(b & 0x0f) as usize] as char,
    )
}

const fn hex_nibble_digit(b: u8) -> u8 {
    match b {
        b'0' => 0,
        b'1' => 1,
        b'2' => 2,
        b'3' => 3,
        b'4' => 4,
        b'5' => 5,
        b'6' => 6,
        b'7' => 7,
        b'8' => 8,
        b'9' => 9,
        b'a' | b'A' => 10,
        b'b' | b'B' => 11,
        b'c' | b'C' => 12,
        b'd' | b'D' => 13,
        b'e' | b'E' => 14,
        b'f' | b'F' => 15,
        _ => 0,
    }
}

const fn pack_hex_byte(b0: u8, b1: u8) -> u8 {
    let hi = (hex_nibble_digit(b0) & 0x0f) << 4;
    let lo = hex_nibble_digit(b1) & 0x0f;
    hi | lo
}

/// Derive the canonical node principal name (`n-<16 lowercase hex chars>`) from a
/// 32-byte Ed25519 identity public key (`REQUIREMENTS` R6).
///
/// When a client connects over `Broker::serve_tcp_identity`, its transport-layer
/// Ed25519 public key is cryptographically verified by the SIGMA handshake. Any
/// capability grant attached on that connection whose principal has the node-identity
/// shape (`n-` followed by 16 hex digits) MUST match `identity_principal(&peer_pubkey)`
/// or `Attach` is refused — preventing a compromised host from attaching a capability
/// minted for another node's principal.
#[must_use]
pub fn identity_principal(pubkey: &[u8; 32]) -> String {
    let mut out = String::with_capacity(18);
    out.push_str("n-");
    for &b in &pubkey[..8] {
        let (hi, lo) = hex_byte_chars(b);
        out.push(hi);
        out.push(lo);
    }
    out
}

/// Whether `principal` has the node-identity principal shape (`n-` + 16 hex digits).
#[cfg(feature = "cap")]
fn is_identity_shaped_principal(principal: &str) -> bool {
    let Some(hex_part) = principal.strip_prefix("n-") else {
        return false;
    };
    if hex_part.len() != 16 {
        return false;
    }
    for &b in hex_part.as_bytes() {
        if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// The refusal of an empty capability secret: an HMAC under an empty key is computable
/// by anyone, so it would admit any capability anyone cares to mint.
fn empty_secret() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "open_guarded: the capability secret is empty, so anyone could mint a \
         capability it accepts",
    )
}

/// A broker over one durable [`BrokerLog`].
pub struct Broker {
    shared: Arc<Shared>,
}

impl Broker {
    /// Open (creating if absent) a broker backed by the Strict durable log at `log_path`.
    pub fn open(log_path: impl AsRef<Path>) -> io::Result<Broker> {
        Broker::open_with(log_path, Durability::Strict)
    }

    /// Open a broker at the given [`Durability`] tier (Strict fsyncs every batch;
    /// Ordered acks on an ordering barrier — a power loss leaves a prefix of whole
    /// batches, the newest possibly lost and announced; Relaxed acks on page-cache write
    /// — lower latency, survives process crash but not power loss). No escalation table
    /// (that is [`open_opts`](Self::open_opts)'s). The log is exclusively locked while
    /// open (a second broker on the same log fails `AlreadyExists`), and refuses to open
    /// a corrupt log — see [`BrokerLog::open_with`].
    pub fn open_with(log_path: impl AsRef<Path>, durability: Durability) -> io::Result<Broker> {
        let log = BrokerLog::open_with(log_path, durability)?;
        Ok(Self::start(log, None, None))
    }

    /// Open at `durability`, preparing embedder sidecars under the log's own lock.
    ///
    /// `prepare` runs once after the file is opened, exclusively locked where
    /// advisory locks are supported, and its directory synced. It runs before
    /// recovery or identity sidecar reads. The same file remains owned through
    /// recovery and by the returned broker; no writer starts before preparation
    /// and recovery both succeed. A busy log refuses before calling `prepare`.
    ///
    /// The callback must not remove, replace, truncate, or otherwise change the
    /// log file itself. It may prepare its own sidecars, and must make those
    /// changes crash safe: a callback error or later recovery error does not roll
    /// them back. A newly created log may remain after an error. Callback errors
    /// propagate unchanged, releasing the file lock on return or unwind.
    pub fn open_with_prepare(
        log_path: impl AsRef<Path>,
        durability: Durability,
        prepare: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Broker> {
        let log = BrokerLog::open_with_prepare(log_path, durability, prepare)?;
        Ok(Self::start(log, None, None))
    }

    /// Open a broker as `opts` say — the open the other constructors are shorthands
    /// for: [`BrokerLog::open_opts`] (the tier, the escalation table, the boot and volume
    /// the identity rules read, the embedder's `prepare` and its [`Prepared`] verdict),
    /// plus, with the `cap`
    /// feature, `opts.guard`: `Some(secret)` enforces capabilities as
    /// `open_guarded` does, an empty secret refused before the log
    /// is opened or `prepare` runs; and `opts.health_every`: `Some(period)` keeps the
    /// health sidecar `<log>.health` from the open on ([`keep_health`](Self::keep_health)).
    pub fn open_opts(
        log_path: impl AsRef<Path>,
        opts: OpenOptions,
        prepare: impl FnOnce() -> io::Result<Prepared>,
    ) -> io::Result<Broker> {
        #[cfg(feature = "cap")]
        let guard = opts.guard.clone();
        #[cfg(not(feature = "cap"))]
        let guard: Option<Vec<u8>> = None;
        if let Some(g) = &guard {
            if g.is_empty() {
                return Err(empty_secret());
            }
        }
        let health_every = opts.health_every;
        let log = BrokerLog::open_opts(log_path, opts, prepare)?;
        let broker = Self::start(log, None, guard);
        if let Some(every) = health_every {
            // The open has happened (and fired the wills the log held): a sidecar it
            // cannot write is said, not a refused open — the thread tries again every
            // period, and `write_health` answers an embedder that must know.
            if let Err(e) = broker.keep_health(every) {
                eprintln!(
                    "astream-broker: {}: could not write its health sidecar ({e}); trying \
                     again every {} ms",
                    broker.shared.log_path.display(),
                    every.as_millis()
                );
            }
        }
        Ok(broker)
    }

    /// Open a broker that serves as a REPLICATION TARGET: a follower a leader ships
    /// `Replicate` records to. Exactly [`open_with`](Self::open_with) at the same tier,
    /// plus the log declared a replica before it takes its first record — the tier is
    /// the operator's choice here as everywhere else, and `open_replica` is
    /// [`open`](Self::open)'s Strict default.
    ///
    /// A follower brought up on an EMPTY log never needs this: the first record a
    /// leader ships to a still-empty log declares it. It is REQUIRED for the other
    /// case — a follower seeded from a copy of the leader's log, whose log already has
    /// records but has never taken one through replication. A replicated record
    /// accepted beside records that were already there does NOT declare it, because
    /// accepting one says nothing about whose log it is (any client can send a
    /// `Replicate`), so on a seeded follower the declaration is the only thing that
    /// tells the two apart.
    ///
    /// WHAT BEING A REPLICA MEANS, EXACTLY: this broker appends nothing to the log on
    /// its OWN initiative — it does not fire the wills the log holds when it opens
    /// (they are the leader's, and they fire there). That is the whole of what the
    /// marker enforces.
    ///
    /// It does NOT make the log append-proof. A client that reaches this broker's
    /// listener can still `Publish`, `Commit` or `ProcessAndProduce`, and the record
    /// lands at the follower's own next offset — after which the leader's next ship to
    /// that offset is refused `Diverged`, the link is fenced for good, and with
    /// quorum == follower count every later leader publish answers "not replicated to
    /// quorum". So do not expose a replica's listener to publishers: a monitoring
    /// script or a bridge pointed at the wrong endpoint is enough to make the cluster
    /// write-unavailable.
    pub fn open_replica(log_path: impl AsRef<Path>) -> io::Result<Broker> {
        Self::open_replica_with(log_path, Durability::Strict)
    }

    /// [`open_replica`](Self::open_replica) at an explicit [`Durability`] tier.
    pub fn open_replica_with(
        log_path: impl AsRef<Path>,
        durability: Durability,
    ) -> io::Result<Broker> {
        let mut log = BrokerLog::open_with(log_path, durability)?;
        log.declare_replica()?;
        Ok(Self::start(log, None, None))
    }

    /// [`open_replica`](Self::open_replica) with capability enforcement on the accept
    /// path (see [`open_guarded`](Self::open_guarded)). Every connection to this
    /// follower — including the leader's replication link — must `Attach` a capability
    /// signed under `secret` before its requests are served; an unbound read-write
    /// capability is required for `Request::Replicate`. Strict durability by default.
    #[cfg(feature = "cap")]
    pub fn open_replica_guarded(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
    ) -> io::Result<Broker> {
        Self::open_replica_guarded_with(log_path, secret, Durability::Strict)
    }

    /// [`open_replica_guarded`](Self::open_replica_guarded) at an explicit
    /// [`Durability`] tier.
    #[cfg(feature = "cap")]
    pub fn open_replica_guarded_with(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
        durability: Durability,
    ) -> io::Result<Broker> {
        if secret.is_empty() {
            return Err(empty_secret());
        }
        let mut log = BrokerLog::open_with(log_path, durability)?;
        log.declare_replica()?;
        Ok(Self::start(log, None, Some(secret)))
    }

    /// Open a broker whose durable log is ENCRYPTED AT REST: every record's payload
    /// is sealed on disk under `key` (XChaCha20-Poly1305), so the forensic value of
    /// the log — a verbatim recording of every keystroke and screen frame — is gone
    /// to anyone without the key. The wire protocol and all in-memory behaviour are
    /// unchanged; recovery decrypts, and refuses (rather than discards) on a wrong key
    /// or a tampered log. Off by default (the `at-rest` feature). Strict.
    #[cfg(feature = "at-rest")]
    pub fn open_encrypted(log_path: impl AsRef<Path>, key: [u8; 32]) -> io::Result<Broker> {
        let log = BrokerLog::open_encrypted(log_path, key)?;
        Ok(Self::start(log, None, None))
    }

    /// [`open_encrypted`](Self::open_encrypted) at a chosen [`Durability`].
    #[cfg(feature = "at-rest")]
    pub fn open_encrypted_with(
        log_path: impl AsRef<Path>,
        durability: Durability,
        key: [u8; 32],
    ) -> io::Result<Broker> {
        let log = BrokerLog::open_encrypted_with(log_path, durability, key)?;
        Ok(Self::start(log, None, None))
    }

    /// [`open_encrypted`](Self::open_encrypted) with ANTI-ROLLBACK: an authenticated
    /// monotonic head watermark is maintained and verified, so a write-capable attacker
    /// cannot silently truncate the log to an earlier state. A rolled-back log (or a
    /// deleted watermark on a non-empty log) is refused. Bootstrap once with
    /// [`open_encrypted_verified_init`](Self::open_encrypted_verified_init). Strict.
    #[cfg(feature = "anti-rollback")]
    pub fn open_encrypted_verified(
        log_path: impl AsRef<Path>,
        key: [u8; 32],
    ) -> io::Result<Broker> {
        let log = BrokerLog::open_encrypted_verified(log_path, key)?;
        Ok(Self::start(log, None, None))
    }

    /// Bootstrap [`open_encrypted_verified`](Self::open_encrypted_verified): establish
    /// the head watermark at the current head (first adoption of anti-rollback).
    #[cfg(feature = "anti-rollback")]
    pub fn open_encrypted_verified_init(
        log_path: impl AsRef<Path>,
        key: [u8; 32],
    ) -> io::Result<Broker> {
        let log = BrokerLog::open_encrypted_verified_init(log_path, key)?;
        Ok(Self::start(log, None, None))
    }

    /// RETENTION: compact the durable log, dropping records with offset `< min_offset`
    /// (kept records keep their absolute offsets). A consumer below the new base
    /// catches up from the earliest retained. Returns the number of records dropped. Off
    /// by default (the `retention` feature).
    ///
    /// OFF THE ACK PATH. The compaction begins between batches — it waits for a batch the
    /// writer is in the middle of, its write and flush included
    /// ([`BrokerStats::retention_waits`]) — and settles its floor there. Then it copies
    /// the kept suffix to the durable length it began at with NEITHER the log lock NOR
    /// the writer's batch lock held: those bytes are durable and nothing rewrites them, so
    /// the writer commits and acks batches meanwhile. Only the bytes appended since the
    /// begin are copied under both locks, before the switch. Crash-safe: an open after a
    /// crash anywhere in it finishes or discards it ([`BrokerLog::retain_before`]).
    /// Compactions run one at a time.
    ///
    /// REFUSED (`InvalidInput`) for a `min_offset` above the subscriber-visible head:
    /// records at or above it are committed but not yet shown — a batch held for the full
    /// flush its escalated record owes, or (Replicated) records no quorum holds yet — and
    /// pruning them would drop records no reader could ever have been delivered.
    #[cfg(feature = "retention")]
    pub fn retain_before(&self, min_offset: u64) -> io::Result<u64> {
        self.compact(min_offset, false)
    }

    /// THE BROKER'S COMPACTION, off the ack path (see [`retain_before`](Self::retain_before)):
    /// begin between batches, copy with no lock held, finish between batches.
    #[cfg(feature = "retention")]
    fn compact(&self, min_offset: u64, carry_state: bool) -> io::Result<u64> {
        let _one = self
            .shared
            .compaction
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut waited = false;
        let begun = {
            let _batch = self.batch_settled(&mut waited);
            let mut log = self
                .shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.refuse_above_visible(min_offset.min(log.head().0))?;
            log.compact_begin(Offset(min_offset), carry_state)?
        };
        let Some(mut c) = begun else {
            return Ok(0);
        };
        // The bulk of the copy: no lock held, so no publish's ack waits for it.
        if let Err(e) = c.copy() {
            c.abandon();
            return Err(e);
        }
        let _batch = self.batch_settled(&mut waited);
        let mut log = self
            .shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let dropped = log.compact_finish(c)?;
        self.shared.base.store(log.base().0, Ordering::Release);
        Ok(dropped)
    }

    /// Take the writer's batch lock (`Shared::batch`) for a retention call: at once
    /// between batches, or — counted in [`BrokerStats::retention_waits`], once per call
    /// (`waited`) — once the batch the writer is in has committed or rolled back. Held
    /// across a compaction's begin and again across its finish, so no batch begins under
    /// either. A writer that panicked holding it poisons it, and this panics, as the log
    /// lock's poisoning did when the writer held that instead.
    ///
    /// THE POISONED ARM PANICS WITH THE GUARD IT WAS HANDED, and never locks again. A
    /// poisoned `try_lock` still TAKES the mutex: its error carries the guard, and the
    /// match's scrutinee keeps it alive through the arm, so a `lock()` there is this
    /// thread waiting on itself — every retention call after a writer panic would hang
    /// its caller instead of failing (`retention_after_a_writer_panic_fails_and_never_hangs`).
    /// Bound here, the guard is dropped as the panic unwinds.
    #[cfg(feature = "retention")]
    fn batch_settled(&self, waited: &mut bool) -> std::sync::MutexGuard<'_, ()> {
        match self.shared.batch.try_lock() {
            Ok(held) => held,
            Err(std::sync::TryLockError::WouldBlock) => {
                if !*waited {
                    *waited = true;
                    self.shared.retention_waits.fetch_add(1, Ordering::Relaxed);
                }
                self.shared
                    .batch
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                panic!("retention: the writer panicked inside a batch ({poisoned})")
            }
        }
    }

    /// The guard both retention calls take, with the log lock held (the visible head is
    /// published under it). `min_offset` is what retention would take: clamped to the
    /// head, as `retain_before` itself clamps it, so a call that asks for "everything" is
    /// refused only while some committed record is not yet visible.
    #[cfg(feature = "retention")]
    fn refuse_above_visible(&self, min_offset: u64) -> io::Result<()> {
        let visible = self.shared.head.load(Ordering::Relaxed);
        if min_offset > visible {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "retention to offset {min_offset} would prune records above the \
                     subscriber-visible head {visible}, which no reader has been shown yet"
                ),
            ));
        }
        Ok(())
    }

    /// STATE-CARRYING RETENTION: compact the durable log to `min_offset`, persisting
    /// any producer bindings (`/a/bind`), active pending wills (`/a/will`), producer
    /// sequence fences, and consumer-group commit watermarks from the dropped prefix
    /// into the `<log>.state` sidecar so compaction succeeds past early bindings and
    /// wills (`ROADMAP.md`, `AUDIT-2026-09.md`).
    ///
    /// Off the ack path, crash-safe, waits for a batch in flight, and is refused above the
    /// subscriber-visible head, as [`retain_before`](Self::retain_before) does and is.
    #[cfg(feature = "retention")]
    pub fn retain_before_carrying_state(&self, min_offset: u64) -> io::Result<u64> {
        self.compact(min_offset, true)
    }

    /// Configure automatic writer-side state-carrying compaction whenever the log's
    /// retained record count exceeds `max` (`0` = unlimited, the default).
    ///
    /// When triggered after a batch commits, the writer compacts the oldest half of the
    /// safe visible prefix via [`Self::retain_before_carrying_state`], preserving all
    /// producer bindings, pending wills, producer sequence fences, consumer-group
    /// commits, and the latest record of every quiet subject (`Last`) in `<log>.state`.
    /// It runs the whole compaction, its copy included, on the writer thread under the
    /// batch and log locks, so the batches behind it wait for it; it skips a batch while
    /// a retention call is compacting (one compaction at a time).
    #[cfg(feature = "retention")]
    pub fn set_max_retained_records(&self, max: usize) {
        self.shared
            .max_retained_records
            .store(max, Ordering::Relaxed);
    }

    /// The configured automatic compaction record threshold (`0` = unlimited).
    #[cfg(feature = "retention")]
    #[must_use]
    pub fn max_retained_records(&self) -> usize {
        self.shared.max_retained_records.load(Ordering::Relaxed)
    }

    /// Configure automatic writer-side state-carrying compaction whenever the log's
    /// retained byte size exceeds `max` (`0` = unlimited, the default).
    #[cfg(feature = "retention")]
    pub fn set_max_retained_bytes(&self, max: usize) {
        self.shared.max_retained_bytes.store(max, Ordering::Relaxed);
    }

    /// The configured automatic compaction byte threshold (`0` = unlimited).
    #[cfg(feature = "retention")]
    #[must_use]
    pub fn max_retained_bytes(&self) -> usize {
        self.shared.max_retained_bytes.load(Ordering::Relaxed)
    }

    /// Operational snapshot of this broker's log, subscriber-visible head, active
    /// connections, and active listeners (`REQUIREMENTS` R5).
    pub fn stats(&self) -> BrokerStats {
        stats_of(&self.shared)
    }

    /// The broker's monotone millisecond clock (`REQUIREMENTS` R3): never regresses even
    /// if the OS wall clock steps backwards, so a capability expiry or fabric deadline
    /// (`bt=`) that has lapsed can never become un-expired.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        now_unix_ms()
    }

    /// Revoke an Ed25519 peer identity public key (`REQUIREMENTS` R6).
    ///
    /// Any future connection attempt presenting `pubkey` on `Self::serve_tcp_identity`
    /// is refused during the handshake, any capability bound to `identity_principal(&pubkey)`
    /// (`n-<16 hex>`) is revoked, and any already-open connection authenticated under
    /// `pubkey` is terminated with an `unauthorized` error on its next request or
    /// subscription delivery (with any pending last will suppressed). Returns `true`
    /// if `pubkey` was newly added to the revocation set.
    pub fn revoke_identity(&self, pubkey: [u8; 32]) -> bool {
        let (added, changed) = insert_identity_revocation(
            &mut self
                .shared
                .revoked_identities
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            &mut self
                .shared
                .revoked_principals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            pubkey,
        );
        if changed {
            self.shared.revocation_epoch.fetch_add(1, Ordering::AcqRel);
            self.shared.tail.notify_all();
        }
        added
    }

    /// Remove `pubkey` (and its derived `n-<16 hex>` principal) from the live identity
    /// revocation set. Returns `true` if `pubkey` was present.
    pub fn unrevoke_identity(&self, pubkey: &[u8; 32]) -> bool {
        let _file_state = self
            .shared
            .revocation_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = self
            .shared
            .revoked_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(pubkey);
        let derived = identity_principal(pubkey);
        let principal_removed = self
            .shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&derived);
        if removed || principal_removed {
            self.shared.revocation_epoch.fetch_add(1, Ordering::AcqRel);
            self.shared.tail.notify_all();
        }
        removed
    }

    /// Whether `pubkey` is currently in the live identity revocation set.
    pub fn is_identity_revoked(&self, pubkey: &[u8; 32]) -> bool {
        maybe_reload_revocation_file(&self.shared);
        self.shared
            .revoked_identities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(pubkey)
    }

    /// Revoke a capability principal (`p=<principal>`, e.g. `n-<16 hex>` or `s-<id>`).
    /// Any future `Attach` or request using a grant bound to `principal` is refused,
    /// any open subscription authorized solely by `principal` is terminated, and any
    /// pending last will registered under `principal` is suppressed.
    pub fn revoke_principal(&self, principal: &str) -> bool {
        let added = self
            .shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(principal.to_ascii_lowercase());
        if added {
            self.shared.revocation_epoch.fetch_add(1, Ordering::AcqRel);
            self.shared.tail.notify_all();
        }
        added
    }

    /// Remove `principal` from the revoked-principal set.
    pub fn unrevoke_principal(&self, principal: &str) -> bool {
        let lower = principal.to_ascii_lowercase();
        let _file_state = self
            .shared
            .revocation_file
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let removed = self
            .shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&lower);
        if removed {
            self.shared.revocation_epoch.fetch_add(1, Ordering::AcqRel);
            self.shared.tail.notify_all();
        }
        removed
    }

    /// Whether `principal` is currently revoked.
    pub fn is_principal_revoked(&self, principal: &str) -> bool {
        maybe_reload_revocation_file(&self.shared);
        self.shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&principal.to_ascii_lowercase())
    }

    /// Load a revocation file (`REQUIREMENTS` R6). On Apple targets, also register its
    /// path for automatic `SIGHUP` reload; elsewhere call this method again to reload:
    /// each non-empty, non-`#`-comment line is either a 64-hex-character
    /// Ed25519 public key (revoking both the transport key and its derived `n-<16 hex>`
    /// principal) or a principal string (`n-...`, `s-...`, `a-...`, `h-...`).
    ///
    /// Parsed atomically and fail-closed: if the file cannot be read or contains any
    /// malformed line, this call (or a later `SIGHUP` reload) returns an error and
    /// leaves all existing revocations untouched. An Apple handler-installation failure
    /// also returns an error before changing revocations. File replacement preserves
    /// manual revocations; explicit un-revokes clear both sources until a later reload.
    /// Returns the number of entries loaded.
    pub fn load_revocation_file(&self, path: impl AsRef<Path>) -> io::Result<usize> {
        apply_revocation_file(&self.shared, path.as_ref())
    }

    /// Spawn the writer over an opened log.
    fn start(
        mut log: BrokerLog,
        replicator: Option<Replicator>,
        cap_secret: Option<Vec<u8>>,
    ) -> Broker {
        // Strict/Relaxed: everything durable is visible. Replicated: only what a quorum
        // already confirmed (the caller caught the followers up before handing us
        // the replicator, so the watermark is the current one).
        let head = match &replicator {
            Some(rep) => Replicator::watermark(
                &rep.links
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
                rep.quorum,
            ),
            None => log.head().0,
        };
        log.start_last_visible(head);
        let durable = log.durable_head().0;
        let base = log.base().0;
        let log_head = log.head().0;
        let is_replica = log.is_replica();
        let durable_marks = log.served().served == crate::proto::Tier::Ordered;
        let horizon = log.horizon();
        let (writes, rx) = mpsc::channel::<Queued>();
        // The copies the handshake reads instead of the log (see their fields): taken
        // here, before any connection or the writer can run. Only a guarded broker
        // checks bindings at attach, so only it keeps their copy.
        let hello = HelloCopy::of(&log);
        let vitals = log.vitals();
        let log_path = log.path().to_path_buf();
        let mut bindings = HashMap::new();
        if cap_secret.is_some() {
            for (id, principal, off) in log.durable_bindings() {
                bindings.insert(id, (principal.to_string(), off.0));
            }
        }
        let shared = Arc::new(Shared {
            log: Mutex::new(log),
            tail: Condvar::new(),
            head: AtomicU64::new(head),
            durable: AtomicU64::new(durable),
            durable_marks,
            horizon,
            running: AtomicBool::new(true),
            writes,
            conns: Mutex::new(HashMap::new()),
            conn_seq: AtomicU64::new(0),
            replicator,
            cap_secret,
            first_frame_timeout_ms: AtomicU64::new(FIRST_FRAME_TIMEOUT.as_millis() as u64),
            write_timeout_ms: AtomicU64::new(ACK_WRITE_TIMEOUT.as_millis() as u64),
            max_conns: AtomicUsize::new(MAX_CONNS),
            idle_mark_ms: AtomicU64::new(IDLE_MARK.as_millis() as u64),
            liveness_poll_ms: AtomicU64::new(LIVENESS_POLL.as_millis() as u64),
            keepalive_deadline_ms: AtomicU64::new(KEEPALIVE_DEADLINE.as_millis() as u64),
            refusing: AtomicUsize::new(0),
            active_listeners: AtomicUsize::new(0),
            writer: Mutex::new(None),
            accepted: AtomicU64::new(0),
            acceptor_wakeups: AtomicU64::new(0),
            acceptor_sleeps: AtomicU64::new(0),
            acceptors_abandoned: AtomicU64::new(0),
            revoked_identities: Mutex::new(RevocationSet::new()),
            revoked_principals: Mutex::new(RevocationSet::new()),
            revocation_file: Mutex::new(None),
            has_revocation_file: AtomicBool::new(false),
            sighup_seen: AtomicU64::new(0),
            revocation_epoch: AtomicU64::new(0),
            last_round_pause: Mutex::new(None),
            delivery_crc_cache: Box::new(std::array::from_fn(|_| Mutex::new(None))),
            hello: Mutex::new(hello),
            bindings: Mutex::new(bindings),
            writer_round_trips: AtomicU64::new(0),
            writer_batches: AtomicU64::new(0),
            writer_wakeups: AtomicU64::new(0),
            empty_batches: AtomicU64::new(0),
            binds_settled: AtomicU64::new(0),
            wills_settled: AtomicU64::new(0),
            tail_broadcasts: AtomicU64::new(0),
            tail_wakeups: AtomicU64::new(0),
            peer_probes: AtomicU64::new(0),
            parked_tails: AtomicUsize::new(0),
            batch: Mutex::new(()),
            retention_waits: AtomicU64::new(0),
            #[cfg(feature = "retention")]
            compaction: Mutex::new(()),
            batch_pause: Mutex::new(BatchPauseSeam::default()),
            batch_pause_armed: AtomicBool::new(false),
            unclean_stop: AtomicBool::new(false),
            vitals,
            log_path,
            health: Mutex::new(None),
            health_thread: Mutex::new(None),
            base: AtomicU64::new(base),
            log_head: AtomicU64::new(log_head),
            is_replica: AtomicBool::new(is_replica),
            max_retained_records: AtomicUsize::new(0),
            max_retained_bytes: AtomicUsize::new(0),
        });
        let w_shared = shared.clone();
        let writer = thread::spawn(move || {
            let _exit = WriterExit(&w_shared);
            writer_loop(&w_shared, rx);
        });
        *shared
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(writer);
        // A BROKER RESTART IS A PRESENCE EPOCH — for a broker that OWNS its log.
        // Every connection died with no connection thread left to fire its will, so on
        // open every will the log still holds is fired through the same fence. A
        // producer that reconnects and publishes above its will's sequence suppresses
        // it; a will that already fired dedups to a no-op; a producer that really died
        // is marked gone exactly once. Without this a node that died while the broker
        // was down would read `live` forever.
        //
        // NOT ON A REPLICA. A replication target's log is the LEADER's spine, and the
        // wills on it are the leader's — shipped like every other record. Firing one
        // here would append, at this follower's own next offset, a record the leader
        // does not have: a forged death notice for a node that is still live, a log
        // that is no longer a prefix of the leader's, and — at the leader's next ship
        // to that offset — a `Diverged` refusal that fences the link for good. Whose
        // log this is, is a durable property of the log itself
        // ([`BrokerLog::is_replica`]), so a follower restarted through plain
        // [`Broker::open`] is still a follower.
        let pending = {
            let log = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if log.is_replica() {
                Vec::new()
            } else {
                log.pending_wills()
            }
        };
        for will in pending {
            fire_will(&shared, will);
        }
        Broker { shared }
    }

    /// Open a broker that ENFORCES capabilities on its accept path: every
    /// connection must `Attach` a capability (a signed astream_wire Filter) that
    /// grants each subject it publishes and contains each filter it subscribes;
    /// unauthorized requests are refused. Without the `cap` feature there is no way
    /// to set a secret, so the default broker never enforces (and stays zero-dep).
    ///
    /// An EMPTY `secret` is refused (`InvalidInput`): an HMAC under an empty key is
    /// computable by anyone, so it would admit any capability anyone cares to mint.
    ///
    /// A connection must have a capability accepted within [`FIRST_FRAME_TIMEOUT`] of
    /// connecting ([`set_first_frame_timeout`](Self::set_first_frame_timeout)), or it
    /// is refused and closed: nothing it can send before that authenticates it.
    ///
    /// Strict: [`open_guarded_with`](Self::open_guarded_with) at
    /// [`Durability::Strict`], as [`open`](Self::open) is
    /// [`open_with`](Self::open_with)'s.
    #[cfg(feature = "cap")]
    pub fn open_guarded(log_path: impl AsRef<Path>, secret: Vec<u8>) -> io::Result<Broker> {
        Self::open_guarded_with(log_path, secret, Durability::Strict)
    }

    /// [`open_guarded`](Self::open_guarded) at an explicit [`Durability`] tier. The
    /// guard and the tier are ORTHOGONAL: the guard decides which requests a
    /// connection may make, the tier what an `ack` to an authorized one promises, and
    /// neither reads the other. So a Relaxed guarded broker enforces exactly what a
    /// Strict one does — the same capability checks on the same accept path, the same
    /// refusal of an empty `secret` — and acks after the batch's page-cache write
    /// instead of its fsync, which is [`Durability::Relaxed`]'s whole difference.
    ///
    /// WHAT RELAXED COSTS A CLIENT THAT KEEPS OFFSETS. A Relaxed ack survives a process
    /// crash, not a power loss. A power loss is ANNOUNCED: the first open after the
    /// reboot finds the run did not stop cleanly in that boot, keeps the longest intact
    /// prefix above the durable floor, and moves the log's
    /// [`identity`](Self::log_identity) generation with lineage — so a client compares
    /// the identity it kept offsets under, and resumes at
    /// [`LogIdentity::continues`](crate::LogIdentity::continues) (or forgets, against a
    /// broker or a log that cannot say where it continues). The embedder does not move
    /// the identity itself; one with its own unclean verdict hands it to the open
    /// ([`Prepared::Unclean`](crate::Prepared), through
    /// [`open_opts`](Self::open_opts)), and the generation moves once. A stop through
    /// [`BrokerHandle::shutdown`] closes the log cleanly and moves nothing at the next
    /// boot.
    ///
    /// [`Durability::Ordered`] costs the same kind of announced loss, bounded: at most
    /// the batches acked since the last full flush (the horizon's). Through this
    /// shorthand its escalation table is EMPTY, so every record rides the horizon; a
    /// broker whose records a person would miss names them in
    /// [`OpenOptions::escalation`] through [`open_opts`](Self::open_opts), with the
    /// guard in [`OpenOptions::guard`] — a guard is what binds producers to principals,
    /// the `PrincipalPrefix` rule's whole input.
    #[cfg(feature = "cap")]
    pub fn open_guarded_with(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
        durability: Durability,
    ) -> io::Result<Broker> {
        if secret.is_empty() {
            return Err(empty_secret());
        }
        let log = BrokerLog::open_with(log_path, durability)?;
        Ok(Self::start(log, None, Some(secret)))
    }

    /// [`open_guarded_with`](Self::open_guarded_with) with sidecar preparation
    /// under the log's own lock, as in [`open_with_prepare`](Self::open_with_prepare).
    /// An empty capability secret is refused before opening the log or calling
    /// `prepare`; capability enforcement and durability otherwise stay unchanged.
    #[cfg(feature = "cap")]
    pub fn open_guarded_with_prepare(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
        durability: Durability,
        prepare: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<Broker> {
        if secret.is_empty() {
            return Err(empty_secret());
        }
        let log = BrokerLog::open_with_prepare(log_path, durability, prepare)?;
        Ok(Self::start(log, None, Some(secret)))
    }

    /// Open a broker in the REPLICATED durability tier. The local write is page-cache
    /// (Relaxed, no fsync); an `ack` ADDITIONALLY requires a `quorum` of `follower_addrs`
    /// (other brokers reachable over TCP — ordinary `serve_tcp` brokers) to hold the
    /// record in memory. So an acked record survives the loss of the leader node — a
    /// follower holds it — though not a full-cluster power loss. This is the dial's
    /// middle setting (Kafka's default posture: durability from replicas, not fsync).
    ///
    /// The leader ships every committed record (data, commit records, and their
    /// annotations) at its exact leader offset, so a follower's log is an identical
    /// prefix of the leader's; shipping resumes from each follower's confirmed prefix,
    /// so a follower whose link dropped is caught up automatically once it is back,
    /// and an ack — for a new record, a deduped retry, or a pure commit — is given only
    /// once the record is below the quorum watermark. Leader-side subscribers are
    /// delivered only quorum-confirmed records.
    ///
    /// At open, every follower is dialed and the reachable ones are caught up to the
    /// local log (an error if a quorum does not confirm it — a follower refused or
    /// diverged). A follower that cannot be dialed is a DOWN link, re-dialed on every
    /// batch and caught up when it is back — unless fewer than `quorum` followers are
    /// reachable, which is refused as a configuration error. `quorum` is clamped to
    /// `1..=followers.len()`, and a follower-less "replicated" broker is refused
    /// (`InvalidInput`) rather than degrading silently to Relaxed with a vacuous quorum.
    /// A follower whose log DIVERGED (a different record at an offset the leader ships)
    /// is fenced: nothing more is shipped to it and it never counts toward the quorum.
    /// A log compacted by retention opens too: shipping starts at its base, and a
    /// follower that lacks records below the base — gone from the leader — cannot be
    /// caught up, so it is left behind, counting toward the quorum only for what it
    /// holds. Follower round-trips use [`REPLICA_IO_TIMEOUT`]-bounded sockets: a follower
    /// that never answers fails that batch's confirmation rather than stalling the
    /// writer, and shutdown force-closes the links.
    pub fn open_replicated(
        log_path: impl AsRef<Path>,
        follower_addrs: &[String],
        quorum: usize,
    ) -> io::Result<Broker> {
        Self::open_replicated_with(log_path, follower_addrs, quorum, REPLICA_IO_TIMEOUT)
    }

    /// [`open_replicated`](Self::open_replicated) with an explicit follower I/O timeout
    /// (connect / per-read / per-write on each follower link; the bound on how long a
    /// batch waits on an unresponsive follower).
    pub fn open_replicated_with(
        log_path: impl AsRef<Path>,
        follower_addrs: &[String],
        quorum: usize,
        replica_timeout: Duration,
    ) -> io::Result<Broker> {
        Self::open_replicated_inner(
            log_path,
            None,
            follower_addrs,
            #[cfg(feature = "cap")]
            None,
            quorum,
            replica_timeout,
        )
    }

    /// [`open_replicated`](Self::open_replicated) with capability enforcement on the
    /// leader's accept path (`secret`) AND capability attachment (`follower_cap`) on
    /// every follower replication link (at initial dial and on every reconnect).
    ///
    /// `follower_cap` must be an unbound read-write capability accepted by the
    /// followers (e.g. minted with `"rw:/>"` under the followers' secret); a follower
    /// that refuses `Attach` or `Replicate` with `ERR_UNAUTHORIZED` is fenced. An
    /// empty `secret` is refused (`InvalidInput`) before opening the log or dialing
    /// any follower.
    #[cfg(feature = "cap")]
    pub fn open_replicated_guarded(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
        follower_addrs: &[String],
        follower_cap: astream_cap::Capability,
        quorum: usize,
    ) -> io::Result<Broker> {
        Self::open_replicated_guarded_with(
            log_path,
            secret,
            follower_addrs,
            follower_cap,
            quorum,
            REPLICA_IO_TIMEOUT,
        )
    }

    /// [`open_replicated_guarded`](Self::open_replicated_guarded) with an explicit
    /// follower I/O timeout.
    #[cfg(feature = "cap")]
    pub fn open_replicated_guarded_with(
        log_path: impl AsRef<Path>,
        secret: Vec<u8>,
        follower_addrs: &[String],
        follower_cap: astream_cap::Capability,
        quorum: usize,
        replica_timeout: Duration,
    ) -> io::Result<Broker> {
        if secret.is_empty() {
            return Err(empty_secret());
        }
        match astream_cap::Grant::parse(follower_cap.filter.as_str()) {
            Ok(g) if g.mode == astream_cap::Mode::ReadWrite && g.principal.is_none() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "follower capability {:?} must be an unbound read-write grant (e.g. \"rw:/>\"); \
                         a bound or read-only grant cannot authorize Request::Replicate",
                        follower_cap.filter
                    ),
                ));
            }
            Err(e) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("invalid follower capability {:?}: {e}", follower_cap.filter),
                ));
            }
        }
        Self::open_replicated_inner(
            log_path,
            Some(secret),
            follower_addrs,
            Some(follower_cap),
            quorum,
            replica_timeout,
        )
    }

    fn open_replicated_inner(
        log_path: impl AsRef<Path>,
        secret: Option<Vec<u8>>,
        follower_addrs: &[String],
        #[cfg(feature = "cap")] follower_cap: Option<astream_cap::Capability>,
        quorum: usize,
        replica_timeout: Duration,
    ) -> io::Result<Broker> {
        if follower_addrs.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "the Replicated tier needs at least one follower (no replica means no \
                 quorum, which would silently be the Relaxed tier)",
            ));
        }
        let mut log = BrokerLog::open_with(log_path, Durability::Relaxed)?;
        // Every link's confirmed prefix starts at the log's retention BASE: records
        // below it were pruned, so there is nothing below it to ship or confirm.
        let base = log.base().0;
        let quorum = quorum.clamp(1, follower_addrs.len());
        let mut links = Vec::new();
        let mut kill = Vec::new();
        let mut live = 0usize;
        let mut last_err: Option<io::Error> = None;
        for addr in follower_addrs {
            // A follower that cannot be dialed now is a DOWN link (re-dialed on every
            // batch, then caught up), not a fatal error — unless so many are down that
            // the quorum could never be met, which is a configuration error to fail
            // fast on rather than a broker that refuses every ack.
            #[allow(unused_mut)]
            let mut init_fenced: Option<String> = None;
            let stream = match connect_follower(addr, replica_timeout) {
                #[allow(unused_mut)]
                Ok(mut s) => {
                    #[cfg(feature = "cap")]
                    let auth_res = match follower_cap.as_ref() {
                        Some(cap) => attach_follower_cap(&mut s, cap, replica_timeout),
                        None => Ok(()),
                    };
                    #[cfg(not(feature = "cap"))]
                    let auth_res: Result<(), (bool, io::Error)> = Ok(());
                    match auth_res {
                        Ok(()) => {
                            live = live.saturating_add(1);
                            kill.push(match s.try_clone() {
                                Ok(c) => Some(c),
                                Err(_) => None,
                            });
                            Some(s)
                        }
                        Err((is_unauth, e)) => {
                            if is_unauth {
                                init_fenced = Some(e.to_string());
                            }
                            last_err = Some(e);
                            kill.push(None);
                            None
                        }
                    }
                }
                Err(e) => {
                    last_err = Some(e);
                    kill.push(None);
                    None
                }
            };
            links.push(FollowerLink {
                leader: log.identity(),
                addr: addr.clone(),
                stream,
                upto: base,
                fenced: init_fenced,
                fresh: false,
                timeout_failures: 0,
                next_dial_at: None,
                #[cfg(feature = "cap")]
                cap: follower_cap.clone(),
            });
        }
        if live < quorum {
            let e = match last_err {
                Some(e) => e,
                None => io::Error::other("no follower reachable"),
            };
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "Replicated tier: only {live} of {} followers reachable, so a quorum of \
                     {quorum} could never confirm anything ({e})",
                    follower_addrs.len()
                ),
            ));
        }
        let rep = Replicator {
            links: Mutex::new(links),
            kill: Mutex::new(kill),
            quorum,
            io_timeout: replica_timeout,
        };
        // CATCH-UP: bring every follower up to the local log before serving, so the
        // leader starts with its visible head at the quorum watermark and a follower
        // restarted with its log intact merely confirms what it already holds.
        let head = log.head().0;
        let backlog = log.read_from(Offset(base));
        let wm = rep.replicate(&backlog, &AtomicBool::new(true));
        let committed_wm = log.record_replica_ack(Offset(wm)).0;
        if committed_wm < head {
            return Err(io::Error::other(format!(
                "Replicated tier: a quorum confirmed only {committed_wm} of the {head} local records \
                 at open (a follower refused, diverged, or timed out)"
            )));
        }
        Ok(Self::start(log, Some(rep), secret))
    }

    /// The durable head of the log (its record count; the offset the next commit
    /// takes), read without waiting on the log lock.
    pub fn head(&self) -> u64 {
        self.shared.log_head.load(Ordering::Acquire)
    }

    /// The lowest offset still retained in the log (records below it were pruned by
    /// retention), read without waiting on the log lock.
    pub fn base(&self) -> u64 {
        self.shared.base.load(Ordering::Acquire)
    }

    /// The [`LogIdentity`](crate::LogIdentity) of the log this broker serves — what
    /// it tells every client in the answer to `Hello` (on a replica, its leader's once
    /// a replicated batch carrying it has committed and reached its newest record).
    /// Read from the same copy `Hello` answers from, so the two never disagree, and
    /// like `Hello` it does not wait on the log lock.
    pub fn log_identity(&self) -> crate::LogIdentity {
        self.shared
            .hello
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .identity
    }

    /// The SUBSCRIBER-VISIBLE head. Equal to [`head`](Self::head) in the Strict tier,
    /// and in the Ordered and Relaxed tiers but for one window: a batch holding a record
    /// the escalation table names ([`OpenOptions::escalation`]) is HELD here, at that
    /// record, from its commit until the full flush it owes has succeeded — so the record
    /// is durable before anyone can see it or be acked for it; if that flush fails, for
    /// the rest of the open, which the failure poisons. In the Replicated tier it
    /// is the quorum watermark — records at or above it are committed locally but not yet
    /// held by a quorum of followers, so they are neither acked nor delivered.
    pub fn visible_head(&self) -> u64 {
        self.shared.head.load(Ordering::Relaxed)
    }

    /// The DURABLE head ([`BrokerLog::durable_head`]): every record below this offset is
    /// on stable media by a full flush this broker saw succeed. Strict: the head, after
    /// every batch. Ordered and Relaxed: it moves at the open, at each full flush (an
    /// escalated record's, the Ordered horizon's, [`flush_durable`](Self::flush_durable))
    /// and at the clean close — never at an Ordered batch's barrier. Read without the log
    /// lock (a sampler can poll it), and the value every `Mark` carries.
    pub fn durable_head(&self) -> u64 {
        self.shared.durable.load(Ordering::Acquire)
    }

    /// The tier this broker serves, with the one it was asked for and why they differ
    /// ([`ServedInfo`](crate::ServedInfo)) — exactly what the answer to `Hello` tells a
    /// client (`Client::served_durability`), read from the same copy, without the log
    /// lock. It changes in a running broker only when the log latches to Strict, which
    /// it reports from the moment the log latches (read from the log's vitals, beside
    /// the poison): once [`poisoned`](Self::poisoned) has named a failure that latched
    /// the log, this names the latch too.
    pub fn served_durability(&self) -> crate::ServedInfo {
        // The copy's lock is released before the log's could be taken (the writer
        // takes them in the other order).
        let told = self
            .shared
            .hello
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ext
            .durability;
        match told {
            Some(s) => self.shared.vitals.served(s),
            None => self
                .shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .served(),
        }
    }

    /// How many flushes the log has issued since it opened, by kind ([`FlushCounts`]):
    /// barriers, full flushes, full flushes skipped because one already covered the
    /// head, and flushes that failed. Read WITHOUT the log lock (each count is exact;
    /// the four are not one instant) — the same counts every `Hello` answer carries
    /// ([`HelloHealth`](crate::HelloHealth)).
    pub fn flush_counts(&self) -> FlushCounts {
        self.shared.vitals.flush_counts()
    }

    /// `Some(reason)` once the log is POISONED ([`BrokerLog::poisoned`]): it refuses
    /// every write until it is reopened, and no resend to this broker lands. The cure is
    /// the embedder's: drop this broker and open the log again — in a supervised
    /// process, exit and let the supervisor start it — which moves the generation when
    /// the failure may have lost acked records (the run is recorded `Lossy`). Every write
    /// refused meanwhile carries the reason too, in a refusal that starts
    /// [`LOG_POISONED_PREFIX`]
    /// ([`BrokerRefusal::is_poisoned`](crate::BrokerRefusal::is_poisoned)), and so does
    /// the answer to every `Hello` ([`HelloHealth`](crate::HelloHealth)).
    ///
    /// [`stats`](Self::stats)' `log.poisoned` says the same as a flag; this names why, so
    /// an embedder's exit line can say what astream decided. The first reason stands: the
    /// poison is set once.
    ///
    /// LOCK-FREE: one atomic load, never the log lock. The writer holds that lock across
    /// a rollback's truncate and across the `<log>.id` write a failed flush records
    /// `Lossy` in — filesystem I/O that a failing disk makes slow, at exactly the moment
    /// this is asked — so a look that took it could wait out the very failure it was
    /// looking for (`broker.poisoned-refusal`). [`wait_poisoned`](Self::wait_poisoned)
    /// sleeps on it instead of polling.
    pub fn poisoned(&self) -> Option<String> {
        match self.shared.vitals.poisoned() {
            Some(s) => Some(s.to_string()),
            None => None,
        }
    }

    /// Sleep until the log is POISONED ([`poisoned`](Self::poisoned)) and return why, or
    /// return `None` once `timeout` has passed or the broker has stopped serving its log
    /// (its last listener shut down, or an unserved broker dropped), whichever is first.
    /// For an embedder that exits on the poison, so its supervisor reopens the log: a
    /// thread parked here wakes at the poison itself, with no poll and without the log
    /// lock.
    pub fn wait_poisoned(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now().checked_add(timeout);
        self.shared
            .vitals
            .wait(deadline, &self.shared.running, true);
        self.poisoned()
    }

    /// The broker's health now (`REQUIREMENTS` R5): what the log holds, who is attached
    /// and parked, what its flushes did, whether it is poisoned — the snapshot
    /// `<log>.health` carries ([`BrokerHealth`](crate::BrokerHealth)), stamped with this
    /// process's pid and [`now_ms`](Self::now_ms). Takes the log lock, as
    /// [`stats`](Self::stats) does.
    pub fn health(&self) -> crate::health::BrokerHealth {
        health_of(&self.shared)
    }

    /// Write `<log>.health` now ([`health`](Self::health)'s snapshot), replacing the one
    /// there, and hold it: until the log closes, [`read_health`](crate::read_health)
    /// tells a reader in another process that this broker still serves the log. For an
    /// embedder that writes it on its own schedule; [`keep_health`](Self::keep_health)
    /// writes it on a period.
    ///
    /// # Errors
    ///
    /// The sidecar could not be written (the one published before, if any, stays, and
    /// stays held), or the broker has stopped serving its log — a broker that has
    /// closed its log publishes nothing a reader would take for a serving one.
    pub fn write_health(&self) -> io::Result<()> {
        write_health_of(&self.shared).1
    }

    /// Keep `<log>.health` from now on: write it now, then again every `every` — and at
    /// once when the log is poisoned — until the log closes, when the last one is
    /// written, after the log's clean close, and let go
    /// ([`read_health`](crate::read_health) then reads its writer
    /// [`Gone`](crate::HealthWriter::Gone)). [`OpenOptions::health_every`] asks for the
    /// same at the open. A second call writes now and keeps the first call's period.
    ///
    /// "At once" is the period's thread waking at the poison, which nothing orders
    /// before a [`wait_poisoned`](Self::wait_poisoned) that wakes at it too: an embedder
    /// that exits on the poison and wants the file to name it calls
    /// [`write_health`](Self::write_health) first, or stops the broker (drops it, or
    /// shuts its last listener down), whose last word names it.
    ///
    /// A write the period's thread cannot make is said once on stderr and tried again
    /// next period; the file published before it stays.
    ///
    /// # Errors
    ///
    /// The first write failed (the thread still starts and keeps trying), or the broker
    /// has stopped serving its log.
    pub fn keep_health(&self, every: Duration) -> io::Result<()> {
        let (named, first) = write_health_of(&self.shared);
        if !self.shared.running.load(Ordering::Acquire) {
            return first;
        }
        let mut slot = self
            .shared
            .health_thread
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.is_none() {
            let shared = Arc::clone(&self.shared);
            *slot = Some(thread::spawn(move || health_loop(&shared, every, named)));
        }
        first
    }

    /// FLUSH THE LOG IN FULL, through the writer: the op stages nothing, rides the next
    /// batch, and is answered once a full flush covers everything committed before it —
    /// issued outside the log lock, after that batch's other acks, and skipped (counted
    /// so) when one already covers the head. Returns the durable head after it.
    ///
    /// For an embedder that must know a moment's records are on stable media (a clean
    /// hand-off, a checkpoint), and for tests: `Durability::Ordered { horizon: None }`
    /// plus this makes durability deterministic. On Strict it is always skipped.
    ///
    /// # Errors
    /// The flush failed (the run is then recorded `Lossy` and the log refuses every write
    /// until it is reopened; see [`InjectedFaults::sync`]), or the broker is stopping.
    pub fn flush_durable(&self) -> io::Result<u64> {
        match submit_blocking(&self.shared, WriteKind::Flush) {
            Ok((durable, _)) => Ok(durable),
            Err((code, msg)) => Err(crate::client::BrokerRefusal { code, msg }.into()),
        }
    }

    /// [`BrokerStats::writer_round_trips`] alone, read WITHOUT the log lock — what
    /// [`visible_head`](Self::visible_head) is to [`head`](Self::head).
    /// [`stats`](Self::stats) snapshots the log under its lock, so it waits out the
    /// writer's staging and promotion of a batch — and a writer parked at
    /// [`pause_next_batch`](Self::pause_next_batch), which holds the lock — though not
    /// the batch's write and flush, which run with the lock released (there it answers
    /// at once with the head before the batch, so it neither waits for a write nor
    /// blocks behind one); this takes no lock at all. The counter is bumped by
    /// the connection thread BEFORE it hands the op to the writer, so a test that has
    /// parked the writer ([`pause_next_batch`](Self::pause_next_batch)) reads here that
    /// a round trip is now queued behind the parked batch.
    pub fn writer_round_trips(&self) -> u64 {
        self.shared.writer_round_trips.load(Ordering::Relaxed)
    }

    /// Arm (or clear) the log's documented fault-injection seam — see
    /// [`InjectedFaults`]. A test hook; inert unless called.
    pub fn inject_faults(&self, faults: InjectedFaults) {
        self.shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .inject_faults(faults);
    }

    /// Arm the UNCLEAN-STOP seam: when this broker stops — its last listener's
    /// [`shutdown`](BrokerHandle::shutdown) (or that handle's drop), or the drop of a
    /// broker never served — it stops WITHOUT the clean close. Everything else about the
    /// stop is unchanged: the writer commits what was queued and is joined, the
    /// connections close, the log is latched shut — this broker writes nothing to it
    /// again, a retention call on the `Broker` value kept after the stop refused as after
    /// a clean close — and the log's lock is released, so the next open (in this process
    /// too) proceeds. But the log is not flushed in full, and `<log>.id` is left as this
    /// run last wrote it: the record its open wrote (`Open`, the floor at the open's
    /// head), unless the run itself rewrote it since (a full flush that failed records
    /// `Lossy`; a replica records the identity it adopted). That is what a killed process
    /// leaves, and what a power loss leaves minus the page cache, so the next open judges
    /// the log by the unclean-stop rules (the [`LogIdentity`](crate::LogIdentity) module
    /// docs). In ANOTHER boot a run that SERVED Ordered or Relaxed moves the generation
    /// once, with lineage at the head the open recovers, and a run served Strict moves
    /// nothing, its acks having been on stable media. In the SAME boot nothing moves
    /// only where the open can trust the page cache: the boot named (`Boot::Known`, or
    /// `Boot::Detect` on a host that names it) and the log on the boot volume
    /// (`Volume::Boot`, or `Volume::Detect` finding it there; see [`Boot`](crate::Boot)
    /// and [`Volume`](crate::Volume)). Under `Boot::Unknown`, or off the boot volume — a
    /// Linux tmpfs `/tmp` is, to `Volume::Detect` — every unclean stop of a run that
    /// served a weak tier moves the generation, a same-boot reopen included, exactly as a
    /// kill does there: a test that stands in "a process crash" names the boot and the
    /// volume.
    ///
    /// A test seam; inert unless called, and nothing in astream calls it. It exists so a
    /// test of a reader that keeps offsets outside the log (an embedder's bridge, its
    /// notifier, a cursor file) can stand in a crash or a reboot of a broker it serves in
    /// process. Without it such a test saved `<log>.id` after the open and wrote it back
    /// after the stop: it forged a file whose format, and the moments it is written at,
    /// are astream's own. It grants nothing a process cannot do to itself by dying, and
    /// its misuse costs the safe direction: a reboot after the stop moves a weak run's
    /// generation, and readers re-read. Armed, it stays armed for the broker's one stop;
    /// there is nothing to clear. The health sidecar (`<log>.health`) still gets its last
    /// word, which reports this process and which no open reads. A [`BrokerLog`] used
    /// directly needs no seam: dropped without [`close_clean`](BrokerLog::close_clean),
    /// it stops this way.
    pub fn arm_unclean_stop(&self) {
        self.shared.unclean_stop.store(true, Ordering::Release);
    }

    /// Override how long a freshly accepted connection may take to send its first
    /// complete frame — on a guarded broker, to have a capability accepted — before it
    /// is reaped (default [`FIRST_FRAME_TIMEOUT`], 30 s). Applies to connections
    /// accepted after the call.
    pub fn set_first_frame_timeout(&self, timeout: Duration) {
        self.shared.first_frame_timeout_ms.store(
            timeout.as_millis().clamp(1, u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// Override how long ONE response write on a served connection may block before
    /// the connection is torn down (default [`ACK_WRITE_TIMEOUT`], 10 s). A peer that
    /// stops reading otherwise parks the connection thread inside `write_all` for good,
    /// holding its fd and its [`MAX_CONNS`] slot with it. Applies to connections
    /// accepted after the call.
    ///
    /// It bounds EVERY write on the connection: the request thread's, and the pipelined
    /// ack-writer's, which runs on a `try_clone` of the same socket and takes this value
    /// too (`SO_SNDTIMEO` belongs to the socket, not to the descriptor, so the two
    /// halves cannot hold different bounds). A streaming verb — `Subscribe`, `SubscribeGroup`, `ForkSubscribe` — raises it to
    /// [`STREAM_WRITE_TIMEOUT`] once it takes the socket over.
    pub fn set_write_timeout(&self, timeout: Duration) {
        self.shared.write_timeout_ms.store(
            timeout.as_millis().clamp(1, u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// Override the cap on simultaneously open connections (default [`MAX_CONNS`],
    /// 1024); a connection beyond it is answered with an error and closed.
    pub fn set_max_conns(&self, max: usize) {
        self.shared.max_conns.store(max, Ordering::Relaxed);
    }

    /// Override how long an opted-in caught-up subscription goes with nothing written before
    /// the broker marks it (default [`IDLE_MARK`], 10 s). A parked tail looks at the
    /// clock once per liveness poll (200 ms, [`set_liveness_poll`](Self::set_liveness_poll)),
    /// so that is the finest interval served.
    /// Applies to subscriptions opened after the call.
    pub fn set_idle_mark(&self, interval: Duration) {
        self.shared.idle_mark_ms.store(
            interval.as_millis().clamp(1, u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// Override how often a subscriber tail checks that its subscriber is still there
    /// (default [`LIVENESS_POLL`], 200 ms): a parked tail wakes this often to probe its
    /// peer, look at the clock and mark an idle stream, and a tail that writes nothing
    /// probes at most this often. It bounds how long the thread and descriptor of a
    /// subscriber that left without a word outlive it, and is the finest interval
    /// [`set_idle_mark`](Self::set_idle_mark) can serve. It never delays a delivery: a
    /// commit wakes a parked tail through the `Condvar`, not this poll. Applies to
    /// subscriptions opened after the call.
    pub fn set_liveness_poll(&self, interval: Duration) {
        self.shared.liveness_poll_ms.store(
            interval.as_millis().clamp(1, u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// Override how long a TCP connection that keeps alive may go with nothing read
    /// from it before the broker ends it, firing its will (default
    /// [`KEEPALIVE_DEADLINE`], 60 s). Keep it a few times the interval clients keep
    /// alive at ([`KEEPALIVE_INTERVAL`]): shorter than that cuts live clients. Applies
    /// to connections whose first [`Request::Keepalive`] arrives after the call.
    pub fn set_keepalive_deadline(&self, deadline: Duration) {
        self.shared.keepalive_deadline_ms.store(
            deadline.as_millis().clamp(1, u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }

    /// Pause the next `Last` walk at the SCAN-ROUND BOUNDARY after round `round`:
    /// after that round has added its rows, before the next round re-takes the log
    /// lock — the exact window a concurrent publisher gets. One-shot: the first walk
    /// to reach that boundary takes the pause.
    ///
    /// A test seam; inert unless called. It exists so a test can OCCUPY that window
    /// instead of racing for it — the walk's connection thread stops there with the log
    /// lock released, so another thread may [`wait`](LastRoundPause::wait) for it,
    /// publish through a second connection and block on the ack, and the walk does not
    /// resume until that thread [`resume`](LastRoundPause::resume)s it (or drops the
    /// handle).
    pub fn pause_last_scan_round(&self, round: usize) -> LastRoundPause {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *self
            .shared
            .last_round_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(RoundPause {
            round,
            reached: reached_tx,
            resume: resume_rx,
        });
        LastRoundPause {
            reached: reached_rx,
            resume: resume_tx,
        }
    }

    /// Pause the writer inside its NEXT batch: after it has taken the log lock and
    /// staged the batch, before it begins the commit — the part of a batch that still
    /// holds the log lock (staging, the seal, then the promotion; the write and the
    /// flush run without it, see [`pause_next_write`](Self::pause_next_write)). One-shot:
    /// the first batch to reach that point after this call takes the pause.
    ///
    /// A test seam; inert unless called. It exists so a test can OCCUPY the writer's
    /// lock hold instead of racing a real batch for it: the writer stops there with
    /// the log lock HELD, so another thread may [`wait`](BatchPause::wait) for it and
    /// then show that something completes without the log lock (it would hang
    /// otherwise), and the batch does not commit until that thread
    /// [`resume`](BatchPause::resume)s it (or drops the handle). Anything that needs the
    /// log lock — [`stats`](Self::stats) included — waits for the resume.
    ///
    /// Arming replaces any pause still armed (its [`wait`](BatchPause::wait) answers
    /// `false`); on a broker whose writer has stopped, `wait` answers `false` at once.
    pub fn pause_next_batch(&self) -> BatchPause {
        self.arm_batch_pause(BatchPoint::Staged)
    }

    /// Pause the writer inside its NEXT non-empty batch at the start of its WRITE: the
    /// batch staged, sealed, its flush chosen and its bytes taken, the log lock
    /// RELEASED, and not one byte of it written or flushed. One-shot, like
    /// [`pause_next_batch`](Self::pause_next_batch), and replaced or answered `false`
    /// the same way. A batch with nothing to write (every op deduplicated or refused)
    /// never lets the lock go and does not take this pause.
    ///
    /// A test seam; inert unless called. It exists so a test can stand where a reader
    /// stands beside a busy writer's write and fsync, and show what holds there: the log
    /// lock is free ([`stats`](Self::stats), `Fetch`, `Last` and a tail's catch-up all
    /// answer), the batch's records are invisible (the head has not moved), and
    /// retention waits for the batch rather than run beside it. Nothing is written until
    /// the test [`resume`](BatchPause::resume)s it (or drops the handle).
    pub fn pause_next_write(&self) -> BatchPause {
        self.arm_batch_pause(BatchPoint::Writing)
    }

    /// Pause the writer just before it ANSWERS its next batch: the batch is committed,
    /// its head published and the bindings it promoted copied for the attach path
    /// (`Shared::bindings`), the log lock released — and not one of its acks sent.
    /// One-shot, like [`pause_next_batch`](Self::pause_next_batch), and replaced or
    /// answered `false` the same way.
    ///
    /// A test seam; inert unless called. It exists so a test can show what a batch's
    /// acks may RELY on: anything a connection does on the strength of its ack happens
    /// after this point, so whatever a test finds already true here — a principal the
    /// batch bound attaching settled on another connection, with no round trip — is
    /// true for every ack the batch sends. The log lock is free in the window, so
    /// [`stats`](Self::stats) answers in it.
    pub fn pause_next_acks(&self) -> BatchPause {
        self.arm_batch_pause(BatchPoint::BeforeAcks)
    }

    /// Pause the writer inside the next batch that OWES A FULL FLUSH (the held head: an
    /// escalated record, a resend of one no full flush covers, a `flush_durable`), between
    /// the acks it sends at once and the flush: every op the held head does not keep back
    /// has been answered and the tails woken, the log lock is released, and the flush has
    /// not been issued. Batches that owe none pass the point by. One-shot, like
    /// [`pause_next_batch`](Self::pause_next_batch), and replaced or answered `false` the
    /// same way.
    ///
    /// A test seam; inert unless called. It exists so a test can hold the window the held
    /// head is FOR instead of racing a real flush through it: in it an escalated record
    /// must be unseen ([`visible_head`](Self::visible_head) at it, the durable head below
    /// it) and its op unanswered — an assertion that, made after the ack arrives, a broker
    /// that acked first and flushed a moment later passes whenever the test thread is
    /// slower than the flush.
    pub fn pause_next_flush(&self) -> BatchPause {
        self.arm_batch_pause(BatchPoint::BeforeFlush)
    }

    /// Arm the batch-pause seam at `at` (see [`BatchPauseSeam`]).
    fn arm_batch_pause(&self, at: BatchPoint) -> BatchPause {
        let (reached_tx, reached_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let mut seam = self
            .shared
            .batch_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // A stopped writer reaches no batch: the slot is not stored, its `reached`
        // sender drops here, and `wait` answers `false` instead of blocking.
        if !seam.writer_stopped {
            seam.slot = Some(BatchPauseSlot {
                at,
                reached: reached_tx,
                resume: resume_rx,
            });
            self.shared.batch_pause_armed.store(true, Ordering::Release);
        }
        BatchPause {
            reached: reached_rx,
            resume: resume_tx,
        }
    }

    /// Bind the Unix socket at `socket_path` and start accepting clients. Returns a
    /// [`BrokerHandle`] that shuts the broker down and unlinks the socket on demand.
    /// A leftover socket file is unlinked only if nobody answers on it (stale, from a
    /// crashed broker); a LIVE broker there is `AddrInUse`, never silently displaced,
    /// and a path that is not a socket at all is `AlreadyExists`, never removed.
    /// cfg(unix): std has no Unix-domain sockets on Windows — use
    /// [`serve_tcp`](Self::serve_tcp) there.
    #[cfg(unix)]
    pub fn serve(&self, socket_path: impl AsRef<Path>) -> io::Result<BrokerHandle> {
        use std::os::unix::fs::FileTypeExt;
        let socket_path = socket_path.as_ref().to_path_buf();
        if let Ok(meta) = std::fs::symlink_metadata(&socket_path) {
            if !meta.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{}: exists and is not a socket; refusing to replace it",
                        socket_path.display()
                    ),
                ));
            }
            match UnixStream::connect(&socket_path) {
                Ok(_live) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!(
                            "{}: a broker is already listening on this socket",
                            socket_path.display()
                        ),
                    ))
                }
                Err(_) => {
                    let _ = crate::store::remove_file_path(&socket_path); // stale: nobody answers
                }
            }
        }
        let listener = UnixListener::bind(&socket_path)?;
        // The wake name first: without one, shutdown could not reach a blocking
        // acceptor once the embedder unlinked `socket_path`, so the listener polls.
        let node = node_at(&socket_path);
        let wake = match node {
            Some(n) => link_wake_name(&socket_path, n),
            None => None,
        };
        let polls = wake.is_none();
        if let Err(e) = listener.set_nonblocking(polls) {
            if let Some(w) = &wake {
                let _ = crate::store::remove_file_path(w);
            }
            return Err(e);
        }
        let shared = self.shared.clone();
        let listener_running = Arc::new(AtomicBool::new(true));
        let acc_running = Arc::clone(&listener_running);
        let (exited_tx, exited) = mpsc::channel::<()>();
        let turns = Arc::new(AtomicU64::new(0));
        let acc_turns = Arc::clone(&turns);
        let spawned = thread::Builder::new().spawn(move || {
            // Dropped as this thread returns: shutdown's signal that it left.
            let _exited = exited_tx;
            accept_loop(
                &shared,
                &acc_running,
                &acc_turns,
                polls,
                || match listener.accept() {
                    Ok((s, _)) => Ok(s),
                    Err(e) => Err(e),
                },
                |s| {
                    // Accepted sockets MUST be blocking (the frame reader uses
                    // read_exact); a polling listener's non-blocking mode may be
                    // inherited (BSD accept), so clear it.
                    if polls {
                        let _ = s.set_nonblocking(false);
                    }
                    let sh = shared.clone();
                    // Detached per-connection thread; ends when its socket ends.
                    // A spawn that fails (thread exhaustion) drops this socket and
                    // keeps accepting — `thread::spawn` would panic the acceptor.
                    let _ = thread::Builder::new().spawn(move || {
                        let _ = handle_conn(&sh, s, Wire::Unix);
                    });
                },
            );
        });
        let acceptor = match spawned {
            Ok(j) => j,
            Err(e) => {
                if let Some(w) = &wake {
                    let _ = crate::store::remove_file_path(w);
                }
                return Err(e);
            }
        };
        self.shared.active_listeners.fetch_add(1, Ordering::AcqRel);
        Ok(BrokerHandle {
            shared: self.shared.clone(),
            listener_running,
            acceptor: Some(acceptor),
            wake: if polls {
                AcceptWake::Polls
            } else {
                AcceptWake::Blocks {
                    exited: Mutex::new(exited),
                    turns,
                }
            },
            endpoint: Endpoint::Unix {
                path: socket_path,
                node,
                wake,
            },
        })
    }

    /// Like [`serve`](Self::serve) but over TCP — multi-machine, PLAINTEXT and
    /// unauthenticated, so for a TRUSTED network only (the sealed transport is
    /// `serve_tcp_sealed`, with the `aead` feature). Binds `addr` (use `127.0.0.1:0`
    /// for an ephemeral port) and serves the SAME Frame protocol. Returns the handle;
    /// read the bound address from it if you bound port 0.
    pub fn serve_tcp(&self, addr: impl std::net::ToSocketAddrs) -> io::Result<BrokerHandle> {
        // Identity wrap: the raw TcpStream IS the connection stream.
        self.serve_tcp_with(addr, |s| Ok((s, None)))
    }

    /// Like [`serve_tcp`](Self::serve_tcp) but the transport is **confidential and
    /// authenticated**: every connection is wrapped in an XChaCha20-Poly1305
    /// [`SealedStream`](astream_aead::SealedStream) under the pre-shared `key`, so
    /// the same `Frame` protocol rides inside an encrypted, ordered record layer.
    /// Each connection opens with the sealed handshake — a fresh hello from each
    /// side, bound with the direction and the record sequence into every record's
    /// AAD, and a key-confirming record each way — so a peer that lacks the key is
    /// refused inside the handshake (it is never handed a frame), and a record
    /// recorded from any other connection or direction cannot be replayed into
    /// this one. Unauthenticated peers are bounded on the accept path: a handshake
    /// must complete within a deadline, only a fixed number of connections may sit
    /// in one at once, and none can force an allocation beyond an empty confirm
    /// record. A client connects with [`crate::client::Client::connect_tcp_sealed`]. This is the
    /// AEAD wire the capability mint's authorization half does not cover.
    #[cfg(feature = "aead")]
    pub fn serve_tcp_sealed(
        &self,
        addr: impl std::net::ToSocketAddrs,
        key: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<BrokerHandle> {
        self.serve_tcp_with(addr, move |s| {
            let sealed = astream_aead::SealedStream::handshake_server(s, key)?;
            Ok((sealed, None))
        })
    }

    /// Like [`serve_tcp_sealed`](Self::serve_tcp_sealed) but with **forward secrecy**:
    /// each connection first runs the Rung-7 key-agreement handshake (ephemeral
    /// X25519 authenticated by the pre-shared `psk`), so the per-session key is
    /// fresh and a later `psk` compromise cannot decrypt past traffic. The handshake
    /// runs in the connection's own thread; a peer that fails it (wrong protocol,
    /// non-contributory point) is dropped. A client connects with
    /// [`crate::client::Client::connect_tcp_handshake`].
    #[cfg(feature = "handshake")]
    pub fn serve_tcp_handshake(
        &self,
        addr: impl std::net::ToSocketAddrs,
        psk: [u8; astream_aead::KEY_LEN],
    ) -> io::Result<BrokerHandle> {
        self.serve_tcp_with(addr, move |s| {
            // Bound the handshake I/O: a peer that connects then stalls (sends
            // nothing, or a partial message) must not park this thread + fd
            // forever — nothing gates the pre-handshake read, so an unauthenticated
            // slowloris would otherwise exhaust threads unbounded. Reset to blocking
            // once the handshake completes; the request loop and tail_loop then set
            // their own timeouts.
            s.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
            s.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
            let sealed = astream_aead::server_handshake(s, &psk)?;
            sealed.get_ref().set_read_timeout(None)?;
            sealed.get_ref().set_write_timeout(None)?;
            Ok((sealed, None))
        })
    }

    /// Like [`serve_tcp_handshake`](Self::serve_tcp_handshake) but with **static
    /// public-key identity and no shared secret** (Rung 8): each connection runs a
    /// mutual signed-DH (SIGMA/Ed25519) handshake — the broker proves its host
    /// `identity`, and the client's identity must be in `authorized_clients`. The
    /// session key still comes from the ephemeral DH alone, so forward secrecy
    /// survives even an identity-key compromise. The handshake runs in the
    /// connection's own thread under the same deadline. A client connects with
    /// [`crate::client::Client::connect_tcp_identity`].
    #[cfg(feature = "identity")]
    pub fn serve_tcp_identity(
        &self,
        addr: impl std::net::ToSocketAddrs,
        identity: astream_aead::IdentityKeypair,
        authorized_clients: Vec<[u8; astream_aead::ID_PUB_LEN]>,
    ) -> io::Result<BrokerHandle> {
        let shared = self.shared.clone();
        self.serve_tcp_with(addr, move |s| {
            s.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
            s.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
            let (sealed, client_id) =
                astream_aead::server_identity_handshake(s, &identity, &authorized_clients)?;
            if is_peer_revoked(&shared, Some(client_id)) {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unauthorized: peer identity key has been revoked",
                ));
            }
            sealed.get_ref().set_read_timeout(None)?;
            sealed.get_ref().set_write_timeout(None)?;
            Ok((sealed, Some(client_id)))
        })
    }

    /// Shared TCP accept loop, generic over how each accepted [`TcpStream`] is
    /// wrapped into the per-connection [`Stream`] (`Ok` for plaintext, the sealed
    /// handshake for the AEAD transport). The wrap runs on the connection's own
    /// thread, never on the acceptor, under pre-authentication bounds — a deadline
    /// and a cap on how many connections may be inside their wrap at once — so a
    /// peer that cannot authenticate can hold up neither other accepts nor a
    /// thread for long (the sealed handshake itself bounds memory: it allocates
    /// nothing an unkeyed peer can size). The cap counts connections INSIDE the
    /// wrap, so the instant plaintext wrap never holds a slot and a burst of
    /// plaintext accepts is never refused by it. A connection that fails to wrap
    /// is dropped.
    fn serve_tcp_with<S, F>(
        &self,
        addr: impl std::net::ToSocketAddrs,
        wrap: F,
    ) -> io::Result<BrokerHandle>
    where
        S: Stream,
        F: Fn(TcpStream) -> io::Result<(S, Option<[u8; 32]>)> + Send + Sync + 'static,
    {
        // How long a connection may take to complete its wrap (the sealed handshake):
        // the socket read timeout until the wrap returns, lifted once it has — the
        // protocol above sets its own timeouts.
        const PREAUTH_TIMEOUT: Duration = Duration::from_secs(5);
        // How many connections may sit inside the wrap at once; an accept beyond
        // it is dropped at once rather than given a thread.
        const MAX_PREAUTH_CONNS: usize = 64;

        let listener = TcpListener::bind(addr)?;
        // Report a *connectable* address: a wildcard bind (0.0.0.0 / ::) is not a valid
        // destination for a client, so map an unspecified host to loopback (keeping
        // the port) for `tcp_addr`. It is also what shutdown dials to get the
        // blocking acceptor out of `accept()` — an address cannot be unlinked, so TCP
        // needs no wake name and never polls.
        let dial = connectable_addr(listener.local_addr()?);
        let shared = self.shared.clone();
        let listener_running = Arc::new(AtomicBool::new(true));
        let acc_running = Arc::clone(&listener_running);
        let wrap = Arc::new(wrap);
        let (exited_tx, exited) = mpsc::channel::<()>();
        let turns = Arc::new(AtomicU64::new(0));
        let acc_turns = Arc::clone(&turns);
        let acceptor = thread::Builder::new().spawn(move || {
            // Dropped as this thread returns: shutdown's signal that it left.
            let _exited = exited_tx;
            // Connections currently INSIDE their wrap (the sealed handshake): the
            // pre-authentication population. Counted on the connection thread
            // around the wrap itself — so an instant wrap (plaintext) holds a slot
            // for no measurable time — and pre-checked here so a peer beyond the
            // cap is dropped without being given a thread.
            let preauth = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            accept_loop(
                &shared,
                &acc_running,
                &acc_turns,
                false,
                || listener.accept().map(|(s, _)| s),
                |s| {
                    let _ = s.set_nodelay(true);
                    if preauth.load(Ordering::Acquire) >= MAX_PREAUTH_CONNS {
                        return; // dropped: beyond the pre-authentication cap
                    }
                    // The pre-authentication deadline: armed BEFORE the wrap;
                    // the connection thread lifts it once the wrap succeeded.
                    let _ = s.set_read_timeout(Some(PREAUTH_TIMEOUT));
                    let sh = shared.clone();
                    let wrap = Arc::clone(&wrap);
                    let preauth = Arc::clone(&preauth);
                    // Detached per-connection thread; the wrap (the sealed
                    // handshake) runs HERE so a stalling peer cannot hold up
                    // other accepts. Ends when its socket ends. A spawn that fails
                    // (thread exhaustion) drops this socket and keeps accepting —
                    // `thread::spawn` would panic the acceptor.
                    let _ = thread::Builder::new().spawn(move || {
                        // Take a slot; the hard bound (the pre-check above races
                        // a burst). Beyond the cap the socket drops here.
                        if preauth.fetch_add(1, Ordering::AcqRel) >= MAX_PREAUTH_CONNS {
                            preauth.fetch_sub(1, Ordering::AcqRel);
                            return;
                        }
                        let wrapped = wrap(s);
                        preauth.fetch_sub(1, Ordering::AcqRel);
                        // Couldn't establish the (sealed) transport — the socket
                        // drops with the Err.
                        if let Ok((stream, peer_id)) = wrapped {
                            let _ = stream.set_read_timeout(None);
                            let _ = handle_conn_with_peer(&sh, stream, Wire::Tcp, peer_id);
                        }
                    });
                },
            );
        })?;
        self.shared.active_listeners.fetch_add(1, Ordering::AcqRel);
        Ok(BrokerHandle {
            shared: self.shared.clone(),
            listener_running,
            acceptor: Some(acceptor),
            wake: AcceptWake::Blocks {
                exited: Mutex::new(exited),
                turns,
            },
            endpoint: Endpoint::Tcp {
                dial,
                shown: dial.to_string(),
            },
        })
    }
}

/// [`Broker::stats`]: the log's snapshot under its lock, and the counters beside it.
fn stats_of(shared: &Shared) -> BrokerStats {
    let log = shared
        .log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .stats();
    let visible_head = shared.head.load(Ordering::Acquire);
    let active_conns = shared
        .conns
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .len();
    let active_listeners = shared.active_listeners.load(Ordering::Acquire);
    BrokerStats {
        log,
        visible_head,
        active_conns,
        active_listeners,
        accepted: shared.accepted.load(Ordering::Relaxed),
        acceptor_wakeups: shared.acceptor_wakeups.load(Ordering::Relaxed),
        acceptor_sleeps: shared.acceptor_sleeps.load(Ordering::Relaxed),
        acceptors_abandoned: shared.acceptors_abandoned.load(Ordering::Relaxed),
        writer_round_trips: shared.writer_round_trips.load(Ordering::Relaxed),
        writer_batches: shared.writer_batches.load(Ordering::Relaxed),
        writer_wakeups: shared.writer_wakeups.load(Ordering::Relaxed),
        empty_batches: shared.empty_batches.load(Ordering::Relaxed),
        binds_settled: shared.binds_settled.load(Ordering::Relaxed),
        wills_settled: shared.wills_settled.load(Ordering::Relaxed),
        tail_broadcasts: shared.tail_broadcasts.load(Ordering::Relaxed),
        tail_wakeups: shared.tail_wakeups.load(Ordering::Relaxed),
        peer_probes: shared.peer_probes.load(Ordering::Relaxed),
        parked_tails: shared.parked_tails.load(Ordering::Relaxed),
        retention_waits: shared.retention_waits.load(Ordering::Relaxed),
    }
}

/// `n` as a count in the health snapshot (a `usize` never exceeds a `u64` here; the
/// saturation is for the type, not a case).
fn count64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

/// [`Broker::health`]: [`stats_of`] (the log lock), then the heads and the identity from
/// the copies beside the log, and the poison, the tier served (with the latch the copy
/// may not carry yet) and the flush counts as the `Hello` answer reads them
/// ([`hello_answer`]) — read last, so a poison the snapshot's other facts were taken
/// under is in it, and so is the latch that came with it.
fn health_of(shared: &Shared) -> crate::health::BrokerHealth {
    let stats = stats_of(shared);
    let told = shared
        .hello
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let HelloCopy { identity, ext } = hello_answer(told, &shared.vitals);
    let served = match ext.durability {
        Some(s) => s,
        None => shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .served(),
    };
    let (poisoned, flushes) = match ext.health {
        Some(h) => (h.poisoned, h.flushes),
        None => (None, shared.vitals.flush_counts()),
    };
    crate::health::BrokerHealth {
        pid: std::process::id(),
        written_at_ms: now_unix_ms(),
        identity,
        served,
        base: stats.log.base.0,
        head: stats.log.head.0,
        visible_head: stats.visible_head,
        durable_head: shared.durable.load(Ordering::Acquire),
        retained_records: count64(stats.log.retained_records),
        retained_bytes: count64(stats.log.retained_bytes),
        distinct_subjects: count64(stats.log.distinct_subjects),
        bound_producers: count64(stats.log.bound_producers),
        pending_wills: count64(stats.log.pending_wills),
        group_commits: count64(stats.log.group_commits),
        replica: stats.log.is_replica,
        active_conns: count64(stats.active_conns),
        parked_tails: count64(stats.parked_tails),
        flushes,
        poisoned,
    }
}

/// [`Broker::write_health`]. `running` is read under the health mutex, which
/// [`stop_health`] takes after clearing it: a write either lands before the stop lets
/// the file go, or is refused — never a held file for a log that has closed.
///
/// Says, beside the write's outcome, whether the snapshot it took NAMES THE POISON: what
/// the period's thread waits on next ([`health_loop`]).
fn write_health_of(shared: &Shared) -> (bool, io::Result<()>) {
    let snapshot = health_of(shared);
    let named = snapshot.poisoned.is_some();
    let mut slot = shared
        .health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !shared.running.load(Ordering::Acquire) {
        return (
            named,
            Err(io::Error::other(
                "the broker has stopped serving its log; it publishes no health",
            )),
        );
    }
    let file = match slot.as_mut() {
        Some(f) => f,
        None => slot.insert(crate::health::HealthFile::new(&shared.log_path)),
    };
    (named, file.write(&snapshot))
}

/// [`Broker::keep_health`]'s thread: sleep a period — or, while the last snapshot it
/// tried to write did not name the poison, until the log is poisoned — then write;
/// until the broker stops. `named` is what [`keep_health`](Broker::keep_health)'s own
/// first write said. A failed write is said on stderr once per distinct error, then
/// tried again next period.
///
/// WHY THE SNAPSHOT, and not a look at the poison before each wait. A poison that lands
/// after a write's snapshot is taken but before that look — while the file is created,
/// locked, written and renamed, which is slow on exactly the failing disk that poisons a
/// log — is in no file, yet the look finds it set, and its wake was spent while nobody
/// waited: the thread would sleep a whole period while the sidecar said `poisoned=no`
/// of a log that refuses every write. Waiting on what the file says closes that: a
/// snapshot that did not name the poison is followed at once by one that does. And one
/// that named it but could not be written waits the period, as any failed write does,
/// rather than spinning on a disk that fails.
fn health_loop(shared: &Shared, every: Duration, mut named: bool) {
    let mut said: Option<String> = None;
    loop {
        let deadline = Instant::now().checked_add(every);
        shared.vitals.wait(deadline, &shared.running, !named);
        if !shared.running.load(Ordering::Acquire) {
            return;
        }
        let (now_named, res) = write_health_of(shared);
        named = now_named;
        match res {
            Ok(()) => said = None,
            Err(e) => {
                let msg = e.to_string();
                if said.as_deref() != Some(msg.as_str()) {
                    eprintln!(
                        "astream-broker: {}: could not write its health sidecar ({msg}); \
                         trying again every {} ms",
                        shared.log_path.display(),
                        every.as_millis()
                    );
                    said = Some(msg);
                }
            }
        }
    }
}

/// THE LOG'S STOP, once the writer is joined (`running` is already cleared and the
/// waiters woken): close the log cleanly — a full flush, then `Closed` in its sidecar —
/// so a reboot after this stop keeps the identity at any tier; then the health
/// sidecar's last word ([`stop_health`]); then the log's file lock, released last. The
/// release latches the log shut (`BrokerLog::unlock`), whether the clean close ran,
/// failed or was skipped under the unclean-stop seam: from then on a successor may serve
/// the file, and nothing this broker still holds — the `Broker` value, a retention call
/// on it — may write to it.
///
/// The close comes BEFORE the last word, which is what makes that word the log's final
/// state: the clean close moves the durable head to the head, and a snapshot taken
/// before it told a stopped Relaxed or Ordered broker's records as never durable when
/// the close had flushed them all. The durable head the close reached is published to
/// the copy the snapshot reads ([`Shared::durable`]); a close that failed leaves it
/// where it was, so the last word says what is not known durable. The log's guard is
/// let go for the snapshot, which takes it ([`stats_of`]): the writer is joined, so
/// nothing appends in between.
fn stop_log(shared: &Shared) {
    {
        let mut log = shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Under the unclean-stop seam (`Broker::arm_unclean_stop`) the log is left as a
        // killed process leaves it: no full flush, `<log>.id` as the run last wrote it.
        // The unlock below still latches it shut, as a kill leaves no writer behind.
        if !shared.unclean_stop.load(Ordering::Acquire) {
            log.close_clean_at_shutdown();
        }
        shared
            .durable
            .store(log.durable_head().0, Ordering::Release);
    }
    stop_health(shared);
    shared
        .log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .unlock();
}

/// Stop keeping `<log>.health`: join the period's thread, write the last snapshot — the
/// writer is joined and the log closed ([`stop_log`]), so it is the log's final state —
/// and let the file go. Called BEFORE the log's own lock is released, so no reader is
/// told a broker serves a log it has closed; a reader in between reads the writer
/// `Gone` a moment early, the safe direction.
fn stop_health(shared: &Shared) {
    let thread = shared
        .health_thread
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(t) = thread {
        let _ = t.join();
    }
    let snapshot = health_of(shared);
    let mut slot = shared
        .health
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(file) = slot.as_mut() {
        if let Err(e) = file.write(&snapshot) {
            eprintln!(
                "astream-broker: {}: could not write its last health snapshot ({e}); the \
                 one before it stays",
                shared.log_path.display()
            );
        }
        file.release();
    }
    *slot = None;
}

impl Drop for Broker {
    fn drop(&mut self) {
        // When `serve*` has been called, the active `BrokerHandle`(s) own the writer
        // lifecycle and shut it down when the last listener stops. If `serve*` was
        // never called (`active_listeners == 0`), this `Broker` IS the lifecycle: stop
        // the writer and release the log's file lock.
        if self.shared.active_listeners.load(Ordering::Acquire) == 0 {
            stop_writer(&self.shared);
        }
    }
}

/// Stop the shared writer thread and release the log: the last step of the last
/// listener's [`BrokerHandle::shutdown`], and all of dropping a [`Broker`] never
/// served. Clears `running`, queues the writer's STOP behind every op already queued,
/// and joins the writer once it has committed them and whatever else was queued when
/// it got there ([`writer_loop`]) — so its final batch is done. Nothing more is
/// appended then, so the log is closed cleanly before its lock is released to a
/// successor ([`stop_log`]), and a reboot after this stop keeps the identity at any
/// tier — unless [`Broker::arm_unclean_stop`] armed the seam that skips the clean
/// close. The join waits on that final batch and nothing
/// else: the writer sleeps on its queue, and the stop wakes it at once. A no-op once
/// the writer was taken.
fn stop_writer(shared: &Shared) {
    let Some(w) = shared
        .writer
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return;
    };
    shared.running.store(false, Ordering::Relaxed);
    // A `wait_poisoned` and the health thread sleep on the vitals: wake them to read
    // `running`.
    shared.vitals.wake();
    // The writer holds the queue's receiver until it returns, so this lands; it fails
    // only when the writer is gone already (it panicked), and the join answers at once.
    let _ = shared.writes.send(Queued::Stop);
    let _ = w.join();
    // The writer is joined, so nothing more is appended: close the log cleanly, then
    // the health sidecar's last word, then the log's lock.
    stop_log(shared);
}

/// The broker's per-connection [`Stream`] behaviours for the sealed transport:
/// timeouts and shutdown act on the underlying socket (they never touch the
/// record bytes), and `try_clone` shares the AEAD record counters so the
/// read half and the ack-writer half stay one ordered record sequence.
#[cfg(feature = "aead")]
impl ConnShutdown for astream_aead::SealedStream<TcpStream> {
    fn shutdown_both(&self) {
        let _ = self.get_ref().shutdown(Shutdown::Both);
    }
    fn shutdown_write(&self) {
        let _ = self.get_ref().shutdown(Shutdown::Write);
    }
}

#[cfg(feature = "aead")]
impl Stream for astream_aead::SealedStream<TcpStream> {
    fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.get_ref().set_read_timeout(d)
    }
    fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.get_ref().set_write_timeout(d)
    }
    fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        self.get_ref().set_nonblocking(on)
    }
    fn try_clone(&self) -> io::Result<Self> {
        astream_aead::SealedStream::try_clone(self)
    }
}

/// Owns a running listener acceptor; [`shutdown`](BrokerHandle::shutdown) stops this
/// listener and (for a Unix socket) unlinks it. When the LAST active `BrokerHandle`
/// on a [`Broker`] shuts down, it also stops the shared writer thread, closes all live
/// connections, and releases the log's file lock. Dropping the handle shuts down too.
pub struct BrokerHandle {
    shared: Arc<Shared>,
    listener_running: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
    wake: AcceptWake,
    endpoint: Endpoint,
}

impl BrokerHandle {
    /// The bound TCP address (useful when serving on port 0), or `None` for a Unix
    /// socket.
    pub fn tcp_addr(&self) -> Option<&str> {
        match &self.endpoint {
            Endpoint::Tcp { shown, .. } => Some(shown),
            #[cfg(unix)]
            Endpoint::Unix { .. } => None,
        }
    }

    /// Whether this listener's acceptor blocks in `accept()` — a new connection is
    /// taken the instant it arrives and an idle listener never wakes. Always `true` for
    /// TCP. `false` for a Unix socket that could not get its `<socket>.wake` name — too
    /// long for a socket address, a hard link the filesystem refuses, or a foreign file
    /// or another broker's live socket already there: that acceptor polls (every 20 ms),
    /// and a connection waits out up to one poll.
    pub fn blocks_in_accept(&self) -> bool {
        matches!(self.wake, AcceptWake::Blocks { .. })
    }

    /// Dial this listener once to wake a blocking acceptor: through `<socket>.wake`,
    /// else the socket path, each only while it still names the listener (a name
    /// someone replaced would wake a stranger); for TCP, the bound address. `None` when
    /// no dial connected. A dial that connects may still have reached another listener
    /// (a loopback-specific bind beside a wildcard one; a recorded inode that was
    /// never ours): [`Self::wake_acceptor`] trusts only this acceptor's own answer.
    fn dial_listener(&self) -> Option<WakeDial> {
        match &self.endpoint {
            Endpoint::Tcp { dial, .. } => {
                match TcpStream::connect_timeout(dial, ACCEPT_WAKE_RETRY) {
                    Ok(s) => Some(WakeDial::Tcp(s)),
                    Err(_) => None,
                }
            }
            #[cfg(unix)]
            Endpoint::Unix { path, node, wake } => {
                let node = (*node)?;
                if let Some(wake_path) = wake {
                    if node_at(wake_path) == Some(node) {
                        if let Ok(s) = UnixStream::connect(wake_path) {
                            return Some(WakeDial::Unix(s));
                        }
                    }
                }
                if node_at(path) == Some(node) {
                    if let Ok(s) = UnixStream::connect(path) {
                        return Some(WakeDial::Unix(s));
                    }
                }
                None
            }
        }
    }

    /// Get this listener's acceptor out of `accept()` once the flags are cleared:
    /// `true` when it has left (join it), `false` when it is ABANDONED.
    ///
    /// A polling acceptor leaves by itself within [`ACCEPT_POLL`]. A blocking one is
    /// dialed, and its exit signal awaited for [`ACCEPT_WAKE_RETRY`], again and again
    /// until the signal fires: one dial is enough unless the acceptor read a stale flag
    /// or was busy, and a repeat costs one more queued connection the closing listener
    /// discards. [`ACCEPT_WAKE_ATTEMPTS`] rounds in a row that THIS acceptor never
    /// answers — its own `accept()` did not return, whether or not the dial connected
    /// somewhere — give up: no name reaches it, or another listener answers in its
    /// place, and waiting would turn shutdown's bound into a hang (see
    /// [`BrokerStats::acceptors_abandoned`]). A busy acceptor answers as soon as it is
    /// back in `accept()`, since the dial is queued on its listener.
    fn wake_acceptor(&self) -> bool {
        let AcceptWake::Blocks { exited, turns } = &self.wake else {
            return true;
        };
        let exited = exited
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut unanswered: u32 = 0;
        loop {
            if !matches!(exited.try_recv(), Err(mpsc::TryRecvError::Empty)) {
                return true;
            }
            let before = turns.load(Ordering::Relaxed);
            let dial = self.dial_listener();
            if !matches!(
                exited.recv_timeout(ACCEPT_WAKE_RETRY),
                Err(mpsc::RecvTimeoutError::Timeout)
            ) {
                return true;
            }
            drop(dial);
            if turns.load(Ordering::Relaxed) != before {
                unanswered = 0;
            } else {
                unanswered = unanswered.saturating_add(1);
                if unanswered >= ACCEPT_WAKE_ATTEMPTS {
                    return false;
                }
            }
        }
    }

    /// Force-close every registered connection socket, so a thread parked in a
    /// blocking read_frame (a quiet producer) or a wedged socket write returns at
    /// once and is reaped — along with its ack-writer thread and cloned fd.
    fn force_close_conns(shared: &Shared) {
        for (_, c) in shared
            .conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain()
        {
            c.shutdown_both();
        }
    }

    /// Stop this handle's acceptor and clean up its endpoint. When this is the last
    /// active listener on the [`Broker`], also wake parked subscribers, force-close
    /// every live connection (a connection thread has no idle timeout unless its client
    /// keeps alive, [`KEEPALIVE_DEADLINE`] — this is what reaps the rest), join the
    /// shared writer thread, and release the log's file lock. Follower shutdown
    /// handles include sockets authenticating a capability. Name resolution, dialing
    /// each address and platform calls are not a hard wall-clock bound.
    pub fn shutdown(&mut self) {
        let Some(j) = self.acceptor.take() else {
            return;
        };
        self.listener_running.store(false, Ordering::Relaxed);
        let prev = self.shared.active_listeners.fetch_sub(1, Ordering::AcqRel);
        let is_last = prev <= 1;
        if is_last {
            self.shared.running.store(false, Ordering::Relaxed);
            // Wake parked subscriber threads so they observe shutdown and exit.
            self.shared.tail.notify_all();
            // And a `wait_poisoned` and the health thread, asleep on the vitals.
            self.shared.vitals.wake();
            // Unblock the writer if it is parked on a follower that never answers.
            if let Some(rep) = &self.shared.replicator {
                rep.shutdown_links();
            }
            Self::force_close_conns(&self.shared);
        }
        // A blocking acceptor is dialed out of `accept()` (a polling one observes the
        // flags within ACCEPT_POLL), then joined. One no dial reaches any more — no name
        // left, or another listener answering in its place — is left parked rather
        // than waited on forever: it holds only its listener and an `Arc` of the shared
        // state, and if a connection ever reaches it, it sees the cleared flags and
        // returns.
        let reached = self.wake_acceptor();
        if reached {
            let _ = j.join();
        } else {
            self.shared
                .acceptors_abandoned
                .fetch_add(1, Ordering::Relaxed);
            drop(j);
        }
        if is_last {
            // A connection the acceptor handed off just before exiting registers itself on
            // its own thread — possibly after the drain above. Drain again now that no more
            // can arrive; handle_conn also leaves on its own if it sees `running` false
            // right after registering, so between the two no connection is missed.
            Self::force_close_conns(&self.shared);
            // Stop the writer behind whatever is queued — its final batch done, never an
            // idle poll waited out — then close the log cleanly before its lock is
            // released to a successor.
            stop_writer(&self.shared);
        }
        // Unlink the socket path and the wake name — each only while it still names
        // THIS listener: the embedder may have unlinked the path, and another broker
        // may since have bound its own socket there. An ABANDONED acceptor leaves the
        // socket path alone: no dial through it reached this acceptor, so a path that
        // still carries `node` names some other broker's socket — the winner of a
        // `serve` that raced this one past its bind, whose inode this one recorded.
        // The wake name is this broker's own (it made the link) whatever it names.
        #[cfg(unix)]
        if let Endpoint::Unix { path, node, wake } = &self.endpoint {
            match node {
                Some(node) => {
                    if reached {
                        unlink_if_listener(path, *node);
                    }
                    if let Some(w) = wake {
                        unlink_if_listener(w, *node);
                    }
                }
                None => {
                    let _ = crate::store::remove_file_path(path);
                }
            }
        }
    }
}

impl Drop for BrokerHandle {
    fn drop(&mut self) {
        if self.acceptor.is_some() {
            self.shutdown();
        }
    }
}

/// Which listener accepted a connection: what decides whether a [`Request::Keepalive`]
/// holds it to [`KEEPALIVE_DEADLINE`] (TCP, sealed or not) or to nothing (a Unix
/// socket, whose peer cannot vanish without its kernel closing it).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wire {
    #[cfg_attr(not(unix), allow(dead_code))]
    Unix,
    Tcp,
}

/// Serve one connection, then FIRE its registered will (if any) on ANY return — a
/// clean EOF, an I/O error (a keepalive deadline passing among them), a shutdown
/// force-close, or a panic-free early exit alike — provided the capability that
/// authorized the will has not lapsed and the peer's identity has not been revoked.
/// cfg(unix): its one caller is the Unix-socket acceptor; a TCP connection, the only
/// kind Windows has, goes straight to `handle_conn_with_peer`.
#[cfg(unix)]
fn handle_conn<S: Stream>(shared: &Arc<Shared>, stream: S, wire: Wire) -> io::Result<()> {
    handle_conn_with_peer(shared, stream, wire, None)
}

fn handle_conn_with_peer<S: Stream>(
    shared: &Arc<Shared>,
    stream: S,
    wire: Wire,
    peer_id: Option<[u8; 32]>,
) -> io::Result<()> {
    let conn_id = shared.conn_seq.fetch_add(1, Ordering::Relaxed);
    let mut will: Vec<(WillRecord, u64)> = Vec::new();
    let mut had_will = false;
    let res = serve_conn(
        shared,
        stream,
        wire,
        conn_id,
        &mut will,
        &mut had_will,
        peer_id,
    );
    if let Some((will, until)) = will.pop() {
        if lapsed(until).is_none()
            && !is_peer_revoked(shared, peer_id)
            && !is_will_principal_revoked(shared, will.producer_id)
        {
            fire_will_for_conn(shared, Some(conn_id), will);
        } else {
            drop_will_for_conn(shared, conn_id);
        }
    } else if had_will {
        drop_will_for_conn(shared, conn_id);
    }
    res
}

/// Enqueue a durable mutation on the writer queue and WAIT for its ack — the shape a
/// request answered in the connection loop needs (an `Attach` that binds a producer id,
/// a `Will` that must be persisted before it is acknowledged), as opposed to the
/// pipelined path, whose acks a separate thread streams back.
///
/// The wait ends on the OP'S OWN FATE, never on a flag. An op queued just as the
/// writer thread exits must not park this thread for good — but `running == false` is
/// not that fact: `writer_loop` does not discard what is already queued, it drains and
/// commits it, so a wait that ended on the flag could answer "the will was not
/// persisted" for a `/a/will` record the log went on to hold (and which the next open
/// would then fire, for a producer that believes it registered nothing). The honest
/// signal is the writer DROPPING the op: when `writer_loop` returns it drops the
/// receiver, every queued `WriteOp` with it, and every ack `Sender` those held —
/// which is exactly `Disconnected` here.
/// The words a request that WAITED on the writer ([`submit_blocking`]) answers a refusal
/// of the writer's with: `<what>: <the writer's words>` — except a POISONED log's
/// refusal, which keeps its words FIRST (`<the writer's words> — <what>`), because a
/// producer tells that refusal by how it begins ([`LOG_POISONED_PREFIX`],
/// [`BrokerRefusal::is_poisoned`](crate::BrokerRefusal::is_poisoned)): an attach's
/// binding or a will on a poisoned log is as worthless to resend on this connection as a
/// publish is, and the producer must be able to read that off either.
fn waited_refusal(code: u8, msg: &str, what: &str) -> String {
    if code == ERR_REFUSED && msg.starts_with(LOG_POISONED_PREFIX) {
        format!("{msg} — {what}")
    } else {
        format!("{what}: {msg}")
    }
}

fn submit_blocking(shared: &Arc<Shared>, kind: WriteKind) -> AckResult {
    shared.writer_round_trips.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    if shared
        .writes
        .send(Queued::Op(WriteOp { kind, ack: tx }))
        .is_err()
    {
        return Err(refusal(ERR_REFUSED, "broker is stopping"));
    }
    match rx.recv() {
        Ok(res) => res,
        // The writer is gone and took this op with it, unacked and uncommitted.
        Err(mpsc::RecvError) => Err(refusal(ERR_REFUSED, "broker is stopping")),
    }
}

/// Whether `producer_id` is DURABLY bound to exactly `principal` at an offset below
/// the subscriber-visible head — the case in which the writer's own `Bind` would find
/// the binding, append nothing and ack it — read from `Shared::bindings` without the
/// log lock.
///
/// Below the visible head, because that is the rule the writer's ack of a re-bind
/// applies: in the Replicated tier a binding held locally but not yet by a quorum is
/// refused "not replicated to quorum", and this answers it no differently by falling
/// through to the writer. In Strict and Relaxed every copied binding is already below
/// the head (the writer publishes the head before it copies), so the check never
/// declines one there. The head is read AFTER the copy, and the copy's mutex orders it:
/// the writer stores the head before it locks the copy to add an entry, so a reader
/// that saw the entry sees that head or a later one.
#[cfg(feature = "cap")]
fn binding_settled(shared: &Shared, producer_id: u64, principal: &str) -> bool {
    let at = match shared
        .bindings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&producer_id)
    {
        Some((held, at)) if held == principal => *at,
        _ => return false,
    };
    crate::proto::offset_is_committed(shared.head.load(Ordering::Relaxed), at)
}

/// Copy into `Shared::bindings` whatever bindings the log has promoted that the copy
/// lacks. Called by the writer with the log lock held, after the batch is committed
/// and its head published. The log's table only grows and never changes an entry, so
/// equal sizes mean equal tables and the common batch (no new binding) costs one
/// comparison; a batch that added one visits the table once.
fn copy_new_bindings(shared: &Shared, log: &BrokerLog) {
    let mut copy = shared
        .bindings
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if copy.len() == log.durable_binding_count() {
        return;
    }
    for (id, principal, off) in log.durable_bindings() {
        if !copy.contains_key(&id) {
            copy.insert(id, (principal.to_string(), off.0));
        }
    }
}

/// Enqueue one will as a fenced publish, discarding its ack. Fire-and-forget on
/// purpose: waiting could park a connection thread on a writer that a concurrent
/// shutdown has already stopped, and the `/a/will` record is the durable record of
/// intent either way.
fn fire_will(shared: &Arc<Shared>, will: WillRecord) {
    fire_will_for_conn(shared, None, will);
}

fn fire_will_for_conn(shared: &Arc<Shared>, conn_id: Option<u64>, will: WillRecord) {
    let (tx, _rx) = mpsc::channel();
    let _ = shared.writes.send(Queued::Op(WriteOp {
        kind: WriteKind::WillFire { conn_id, will },
        ack: tx,
    }));
}

fn drop_will_for_conn(shared: &Arc<Shared>, conn_id: u64) {
    let (tx, _rx) = mpsc::channel();
    let _ = shared.writes.send(Queued::Op(WriteOp {
        kind: WriteKind::WillDrop { conn_id },
        ack: tx,
    }));
}

/// Let a client refused at the connection cap — or for speaking another protocol —
/// READ its refusal. Closing at once loses it: a client whose first request lands after
/// the close gets a broken pipe (Unix) and one whose request is still unread when the
/// socket closes gets a reset (TCP).
/// So the refused socket shuts only its write half (the client reads the error, then
/// EOF) and stays open for up to [`REFUSAL_LINGER`], reading and discarding input
/// until the client closes; at most [`MAX_LINGERING_REFUSALS`] do so at once, and past
/// that a refusal closes immediately.
fn linger_refused<S: Stream>(shared: &Shared, stream: &mut S) {
    if shared.refusing.fetch_add(1, Ordering::AcqRel) >= MAX_LINGERING_REFUSALS {
        shared.refusing.fetch_sub(1, Ordering::AcqRel);
        return;
    }
    // The refusal is all the client gets: a client still reading sees EOF right after
    // it, instead of waiting out the linger.
    stream.shutdown_write();
    let deadline = std::time::Instant::now() + REFUSAL_LINGER;
    let mut sink = [0u8; 4096];
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match stream.read(&mut sink) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    shared.refusing.fetch_sub(1, Ordering::AcqRel);
}

/// A connection's stream read against an absolute `deadline`: each read is bounded by
/// the time left, so a peer that sends frame after frame, or one frame a byte at a
/// time, cannot stretch the wait past it (a plain socket timeout re-arms on every
/// read). Used for the pre-authentication phase ([`FIRST_FRAME_TIMEOUT`]).
struct Deadlined<'a, S: Stream> {
    stream: &'a mut S,
    deadline: Instant,
}

impl<S: Stream> Read for Deadlined<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pre-authentication deadline passed",
            ));
        }
        self.stream.set_read_timeout(Some(left))?;
        self.stream.read(buf)
    }
}

impl<S: Stream> Write for Deadlined<'_, S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pre-authentication deadline passed",
            ));
        }
        self.stream.set_write_timeout(Some(left))?;
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "pre-authentication deadline passed",
            ));
        }
        self.stream.set_write_timeout(Some(left))?;
        self.stream.flush()
    }
}

fn serve_conn<S: Stream>(
    shared: &Arc<Shared>,
    mut stream: S,
    wire: Wire,
    conn_id: u64,
    will: &mut Vec<(WillRecord, u64)>,
    had_will: &mut bool,
    peer_id: Option<[u8; 32]>,
) -> io::Result<()> {
    // Register a shutdown handle so BrokerHandle::shutdown can force-close this socket
    // (both directions) and unblock a thread parked in read_frame / a socket write —
    // the connection thread (and any ack-writer it spawned) is then reaped promptly
    // instead of lingering until the client happens to send or disconnect. The
    // registry also enforces the connection cap.
    let max_conns = shared.max_conns.load(Ordering::Relaxed);
    {
        let mut conns = shared
            .conns
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if conns.len() >= max_conns {
            drop(conns);
            let _ = stream.set_write_timeout(Some(ACK_WRITE_TIMEOUT));
            let _ = write_frame(
                &mut stream,
                &encode_response(&Response::Error {
                    code: ERR_TOO_MANY_CONNS,
                    msg: format!("too many connections (limit {max_conns})"),
                }),
            );
            linger_refused(shared, &mut stream);
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                "connection limit reached",
            ));
        }
        // No shutdown handle would mean no cap accounting and no force-close — under
        // fd exhaustion (exactly what the cap exists for) `try_clone` is what fails
        // first. Refuse rather than serve a connection the broker cannot bound or reap.
        conns.insert(conn_id, Box::new(stream.try_clone()?));
    }
    let _conn_guard = ConnGuard {
        shared,
        id: conn_id,
    };
    // Registered; if the broker began stopping meanwhile (this socket may have been
    // accepted after shutdown's first registry drain), leave now rather than park.
    if !shared.running.load(Ordering::Relaxed) {
        return Ok(());
    }
    // Nothing is known about the peer until its first frame — on a guarded broker,
    // until it has a capability accepted: bound that wait (see FIRST_FRAME_TIMEOUT).
    // `preauth` is that phase; `preauth_deadline` the instant a guarded connection
    // must end it by (`None` when the configured timeout is too far off to represent
    // as an instant, which leaves the socket timeout as the bound).
    let preauth_timeout =
        Duration::from_millis(shared.first_frame_timeout_ms.load(Ordering::Relaxed));
    let _ = stream.set_read_timeout(Some(preauth_timeout));
    let preauth_deadline = Instant::now().checked_add(preauth_timeout);
    let guarded = shared.cap_secret.is_some();
    let mut preauth = true;
    // And bound every RESPONSE write on this connection, for the whole of its life.
    // A peer that stops reading — SIGSTOPped, partitioned so its receive window shuts
    // and never reopens — otherwise parks this thread inside `write_all` forever, with
    // the fd and the `MAX_CONNS` slot still charged to it. It is set here, not only by
    // the pipelined ack-writer, because a connection that only ever READS (`Hello`,
    // `Attach`, `Last`, `Fetch`) never starts a `Pipeline`. Both halves take THIS
    // value: `SO_SNDTIMEO` is a property of the socket, not of the descriptor, so
    // whichever half sets it last sets it for both. A streaming verb raises it to
    // STREAM_WRITE_TIMEOUT when it takes the socket over — `tail_loop` for
    // subscriptions, the `ForkSubscribe` arm for a snapshot delivered on this thread.
    let _ = stream.set_write_timeout(Some(Duration::from_millis(
        shared.write_timeout_ms.load(Ordering::Relaxed),
    )));
    // Write ops (publish / commit / process_and_produce / replicate) and their
    // acks/errors flow through a lazily-started Pipeline: a second thread streams acks
    // back IN ORDER on a cloned write half while this thread keeps reading requests, so
    // ONE connection can have many writes in flight (they coalesce into the same
    // group-commit batch). A streaming verb (subscribe / fork) first FINISHes the
    // pipeline — flushing every pending ack and reclaiming the socket — before it
    // takes over for delivery.
    let mut pipe: Option<Pipeline> = None;
    // This connection's capability KEYRING: `Attach` APPENDS to it (a repeated grant
    // string replaces), and every request is authorized existentially over it, so one
    // connection can hold several narrow grants — read one subtree, commit under
    // another, publish as a bound principal in a third — instead of one wide one.
    #[cfg(feature = "cap")]
    let mut keyring: Vec<astream_cap::Capability> = Vec::new();
    // The nonce this connection's `Attach` proofs are computed over, once `Hello` has
    // handed one out. Kept on the DEFAULT build too, because `Hello` is answered by an
    // unguarded broker as well — one client works against either.
    let mut nonce: Option<[u8; 32]> = None;
    // Legacy clients never ask: their recv skips marks, which would reset the
    // socket timeout an old take/drain uses to stop on idle.
    let mut idle_marks = false;
    loop {
        // Guarded, the deadline is ABSOLUTE; unguarded, the socket timeout set above
        // bounds each read of the first frame.
        let read = match preauth_deadline {
            Some(deadline) if preauth && guarded => read_frame(&mut Deadlined {
                stream: &mut stream,
                deadline,
            }),
            _ => read_frame(&mut stream),
        };
        let payload = match read {
            Ok(Some(p)) => p,
            Ok(None) => break, // clean EOF: client closed
            Err(e) => {
                // A guarded connection out of time before it attached is told why
                // (best effort: it is closed either way).
                if preauth
                    && guarded
                    && matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    )
                {
                    let _ = write_frame(
                        &mut stream,
                        &encode_response(&Response::Error {
                            code: ERR_UNAUTHORIZED,
                            msg: format!(
                                "unauthorized: no capability attached within {} ms of connecting",
                                preauth_timeout.as_millis()
                            ),
                        }),
                    );
                }
                finish_pipe(&mut pipe)?;
                return Err(e);
            }
        };
        // Unguarded, the first frame ends the pre-authentication phase: there is
        // nothing to authenticate. Guarded, only an accepted `Attach` ends it.
        if preauth && !guarded {
            preauth = false;
            let _ = stream.set_read_timeout(None);
        }
        let req = match decode_request(&payload) {
            Some(r) => r,
            // ANOTHER PROTOCOL: say so in the one layout every version reads, stamped
            // with the peer's own version byte (`encode_protocol_refusal`), and close.
            // A "malformed request" here would be stamped with OURS, which the peer's
            // decoder refuses at its first byte — a broker that answered and was read
            // as a garbled stream. Nothing more the peer sends is decodable, so the
            // connection ends; the linger lets the refusal be read rather than reset
            // away by frames the peer pipelined behind it.
            None if matches!(payload.first(), Some(&v) if v != PROTO_VERSION) => {
                finish_pipe(&mut pipe)?;
                let ver = payload.first().copied().unwrap_or(0);
                let _ = write_frame(&mut stream, &encode_protocol_refusal(ver));
                linger_refused(shared, &mut stream);
                return Ok(());
            }
            None => {
                if !emit(
                    &mut stream,
                    &mut pipe,
                    Err(refusal(ERR_REFUSED, "malformed request")),
                )? {
                    break;
                }
                continue;
            }
        };
        // Bounded names, before anything (the capability gate included) parses one.
        if let Some(err) = oversized_name(&req) {
            if !emit(
                &mut stream,
                &mut pipe,
                Err(refusal(ERR_RECORD_REFUSED, err)),
            )? {
                break;
            }
            continue;
        }
        if is_peer_revoked(shared, peer_id) {
            will.clear();
            let _ = emit(
                &mut stream,
                &mut pipe,
                Err(refusal(
                    ERR_UNAUTHORIZED,
                    "unauthorized: peer identity key has been revoked",
                )),
            );
            break;
        }
        // `Hello` opens the capability handshake on EVERY build: it hands out this
        // connection's nonce and grants nothing, so an unguarded broker answers it too
        // and one client works against either kind. Answered in the request loop after
        // finish_pipe, like every other non-streaming verb.
        if matches!(req, Request::Hello) {
            finish_pipe(&mut pipe)?;
            let n = *nonce.insert(fresh_nonce());
            // The log's identity rides the answer every client already asks for, so a
            // client that kept offsets can check them before it uses one. Read now,
            // not at open: a replica adopts its leader's. Beside it, the extension
            // block: the tier this broker serves (read from the broker itself, never
            // inferred from how it was started) and where the log's generations agree,
            // so a reader that kept offsets under an earlier generation resumes
            // instead of forgetting. Both from the copy the writer keeps beside the log
            // (`Shared::hello`), never under the log lock: the first reply of every
            // connection must not wait out a writer's fsync. And the log's health —
            // poisoned or not, and its flushes — read from the vitals as it is now,
            // lock-free too (the block's tag 0x04), with the latch the copy may not
            // carry yet (`hello_answer`).
            let told = shared
                .hello
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            let HelloCopy { identity, ext } = hello_answer(told, &shared.vitals);
            let log = Some(identity);
            write_frame(
                &mut stream,
                &encode_response(&Response::Nonce {
                    nonce: n.to_vec(),
                    log,
                    ext,
                }),
            )?;
            continue;
        }
        // Capability gate. `Attach` adds to the connection's keyring; every other
        // request is authorized against that ring when the broker is guarded. Off
        // entirely without the `cap` feature (no secret can be set), so the default
        // broker is unchanged. `until` is when the request's authority lapses: a
        // streaming verb, one request for the life of its stream, ends there.
        #[cfg(not(feature = "cap"))]
        let until = u64::MAX;
        #[cfg(feature = "cap")]
        let until = {
            if let Request::Attach { grant, proof } = &req {
                finish_pipe(&mut pipe)?;
                let res = match shared.cap_secret.as_deref() {
                    // Unguarded: acknowledge and enforce nothing (the ring is unused).
                    None => Ok(()),
                    Some(secret) => attach_grant(
                        shared,
                        secret,
                        &mut keyring,
                        nonce.as_ref(),
                        peer_id,
                        grant,
                        proof,
                    ),
                };
                let resp = match res {
                    Ok(()) => {
                        // Authenticated: the pre-authentication deadline is lifted.
                        if preauth {
                            preauth = false;
                            let _ = stream.set_read_timeout(None);
                        }
                        mark_at_head(shared)
                    }
                    Err((code, msg)) => {
                        let _ = writeln!(
                            io::stderr(),
                            "astream-broker: warn: refused Attach for grant {grant:?}: {msg}"
                        );
                        Response::Error { code, msg }
                    }
                };
                write_frame(&mut stream, &encode_response(&resp))?;
                continue;
            }
            match shared.cap_secret.as_deref() {
                None => u64::MAX,
                // THE INSTANT IS TAKEN PER REQUEST, not once per connection: a
                // capability that expires while a connection is open stops
                // authorizing its next request, and a stream it admitted ends at
                // the deadline (`until`). Any capability whose bound principal was
                // revoked since `Attach` is pruned before authorization.
                Some(secret) => {
                    prune_revoked_capabilities(shared, &mut keyring);
                    if will
                        .last()
                        .is_some_and(|(w, _)| is_will_principal_revoked(shared, w.producer_id))
                    {
                        will.clear();
                    }
                    let now = now_unix_ms();
                    match cap_authorized_until(secret, &keyring, &req, now) {
                        Some(until) => until,
                        None => {
                            let why = ungranted(secret, &keyring, &req, now);
                            if !emit(&mut stream, &mut pipe, Err(refusal(ERR_UNAUTHORIZED, why)))? {
                                break;
                            }
                            continue;
                        }
                    }
                }
            }
        };
        #[cfg(not(feature = "cap"))]
        {
            // Unguarded (no `cap` feature at all): accept an Attach and acknowledge it,
            // so a client's attach round trip completes against either broker.
            if matches!(req, Request::Attach { .. }) {
                finish_pipe(&mut pipe)?;
                write_frame(&mut stream, &encode_response(&mark_at_head(shared)))?;
                continue;
            }
        }
        if !dispatch_request(
            shared,
            &mut stream,
            wire,
            conn_id,
            &mut pipe,
            will,
            had_will,
            &mut idle_marks,
            until,
            peer_id,
            #[cfg(feature = "cap")]
            &mut keyring,
            req,
        )? {
            break;
        }
    }
    finish_pipe(&mut pipe)?;
    Ok(())
}

#[cfg(feature = "cap")]
fn prune_revoked_capabilities(shared: &Shared, keyring: &mut Vec<astream_cap::Capability>) {
    maybe_reload_revocation_file(shared);
    let revoked = shared
        .revoked_principals
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    prune_capability_keyring(&revoked, keyring);
}

#[cfg(feature = "cap")]
fn prune_capability_keyring(
    revoked: &RevocationSet<String>,
    keyring: &mut Vec<astream_cap::Capability>,
) {
    if !revoked.is_empty() {
        let mut kept = Vec::new();
        for c in std::mem::take(keyring) {
            let is_revoked = match astream_cap::Grant::parse(&c.filter) {
                Ok(astream_cap::Grant {
                    principal: Some(p), ..
                }) => revoked.contains(&p.to_ascii_lowercase()),
                _ => false,
            };
            if !is_revoked {
                kept.push(c);
            }
        }
        *keyring = kept;
    }
}

#[allow(clippy::too_many_arguments)]
fn dispatch_request<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    wire: Wire,
    conn_id: u64,
    pipe: &mut Option<Pipeline>,
    will: &mut Vec<(WillRecord, u64)>,
    had_will: &mut bool,
    idle_marks: &mut bool,
    until: u64,
    peer_id: Option<[u8; 32]>,
    #[cfg(feature = "cap")] keyring: &mut Vec<astream_cap::Capability>,
    req: Request,
) -> io::Result<bool> {
    match req {
        Request::Publish {
            producer_id,
            producer_seq,
            subject,
            body,
        } => serve_publish(
            shared,
            stream,
            pipe,
            producer_id,
            producer_seq,
            subject,
            body,
        ),
        Request::Commit { group, upto } => {
            emit_op(shared, stream, pipe, WriteKind::Commit { group, upto })
        }
        Request::ProcessAndProduce {
            producer_id,
            producer_seq,
            out_subject,
            out_body,
            group,
            upto,
        } => {
            if let Some((code, err)) = publish_subject_error(&out_subject) {
                return emit(stream, pipe, Err((code, format!("output {err}"))));
            }
            emit_op(
                shared,
                stream,
                pipe,
                WriteKind::ProcessAndProduce {
                    producer_id,
                    producer_seq,
                    out_subject,
                    out_body,
                    group,
                    upto,
                },
            )
        }
        Request::Replicate {
            seq,
            producer_id,
            producer_seq,
            subject,
            body,
            commit,
            leader,
        } => serve_replicate(
            shared,
            stream,
            pipe,
            seq,
            producer_id,
            producer_seq,
            subject,
            body,
            commit,
            leader,
        ),
        Request::Subscribe {
            from_offset,
            filter,
        } => {
            finish_pipe(pipe)?; // flush pending acks, reclaim the socket
            let filt = match Filter::new(filter.as_str()) {
                Ok(f) => f,
                Err(_) => {
                    write_frame(
                        stream,
                        &encode_response(&Response::Error {
                            code: ERR_BAD_FILTER,
                            msg: format!("bad filter {filter:?}"),
                        }),
                    )?;
                    return Ok(false);
                }
            };
            #[cfg(feature = "cap")]
            let cap_check = StreamCapCheck::new(
                shared,
                keyring,
                Request::Subscribe {
                    from_offset,
                    filter,
                },
            );
            #[cfg(not(feature = "cap"))]
            let cap_check = StreamCapCheck::new(shared);
            tail_loop(
                shared,
                stream,
                from_offset,
                filt,
                until,
                *idle_marks,
                peer_id,
                cap_check,
            )?;
            Ok(false)
        }
        Request::ForkSubscribe {
            fork_at,
            replacement_subject,
            replacement_body,
            filter,
        } => {
            finish_pipe(pipe)?;
            #[cfg(feature = "cap")]
            let cap_check = StreamCapCheck::new(
                shared,
                keyring,
                Request::ForkSubscribe {
                    fork_at,
                    replacement_subject: replacement_subject.clone(),
                    replacement_body: Vec::new(),
                    filter: filter.clone(),
                },
            );
            #[cfg(not(feature = "cap"))]
            let cap_check = StreamCapCheck::new(shared);
            serve_fork_subscribe(
                shared,
                stream,
                fork_at,
                replacement_subject,
                replacement_body,
                filter,
                until,
                peer_id,
                cap_check,
            )?;
            Ok(false)
        }
        Request::SubscribeGroup { group, filter } => {
            finish_pipe(pipe)?;
            let filt = match Filter::new(filter.as_str()) {
                Ok(f) => f,
                Err(_) => {
                    write_frame(
                        stream,
                        &encode_response(&Response::Error {
                            code: ERR_BAD_FILTER,
                            msg: format!("bad filter {filter:?}"),
                        }),
                    )?;
                    return Ok(false);
                }
            };
            // Resume from the group's committed offset (broker-enforced): never from a
            // commit a held batch carries, which is neither visible nor durable yet
            // (`BrokerLog::group_start`) — a second reader of a person's inbox must not
            // skip mail on the strength of a read mark that may still be refused or lost.
            let start = {
                let log = shared
                    .log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                log.group_start(&group)
            };
            #[cfg(feature = "cap")]
            let cap_check = StreamCapCheck::new(
                shared,
                keyring,
                Request::SubscribeGroup { group, filter },
            );
            #[cfg(not(feature = "cap"))]
            let cap_check = StreamCapCheck::new(shared);
            tail_loop(
                shared,
                stream,
                start.0,
                filt,
                until,
                *idle_marks,
                peer_id,
                cap_check,
            )?;
            Ok(false)
        }
        Request::Last { filter, after, max } => {
            serve_last(shared, stream, pipe, filter, after, max)
        }
        Request::Will {
            producer_id,
            producer_seq,
            subject,
            body,
        } => {
            serve_will(
                shared,
                stream,
                conn_id,
                pipe,
                will,
                had_will,
                producer_id,
                producer_seq,
                subject,
                body,
                until,
            )?;
            Ok(true)
        }
        Request::Keepalive => {
            // Answered in the request loop, like `Will`, after the pending acks.
            // From here on every read on a TCP connection is bounded by the
            // keepalive deadline — SO_RCVTIMEO is per read, so each frame that
            // arrives re-arms it — and a read that runs it out returns from this
            // function like any I/O error: the will fires and the slot is freed
            // (KEEPALIVE_DEADLINE). Nothing is written first: a peer silent past
            // its own promise is presumed gone, and a write could only park.
            finish_pipe(pipe)?;
            if wire == Wire::Tcp {
                let deadline =
                    Duration::from_millis(shared.keepalive_deadline_ms.load(Ordering::Relaxed));
                let _ = stream.set_read_timeout(Some(deadline));
            }
            write_frame(stream, &encode_response(&mark_at_head(shared)))?;
            Ok(true)
        }
        Request::IdleMarks => {
            finish_pipe(pipe)?;
            *idle_marks = true;
            write_frame(stream, &encode_response(&mark_at_head(shared)))?;
            Ok(true)
        }
        Request::Budget { producer_id } => {
            // Answered in the request loop after the pending acks, so a count read
            // right after this connection's own publishes includes them. Read under
            // the log lock, where the writer promotes a whole batch in one hold (its
            // write and flush run with the lock released, but nothing is promoted
            // then), so the count is never one a half-promoted batch is still moving.
            finish_pipe(pipe)?;
            let (subjects, reserved) = {
                let log = shared
                    .log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    log.subjects_per_producer(producer_id),
                    log.will_reserved_subjects(producer_id),
                )
            };
            write_frame(
                stream,
                &encode_response(&Response::Budget {
                    subjects: subjects as u64,
                    reserved: reserved as u64,
                    max: MAX_SUBJECTS_PER_PRODUCER as u64,
                }),
            )?;
            Ok(true)
        }
        Request::Fetch {
            from_offset,
            filter,
            max,
        } => serve_fetch(shared, stream, pipe, from_offset, filter, max),
        Request::Retire {
            producer_id,
            subject,
            offset,
            log,
        } => serve_retire(shared, stream, pipe, producer_id, subject, offset, log),
        // Both are fully handled by the capability gate above, which `continue`s.
        Request::Attach { .. } | Request::Hello => Ok(true),
    }
}

fn serve_retire<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    producer_id: u64,
    subject: String,
    offset: u64,
    log: crate::LogIdentity,
) -> io::Result<bool> {
    let retire_err = if is_hidden_subject(&subject) || subject.starts_with("/a/") || subject == "/a"
    {
        Some(refusal(ERR_RECORD_REFUSED, reserved_msg(&subject)))
    } else if Subject::new(&subject).is_err() {
        Some(refusal(ERR_BAD_SUBJECT, format!("bad subject {subject:?}")))
    } else {
        None
    };
    if let Some(err) = retire_err {
        return emit(stream, pipe, Err(err));
    }
    emit_op(
        shared,
        stream,
        pipe,
        WriteKind::RetireConditional {
            producer_id,
            subject,
            offset,
            log,
        },
    )
}

fn serve_publish<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    producer_id: u64,
    producer_seq: u64,
    subject: String,
    body: Vec<u8>,
) -> io::Result<bool> {
    if subject == RETIRE_SUBJECT {
        let target_subject = match std::str::from_utf8(&body) {
            Ok(s) => s.to_owned(),
            Err(_) => {
                return emit(
                    stream,
                    pipe,
                    Err(refusal(
                        ERR_RECORD_REFUSED,
                        "retire body must be a valid UTF-8 subject",
                    )),
                );
            }
        };
        if target_subject.len() > MAX_NAME_LEN {
            return emit(
                stream,
                pipe,
                Err(refusal(
                    ERR_RECORD_REFUSED,
                    format!(
                        "subject too long: {} bytes (limit {MAX_NAME_LEN})",
                        target_subject.len()
                    ),
                )),
            );
        }
        let retire_err = if is_hidden_subject(&target_subject)
            || target_subject.starts_with("/a/")
            || target_subject == "/a"
        {
            Some(refusal(ERR_RECORD_REFUSED, reserved_msg(&target_subject)))
        } else if Subject::new(&target_subject).is_err() {
            Some(refusal(
                ERR_BAD_SUBJECT,
                format!("bad subject {target_subject:?}"),
            ))
        } else {
            None
        };
        if let Some(err) = retire_err {
            return emit(stream, pipe, Err(err));
        }
        return emit_op(
            shared,
            stream,
            pipe,
            WriteKind::Retire {
                producer_id,
                target_subject,
            },
        );
    }
    if let Some(err) = publish_subject_error(&subject) {
        return emit(stream, pipe, Err(err));
    }
    emit_op(
        shared,
        stream,
        pipe,
        WriteKind::Publish {
            producer_id,
            producer_seq,
            subject,
            body,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn serve_replicate<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    seq: u64,
    producer_id: u64,
    producer_seq: u64,
    subject: String,
    body: Vec<u8>,
    commit: Option<(String, u64)>,
    leader: Option<crate::LogIdentity>,
) -> io::Result<bool> {
    let is_retire_marker =
        commit.as_ref().map(|(g, _)| g.as_str()) == Some(crate::proto::REPLICATE_RETIRE_GROUP);
    let pure_commit = !is_retire_marker
        && producer_id == 0
        && producer_seq == 0
        && body.is_empty()
        && commit.is_some();
    let rec = BrokerRecord {
        seq: Offset(seq),
        producer_id,
        producer_seq,
        subject: subject.clone(),
        body,
        commit: if is_retire_marker {
            None
        } else {
            match commit {
                Some((group, upto)) => Some(GroupCommit { group, upto }),
                None => None,
            }
        },
    };
    let err = if Subject::new(subject.as_str()).is_err() {
        Some(refusal(ERR_BAD_SUBJECT, format!("bad subject {subject:?}")))
    } else if subject == COMMIT_SUBJECT {
        if !pure_commit {
            Some(refusal(
                ERR_RECORD_REFUSED,
                format!("reserved subject {COMMIT_SUBJECT}"),
            ))
        } else {
            None
        }
    } else if crate::store::is_hidden_record(&rec)
        && !(shared.is_replica.load(Ordering::Acquire)
            || shared.log_head.load(Ordering::Acquire) == 0)
    {
        Some(refusal(
            ERR_RECORD_REFUSED,
            replicated_reserved_msg(&subject),
        ))
    } else {
        None
    };
    if let Some(err) = err {
        return emit(stream, pipe, Err(err));
    }
    emit_op(shared, stream, pipe, WriteKind::Replicate { rec, leader })
}

#[allow(clippy::too_many_arguments)]
fn serve_fork_subscribe<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    fork_at: u64,
    replacement_subject: String,
    replacement_body: Vec<u8>,
    filter: String,
    mut until: u64,
    peer_id: Option<[u8; 32]>,
    mut cap_check: StreamCapCheck,
) -> io::Result<()> {
    if let Some((code, err)) = publish_subject_error(&replacement_subject) {
        write_frame(
            stream,
            &encode_response(&Response::Error {
                code,
                msg: format!("replacement {err}"),
            }),
        )?;
        return Ok(());
    }
    let filt = match Filter::new(filter.as_str()) {
        Ok(f) => f,
        Err(_) => {
            write_frame(
                stream,
                &encode_response(&Response::Error {
                    code: ERR_BAD_FILTER,
                    msg: format!("bad filter {filter:?}"),
                }),
            )?;
            return Ok(());
        }
    };
    let replacement = BrokerRecord {
        seq: Offset(fork_at),
        producer_id: 0,
        producer_seq: 0,
        subject: replacement_subject,
        body: replacement_body,
        commit: None,
    };
    let _ = stream.set_write_timeout(Some(stream_write_timeout(shared)));
    let mut pinned: Option<u64> = None;
    let mut cursor = 0u64;
    let mut out = Vec::new();
    if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
        return end_stream_failure(stream, &mut out, failure);
    }
    let visible = loop {
        let (chunk, visible) = {
            let log = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let visible = match pinned {
                Some(v) => v,
                None => {
                    let v = shared.head.load(Ordering::Relaxed);
                    pinned = Some(v);
                    v
                }
            };
            (next_chunk(&log, &mut cursor, visible), visible)
        };
        if chunk.is_empty() {
            break visible;
        }
        for rec in chunk {
            if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
                return end_stream_failure(stream, &mut out, failure);
            }
            // Below the pinned head, so never the last offset there is.
            cursor = rec.seq.0.saturating_add(1);
            let rec: &BrokerRecord = if rec.seq.0 == fork_at {
                &replacement
            } else {
                &rec
            };
            if crate::store::is_hidden_record(rec) {
                continue;
            }
            if filt.matches_str(&rec.subject) {
                queue_delivery(stream, &mut out, rec)?;
            }
        }
    };
    flush_egress(stream, &mut out)?;
    if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
        return end_stream_failure(stream, &mut out, failure);
    }
    write_frame(
        stream,
        &encode_response(&mark(
            visible,
            visible,
            durable_for(shared, visible),
            String::new(),
        )),
    )?;
    Ok(())
}

fn serve_last<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    filter: String,
    after: String,
    max: u32,
) -> io::Result<bool> {
    finish_pipe(pipe)?;
    let filt = match Filter::new(filter.as_str()) {
        Ok(f) => f,
        Err(_) => {
            return emit(
                stream,
                pipe,
                Err(refusal(ERR_BAD_FILTER, format!("bad filter {filter:?}"))),
            );
        }
    };
    let want = max.min(LAST_PAGE_MAX) as usize;
    let (page, resume, head) = match last_page(shared, &filt, &after, want) {
        Ok(v) => v,
        Err(msg) => {
            return emit(stream, pipe, Err(refusal(ERR_REFUSED, msg)));
        }
    };
    let mut out = Vec::new();
    for rec in page {
        queue_delivery_cached(shared, stream, &mut out, &rec)?;
    }
    let resume_str = match resume {
        Some(r) => r,
        None => String::new(),
    };
    append_frame(
        &mut out,
        &encode_response(&mark(head, head, durable_for(shared, head), resume_str)),
    )?;
    flush_egress(stream, &mut out)?;
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn serve_will<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    conn_id: u64,
    pipe: &mut Option<Pipeline>,
    will: &mut Vec<(WillRecord, u64)>,
    had_will: &mut bool,
    producer_id: u64,
    producer_seq: u64,
    subject: String,
    body: Vec<u8>,
    until: u64,
) -> io::Result<()> {
    finish_pipe(pipe)?;
    if let Some((code, msg)) = publish_subject_error(&subject) {
        write_frame(stream, &encode_response(&Response::Error { code, msg }))?;
        return Ok(());
    }
    if let Some((held, _)) = will.last() {
        if held.producer_id != producer_id {
            write_frame(
                stream,
                &encode_response(&Response::Error {
                    code: ERR_RECORD_REFUSED,
                    msg: format!(
                        "this connection already holds a will for producer {}: a second \
                         Will replaces the first, and replacement is durable only under \
                         the SAME producer id (use another connection for producer {})",
                        held.producer_id, producer_id
                    ),
                }),
            )?;
            return Ok(());
        }
    }
    let candidate = WillRecord {
        producer_id,
        producer_seq,
        subject,
        body,
    };
    if let Err((code, msg)) = submit_blocking(
        shared,
        WriteKind::WillRegister {
            conn_id: Some(conn_id),
            will: candidate.clone(),
        },
    ) {
        write_frame(
            stream,
            &encode_response(&Response::Error {
                code,
                msg: waited_refusal(code, &msg, "will was not persisted"),
            }),
        )?;
        return Ok(());
    }
    *had_will = true;
    will.clear();
    will.push((candidate, until));
    write_frame(stream, &encode_response(&mark_at_head(shared)))
}

fn serve_fetch<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    from_offset: u64,
    filter: String,
    max: u32,
) -> io::Result<bool> {
    finish_pipe(pipe)?;
    let filt = match Filter::new(filter.as_str()) {
        Ok(f) => f,
        Err(_) => {
            return emit(
                stream,
                pipe,
                Err(refusal(ERR_BAD_FILTER, format!("bad filter {filter:?}"))),
            );
        }
    };
    let (page, next, head, durable) = {
        let log = shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let head = shared.head.load(Ordering::Relaxed);
        let (page, next) = log.fetch(from_offset, &filt, max as usize, FETCH_SCAN_MAX, head);
        (page, next, head, durable_for(shared, head))
    };
    let mut out = Vec::new();
    for rec in page {
        queue_delivery_cached(shared, stream, &mut out, &rec)?;
    }
    append_frame(
        &mut out,
        &encode_response(&mark(next, head, durable, String::new())),
    )?;
    flush_egress(stream, &mut out)?;
    Ok(true)
}

/// Why a client-chosen subject cannot be appended to: not a valid `Subject`, or the
/// broker-internal commit subject (a record there would bypass exactly-once dedup
/// and never be delivered — consumer-group commits go through `Commit` /
/// `ProcessAndProduce`). Either is permanent.
fn publish_subject_error(subject: &str) -> Option<Refusal> {
    if Subject::new(subject).is_err() {
        Some(refusal(ERR_BAD_SUBJECT, format!("bad subject {subject:?}")))
    } else if is_hidden_subject(subject) {
        Some(refusal(ERR_RECORD_REFUSED, reserved_msg(subject)))
    } else {
        None
    }
}

/// Add one capability to a guarded connection's keyring, or say why not.
///
/// The §8.2 attach rules, in order: an `Attach` with no preceding `Hello` is refused
/// (there is no nonce to bind the proof to); the proof must verify as
/// `HMAC-SHA256(HMAC(secret, grant), nonce ‖ grant)`, which proves possession of the
/// tag without the tag ever crossing the wire and refuses a proof captured from
/// another connection; a grant that names a principal it may PUBLISH as binds that
/// principal's derived producer id in the durable binding table, and is refused if
/// that id is already bound to a DIFFERENT principal; and the ring is bounded, a
/// repeated grant string replacing rather than growing it.
///
/// Binding is the writer's job only the FIRST time: a principal the table already
/// binds to that id, below the visible head, is answered from the broker's copy of
/// the table (`Shared::bindings`) with no writer round trip and no log lock. The copy
/// can be trusted because a promoted binding is never undone while the log is open,
/// and each open seeds its own copy; see the field.
///
/// The ring's bound is decided BEFORE the binding is appended, so an attach that will
/// be refused writes nothing: the durable mutation is the last thing that happens.
#[cfg(feature = "cap")]
fn attach_grant(
    shared: &Arc<Shared>,
    secret: &[u8],
    keyring: &mut Vec<astream_cap::Capability>,
    nonce: Option<&[u8; 32]>,
    peer_id: Option<[u8; 32]>,
    grant: &str,
    proof: &[u8],
) -> Result<(), Refusal> {
    let Some(nonce) = nonce else {
        return Err(refusal(
            ERR_UNAUTHORIZED,
            "unauthorized: Attach without a preceding Hello (the proof of possession is \
             computed over this connection's nonce)",
        ));
    };
    if !astream_cap::verify_attach(secret, grant, nonce, proof) {
        return Err(refusal(
            ERR_UNAUTHORIZED,
            "unauthorized: capability proof does not verify",
        ));
    }
    // EXPIRY IS ITS OWN REFUSAL, after possession is proved and before anything
    // is bound. A separate message because the two failures need separate
    // remedies: "does not verify" means the wrong secret or a tampered string,
    // "expired" means a correct capability whose time is up and which a mint can
    // reissue. Reporting both as one would send an operator to rotate a secret
    // that is fine. The authorization path re-checks this per request; this
    // check is here so an expired attach fails at the door rather than binding a
    // producer id and then authorizing nothing.
    let now_ms = now_unix_ms();
    if astream_cap::is_expired(grant, now_ms) {
        let exp = astream_cap::expires_at(grant).unwrap_or(0);
        return Err(refusal(
            ERR_UNAUTHORIZED,
            format!("{CAP_EXPIRED_PREFIX} at {exp} (now {now_ms}, unix ms)"),
        ));
    }
    // Genuine under this broker's secret, so `parse` cannot fail (verify_attach parses
    // first) — but fail closed rather than unwrap on a library change.
    let parsed = match astream_cap::Grant::parse(grant) {
        Ok(p) => p,
        Err(e) => return Err(refusal(ERR_UNAUTHORIZED, format!("unauthorized: {e}"))),
    };
    if let Some(principal) = &parsed.principal {
        maybe_reload_revocation_file(shared);
        if shared
            .revoked_principals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&principal.to_ascii_lowercase())
        {
            return Err(refusal(
                ERR_UNAUTHORIZED,
                format!("unauthorized: principal {principal:?} has been revoked"),
            ));
        }
    }
    // When running over the authenticated `identity` transport AND the grant names an
    // identity-shaped principal (`n-<16 hex chars>`), require that principal to match
    // the peer's authenticated Ed25519 public key (`identity_principal(&pk)`), closing
    // the principal-impersonation gap if a capability string is leaked between nodes
    // (`REQUIREMENTS` R6). Non-identity role principals (e.g. `agent-7`, `pump`) remain
    // usable over any transport so multi-role capabilities on a single node continue
    // to work.
    if let (Some(pk), Some(principal)) = (peer_id, &parsed.principal) {
        if is_identity_shaped_principal(principal) {
            let expected = identity_principal(&pk);
            if !principal.eq_ignore_ascii_case(&expected) {
                return Err(refusal(
                    ERR_UNAUTHORIZED,
                    format!(
                        "unauthorized: grant principal {principal:?} does not match authenticated \
                         peer identity {expected:?}"
                    ),
                ));
            }
        }
    }
    // DECIDE THE RING FIRST, MUTATE THE LOG SECOND. Minting and the ring's capacity are
    // pure, local checks; the producer-id binding below is a DURABLE append, so an
    // attach refused for a full ring must be refused before it, leaving the log
    // untouched.
    let cap = match astream_cap::mint(secret, grant) {
        Ok(c) => c,
        Err(e) => return Err(refusal(ERR_UNAUTHORIZED, format!("unauthorized: {e}"))),
    };
    let mut already_present = false;
    for c in keyring.iter() {
        if c.filter == cap.filter {
            already_present = true;
            break;
        }
    }
    if !already_present && keyring.len() >= MAX_KEYRING {
        return Err(refusal(
            ERR_UNAUTHORIZED,
            format!("unauthorized: capability keyring is full ({MAX_KEYRING} grants)"),
        ));
    }
    match (&parsed.mode, &parsed.principal) {
        // A grant that can PUBLISH under a named principal binds that principal's
        // derived producer id, durably, before it authorizes anything.
        (astream_cap::Mode::ReadWrite, Some(principal)) => {
            let producer_id = astream_cap::producer_id_of(principal);
            // ALREADY BOUND: the log's answer is known and final (see
            // `Shared::bindings`), so the writer is not asked again. This is the common
            // case by far — aterm's ring names one principal in four grants, on every
            // connection of every attach — and asking cost an empty batch queued behind
            // the writer's fsync each time. Anything else (unbound, bound to another
            // principal, not yet visible) is the writer's to decide below, unchanged.
            if binding_settled(shared, producer_id, principal) {
                shared.binds_settled.fetch_add(1, Ordering::Relaxed);
            } else if let Err((code, msg)) = submit_blocking(
                shared,
                WriteKind::Bind {
                    producer_id,
                    principal: principal.clone(),
                },
            ) {
                // The writer's code stands: a collision is the authority failing; an
                // I/O error appending the binding is not, and is not known to be
                // permanent.
                return Err(match code {
                    ERR_UNAUTHORIZED => refusal(ERR_UNAUTHORIZED, format!("unauthorized: {msg}")),
                    code => refusal(
                        code,
                        waited_refusal(code, &msg, "the producer binding was not persisted"),
                    ),
                });
            }
        }
        // An UNBOUND read-write grant may publish under any producer id — the god cap
        // the fleet root keeps. It is legal and it is a footgun, so say so once, on
        // stderr, where an operator can see which grant it was.
        (astream_cap::Mode::ReadWrite, None) => {
            eprintln!(
                "astream-broker: warn: attached an UNBOUND read-write grant {grant:?} — it may \
                 publish under ANY producer id (no principal binding); mint it as \
                 \"rw,p=<principal>:<filter>\" unless it is deliberately the fleet root"
            );
        }
        // A read-only grant publishes nothing, so it binds nothing: binding it would
        // let a read-only holder append a /a/bind record.
        (astream_cap::Mode::ReadOnly, _) => {}
    }
    let mut kept = Vec::new();
    for c in std::mem::take(keyring) {
        if c.filter != cap.filter {
            kept.push(c);
        }
    }
    kept.push(cap);
    *keyring = kept;
    Ok(())
}

/// The words of the refusal for a request [`cap_authorized_until`] did not authorize
/// at `now_ms`. When the ring WOULD have authorized it before any deadline, the
/// grants that covered it have lapsed, and the refusal says so with
/// [`CAP_EXPIRED_PREFIX`]: the holder needs a fresh mint of the same grant, not a
/// wider one. Otherwise no grant ever covered it, and the refusal says that.
///
/// "Before any deadline" is the instant 0. It is earlier than every `exp=` a ring can
/// hold, because `attach_grant` refuses an `exp=0` grant whatever the clock says
/// (`0 >= 0` is expired). This decides only the WORDS. The request was already
/// refused at `now_ms`, and nothing here can authorize it.
#[cfg(feature = "cap")]
fn ungranted(
    secret: &[u8],
    keyring: &[astream_cap::Capability],
    req: &Request,
    now_ms: u64,
) -> String {
    if cap_authorized_until(secret, keyring, req, 0).is_some() {
        format!(
            "{CAP_EXPIRED_PREFIX} before this request: every grant that covered it has \
             lapsed (now {now_ms}, unix ms)"
        )
    } else {
        "unauthorized: capability does not grant this subject/filter".to_string()
    }
}

/// Whether `req` is authorized by this connection's capability KEYRING under the
/// broker's `secret` — the §8.2 matrix, EXISTENTIAL over the ring (one grant may
/// authorize the read half and another the write half of the same request).
///
/// A write (`Publish`, the output half of a `ProcessAndProduce`) needs a READ-WRITE
/// grant whose filter matches the subject AND — when that grant names a principal —
/// whose derived producer id is exactly the one the request carries: that binding is
/// what closes dedup-key poisoning, where a co-permitted publisher pre-takes a peer's
/// `(producer_id, producer_seq)` so the peer's genuine record silently dedups away.
/// Every request that advances a consumer group needs a read-write grant on the group
/// AS A SUBJECT. A read (`Subscribe`, `SubscribeGroup`'s filter, `ForkSubscribe`,
/// `Last`, `Fetch`) needs any grant, of either mode, that CONTAINS the filter.
///
/// `Replicate` is not an ordinary write and is not authorized like one: a follower
/// link is not a peer. It carries the ORIGINAL producer's id, never the link's own, so
/// no BOUND grant could ever cover it — and a link grant must therefore be minted
/// UNBOUND (a bare filter, or `rw:<filter>`), which is the god cap, named as such and
/// chosen once by the operator who stands the cluster up. So the verb takes a grant
/// that is genuine, read-write, filter-matching AND unbound, and a bound `rw` grant
/// cannot reach it at all.
///
/// Authorizing it by SUBJECT alone would hand every bound `rw` grant a write under
/// ANY producer id: dedup is keyed `(producer_id, producer_seq)` with no subject in
/// the key, so a holder permitted nowhere near a peer's subtree could burn that peer's
/// dedup keys from inside its own, and the peer's genuine publish would silently dedup
/// away at the attacker's offset, acked as landed.
/// An empty ring authorizes nothing.
///
/// The answer is `None` when `req` is not authorized at `now_ms`, else the unix-ms
/// instant its authority LAPSES. That instant matters to a streaming verb or `Will` —
/// whose authority must still hold when the stream delivers or the connection drops —
/// so those verbs compute it: each half of the request rests on whichever authorizing
/// grant lasts longest, and the request lapses with the first half to go. Any other
/// verb stops at the first grant that authorizes it and answers `u64::MAX` ("authorized
/// now"), which is also the answer for grants without an expiry.
#[cfg(feature = "cap")]
fn cap_authorized_until(
    secret: &[u8],
    keyring: &[astream_cap::Capability],
    req: &Request,
    now_ms: u64,
) -> Option<u64> {
    #[derive(Clone, Copy)]
    enum CapPred<'a> {
        // A write as a named producer: read-write, filter matches, producer id derived.
        Publish(&'a str, u64),
        // A group advance: read-write on the group name as a subject.
        Commit(&'a str),
        // A read: ANY grant (either mode) whose filter contains the requested one.
        Read(&'a str),
        // A REPLICATED write: read-write and filter-matching, like any write — and
        // UNBOUND, because the record carries a producer id this connection's principal
        // does not derive and never could.
        Replicate(&'a str),
        // Any genuine, unexpired grant on the connection.
        AnyValid,
        // A genuine, unexpired read-write grant that publishes as `producer_id`.
        Budget(u64),
    }

    let streaming = matches!(
        req,
        Request::Subscribe { .. }
            | Request::SubscribeGroup { .. }
            | Request::ForkSubscribe { .. }
            | Request::Will { .. }
    );
    let (first, second): (CapPred<'_>, Option<CapPred<'_>>) = match req {
        // A `Will` is a publish the broker will make LATER on this connection's
        // behalf, so it is authorized now, exactly as a publish is — including the
        // producer binding. A publish to `RETIRE_SUBJECT` (`/a/retire`) retires the
        // target subject carried in `body`, so it is authorized against that exact
        // target `Subject` under `producer_id` (or `CapPred::Budget(*producer_id)` when
        // `body` is not a valid `Subject`, so an authenticated producer gets
        // `ERR_BAD_SUBJECT` / `ERR_RECORD_REFUSED` from `serve_publish` rather than
        // `ERR_UNAUTHORIZED`).
        Request::Publish {
            subject,
            body,
            producer_id,
            ..
        } if subject == RETIRE_SUBJECT => match std::str::from_utf8(body) {
            Ok(target)
                if Subject::new(target).is_ok()
                    && !is_hidden_subject(target)
                    && !target.starts_with("/a/")
                    && target != "/a" =>
            {
                (CapPred::Publish(target, *producer_id), None)
            }
            _ => (CapPred::Budget(*producer_id), None),
        },
        Request::Retire {
            subject,
            producer_id,
            ..
        } => match Subject::new(subject) {
            Ok(_)
                if !is_hidden_subject(subject)
                    && !subject.starts_with("/a/")
                    && subject != "/a" =>
            {
                (CapPred::Publish(subject, *producer_id), None)
            }
            _ => (CapPred::Budget(*producer_id), None),
        },
        Request::Publish {
            subject,
            producer_id,
            ..
        }
        | Request::Will {
            subject,
            producer_id,
            ..
        } => (CapPred::Publish(subject, *producer_id), None),
        // The commit HALF of a read-process-write also advances a consumer group,
        // so authorize BOTH the output subject AND the group (as a subject the ring
        // grants) — not the output subject alone.
        Request::ProcessAndProduce {
            out_subject,
            producer_id,
            group,
            ..
        } => (
            CapPred::Publish(out_subject, *producer_id),
            Some(CapPred::Commit(group)),
        ),
        // A replicated record needs a LINK grant on its subject — read-write,
        // filter-matching and unbound — plus, if it carries a commit, an advance of
        // that group. A bound `rw` grant does not reach this verb.
        // The codec's marked retirement uses this exact sentinel instead of a group
        // commit. It still needs the unbound replication grant on /a/retire, but
        // advances no consumer group whose name would need a second grant.
        Request::Replicate {
            subject,
            producer_seq,
            commit: Some((group, 0)),
            ..
        } if subject == RETIRE_SUBJECT
            && *producer_seq == crate::brecord::RETIRE_PRODUCER_SEQ
            && group == crate::proto::REPLICATE_RETIRE_GROUP =>
        {
            (CapPred::Replicate(subject), None)
        }
        Request::Replicate {
            subject, commit: c, ..
        } => match c {
            None => (CapPred::Replicate(subject), None),
            Some((g, _)) => (CapPred::Replicate(subject), Some(CapPred::Commit(g))),
        },
        Request::Subscribe { filter, .. }
        | Request::ForkSubscribe { filter, .. }
        | Request::Last { filter, .. }
        | Request::Fetch { filter, .. } => (CapPred::Read(filter), None),
        // A group subscription is authorized for BOTH what it reads (filter) and the
        // durable group it commits under (group-as-subject).
        Request::SubscribeGroup { group, filter } => {
            (CapPred::Read(filter), Some(CapPred::Commit(group)))
        }
        // Commit durably advances a consumer group. It MUST verify the capability and
        // be scoped to the group (the group is a subject the ring must grant), or any
        // connection could force another tenant's committed offset forward.
        Request::Commit { group, .. } => (CapPred::Commit(group), None),
        // Both only ever ADD, and both are fully handled before this gate.
        Request::Attach { .. } | Request::Hello => return Some(u64::MAX),
        // These grant nothing, but their `Mark` names the log's head, which an
        // unattached connection is told nothing about: any capability the ring still
        // holds — genuine and unexpired — answers for it.
        Request::Keepalive | Request::IdleMarks => (CapPred::AnyValid, None),
        // A producer's budget is answered to whoever could spend it: a genuine,
        // unexpired read-write grant that publishes as that producer — bound to it,
        // or unbound (the god grant publishes as any). It names no subject, so no
        // filter is matched; a read-only grant, or one bound to another principal,
        // learns nothing about this producer.
        Request::Budget { producer_id } => (CapPred::Budget(*producer_id), None),
    };

    let mut min_until = u64::MAX;
    for pred in [Some(first), second] {
        let Some(pred) = pred else {
            continue;
        };
        let mut best: Option<u64> = None;
        for c in keyring {
            let matched = match pred {
                CapPred::Publish(subject, producer_id) => {
                    astream_cap::grants_publish(secret, c, subject, producer_id, now_ms)
                }
                CapPred::Commit(group) => astream_cap::grants_commit(secret, c, group, now_ms),
                CapPred::Read(filter) => astream_cap::grants_filter(secret, c, filter, now_ms),
                CapPred::Replicate(subject) => {
                    astream_cap::grants(secret, c, subject, now_ms)
                        && matches!(
                            astream_cap::Grant::parse(c.filter.as_str()),
                            Ok(g) if g.principal.is_none()
                        )
                }
                CapPred::AnyValid => {
                    astream_cap::verify(secret, c)
                        && !astream_cap::is_expired(c.filter.as_str(), now_ms)
                }
                CapPred::Budget(producer_id) => {
                    astream_cap::verify(secret, c)
                        && !astream_cap::is_expired(c.filter.as_str(), now_ms)
                        && match astream_cap::Grant::parse(c.filter.as_str()) {
                            Ok(g) => {
                                g.mode == astream_cap::Mode::ReadWrite
                                    && match g.principal.as_deref() {
                                        None => true,
                                        Some(p) => astream_cap::producer_id_of(p) == producer_id,
                                    }
                            }
                            Err(_) => false,
                        }
                }
            };
            if matched {
                if streaming {
                    let exp = astream_cap::expires_at(c.filter.as_str()).unwrap_or(u64::MAX);
                    best = Some(match best {
                        Some(prev) if prev >= exp => prev,
                        _ => exp,
                    });
                } else {
                    best = Some(u64::MAX);
                    break;
                }
            }
        }
        let pred_until = best?;
        if pred_until < min_until {
            min_until = pred_until;
        }
    }
    Some(min_until)
}

/// The refusal for a request naming a subject, filter, group, cursor or grant longer
/// than [`MAX_NAME_LEN`], if it does. Checked before anything parses the name.
fn oversized_name(req: &Request) -> Option<String> {
    const NONE: (&str, &str) = ("", "");
    let names: [(&str, &str); 2] = match req {
        Request::Publish { subject, .. }
        | Request::Will { subject, .. }
        | Request::Retire { subject, .. } => [("subject", subject), NONE],
        Request::Subscribe { filter, .. } | Request::Fetch { filter, .. } => {
            [("filter", filter), NONE]
        }
        Request::ForkSubscribe {
            replacement_subject,
            filter,
            ..
        } => [("subject", replacement_subject), ("filter", filter)],
        Request::Commit { group, .. } => [("group", group), NONE],
        Request::ProcessAndProduce {
            out_subject, group, ..
        } => [("subject", out_subject), ("group", group)],
        Request::SubscribeGroup { group, filter } => [("group", group), ("filter", filter)],
        Request::Attach { grant, .. } => [("grant", grant), NONE],
        Request::Replicate {
            subject, commit, ..
        } => [
            ("subject", subject),
            match commit.as_ref() {
                Some((g, _)) => ("group", g.as_str()),
                None => NONE,
            },
        ],
        Request::Last { filter, after, .. } => [("filter", filter), ("cursor", after)],
        Request::Hello | Request::Keepalive | Request::IdleMarks | Request::Budget { .. } => {
            [NONE, NONE]
        }
    };
    for (what, name) in names {
        if name.len() > MAX_NAME_LEN {
            return Some(format!(
                "{what} too long: {} bytes (limit {MAX_NAME_LEN})",
                name.len()
            ));
        }
    }
    None
}

/// The pipelined ack path for one connection: an ordered, bounded queue of pending acks
/// feeding a dedicated thread that streams them back on a cloned write half. Lets a
/// connection keep many write ops in flight; back-pressures via the bounded queue.
struct Pipeline {
    /// Ordered queue of per-op ack receivers (bounded → flow control). `send` blocks
    /// when `PIPELINE_DEPTH` acks are outstanding.
    tx: mpsc::SyncSender<Receiver<AckResult>>,
    writer: Option<JoinHandle<io::Result<()>>>,
}

impl Pipeline {
    /// Start an ack-writer on a clone of `stream`'s write half, bounded by
    /// `write_timeout` — the CONNECTION's configured value, not a constant of this
    /// path's own. `set_write_timeout` is `setsockopt(SO_SNDTIMEO)`, a property of the
    /// socket rather than of the descriptor, and `try_clone` shares the file
    /// description: whatever the ack-writer sets here it also sets for the connection
    /// thread's own writes, for the rest of the connection's life, so anything but the
    /// configured [`Broker::set_write_timeout`] value would silently replace it.
    fn start<S: Stream>(stream: &S, write_timeout: Duration) -> io::Result<Pipeline> {
        let wr = stream.try_clone()?;
        let (tx, rx) = mpsc::sync_channel::<Receiver<AckResult>>(PIPELINE_DEPTH);
        // A failed spawn is this connection's error, not a panic that would unwind
        // past `handle_conn` and skip firing the connection's will.
        let writer =
            thread::Builder::new().spawn(move || ack_writer_loop(wr, rx, write_timeout))?;
        Ok(Pipeline {
            tx,
            writer: Some(writer),
        })
    }

    /// Submit a durable mutation: queue its ack slot in order, THEN enqueue the op to
    /// the global writer. Returns false if the ack-writer or the global writer has gone
    /// (the connection should close).
    ///
    /// Ordering matters for correctness: the ack slot is reserved FIRST. The bounded
    /// queue applies back-pressure here (blocks once PIPELINE_DEPTH acks are
    /// outstanding) BEFORE the op reaches the writer, so a record is never committed
    /// unless its ack is already queued to be delivered in order — closing the
    /// "committed durably but ack lost (client sees EOF)" window. If the writer has
    /// since exited, dropping the returned `WriteOp` closes `ack_tx`, so the ack-writer's
    /// blocked recv on the reserved slot returns cleanly instead of hanging.
    fn submit(&self, shared: &Arc<Shared>, kind: WriteKind) -> bool {
        let (ack_tx, ack_rx) = mpsc::channel();
        if self.tx.send(ack_rx).is_err() {
            return false; // ack-writer gone (its socket died); nothing committed
        }
        shared
            .writes
            .send(Queued::Op(WriteOp { kind, ack: ack_tx }))
            .is_ok()
    }

    /// Queue an already-resolved response (e.g. a validation error) in order.
    fn push_result(&self, res: AckResult) -> bool {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(res);
        self.tx.send(rx).is_ok()
    }

    /// Flush every queued ack and join the ack-writer (dropping `tx` makes its outer
    /// recv return `Disconnected` once drained), reclaiming the socket for streaming.
    fn finish(mut self) -> io::Result<()> {
        drop(self.tx);
        match self.writer.take() {
            Some(j) => match j.join() {
                Ok(res) => res,
                Err(_) => Ok(()),
            },
            None => Ok(()),
        }
    }
}

/// Emit a pre-resolved response in order: through the active pipeline, or inline if no
/// write op has started one yet. Returns false when the ack path is gone (close).
fn emit<S: Stream>(
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    res: AckResult,
) -> io::Result<bool> {
    match pipe {
        Some(p) => Ok(p.push_result(res)),
        None => {
            write_ack(stream, res)?;
            Ok(true)
        }
    }
}

/// Submit a write op, lazily starting the connection's pipeline. Returns false when the
/// ack path is gone (close).
fn emit_op<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    pipe: &mut Option<Pipeline>,
    kind: WriteKind,
) -> io::Result<bool> {
    let p = match pipe {
        Some(p) => p,
        None => pipe.insert(Pipeline::start(
            stream,
            Duration::from_millis(shared.write_timeout_ms.load(Ordering::Relaxed)),
        )?),
    };
    Ok(p.submit(shared, kind))
}

/// Finish the pipeline if one is active (flush acks, join the ack-writer).
fn finish_pipe(pipe: &mut Option<Pipeline>) -> io::Result<()> {
    match pipe.take() {
        Some(p) => p.finish(),
        None => Ok(()),
    }
}

/// The ack-writer half of a pipelined connection: stream each op's ack back in order.
///
/// Acks that are ALREADY resolved — a group-commit batch resolves a whole window at
/// once — are gathered and written together, one socket write (one sealed record)
/// for the lot instead of one each. What is gathered is written before this thread
/// blocks on anything, so no ack ever waits on a later one. A socket error tears the
/// connection down; an ack the global writer dropped (shutdown) ends the loop.
fn ack_writer_loop<S: Stream>(
    mut wr: S,
    rx: Receiver<Receiver<AckResult>>,
    write_timeout: Duration,
) -> io::Result<()> {
    use std::sync::mpsc::TryRecvError;
    let _ = wr.set_write_timeout(Some(write_timeout));
    let mut out = Vec::new();
    loop {
        let ack_rx = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                flush_egress(&mut wr, &mut out)?;
                match rx.recv() {
                    Ok(r) => r,
                    Err(_) => return Ok(()),
                }
            }
            Err(TryRecvError::Disconnected) => return flush_egress(&mut wr, &mut out),
        };
        let ack = match ack_rx.try_recv() {
            Ok(a) => a,
            Err(TryRecvError::Empty) => {
                flush_egress(&mut wr, &mut out)?;
                match ack_rx.recv() {
                    Ok(a) => a,
                    Err(_) => return Ok(()),
                }
            }
            Err(TryRecvError::Disconnected) => return flush_egress(&mut wr, &mut out),
        };
        append_frame(&mut out, &encode_response(&ack_response(ack)))?;
        if out.len() >= EGRESS_CHUNK {
            flush_egress(&mut wr, &mut out)?;
        }
    }
}

/// A writer ack as its wire response.
fn ack_response(ack: AckResult) -> Response {
    match ack {
        Ok((offset, deduped)) => Response::PublishAck { offset, deduped },
        Err((code, msg)) => Response::Error { code, msg },
    }
}

/// Turn a writer ack into the wire response.
fn write_ack<S: Stream>(stream: &mut S, ack: AckResult) -> io::Result<()> {
    write_frame(stream, &encode_response(&ack_response(ack)))
}

/// The single writer thread: drain queued mutations into a batch and group-commit them
/// with ONE fsync. The batch self-tunes to load — everything that queued while the
/// previous fsync ran is drained at once — so throughput scales with offered
/// concurrency while each record keeps the Strict (fsync'd-before-ack) guarantee.
///
/// IT LEAVES ON THE STOP, never on a poll. Idle, it SLEEPS on its queue with no timer,
/// so an idle broker's writer never wakes ([`BrokerStats::writer_wakeups`]); a horizon
/// falling due is the only timed wake. Shutdown ([`stop_writer`]) queues a
/// [`Queued::Stop`] behind every op already queued: the writer commits those, then
/// whatever else is queued when it reaches the stop — an op that raced shutdown in is
/// committed, not dropped — and returns, so the join behind it waits on that final
/// batch and nothing else. (It also leaves if the channel closes, which cannot happen
/// while [`Shared`] holds the sender.)
///
/// THE ORDERED HORIZON lives here, on the one thread that appends: a full flush from
/// any other thread would make the writer's next `write()` wait behind it on the same
/// file. `dirty_since` is when the first batch no full flush covers was acked (set as
/// that batch finishes, cleared by every full flush that catches the durable head up);
/// once the horizon has passed since then the writer issues a full flush — after the
/// current batch's acks, or on the idle timeout, which is the time left — outside the
/// log lock, and skipped when one already covers the head. Measuring from the first
/// uncovered ack, not from the last full flush, keeps a sparse burst at ONE full flush:
/// otherwise the first post after every quiet horizon would pay one. So the oldest ack
/// not on stable media is at most horizon + one batch + one full flush old. A stopping
/// writer leaves the horizon to the clean close, which flushes in full. Idle marks and
/// keepalives are wire frames, never records: they never make the log dirty, and a
/// horizon flush writes to no socket.
fn writer_loop(shared: &Arc<Shared>, rx: Receiver<Queued>) {
    let mut dirty_since: Option<Instant> = None;
    let mut stopping = false;
    loop {
        let first = if stopping {
            // Stopped: only what is queued already, then leave.
            match rx.try_recv() {
                Ok(queued) => Some(queued),
                Err(_) => return,
            }
        } else {
            match (shared.horizon, dirty_since) {
                // A horizon running: no later than it falls due.
                (Some(h), Some(t)) => match rx.recv_timeout(h.saturating_sub(t.elapsed())) {
                    Ok(queued) => Some(queued),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        shared.writer_wakeups.fetch_add(1, Ordering::Relaxed);
                        None
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                },
                // Nothing owed: for as long as the queue is empty.
                _ => match rx.recv() {
                    Ok(queued) => Some(queued),
                    Err(mpsc::RecvError) => return,
                },
            }
        };
        if let Some(first) = first {
            let mut batch = Vec::new();
            let mut next = Some(first);
            while let Some(queued) = next {
                match queued {
                    Queued::Op(op) => batch.push(op),
                    Queued::Stop => stopping = true,
                }
                // Nothing more queued right now → commit what we have.
                next = if batch.len() < BATCH_MAX {
                    rx.try_recv().ok()
                } else {
                    None
                };
            }
            if !batch.is_empty() {
                let dirty = process_batch(shared, batch);
                dirty_since = if dirty {
                    Some(match dirty_since {
                        Some(t) => t,
                        None => Instant::now(),
                    })
                } else {
                    None
                };
            }
        }
        if stopping {
            continue;
        }
        if let (Some(h), Some(t)) = (shared.horizon, dirty_since) {
            if t.elapsed() >= h {
                let dirty = horizon_flush(shared);
                dirty_since = if dirty { Some(Instant::now()) } else { None };
            }
        }
    }
}

/// The horizon's full flush: owed through the log's head now, issued outside the lock
/// through the log's own descriptor (shared), skipped when a full flush already covers
/// it. Whether the log is still dirty afterwards: never after a failure, which poisons
/// the log — no later flush can cover what that one was to (`BrokerLog::full_flush_failed`).
fn horizon_flush(shared: &Shared) -> bool {
    let (target, handle, served_before) = {
        let mut log = shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let target = log.head().0;
        if !log.full_flush_owed(target) {
            return false;
        }
        (target, log.flush_handle(), log.served())
    };
    let res = handle.full();
    let (_, dirty, wake) = settle_full_flush(shared, target, res, served_before);
    if wake {
        wake_tails(shared);
    }
    dirty
}

/// Record, under the log lock, the outcome of a full flush through `target` issued
/// outside it, and publish what it changed: the durable head, the visible head a held
/// batch kept back (never in the Replicated tier, whose visible head is the quorum's),
/// whether the log still serves Ordered, and — if the log latched to Strict — the copy
/// `Hello` answers from. The durable head after it (or the error a waiting op is
/// answered with), whether the log is still dirty, and whether the tails must be woken
/// ([`publish_log_state`]).
fn settle_full_flush(
    shared: &Shared,
    target: u64,
    res: io::Result<()>,
    served_before: crate::ServedInfo,
) -> (Result<u64, String>, bool, bool) {
    // Declared before the guard so unwind also releases the lock first.
    let retired;
    let mut log = shared
        .log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let outcome = match res {
        Ok(()) => {
            log.full_flush_done(target);
            Ok(log.durable_head().0)
        }
        Err(e) => {
            log.full_flush_failed(&e);
            Err(format!(
                "durability flush failed: {e}; the outcome is unknown, and the log refuses \
                 every write until it is reopened: resend then"
            ))
        }
    };
    let wake;
    (wake, retired) = publish_log_state(shared, &mut log, served_before);
    let dirty = log.poisoned().is_none() && log.durable_head() < log.head();
    drop(log);
    drop(retired);
    (outcome, dirty, wake)
}

/// Publish the log's heads and tier where the lock-free readers find them. Called with
/// the log lock held, which is what a parked tail's predicate reads them under, so no
/// wakeup is lost between "nothing to do" and "wait".
///
/// Returns whether the parked tails must be woken for it (see the end of
/// `process_batch`): the range from the old visible head to the new holds a record some
/// subscriber can be delivered — in the Strict/Ordered/Relaxed tiers; the Replicated
/// tier decides over its watermark — or, on a log that owes durable marks (it served
/// Ordered at its open: `Shared::durable_marks`), the durable head moved, which may owe
/// an idle-marks subscriber one (at most one wake per full flush: the horizon's, an
/// escalation's per escalated batch, and once the log has latched to Strict, every
/// batch's).
fn publish_log_state(
    shared: &Shared,
    log: &mut BrokerLog,
    served_before: crate::ServedInfo,
) -> (bool, Option<crate::LastViewGarbage>) {
    shared.is_replica.store(log.is_replica(), Ordering::Release);
    shared.log_head.store(log.head().0, Ordering::Release);
    shared.base.store(log.base().0, Ordering::Release);
    // Durable first, then visible: a thread that sees the visible head move past an
    // escalated record then also sees the durable head that covers it.
    let durable_before = shared.durable.swap(log.durable_head().0, Ordering::Release);
    let mut wake = false;
    let published = if shared.replicator.is_none() {
        let visible = log.visible_head();
        let before = shared.head.swap(visible.0, Ordering::Release);
        wake = log.any_deliverable(Offset(before), visible);
        Some(visible.0)
    } else {
        None
    };
    let served = log.served();
    if served != served_before {
        // The tier served is part of what `Hello` answers (its block's tag 0x01); it
        // changes in a running broker only here, when the log latches to Strict. The
        // durable marks a tail is owed do not change with it (`Shared::durable_marks`).
        *shared
            .hello
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = HelloCopy::of(log);
    }
    (
        wake || (shared.durable_marks && log.durable_head().0 > durable_before),
        // Detach last, after bookkeeping that could unwind; the caller's earlier
        // garbage binding then outlives its log guard even on an error path.
        published.and_then(|visible| publish_last_history(log, visible)),
    )
}

// This is called only with the ACTUAL head just published under the log lock,
// never with a Last request's input or the promoted log head. An unexpected domain
// refusal retains history; new reads outside its published range return a typed error.
fn publish_last_history(log: &mut BrokerLog, visible: u64) -> Option<crate::LastViewGarbage> {
    if log.poisoned().is_some() {
        return None;
    }
    match log.publish_last_visible(visible) {
        Ok(retired) => Some(retired),
        Err(error) => {
            eprintln!("astream-broker: cannot advance last-value history: {error}");
            None
        }
    }
}

/// Wake every parked subscriber tail at once, counted (`tail_broadcasts`).
fn wake_tails(shared: &Shared) {
    shared.tail_broadcasts.fetch_add(1, Ordering::Relaxed);
    shared.tail.notify_all();
}

/// The writer's side of [`Broker::pause_next_batch`] and [`Broker::pause_next_acks`],
/// at `point`: take the armed rendezvous if it is for this point (it is one-shot), say
/// the batch is here, and hold — at [`BatchPoint::Staged`] the caller still has the log
/// lock — until the test resumes it or drops its handle. A rendezvous armed for the
/// other point is left for that point to take.
fn hold_paused_batch(shared: &Shared, point: BatchPoint) {
    let slot = {
        let mut seam = shared
            .batch_pause
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &seam.slot {
            Some(slot) if slot.at == point => {}
            _ => return,
        }
        shared.batch_pause_armed.store(false, Ordering::Release);
        seam.slot.take()
    };
    if let Some(slot) = slot {
        let _ = slot.reached.send(());
        let _ = slot.resume.recv();
    }
}

/// Stage a whole batch under the log lock, write and fsync it once with the lock RELEASED,
/// promote it under the lock again, then ack each op. A staged-record
/// I/O error poisons the batch (rolled back, all error); a too-large / reserved /
/// diverged record is an op-level error that does not abort the rest; an fsync error
/// fails the whole batch (none durable). The parked subscriber tails are woken once
/// the batch's head is published, and only if the offsets it made visible hold a
/// record some subscriber could be delivered (see the end). In the Replicated tier the
/// batch is committed locally, then every committed-but-unconfirmed record is shipped
/// to the followers OUTSIDE the lock, and each op is acked only if its offset is below
/// the resulting quorum watermark.
///
/// THE HELD HEAD (Ordered and Relaxed, when a batch holds an escalated record — or a
/// resend of one, or a `Flush`): the Replicated tier's pattern, with a full flush where
/// it ships.
///
/// 1. Stage and commit: staged and begun under the lock, the write and the barrier (or
///    nothing) with it RELEASED, promoted under it again (`commit_begin`,
///    `BatchWrite::run`, `commit_end`). Its pure answer ([`Committed`](crate::store))
///    names the visible head — the first escalated record, or the log's head — and the
///    full flush owed.
/// 2. Still under the lock: publish that visible head, take a flush handle (the file's
///    own descriptor, shared), release the lock.
/// 3. Ack every op whose record is below the visible head (and is not a resend of an
///    escalated record no full flush covers yet), and every will registration, which
///    answers for its own commit. The per-connection ack writer writes
///    each connection's acks in request order whatever order they are sent in, so an
///    early send never reorders a pipeline. Wake the tails if a deliverable record became
///    visible.
/// 4. OUTSIDE the lock: the full flush.
/// 5. Under the lock again: the durable head rises, the visible head is published past
///    the held records; release, ack the rest, and wake the tails for what that made
///    visible (or, on a log that served Ordered at its open, for the durable marks it
///    may owe). A flush that fails records the run `Lossy`, poisons the log until it
///    is reopened (the records it was to cover stay held: a later flush that succeeds
///    says nothing about them) and answers the waiting ops "outcome unknown".
///
/// Every reader is bounded by the visible head (Fetch reads it under the lock, Last
/// pins it, a tail delivers below it), so an escalated record is never visible before
/// it is durable, and no ack precedes visibility (read-your-writes). Strict never takes
/// this path: its every batch is flushed in full by its own commit (in `BatchWrite::run`,
/// with the log lock released), as before.
///
/// Returns whether the log is DIRTY afterwards — records acked that no full flush
/// covers — which starts the writer's horizon.
#[allow(clippy::type_complexity)]
fn stage_batch_ops(
    guard: &mut BrokerLog,
    batch: Vec<WriteOp>,
    visible_head: u64,
) -> (
    Vec<Sender<AckResult>>,
    Vec<AckResult>,
    Vec<bool>,
    bool,
    Vec<bool>,
    bool,
    Option<(crate::LogIdentity, Offset)>,
) {
    let mut acks: Vec<Sender<AckResult>> = Vec::new();
    let mut results: Vec<AckResult> = Vec::new();
    // Which ops are `Flush` requests: answered with the durable head once the batch's
    // full flush (if any) has run.
    let mut flushes: Vec<bool> = Vec::new();
    let mut wants_flush = false;
    // Which ops are acked on the LOCAL commit alone, with no quorum gate. See the
    // `local_only` arm of the ack loop below: a `/a/will` registration is the broker's
    // own bookkeeping, never delivered to anyone, and its acknowledgement decides what
    // the CONNECTION does with the will — so it must answer for the record's own fate,
    // not for a follower's. An ungated moot conditional `Retire` (whose subject is
    // already absent below `visible_head` with no unconfirmed retire in flight) is also
    // answered at once.
    let mut local_only: Vec<bool> = Vec::new();
    let mut poisoned = false;
    // A LOG ALREADY POISONED refuses EVERY op of the batch in the one wording
    // (`store::poisoned_refusal`, which starts `proto::LOG_POISONED_PREFIX`), so a
    // producer reads the condition off its own answer, whichever op of the batch is
    // its. Staging would refuse the first op so and abort the batch, but every op after
    // it would read only "batch aborted (prior append I/O error)", and which op comes
    // first is the order connections reached the writer: a producer whose ops always
    // landed behind another's would never be told the log will take nothing until it
    // is reopened, and would retry on a connection that is worth nothing. The batch
    // aborts exactly as before (nothing staged, the file rolled back to its durable
    // length).
    let refused_poisoned = guard.poisoned().map(crate::store::poisoned_refusal);
    // The leader's log identity, from the highest-offset replicated record this batch
    // staged that carried one, with that offset: adopted once the batch commits, if
    // that record is the log's newest (see below).
    let mut adopt: Option<(crate::LogIdentity, Offset)> = None;
    for op in batch {
        acks.push(op.ack);
        let mut is_local = matches!(op.kind, WriteKind::WillRegister { .. });
        flushes.push(matches!(op.kind, WriteKind::Flush));
        if let Some(msg) = &refused_poisoned {
            local_only.push(is_local);
            results.push(Err(refusal(ERR_REFUSED, msg.clone())));
            poisoned = true;
            continue;
        }
        if poisoned {
            local_only.push(is_local);
            results.push(Err(refusal(
                ERR_REFUSED,
                "batch aborted (prior append I/O error)",
            )));
            continue;
        }
        let staged = match op.kind {
            WriteKind::Publish {
                producer_id,
                producer_seq,
                subject,
                body,
            } => guard.stage_publish(producer_id, producer_seq, subject, body),
            WriteKind::Commit { group, upto } => match guard.stage_commit(group, upto) {
                Ok(off) => Ok((off, false)),
                Err(e) => Err(e),
            },
            WriteKind::ProcessAndProduce {
                producer_id,
                producer_seq,
                out_subject,
                out_body,
                group,
                upto,
            } => guard.stage_process_and_produce(
                producer_id,
                producer_seq,
                out_subject,
                out_body,
                group,
                upto,
            ),
            WriteKind::Replicate { rec, leader } => {
                let staged = guard.stage_replica(rec);
                if let (Ok((off, _)), Some(leader)) = (&staged, leader) {
                    if !matches!(adopt, Some((_, through)) if *off < through) {
                        adopt = Some((leader, *off));
                    }
                }
                staged
            }
            WriteKind::Bind {
                producer_id,
                principal,
            } => guard.stage_bind(producer_id, principal),
            WriteKind::WillRegister { conn_id, will } => {
                match guard.stage_will_register_for_conn(conn_id, &will) {
                    Ok(off) => Ok((off, false)),
                    Err(e) => Err(e),
                }
            }
            WriteKind::WillFire { conn_id, will } => guard.stage_will_fire_for_conn(conn_id, will),
            WriteKind::WillDrop { conn_id } => {
                guard.drop_live_will(conn_id);
                Ok((Offset::ZERO, false))
            }
            WriteKind::Retire {
                producer_id,
                target_subject,
            } => guard.stage_retire(producer_id, target_subject),
            WriteKind::RetireConditional {
                producer_id,
                subject,
                offset,
                log,
            } => match guard.stage_retire_conditional(
                producer_id,
                subject,
                offset,
                log,
                visible_head,
            ) {
                Ok((off, deduped, ungated)) => {
                    is_local = ungated;
                    Ok((off, deduped))
                }
                Err(e) => Err(e),
            },
            // Stages nothing; answered below, with the durable head.
            WriteKind::Flush => {
                wants_flush = true;
                Ok((Offset::ZERO, false))
            }
        };
        local_only.push(is_local);
        // The code says whether the same op can EVER land (`proto::ERR_*`): the
        // record's own shape cannot change on a resend, so it is permanent; the
        // outcomes that are the log's state of the moment (an I/O error, a replica's
        // spine) are not known to be; and a spent subject budget, which only another
        // producer's record or a smaller count lifts, has a code of its own so the
        // producer can decide by what it knows about who writes the subject.
        let (code, msg) = match staged {
            Ok((off, deduped)) => {
                results.push(Ok((off.0, deduped)));
                continue;
            }
            Err(StageErr::TooLarge) => (ERR_RECORD_REFUSED, "message too large".to_string()),
            Err(StageErr::BadSubject(msg)) => (ERR_BAD_SUBJECT, msg),
            Err(StageErr::Reserved(subject)) => (ERR_RECORD_REFUSED, reserved_msg(&subject)),
            Err(StageErr::Cut(msg)) => (ERR_RECORD_REFUSED, msg),
            // Not permanent: another producer can introduce this subject, and an
            // identical request then updates an existing index entry without
            // spending a new slot. Not ERR_REFUSED either: nothing transient failed,
            // and a producer that alone writes the subject can drop the record.
            Err(StageErr::SubjectBound(msg)) => (ERR_SUBJECT_BOUND, msg),
            // Only a `Bind`, at attach: the producer id is another principal's.
            Err(StageErr::Collision(msg)) => (ERR_UNAUTHORIZED, msg),
            Err(
                StageErr::RetireCharged(msg) | StageErr::RetireWill(msg) | StageErr::RetireLog(msg),
            ) => (ERR_RETIRE_REFUSED, msg),
            // Not a failure: the fence did its job and the will appended nothing.
            Err(StageErr::Fenced(msg)) => (ERR_REFUSED, msg),
            // Also not a failure: a replica appends nothing of its own making.
            Err(StageErr::Replica(msg)) => (ERR_REFUSED, msg),
            // A gap is repaired by re-shipping; the leader reads the message, not this.
            Err(StageErr::Diverged(msg)) => (ERR_REFUSED, msg),
            // Decided in here, under the lock that appends: a replicated hidden record
            // offered to a log this broker owns, and a will offered the reserved ack
            // half. Op-level, like every other refusal above — the file is untouched.
            Err(StageErr::ReservedReplica(msg)) => (ERR_RECORD_REFUSED, msg),
            Err(StageErr::SeqReserved(msg)) => (ERR_RECORD_REFUSED, msg),
            Err(StageErr::Io(e)) => {
                poisoned = true;
                (ERR_REFUSED, format!("append failed: {e}"))
            }
        };
        results.push(Err(refusal(code, msg)));
    }
    (
        acks,
        results,
        flushes,
        wants_flush,
        local_only,
        poisoned,
        adopt,
    )
}

fn process_batch(shared: &Arc<Shared>, batch: Vec<WriteOp>) -> bool {
    // Replicated tier: bring down follower links back first — network I/O, so never
    // under the log lock — so the re-ship base below counts exactly the followers
    // this batch can reach.
    if let Some(rep) = &shared.replicator {
        rep.redial(&shared.running);
    }
    shared.writer_batches.fetch_add(1, Ordering::Relaxed);
    // Declared before both guards: release detached history after them on unwind.
    let retired;
    // The batch lock for the batch's whole life, its write and flush included (lock
    // order: batch → log); only retention ever waits on it (`Shared::batch`).
    let batch_held = shared
        .batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut guard = shared
        .log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let head_before = guard.head();
    let served_before = guard.served();
    let visible_head = shared.head.load(Ordering::Relaxed);
    // The wills this batch settles (`BrokerStats::wills_settled`), counted once its head
    // is published below.
    let wills = batch
        .iter()
        .filter(|op| {
            matches!(
                op.kind,
                WriteKind::WillFire { .. } | WriteKind::WillDrop { .. }
            )
        })
        .count();
    let (acks, results, flushes, wants_flush, local_only, poisoned, adopt) =
        stage_batch_ops(&mut guard, batch, visible_head);
    // The `pause_next_batch` test seam: staged, log lock held, nothing written yet.
    if shared.batch_pause_armed.load(Ordering::Acquire) {
        hold_paused_batch(shared, BatchPoint::Staged);
    }
    // Commit the batch: one write and one flush for everyone (unless poisoned — then
    // roll back), in the three halves of `BrokerLog::commit_batch`, with the log lock
    // RELEASED around the write and the flush. Begin, under the lock: seal the batch,
    // pick the flush its tier owes, take its bytes. Nothing is promoted, so nothing a
    // reader can see has changed when the lock goes.
    let begun = if poisoned {
        guard.abort_batch();
        Err(refusal(ERR_REFUSED, "batch aborted before fsync"))
    } else {
        match guard.commit_begin() {
            Ok(b) => Ok(b),
            Err(e) => Err(refusal(ERR_REFUSED, format!("commit failed: {e}"))),
        }
    };
    // The write and the flush, WITHOUT the log lock: every reader — a `Fetch` page, a
    // `Last` round, a tail copying what the last batch made visible — takes the lock
    // meanwhile and reads the log as it stood before this batch. The batch lock is still
    // held (retention waits on it), and the staged batch refuses every other mutation
    // until its end. Then end, under the lock again: promote the batch if the flush
    // returned `Ok` — promotion after durability, as before — or roll it back.
    let (mut guard, commit_res) = match begun {
        Ok(Begun::Write(mut write)) => {
            drop(guard);
            // The `pause_next_write` test seam: begun, log lock RELEASED, nothing
            // written yet.
            if shared.batch_pause_armed.load(Ordering::Acquire) {
                hold_paused_batch(shared, BatchPoint::Writing);
            }
            let res = write.run();
            let mut relocked = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let ended = match relocked.commit_end(write, res) {
                Ok(c) => Ok(c),
                Err(e) => Err(refusal(ERR_REFUSED, format!("commit failed: {e}"))),
            };
            (relocked, ended)
        }
        // An empty batch writes and flushes nothing, so it never lets the lock go (a full
        // flush it still owes — a resent escalated op — is issued below, as for any batch).
        Ok(Begun::Empty(committed)) => (guard, Ok(committed)),
        Err(refused) => (guard, Err(refused)),
    };
    // A REPLICA reports its leader's identity: its offsets are the leader's, so a
    // client that fails over to it keeps the ones it persisted. Adopted only once the
    // records that carried it are durable here, only when they reach the log's newest
    // record (an echo of a shared prefix vouches for nothing above it), and only by a
    // log that IS a replica and has not been found holding a record its leader does
    // not (`adopt_identity` asks all three). Failing to record it is not the batch's
    // failure — the records are committed — and leaves this follower reporting its
    // own identity, which costs a client that fails over a re-read, never a skipped
    // record.
    if let (Ok(_), Some((leader, through))) = (&commit_res, adopt) {
        if let Err(e) = guard.adopt_identity(leader, through) {
            eprintln!(
                "astream-broker: this replica could not record its leader's log identity \
                 {leader}: {e}; it keeps reporting {}",
                guard.identity()
            );
        }
        // The one place a running broker's identity (and with it the lineage) changes,
        // so the copy `Hello` answers from is refreshed here, under the log lock,
        // before this batch's records become visible. Whatever the adoption decided:
        // copying the log's own answer cannot be wrong, and a replicated batch is not
        // a hot path.
        *shared
            .hello
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = HelloCopy::of(&guard);
    }
    // Replicated tier: gather every committed-but-unconfirmed record — from the lowest
    // follower prefix to the head, cheap Arc clones — under the lock, to ship AFTER
    // releasing it: network I/O never holds the log lock (the delivery discipline).
    let to_ship = match &shared.replicator {
        Some(rep) if commit_res.is_ok() => {
            let base = rep.min_upto(guard.base().0, guard.head().0);
            Some(guard.read_from(Offset(base)))
        }
        _ => None,
    };
    // THE FULL FLUSH OWED, if any (step 1 of the held head): what the commit names —
    // an escalated record, or a resend of one, no full flush covers — or, for a
    // `Flush` request, the head (skipped, and counted so, when a full flush already
    // covers it; a flush the commit owes covers the request too). Its handle is taken
    // here, under the lock (step 2); the flush itself runs outside it (step 4). Taking
    // it cannot fail — it shares the log's own descriptor — so a broker short of
    // descriptors never mistakes a flush it could not issue for one that failed.
    let owed = match &commit_res {
        Ok(committed) => match committed.owe_full {
            Some(o) => Some(o.0),
            None => {
                let head = guard.head().0;
                if wants_flush && guard.full_flush_owed(head) {
                    Some(head)
                } else {
                    None
                }
            }
        },
        Err(_) => None,
    };
    let flush = match owed {
        Some(target) => Some((target, guard.flush_handle())),
        None => None,
    };
    // Strict/Ordered/Relaxed: publish the visible head — the log's head, or HELD at the
    // first record a full flush still owes — and the durable head, under the lock (the
    // tail's wait predicate reads them under this lock, so no wakeup is lost between
    // "caught up" and "wait"); and the tier, if the commit latched it to Strict.
    //
    // Whether the tails are woken (see the end) is decided here too, over what this
    // publish made VISIBLE: the old visible head to the new, which a held batch keeps
    // short of the log's head. The records it holds are woken for by the flush that
    // releases them (`settle_full_flush`), not by this commit.
    let wake;
    (wake, retired) = publish_log_state(shared, &mut guard, served_before);
    // After the head: a binding enters the attach path's copy only once the head that
    // covers it is published (`binding_settled` reads them in the other order, and
    // declines a binding at or above the visible head — a held one included). Only
    // from a batch that committed: a failed one promoted nothing to copy (and one that
    // failed AFTER promoting poisoned the log, whose bindings the writer then refuses
    // to re-confirm — they stay out of the copy, as they stay out of an ack). And
    // BEFORE the acks below: a connection's connection thread reads its next grant
    // only once this batch has answered its first, so its later `rw,p=` grants of the
    // same principal find the binding here — one writer round trip per connection, not
    // one per grant. Copied after the acks, they would race this copy for it
    // (`pause_next_acks` lets a test stand at the acks and see the copy done).
    if commit_res.is_ok() && shared.cap_secret.is_some() {
        copy_new_bindings(shared, &guard);
    }
    if guard.head() == head_before {
        shared.empty_batches.fetch_add(1, Ordering::Relaxed);
    }
    // After the head, under the log lock, so a reader that sees a will counted sees
    // whatever it appended, and its own later write queues behind it.
    if wills > 0 {
        shared
            .wills_settled
            .fetch_add(count64(wills), Ordering::Relaxed);
    }
    let durable_now = guard.durable_head().0;
    let dirty = guard.durable_head() < guard.head();
    drop(guard);
    // The batch is committed or rolled back: retention may rewrite the file again.
    drop(batch_held);
    drop(retired);
    let dirty = finish_batch_acks_and_flush(
        shared,
        commit_res,
        to_ship,
        flush,
        acks,
        results,
        local_only,
        flushes,
        wake,
        durable_now,
        dirty,
        served_before,
    );
    #[cfg(feature = "retention")]
    maybe_auto_retain(shared);
    dirty
}

/// Automatic writer-side state-carrying compaction (`set_max_retained_records` /
/// `set_max_retained_bytes`): runs AFTER the batch's acks and follower replication have
/// completed so publishers and subscribers never wait for compaction on the ack path.
/// When both caps are `0` (the default), exits after two `Relaxed` atomic loads without
/// taking any lock.
#[cfg(feature = "retention")]
fn maybe_auto_retain(shared: &Shared) {
    let max_records = shared.max_retained_records.load(Ordering::Relaxed);
    let max_bytes = shared.max_retained_bytes.load(Ordering::Relaxed);
    if max_records == 0 && max_bytes == 0 {
        return;
    }
    // ONE COMPACTION AT A TIME (`Shared::compaction`, lock order compaction → batch →
    // log): a retention call copies with no batch or log lock held, and a compaction run
    // here meanwhile would replace the log under it, failing that call at its finish
    // ("the log was compacted while this compaction copied it"). Skipped, never waited
    // for, so the writer never parks behind a copy: the next batch tries again.
    let _one = match shared.compaction.try_lock() {
        Ok(held) => held,
        Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
        Err(std::sync::TryLockError::WouldBlock) => return,
    };
    let _batch = shared
        .batch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut guard = shared
        .log
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let stats = guard.stats();
    let over_records = max_records > 0 && stats.retained_records > max_records;
    let over_bytes = max_bytes > 0 && stats.retained_bytes > max_bytes;
    if !over_records && !over_bytes {
        return;
    }
    let mut ceiling = shared.head.load(Ordering::Relaxed);
    if let Some(rep) = &shared.replicator {
        ceiling = ceiling.min(rep.min_upto(stats.base.0, stats.head.0));
    }
    if ceiling <= stats.base.0 {
        return;
    }
    let span = ceiling.saturating_sub(stats.base.0);
    let half_span = (span / 2).max(1);
    let mut cut = stats.base.0.saturating_add(half_span);
    if max_records > 0 {
        let target_keep = (max_records / 2).max(1) as u64;
        let by_count = stats.head.0.saturating_sub(target_keep);
        if by_count > cut {
            cut = by_count;
        }
    }
    cut = cut.min(ceiling);
    if cut > stats.base.0 && guard.retain_before_carrying_state(Offset(cut)).is_ok() {
        shared.base.store(guard.base().0, Ordering::Release);
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_batch_acks_and_flush(
    shared: &Arc<Shared>,
    commit_res: Result<crate::store::Committed, (u8, String)>,
    to_ship: Option<Vec<Arc<BrokerRecord>>>,
    flush: Option<(u64, crate::storage::FlushHandle)>,
    acks: Vec<Sender<AckResult>>,
    results: Vec<AckResult>,
    local_only: Vec<bool>,
    flushes: Vec<bool>,
    mut wake: bool,
    durable_now: u64,
    mut dirty: bool,
    served_before: crate::ServedInfo,
) -> bool {
    // Replicated tier: ship, then publish the quorum watermark as the visible head —
    // again under the log lock, for the same no-lost-wakeup reason — and wake.
    let watermark = match (&shared.replicator, to_ship) {
        (Some(rep), Some(records)) => {
            let wm = rep.replicate(&records, &shared.running);
            // This binding outlives the guard even if later bookkeeping unwinds.
            let retired;
            let mut g = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // MONOTONE + HEAD-BOUNDED (`Astream.Broker.TlaEpochWatermarkReplication`):
            // a record a quorum once held is not un-held because a follower later
            // restarted empty and its link reset to re-ship from 0 — the visible head
            // only ever advances (`replica_ack_monotone`) and is bounded by the durable
            // head (`committed_watermark`).
            let committed_wm = g.record_replica_ack(Offset(wm)).0;
            let prev = shared.head.fetch_max(committed_wm, Ordering::Relaxed);
            let hwm = crate::proto::replica_ack_monotone(prev, committed_wm);
            // What a quorum now holds that it did not, whichever batch appended it: a
            // record from an earlier batch becomes visible when a later batch's ship
            // confirms it.
            wake = g.any_deliverable(Offset(prev), Offset(hwm));
            retired = publish_last_history(&mut g, hwm);
            drop(g);
            drop(retired);
            Some(hwm)
        }
        (Some(_), None) => Some(shared.head.load(Ordering::Relaxed)),
        (None, _) => None,
    };
    // The `pause_next_acks` test seam: committed, head published, bindings copied, log
    // lock released, nothing answered yet.
    if shared.batch_pause_armed.load(Ordering::Acquire) {
        hold_paused_batch(shared, BatchPoint::BeforeAcks);
    }
    // Ack each op BEFORE waking 64+ parked subscriber threads (`notify_all`), so the
    // producer's ack-writer immediately receives its batch acks instead of waiting
    // behind a thundering herd of woken subscriber threads.
    //
    // Durable AND (if Replicated) below the quorum watermark, else error.
    // A deduped retry whose original record is still unconfirmed is an error too.
    //
    // WITH ONE EXCEPTION, `local_only`. The quorum gate answers "can a subscriber see
    // this record on a node that will survive me?" — the right question for a publish,
    // and the wrong one for a `/a/will` REGISTRATION, which no subscriber ever sees.
    // Its ack decides whether the connection keeps the will in memory, and the log
    // keeps the record whatever a follower did: answering a quorum shortfall with
    // "will was not persisted" would leave the connection with no will and the log
    // with the record, which the NEXT OPEN fires — a `gone` for a producer told it
    // registered nothing. The in-memory will and the `/a/will` record must never
    // disagree about whether the will exists, so the registration is acked on its own
    // commit. HONEST BOUNDARY: the will is then only
    // as durable as the leader's local log — if the leader is lost before a follower
    // takes the record, the goodbye is lost with it.
    //
    // THE HELD HEAD (step 3): with a full flush owed, an op whose record is at or above
    // the visible head — or a resend of an escalated record no full flush covers — waits
    // for it, and so does a `Flush` request; every other op is answered now.
    //
    // `local_only` IS EXEMPT HERE TOO, for the reason it is exempt from the quorum: a
    // will registration's ack answers for its `/a/will` record's own fate, and that
    // record is committed — promoted, on the log, at the tier's promise like any
    // record the table does not name. Made to wait, a flush that then failed answered it
    // "will was not persisted" while the log kept the will, which the next open fired: a
    // goodbye for a producer told it registered nothing. No reader is shown the record
    // early by acking it (a hidden subject is delivered to nobody), so nothing is lost by
    // answering it with the commit.
    let mut waiting: Vec<(Sender<AckResult>, AckResult, bool, bool)> = Vec::new();
    for (((ack, res), local), is_flush) in
        acks.into_iter().zip(results).zip(local_only).zip(flushes)
    {
        let send = match (&commit_res, res) {
            // An op that failed on its own terms (too large / reserved / diverged / the
            // append or poisoned-log error that aborted the batch) keeps its own, more
            // specific message; the ops a batch failure took down with it get the batch's.
            (Ok(_), Err(refused)) => Err(refused),
            // ...but a batch that rolled back decided nothing for good: its refusals
            // were judged against records that never landed (the subject bound counts
            // the batch's own new subjects), so a resend may land. Not known to be
            // permanent, whatever the op was refused for.
            (Err(_), Err((_, msg))) => Err(refusal(ERR_REFUSED, msg)),
            (Err(msg), Ok(_)) => Err(msg.clone()),
            (Ok(committed), Ok((off, deduped))) => {
                let held = if is_flush {
                    flush.is_some()
                } else {
                    flush.is_some() && !local && !committed.acks_now(off, deduped)
                };
                if held {
                    waiting.push((ack, Ok((off, deduped)), is_flush, local));
                    continue;
                }
                if is_flush {
                    Ok((durable_now, false))
                } else {
                    match watermark {
                        Some(wm) if !crate::proto::offset_is_committed(wm, off) && !local => {
                            Err(refusal(ERR_REFUSED, "not replicated to quorum"))
                        }
                        _ => Ok((off, deduped)),
                    }
                }
            }
        };
        let _ = ack.send(send);
    }
    // WAKE THE TAILS ONLY FOR SOMETHING THEY COULD DELIVER. Every parked tail waits on
    // the one `Condvar`, so a broadcast resumes all of them, and each resumed tail takes
    // the log lock to read what it was woken for. That used to happen after EVERY
    // batch, including the two kinds no subscriber can be delivered anything from:
    //
    // - a batch that made nothing visible. In Strict and Relaxed that is a batch that
    //   appended nothing — every op deduplicated or refused, or an attach's re-bind of
    //   a principal the writer found bound (`empty_batches`). In Replicated it is one
    //   whose ship moved the quorum watermark past nothing new, which an empty batch
    //   need not be: it still ships what the followers lack, and a retry of a publish
    //   refused for want of a quorum can confirm it (`wake` above is `prev..hwm`);
    // - a batch whose newly visible records are all hidden (`is_hidden_subject`): a
    //   group commit, a will registration, a binding, a retirement.
    //
    // A lone delivery through aterm's bridge is a publish batch and then a commit-only
    // batch — aterm commits its inbox group once per burst (aterm `78e8cbfdaf`: once no
    // inbox record is queued, or every 256 records), so a delivery nothing is queued
    // behind pays a commit of its own, while a refused record's notice and its commit go
    // out as one `ProcessAndProduce` ack — and the second broadcast doubled the herd:
    // with S parked tails, 2S wakeups per lone delivery instead of S (cost study B4
    // step 1). A tail that read the commit, matched nothing and then
    // probed its peer also sat out the next wakeup inside the probe (see `tail_loop`).
    //
    // SKIPPING THEM LOSES NOTHING. A tail parks only while `head <= cursor`, read under
    // the log lock the head was published under above, and it is woken by anything
    // else that can end or feed it: the next batch that does make a deliverable record
    // visible, a revocation, shutdown (each broadcasts on its own), its authority's
    // lapse and its liveness poll (each bounds its wait). A tail left parked past a
    // hidden-only batch reads those offsets the next time it wakes, skips them as it
    // always skipped them, and has delivered nothing later for it: there was nothing
    // in them to deliver.
    //
    // NOR DOES IT MOVE A MARK. The tail's cursor sits below those offsets until then,
    // and the cursor is what a `Mark` reports — but a parked tail writes nothing, marks
    // included, and once resumed it reads before it marks again (the park branch marks
    // only after a read came back empty, the delivery branch only after a chunk). So
    // the skip delays no mark and changes none that is written: a mark written before
    // the park named the cursor and head of that moment, as it always did. It is NOT
    // that a mark's cursor has reached the head it names: the delivery branch's idle
    // mark after a chunk cut short at `TAIL_CHUNK`, and the park branch's when a batch
    // lands between the tail's read and its mark, pair the cursor with a newer head,
    // and did before this skip as they do now.
    //
    // A REVOCATION OR SHUTDOWN WAKE IS BOUNDED BY THE POLL, AS IT WAS. Each sets its
    // flag and calls `notify_all` without the log lock, so a notify that lands between a
    // tail's predicate check and its wait is lost. Any later batch, a commit included,
    // used to resume such a tail sooner; now only a deliverable one or the poll does.
    // The worst case is one poll either way (200 ms by default), and a revoked tail is
    // delivered nothing meanwhile: the delivery loop checks revocation per record.
    //
    // NOT YET: WAKING ONLY THE TAILS WHOSE FILTER MATCHES (cost study B4 step 2). A
    // deliverable record still resumes every parked tail, matching or not, so S tails
    // cost S wakeups per message. Waking only the matching ones needs a parker per tail
    // registered with its filter and the writer matching each new record against every
    // registered filter. That replaces the one-lock argument above with a per-tail
    // wake-flag protocol (a tail clears its flag before it reads; the writer sets it
    // after it publishes the head), puts S filter matches per record on the writer's
    // thread, and leaves each non-matching tail up to a liveness poll behind the
    // visible head, to read in one go when it next wakes. The herd it would remove
    // costs microseconds at aterm's handful of tails per broker, so it waits for a
    // broker with many tails to need it. MEASURED, so that broker knows what it pays:
    // since a quiet tail no longer sits deaf in a blocking probe (`tail_loop`), every
    // tail is parked when a publish lands and the publish resumes all of them — at 33
    // tails (Relaxed, publish then commit, `broker_wake_bench fanout 32`) 33.0
    // wakeups per message, where about half the tails used to miss each broadcast
    // inside a probe (28.4-32.2 over two broadcasts), and publish → recv p50 0.165 ms
    // against 0.131 ms before at load 2.9-10.6, 0.141 against 0.128 ms at load
    // 2.8-4.3 (n = 1800 each, interleaved). At 6 tails and at 1 the p50 did not move
    // (0.045-0.073 against 0.052-0.070 ms; 0.022-0.054 against 0.021-0.059 ms).
    //
    // THE HELD HEAD wakes for what it releases: a batch held for its full flush made
    // visible only what precedes its escalated record, and the flush that releases the
    // rest decides its own wake over the same old-to-new visible range
    // (`settle_full_flush`) — plus, on a log that served Ordered at its open, a full
    // flush that moved the durable head, which may owe an idle-marks subscriber a
    // durable mark.
    if wake {
        wake_tails(shared);
    }
    // Steps 4 and 5: the full flush, OUTSIDE the lock, then its outcome under it —
    // the durable head, the visible head past the held records — and the waiting ops.
    if let Some((target, handle)) = flush {
        // The `pause_next_flush` test seam: step 3 done (every op the held head does not
        // keep back answered, the tails woken), the log lock released, the full flush not
        // yet issued.
        if shared.batch_pause_armed.load(Ordering::Acquire) {
            hold_paused_batch(shared, BatchPoint::BeforeFlush);
        }
        let res = handle.full();
        let (outcome, still_dirty, wake) = settle_full_flush(shared, target, res, served_before);
        dirty = still_dirty;
        for (ack, res, is_flush, local) in waiting {
            let send = match (&outcome, res) {
                (Ok(durable), Ok(_)) if is_flush => Ok((*durable, false)),
                (Ok(_), res) => match (watermark, res) {
                    (Some(wm), Ok((off, _)))
                        if !crate::proto::offset_is_committed(wm, off) && !local =>
                    {
                        Err(refusal(ERR_REFUSED, "not replicated to quorum"))
                    }
                    (_, res) => res,
                },
                (Err(msg), _) => Err(refusal(ERR_REFUSED, msg.clone())),
            };
            let _ = ack.send(send);
        }
        if wake {
            wake_tails(shared);
        }
    }
    dirty
}

/// One `Last` page: the rows, the cursor to resume from (`None` once the answer is
/// complete), and the subscriber-visible head they were read with.
///
/// THE WHOLE ANSWER IS READ AS OF ONE HEAD. `visible` is taken from `shared.head`
/// under the log lock on the FIRST round and PINNED for every round after it, and that
/// pinned value is the head the answer reports. So every row is its subject's newest
/// record STRICTLY BELOW the reported head — the one thing a last-value verb must not
/// get wrong.
///
/// Re-reading the head each round would get it wrong: round 1 collects `A@10` under
/// head H1; the lock is released between rounds, so a publisher writes `A@20`; round 3
/// finishes under H3 > 20 and the answer pairs `A@10` with H3, when A's last value
/// below H3 is 20. The cursor has already passed A, and the reader's paired
/// `Subscribe { from: mark.next }` starts at H3, so the stale row is never corrected.
///
/// WHAT THE PIN COSTS, DELIBERATELY. `last_matching` decides a subject from
/// `self.last` — its LATEST offset — and skips it entirely when that offset is not
/// below `visible`. So a subject whose newest record moves to or above the pinned head
/// mid-walk, and that the walk has not yet reached, is OMITTED from the page rather
/// than returned at its older value; reading it at its value as of the pin would need
/// a backward walk from `visible` that the store exposes no call for. This is the same
/// fail-closed direction the Replicated tier already takes for a record above the
/// quorum watermark, and here it loses nothing: the answer's `Mark.next` IS the pinned
/// head, so the record that displaced the omitted subject is delivered on the reader's
/// `Subscribe { from: mark.next }`. An omitted subject is filled in by the tail; a
/// stale row would not be.
///
/// THE PIN IS PER REQUEST. Client-driven paging (`Mark.resume` into the next `Last`)
/// pins a fresh head per page, so a paged reader still has to fold newest-wins across
/// its pages and tail from the FIRST page's `next`, exactly as `Client::last` says.
///
/// THE CURSOR NEVER NAMES A SUBJECT THE FILTER DOES NOT MATCH. `last_matching` walks
/// the ordered subject index from the filter's LITERAL PREFIX (`/f/F/in/*/*/n-1/*` ->
/// `/f/F/in/`) and reports the last entry it VISITED, matched or not. When the page is
/// cut by `max` that entry always matched, so it is inside whatever grant contained the
/// filter; when it is cut by the SCAN bound it is whatever the walk happened to stop
/// on, which under a literal prefix as broad as `/f/F/in/` is routinely another node's,
/// another session's, another human's inbox lane. Putting that on the wire would hand
/// a scoped reader one out-of-grant subject NAME per over-scanned page — and in this
/// fabric provenance is the address, so names are the roster.
///
/// So a page cut by the scan bound on a non-matching entry is CONTINUED here, from that
/// cursor, until the walk either finds enough matches or runs the prefix range out. The
/// lock is dropped between rounds, which is the property `LAST_SCAN_MAX` exists for —
/// no single acquisition visits more than `LAST_SCAN_MAX` entries, and ingest is never
/// stalled by a sparse filter. What a client can pay for is more CPU across rounds, and
/// [`LAST_RESUME_ROUNDS`] bounds that; past it the answer is an error rather than a
/// cursor it is not entitled to (blanking the cursor is not available: an empty
/// `Mark.resume` means "complete", which would be a silent truncation).
#[allow(clippy::type_complexity)]
fn last_page(
    shared: &Arc<Shared>,
    filt: &Filter,
    after: &str,
    want: usize,
) -> Result<(Vec<Arc<BrokerRecord>>, Option<String>, u64), String> {
    let mut out: Vec<Arc<BrokerRecord>> = Vec::new();
    let mut cursor = after.to_string();
    // The head every round reads against and the answer reports. Read under the log
    // lock on the first round (see below) and never re-read: that is what makes the
    // multi-round walk a snapshot rather than a splice of rounds taken at different
    // heads.
    let mut pinned: Option<crate::LastView> = None;
    for round in 0..LAST_RESUME_ROUNDS {
        // The FIRST round reads its head under the SAME lock as its page —
        // `shared.head` (the subscriber-visible head, the quorum watermark in the
        // Replicated tier), never `log.head()`. Under the lock: the seek, at most
        // LAST_SCAN_MAX retirement-plus-index visits, and pointer clones — never the socket writes the
        // caller does, which happen with it released.
        let (step, head) = {
            let log = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if pinned.is_none() {
                let head = shared.head.load(Ordering::Relaxed);
                pinned = Some(log.last_view_at(head).map_err(|error| error.to_string())?);
            }
            let Some(view) = pinned.as_mut() else {
                return Err("last: missing owned view".into());
            };
            let head = view.visible();
            let step = log
                .last_matching(
                    filt,
                    &cursor,
                    want.saturating_sub(out.len()),
                    LAST_SCAN_MAX,
                    view,
                )
                .map_err(|error| error.to_string())?;
            (step, head)
        };
        let (page, resume) = match step {
            crate::LastStep::Pending => continue,
            crate::LastStep::Page(page, resume) => (page, resume),
        };
        out.extend(page);
        match resume {
            // The prefix range ran out: the answer is complete.
            None => return Ok((out, None, head)),
            // Cut by `max`, or stopped on an entry the filter matches: either way the
            // cursor is a subject the caller's own filter covers.
            Some(s) if filt.matches_str(&s) || out.len() >= want => {
                return Ok((out, Some(s), head));
            }
            // Cut by the scan bound on a non-matching entry: keep walking.
            Some(s) => cursor = s,
        }
        // The round boundary, with the log lock released — the window a concurrent
        // publisher gets, and the one a test needs to occupy to prove the head above
        // does not move with it. Taken out of the lock before waiting so the test is
        // free to touch the broker.
        let pause = {
            let mut slot = shared
                .last_round_pause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if matches!(&*slot, Some(p) if p.round == round) {
                slot.take()
            } else {
                None
            }
        };
        if let Some(pause) = pause {
            if pause.reached.send(()).is_ok() {
                let _ = pause.resume.recv();
            }
        }
    }
    Err(format!(
        "last: {LAST_RESUME_ROUNDS} scan rounds under {:?} reached no position this filter \
         covers; narrow the filter's literal prefix",
        filt.as_str()
    ))
}

/// Up to [`TAIL_CHUNK`] records from `*cursor` up to (not including) `head`, read
/// under the log lock the caller holds.
///
/// Records below the retention base are gone for good, so the cursor first moves up to
/// it. What is read is unchanged by that (`read_range` clamps there anyway), but the
/// tail parks on `head <= cursor`, and a cursor left below a base with nothing visible
/// above it would never park — it would spin on the lock.
fn next_chunk(log: &BrokerLog, cursor: &mut u64, head: u64) -> Vec<Arc<BrokerRecord>> {
    *cursor = (*cursor).max(log.base().0);
    let behind = usize::try_from(head.saturating_sub(*cursor)).unwrap_or(usize::MAX);
    log.read_range(Offset(*cursor), behind.min(TAIL_CHUNK))
}

/// Stream every visible record matching `filter` from `cursor`, then tail live, until
/// the peer goes, the broker stops, the peer's identity key or capability principal is
/// revoked, or — on a guarded broker — the authority that admitted the subscription
/// lapses at `until` (unix ms; `u64::MAX`: never), which ends the stream with an
/// `unauthorized` error.
#[allow(clippy::too_many_arguments)]
fn tail_loop<S: Stream>(
    shared: &Arc<Shared>,
    stream: &mut S,
    mut cursor: u64,
    filter: Filter,
    mut until: u64,
    idle_marks: bool,
    peer_id: Option<[u8; 32]>,
    mut cap_check: StreamCapCheck,
) -> io::Result<()> {
    // A short read timeout bounds the liveness probe should the socket refuse to go
    // non-blocking for it (see `peer_gone`); a generous write timeout means a wedged
    // delivery to a stuck consumer errors out (and resumes from its cursor) rather than
    // hanging the thread forever.
    let _ = stream.set_read_timeout(Some(PEER_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(stream_write_timeout(shared)));
    let mut out = Vec::new();
    // When the peer was last probed for having gone, by either branch below, and the
    // liveness poll ([`Broker::set_liveness_poll`]): how often a parked tail wakes to
    // probe, and the least time between two probes of a tail that writes nothing.
    let mut probed = Instant::now();
    let liveness_poll = Duration::from_millis(shared.liveness_poll_ms.load(Ordering::Relaxed));
    // LIVENESS THE OTHER WAY: when this stream was last written to, and whether the
    // tail has yet said it caught up. A caught-up tail marks at once, the first time,
    // and again after every `idle_mark` with nothing written (see IDLE_MARK).
    let idle_mark = Duration::from_millis(shared.idle_mark_ms.load(Ordering::Relaxed));
    let mut wrote_at = Instant::now();
    let mut said_caught_up = false;
    // DURABLE MARKS (on a log that served Ordered at its open, latched to Strict since or
    // not — `Shared::durable_marks` — to a subscriber that asked for idle marks):
    // the durable head the last `Mark` told this subscriber. When the durable head passes
    // an offset it was delivered and not yet told is durable — `durable > marked` while
    // `marked < cursor` — it gets one more `Mark`, at most one per full flush that
    // covered new deliveries. That is how a reader off this host learns, in band, which
    // of the records it was shown survive a power loss (a notifier pages only those).
    let mut marked_durable: u64 = 0;
    loop {
        // Fast path when caught up: clamp `cursor` up to `base` atomically and, if
        // `visible <= cursor`, skip locking `shared.log` for an empty `next_chunk`
        // call and go straight to the Condvar park below (cutting caught-up lock
        // acquisitions in half).
        cursor = cursor.max(shared.base.load(Ordering::Acquire));
        let mut batch = if shared.head.load(Ordering::Acquire) <= cursor {
            Vec::new()
        } else {
            let log = shared
                .log
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let visible = shared.head.load(Ordering::Relaxed);
            next_chunk(&log, &mut cursor, visible)
        };
        if batch.is_empty() {
            if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
                return end_stream_failure(stream, &mut out, failure);
            }
            if idle_marks
                && (!said_caught_up
                    || wrote_at.elapsed() >= idle_mark
                    || (shared.durable_marks
                        && marked_durable < cursor
                        && shared.durable.load(Ordering::Relaxed) > marked_durable))
            {
                let head = shared.head.load(Ordering::Relaxed);
                marked_durable = durable_for(shared, head);
                write_mark(stream, cursor, head, marked_durable)?;
                said_caught_up = true;
                wrote_at = Instant::now();
            }
            // Park until a new commit (head > cursor) or shutdown — releasing the
            // lock — but wake every liveness poll to check whether the peer vanished,
            // and at the latest when this stream's authority lapses.
            let park = match until {
                u64::MAX => liveness_poll,
                t => liveness_poll.min(Duration::from_millis(t.saturating_sub(now_unix_ms()))),
            };
            {
                let guard = shared
                    .log
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // Counted where the writer cannot race them, under the log lock the
                // predicate runs under: `parked` is whether the predicate's last answer
                // sent this tail (back) to wait (`parked_tails`), and every answer after
                // a wait is one resume (`tail_wakeups`) — a broadcast, a revocation,
                // shutdown or the poll's timeout. `wait_timeout_while` re-waits inside
                // itself when a wake leaves the predicate true, so a count taken after
                // it returns would miss those.
                let mut parked = false;
                let mut resumes = 0u64;
                let seen_epoch = cap_check.seen_epoch;
                let (guard, _timed_out) = shared
                    .tail
                    .wait_timeout_while(guard, park, |log| {
                        if parked {
                            resumes = resumes.saturating_add(1);
                        }
                        cursor = cursor.max(log.base().0);
                        let sighup_pending = shared.has_revocation_file.load(Ordering::Relaxed)
                            && astream_sys::sighup_generation()
                                != shared.sighup_seen.load(Ordering::Relaxed);
                        let epoch_changed =
                            Some(shared.revocation_epoch.load(Ordering::Relaxed)) != seen_epoch;
                        // `durable` is written under this lock by the writer, so a full
                        // flush that makes a durable mark due — and wakes the tails for
                        // it (`publish_log_state`) — ends this park.
                        let wait = shared.running.load(Ordering::Relaxed)
                            && !sighup_pending
                            && !epoch_changed
                            && !is_peer_revoked_raw(shared, peer_id)
                            && shared.head.load(Ordering::Relaxed) <= cursor
                            && !(idle_marks
                                && shared.durable_marks
                                && marked_durable < cursor
                                && shared.durable.load(Ordering::Relaxed) > marked_durable);
                        if wait != parked {
                            if wait {
                                shared.parked_tails.fetch_add(1, Ordering::Relaxed);
                            } else {
                                shared.parked_tails.fetch_sub(1, Ordering::Relaxed);
                            }
                            parked = wait;
                        }
                        wait
                    })
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if parked {
                    // Timed out still waiting: no longer parked all the same.
                    shared.parked_tails.fetch_sub(1, Ordering::Relaxed);
                }
                shared.tail_wakeups.fetch_add(resumes, Ordering::Relaxed);
                // Single-lock wakeup: `wait_timeout_while` returns holding `guard`, so
                // grab `next_chunk` right here instead of dropping `guard` and
                // re-locking `shared.log` on the next iteration.
                let visible = shared.head.load(Ordering::Relaxed);
                let sighup_pending = shared.has_revocation_file.load(Ordering::Relaxed)
                    && astream_sys::sighup_generation()
                        != shared.sighup_seen.load(Ordering::Relaxed);
                let epoch_changed =
                    Some(shared.revocation_epoch.load(Ordering::Relaxed)) != seen_epoch;
                if visible > cursor
                    && !sighup_pending
                    && !epoch_changed
                    && !is_peer_revoked_raw(shared, peer_id)
                    && lapsed(until).is_none()
                {
                    batch = next_chunk(&guard, &mut cursor, visible);
                }
            }
            if batch.is_empty() {
                let head = shared.head.load(Ordering::Relaxed);
                if !shared.running.load(Ordering::Relaxed) && head <= cursor {
                    return Ok(()); // shutdown, nothing more to deliver
                }
                if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
                    return end_stream_failure(stream, &mut out, failure);
                }
                // Here with nothing visible, the park ran out its poll (or reached the
                // lapse, or a full flush made a durable mark due, which the next pass
                // writes): a probe is due.
                if head <= cursor {
                    probed = Instant::now();
                    if probe_peer(shared, stream) {
                        return Ok(()); // consumer vanished — reap this thread + fd
                    }
                }
                continue;
            }
        }
        // Deliver outside the lock; a slow consumer blocks only this thread.
        let mut wrote = false;
        for rec in batch {
            if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
                return end_stream_failure(stream, &mut out, failure);
            }
            // Checked, like the engine's offset spine: a u64::MAX seq ends the stream.
            cursor = match rec.seq.checked_next() {
                Some(o) => o.0,
                None => return flush_egress(stream, &mut out),
            };
            // Pure consumer-group commit records are internal bookkeeping — never
            // delivered to data subscribers. A wildcard filter (`/a/>`, `/a/*`)
            // matches the reserved `/a/commit` subject, so the skip must be explicit
            // and filter-independent; the record still consumes an offset (advanced
            // above), it is simply not sent.
            if crate::store::is_hidden_record(&rec) {
                continue;
            }
            if filter.matches_str(&rec.subject) {
                // `rec` is a shared `Arc` (no deep copy out of the log), and its
                // frame is assembled in place from the borrowed subject/body.
                queue_delivery_cached(shared, stream, &mut out, &rec)?;
                wrote = true;
            }
        }
        flush_egress(stream, &mut out)?;
        // A subscriber whose filter matches nothing still drains the log without ever
        // writing, so the park branch (which probes for a vanished peer, and marks an
        // idle stream) is never reached under continuous traffic. Mark and probe here
        // too when a batch produced zero writes, so such a subscriber still hears from
        // the broker and a dead one's thread + fd are still reaped.
        //
        // ONCE PER LIVENESS POLL, NOT ONCE PER BATCH. This probed after every batch a
        // tail matched nothing in and had nothing more behind, which on a busy broker
        // is almost every batch for almost every tail: with 32 tails that never match,
        // 64.8 probes per delivered message (cost study B3). Each probe is a syscall
        // the tail makes for nothing, and while it blocked (it no longer does, see
        // `peer_gone`) the tail missed the next commit's wakeup. A vanished peer only
        // has to be noticed within a poll, which is what the parked tail already
        // promised: `probed` is shared with the park branch, so a tail probes at most
        // once per poll whichever way it spends it, and a dead subscriber's thread and
        // descriptor are reaped within about a poll either way.
        if wrote {
            wrote_at = Instant::now();
        } else if idle_marks && wrote_at.elapsed() >= idle_mark {
            if let Some(failure) = stream_failure(shared, peer_id, &mut cap_check, &mut until) {
                return end_stream_failure(stream, &mut out, failure);
            }
            let head = shared.head.load(Ordering::Relaxed);
            marked_durable = durable_for(shared, head);
            write_mark(stream, cursor, head, marked_durable)?;
            wrote_at = Instant::now();
        }
        if !wrote && probed.elapsed() >= liveness_poll {
            probed = Instant::now();
            if probe_peer(shared, stream) {
                return Ok(());
            }
        }
    }
}

/// Tell a subscriber where its tail stands — resume from `next`, `head` is the visible
/// head, `durable` the durable head — with nothing to deliver: the catch-up, idle and
/// durable marks (see [`IDLE_MARK`]).
fn write_mark<S: Write>(stream: &mut S, next: u64, head: u64, durable: u64) -> io::Result<()> {
    write_frame(
        stream,
        &encode_response(&mark(next, head, durable, String::new())),
    )
}

/// The durable head a `Mark` read with visible head `head` reports: the log's durable
/// head, never above the visible one (in the Replicated tier the quorum watermark can lag
/// what the leader flushed; a reader can ask only about records it can see).
fn durable_for(shared: &Shared, head: u64) -> u64 {
    shared.durable.load(Ordering::Relaxed).min(head)
}

/// A `Mark` as this broker sends every one: with the durable head, so a reader on any
/// host can tell which of the records it was shown are on stable media (`Mark.durable`).
fn mark(next: u64, head: u64, durable: u64, resume: String) -> Response {
    Response::Mark {
        next,
        head,
        resume,
        durable: Some(durable),
    }
}

/// A `Mark` at the visible head now (`next == head`): what acknowledges an `Attach`, a
/// `Will`, a `Keepalive` or `IdleMarks`.
fn mark_at_head(shared: &Shared) -> Response {
    let head = shared.head.load(Ordering::Relaxed);
    mark(head, head, durable_for(shared, head), String::new())
}

/// Bytes of framed output a streaming verb gathers before it writes: one socket write
/// (and, sealed, one AEAD record per 64 KiB) per this much, not one per record.
const EGRESS_CHUNK: usize = 64 * 1024;

/// Gather `rec` as a framed `Delivery` in `out`, writing `out` to `stream` once it
/// holds [`EGRESS_CHUNK`] bytes. Used by `ForkSubscribe` where the replacement record
/// at `fork_offset` is counterfactual and must not populate [`Shared::delivery_crc_cache`].
fn queue_delivery<S: Write>(
    stream: &mut S,
    out: &mut Vec<u8>,
    rec: &BrokerRecord,
) -> io::Result<()> {
    append_delivery_frame(out, rec.seq.0, &rec.subject, &rec.body)?;
    if out.len() >= EGRESS_CHUNK {
        flush_egress(stream, out)?;
    }
    Ok(())
}

const fn crc_cache_slot(
    cache: &[Mutex<Option<CachedDeliveryCrc>>; 4096],
    offset: u64,
) -> &Mutex<Option<CachedDeliveryCrc>> {
    &cache[(offset as usize) & 4095]
}

/// Gather `rec` as a framed `Delivery` in `out` using [`Shared::delivery_crc_cache`] so
/// repeat reads can reuse the payload CRC-32/IEEE, writing `out`
/// to `stream` once it holds [`EGRESS_CHUNK`] bytes. The caller ends its batch with
/// [`flush_egress`].
fn queue_delivery_cached<S: Write>(
    shared: &Shared,
    stream: &mut S,
    out: &mut Vec<u8>,
    rec: &BrokerRecord,
) -> io::Result<()> {
    let slot = crc_cache_slot(shared.delivery_crc_cache.as_ref(), rec.seq.0);
    let cached = *slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let crc = match cached {
        Some(hit) if hit.offset == rec.seq.0 => hit.crc,
        _ => {
            // Hash outside the slot lock. Concurrent misses may duplicate work, never mix keys/CRCs.
            let crc = compute_delivery_crc(rec.seq.0, &rec.subject, &rec.body);
            *slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedDeliveryCrc {
                offset: rec.seq.0,
                crc,
            });
            crc
        }
    };
    append_delivery_frame_with_crc(out, rec.seq.0, &rec.subject, &rec.body, Some(crc))?;
    if out.len() >= EGRESS_CHUNK {
        flush_egress(stream, out)?;
    }
    Ok(())
}

/// Write what `out` has gathered and empty it — handing back any capacity a large
/// record grew it to, so a long-lived subscription does not pin its biggest record.
fn flush_egress<S: Write>(stream: &mut S, out: &mut Vec<u8>) -> io::Result<()> {
    if !out.is_empty() {
        stream.write_all(out)?;
        out.clear();
    }
    out.shrink_to(2 * EGRESS_CHUNK);
    Ok(())
}

/// [`peer_gone`], counted in [`BrokerStats::peer_probes`].
fn probe_peer<S: Stream>(shared: &Shared, stream: &mut S) -> bool {
    shared.peer_probes.fetch_add(1, Ordering::Relaxed);
    peer_gone(stream)
}

/// Whether the subscriber's peer has closed its end. After SUBSCRIBE the client
/// only reads, so a `read` returns EOF (`Ok(0)`) iff the peer closed; a hard
/// error (reset/pipe) is also gone; `WouldBlock`/`TimedOut` means still connected.
/// (`UnixStream::peek` would avoid consuming, but it is unstable on this toolchain;
/// any unexpected post-subscribe byte is simply ignored.)
///
/// THE READ DOES NOT BLOCK. It used to wait out the socket's read timeout,
/// [`PEER_PROBE_TIMEOUT`] (1 ms asked, 1.13 ms measured), and the tail cannot be
/// woken inside a socket read: a commit that landed meanwhile found nobody waiting on
/// the `Condvar`, and its record reached the subscriber only once the probe ended. On
/// the Relaxed tier a matching record 0.3 ms behind a batch some tail did not match
/// took p50 0.80-0.84 ms to arrive against 0.07 ms alone (cost study B3). Strict hid
/// it — the next batch's F_FULLFSYNC (3 ms or more) always outlasted the probe — and
/// the Ordered tier's 0.13-0.6 ms flush would not. Now the socket is switched to
/// non-blocking for this one read and back, so the probe answers at once.
///
/// Why a switch and not a flag on the read: std has no per-call `MSG_DONTWAIT`, and
/// the crate forbids `unsafe`. The switch is safe because the tail is the only thread
/// using the socket from SUBSCRIBE on (the pipeline's ack-writer was finished first;
/// the connection registry's handle only ever shuts the socket down), and the mode is
/// restored before the tail writes again. If the switch is refused the read falls back
/// to the read timeout `tail_loop` arms, still bounded. If the mode cannot be
/// restored the peer is reported gone: a delivery write in non-blocking mode would
/// fail on the first full socket buffer, mid-frame, and ending the subscription here —
/// the client resumes from its cursor — is the honest form of that failure.
fn peer_gone<S: Stream>(stream: &mut S) -> bool {
    let mut probe = [0u8; 1];
    let switched = stream.set_nonblocking(true).is_ok();
    let gone = match stream.read(&mut probe) {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
        ),
    };
    if switched && stream.set_nonblocking(false).is_err() {
        return true;
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "cap")]
    #[test]
    fn a_marked_retire_needs_replication_authority_but_advances_no_group() {
        let secret = b"captured replication authority";
        let ident = crate::LogIdentity {
            id: [7; 16],
            generation: 3,
        };
        let record = BrokerRecord::marked_retire(Offset(7), 42, "/t/session/ev");
        let request = crate::proto::decode_request(&encode_replica_record(&record, &ident))
            .expect("the actual replication encoder and decoder agree on the marker");
        assert!(matches!(
            &request,
            Request::Replicate {
                subject,
                producer_seq: crate::brecord::RETIRE_PRODUCER_SEQ,
                commit: Some((group, 0)),
                ..
            } if subject == RETIRE_SUBJECT && group == crate::proto::REPLICATE_RETIRE_GROUP
        ));
        let mint = |grant| astream_cap::mint(secret, grant).unwrap();
        let marker_cap = mint("rw:/a/retire");
        assert_eq!(
            cap_authorized_until(secret, &[marker_cap], &request, 10),
            Some(u64::MAX),
            "a valid marker advances no consumer group"
        );
        for cap in [
            mint("ro:/a/retire"),
            mint("rw:/elsewhere/>"),
            mint("rw,p=n-peer:/a/retire"),
            mint("rw,exp=10:/a/retire"),
        ] {
            assert_eq!(cap_authorized_until(secret, &[cap], &request, 10), None);
        }
        assert_eq!(cap_authorized_until(secret, &[], &request, 10), None);
        let broad = mint("rw:/>");
        let malformed = |subject: &str, producer_seq, upto| Request::Replicate {
            seq: 7,
            producer_id: 42,
            producer_seq,
            subject: subject.into(),
            body: b"/t/session/ev".to_vec(),
            commit: Some((crate::proto::REPLICATE_RETIRE_GROUP.into(), upto)),
            leader: Some(ident),
        };
        for bad in [
            malformed("/t/not-the-marker", crate::brecord::RETIRE_PRODUCER_SEQ, 0),
            malformed(RETIRE_SUBJECT, 9, 0),
            malformed(RETIRE_SUBJECT, crate::brecord::RETIRE_PRODUCER_SEQ, 1),
        ] {
            assert_eq!(
                cap_authorized_until(secret, std::slice::from_ref(&broad), &bad, 10),
                None,
                "sentinel text alone must not waive a real group grant"
            );
            assert!(crate::proto::decode_request(&crate::proto::encode_request(&bad)).is_none());
        }
        let commit = Request::Replicate {
            seq: 8,
            producer_id: 42,
            producer_seq: 2,
            subject: "/t/output".into(),
            body: b"result".to_vec(),
            commit: Some(("/g/consumer".into(), 5)),
            leader: Some(ident),
        };
        let commit = crate::proto::decode_request(&crate::proto::encode_request(&commit))
            .expect("an ordinary replicated group commit round-trips");
        let output = mint("rw:/t/>");
        let group = mint("rw:/g/consumer");
        assert_eq!(
            cap_authorized_until(secret, std::slice::from_ref(&output), &commit, 10),
            None,
            "a real commit still needs its consumer group's grant"
        );
        assert_eq!(
            cap_authorized_until(secret, std::slice::from_ref(&group), &commit, 10),
            None,
            "the group grant does not grant the replicated output"
        );
        assert_eq!(
            cap_authorized_until(secret, &[output, group], &commit, 10),
            Some(u64::MAX)
        );
    }

    /// A follower link at confirmed prefix `upto`; `live` gives it a connected socket
    /// (a loopback pair), otherwise it is down.
    fn link(upto: u64, fenced: Option<&str>, live: bool) -> FollowerLink {
        let stream = live.then(|| {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            TcpStream::connect(l.local_addr().unwrap()).unwrap()
        });
        FollowerLink {
            leader: crate::LogIdentity {
                id: [0; 16],
                generation: 0,
            },
            addr: "127.0.0.1:0".to_string(),
            stream,
            upto,
            fenced: fenced.map(str::to_string),
            fresh: false,
            timeout_failures: 0,
            next_dial_at: None,
            #[cfg(feature = "cap")]
            cap: None,
        }
    }

    fn replicator(links: Vec<FollowerLink>) -> Replicator {
        Replicator {
            links: Mutex::new(links),
            kill: Mutex::new(Vec::new()),
            quorum: 1,
            io_timeout: Duration::from_secs(1),
        }
    }

    /// A FENCED follower does not hold the re-ship base down. `replicate` ships nothing
    /// to a fenced link, so its `upto` is frozen for the life of the leader; counting it
    /// would pin the base — the start of a slice the writer clones UNDER THE LOG LOCK
    /// on every batch — at that frozen offset.
    #[test]
    fn min_upto_ignores_fenced_links() {
        let r = replicator(vec![
            link(100, Some("replica diverged at offset 100"), true),
            link(500, None, true),
        ]);
        assert_eq!(
            r.min_upto(0, 500),
            500,
            "a diverged follower's frozen prefix became every later batch's base"
        );
        // Healthy links still set it, and the lowest of them wins.
        let r = replicator(vec![
            link(500, None, true),
            link(100, None, true),
            link(300, None, true),
        ]);
        assert_eq!(r.min_upto(0, 500), 100);
        // Every link fenced, or no links at all: nothing can be shipped, so the base is
        // the head and the slice is empty rather than the whole log.
        let r = replicator(vec![
            link(100, Some("diverged"), true),
            link(7, Some("unauthorized"), true),
        ]);
        assert_eq!(r.min_upto(0, 500), 500);
        assert_eq!(replicator(Vec::new()).min_upto(0, 500), 500);
    }

    /// A DOWN follower (its re-dial at the start of the batch failed) does not hold
    /// the re-ship base down either: nothing reaches it this batch, and counting it
    /// would clone its whole backlog under the log lock on every batch it stays down.
    #[test]
    fn min_upto_ignores_down_links() {
        let r = replicator(vec![link(100, None, false), link(500, None, true)]);
        assert_eq!(
            r.min_upto(0, 500),
            500,
            "a down follower's prefix became the batch's base"
        );
        let r = replicator(vec![link(100, None, false), link(300, None, false)]);
        assert_eq!(
            r.min_upto(0, 500),
            500,
            "no follower reachable: nothing to clone"
        );
    }

    /// A follower BEHIND the retention base (it answered a gap, its `upto` reset to 0,
    /// and the records it lacks were pruned) is never shipped to again, so it does not
    /// hold the re-ship base down to the base either — which would be a copy of the
    /// whole retained log under the log lock on every batch.
    #[test]
    fn min_upto_ignores_links_behind_the_base() {
        let r = replicator(vec![link(0, None, true), link(500, None, true)]);
        assert_eq!(
            r.min_upto(200, 500),
            500,
            "a follower behind the base became the batch's base"
        );
        // At or above the floor it counts as before.
        let r = replicator(vec![link(200, None, true), link(500, None, true)]);
        assert_eq!(r.min_upto(200, 500), 200);
        let r = replicator(vec![link(0, None, true)]);
        assert_eq!(r.min_upto(200, 500), 500, "nothing left to ship to");
    }

    /// Removes its files when dropped, so a test leaves nothing behind whether it
    /// passes or panics. Bound before the broker that uses them, so it drops after it.
    #[cfg(unix)]
    struct Cleanup(Vec<String>);

    #[cfg(unix)]
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// A served broker whose log holds `n` records (512-byte bodies on `/t/x`), and
    /// the guard that removes its files.
    #[cfg(unix)]
    fn filled(tag: &str, n: u64) -> (Cleanup, Broker, BrokerHandle, String) {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let k = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let log = format!("/tmp/asb_unit_{tag}_{pid}_{k}.log");
        let sock = format!("/tmp/asb_unit_{tag}_{pid}_{k}.sock");
        let _ = std::fs::remove_file(&log);
        let tmp = Cleanup(vec![log.clone(), sock.clone()]);
        let b = Broker::open_with(&log, Durability::Relaxed).unwrap();
        {
            let mut l = b.shared.log.lock().unwrap();
            for seq in 0..n {
                l.publish(1, seq, "/t/x".to_string(), vec![0u8; 512])
                    .unwrap();
            }
            b.shared.head.store(l.head().0, Ordering::Relaxed);
            b.shared.log_head.store(l.head().0, Ordering::Relaxed);
        }
        let h = b.serve(&sock).unwrap();
        (tmp, b, h, sock)
    }

    /// Open a streaming verb with `req` on a connection that then never reads: the
    /// broker delivers until the socket buffers fill and blocks there, still holding
    /// whatever it copied out of the log and has not yet delivered.
    #[cfg(unix)]
    fn stalled_reader(sock: &str, req: &Request) -> UnixStream {
        let mut s = UnixStream::connect(sock).unwrap();
        write_frame(&mut s, &crate::proto::encode_request(req)).unwrap();
        s
    }

    /// How many of the log's records a reader holds copies of, once that number has
    /// stopped moving (the reader is blocked on the stalled socket).
    #[cfg(unix)]
    fn settled_copies(b: &Broker) -> usize {
        let t = Instant::now();
        let (mut last, mut same) = (usize::MAX, 0);
        loop {
            let n = b.shared.log.lock().unwrap().shared_records();
            same = if n == last && n > 0 { same + 1 } else { 0 };
            if same == 5 {
                return n;
            }
            last = n;
            assert!(
                t.elapsed() < Duration::from_secs(20),
                "the reader never settled (last count {n})"
            );
            thread::sleep(Duration::from_millis(40));
        }
    }

    /// A subscriber catching up from offset 0 copies the log out ONE CHUNK per lock
    /// hold, so the most it can hold at once is one chunk — never the whole suffix,
    /// which it would copy under the log lock while every publisher waits.
    #[cfg(unix)]
    #[test]
    fn a_tail_catching_up_copies_the_log_one_chunk_at_a_time() {
        let n = 3 * TAIL_CHUNK as u64;
        let (_tmp, b, mut h, sock) = filled("tailchunk", n);
        let _reader = stalled_reader(
            &sock,
            &Request::Subscribe {
                from_offset: 0,
                filter: "/t/>".to_string(),
            },
        );
        let held = settled_copies(&b);
        assert!(
            held <= TAIL_CHUNK,
            "a subscriber catching up from 0 held {held} of the log's {n} records at once \
             (one chunk is {TAIL_CHUNK})"
        );
        h.shutdown();
    }

    /// The same for a fork snapshot, which is delivered from the same kind of copy.
    #[cfg(unix)]
    #[test]
    fn a_fork_snapshot_copies_the_log_one_chunk_at_a_time() {
        let n = 3 * TAIL_CHUNK as u64;
        let (_tmp, b, mut h, sock) = filled("forkchunk", n);
        let _reader = stalled_reader(
            &sock,
            &Request::ForkSubscribe {
                fork_at: n - 1,
                replacement_subject: "/t/y".to_string(),
                replacement_body: b"r".to_vec(),
                filter: "/t/>".to_string(),
            },
        );
        let held = settled_copies(&b);
        assert!(
            held <= TAIL_CHUNK,
            "a fork of a {n}-record log held {held} of its records at once \
             (one chunk is {TAIL_CHUNK})"
        );
        h.shutdown();
    }

    #[cfg(feature = "cap")]
    #[test]
    fn stream_checks_refresh_epochs_and_tighten_the_grants_deadline() {
        const SECRET: &[u8] = b"captured-stream-secret";
        let a = astream_cap::mint(SECRET, "ro,p=s-a,exp=1010:/a/>").unwrap();
        let b = astream_cap::mint(SECRET, "ro,p=s-b,exp=1100:/a/>").unwrap();
        let request = || Request::Subscribe {
            from_offset: 0,
            filter: "/a/>".to_string(),
        };
        let mut check = StreamCapCheck {
            keyring: vec![a.clone(), b.clone()],
            req: Some(request()),
            seen_epoch: None,
        };
        let mut until = 1100;
        assert_eq!(check.validate(0, 1000, &mut until, Some(SECRET)), None);
        assert_eq!(check.seen_epoch, Some(0));
        assert_eq!(until, 1100);

        let mut revoked = RevocationSet::new();
        assert!(revoked.insert("s-b".to_string()));
        prune_capability_keyring(&revoked, &mut check.keyring);
        assert_eq!(check.validate(1, 1005, &mut until, Some(SECRET)), None);
        assert_eq!(until, 1010);
        assert_eq!(
            check.validate(1, 1020, &mut until, Some(SECRET)),
            Some(StreamFailure::Expired {
                until: 1010,
                now: 1020
            })
        );

        // The first validation cannot consume an epoch sampled after revocation
        // while trusting the stale keyring captured before it.
        let mut first = StreamCapCheck {
            keyring: vec![b],
            req: Some(request()),
            seen_epoch: None,
        };
        prune_capability_keyring(&revoked, &mut first.keyring);
        let mut until = 1100;
        assert_eq!(
            first.validate(1, 1005, &mut until, Some(SECRET)),
            Some(StreamFailure::Revoked(
                "unauthorized: capability principal has been revoked"
            ))
        );

        // If the remaining covering grant has already expired, time-zero parsing
        // only selects the typed expiry explanation; it does not authorize output.
        let mut expired = StreamCapCheck {
            keyring: vec![a.clone()],
            req: Some(request()),
            seen_epoch: None,
        };
        let mut until = 1100;
        assert_eq!(
            expired.validate(1, 1020, &mut until, Some(SECRET)),
            Some(StreamFailure::Expired {
                until: 1010,
                now: 1020
            })
        );
        let mut shorter = StreamCapCheck {
            keyring: vec![a],
            req: Some(request()),
            seen_epoch: None,
        };
        let mut until = 1008;
        assert_eq!(shorter.validate(0, 1005, &mut until, Some(SECRET)), None);
        assert_eq!(
            until, 1008,
            "reauthorization never extends the original admission"
        );

        // No guard: an unrelated revocation must still advance the accepted epoch
        // before the caller uses it in the park predicate.
        let mut unguarded = StreamCapCheck {
            keyring: Vec::new(),
            req: None,
            seen_epoch: None,
        };
        let mut until = u64::MAX;
        assert_eq!(unguarded.validate(1, 0, &mut until, None), None);
        assert_eq!(unguarded.seen_epoch, Some(1));
        assert_eq!(unguarded.validate(2, 0, &mut until, None), None);
        assert_eq!(unguarded.seen_epoch, Some(2));
    }

    #[test]
    fn revocation_sources_preserve_manual_entries_and_explicit_unrevokes() {
        let mut principals = RevocationSet::new();
        let manual = "s-overlap".to_string();
        let file_only = "s-file".to_string();
        assert!(principals.is_empty());

        // A manual revoke before a matching file entry survives file removal.
        assert!(principals.insert(manual.clone()));
        principals.replace_file(HashSet::from([manual.clone(), file_only.clone()]));
        assert!(principals.contains(&file_only));
        principals.replace_file(HashSet::new());
        assert!(principals.contains(&manual));
        assert!(!principals.contains(&file_only));
        assert!(principals.remove(&manual));
        assert!(!principals.remove(&manual));
        assert!(principals.is_empty());

        // File first: recording a manual source still returns false while effective,
        // yet that source must remain after a later empty file replacement.
        principals.replace_file(HashSet::from([manual.clone()]));
        assert!(!principals.insert(manual.clone()));
        principals.replace_file(HashSet::new());
        assert!(principals.contains(&manual));

        // Explicit un-revoke clears both sources. A later file load can reassert it.
        principals.replace_file(HashSet::from([manual.clone()]));
        assert!(principals.remove(&manual));
        assert!(!principals.contains(&manual));
        principals.replace_file(HashSet::from([manual.clone()]));
        assert!(principals.contains(&manual));
        principals.replace_file(HashSet::new());
        assert!(principals.is_empty());

        let pk = [0x11; 32];
        let derived = identity_principal(&pk);
        let mut identities = RevocationSet::new();
        assert_eq!(
            insert_identity_revocation(&mut identities, &mut principals, pk),
            (true, true)
        );
        identities.replace_file(HashSet::from([pk]));
        principals.replace_file(HashSet::from([derived.clone()]));
        identities.replace_file(HashSet::new());
        principals.replace_file(HashSet::new());
        assert!(identities.contains(&pk));
        assert!(principals.contains(&derived));

        // An identity already present can still restrict its cleared principal.
        // The first answer stays false; the separate change answer must advance
        // the caller's epoch and wake tails.
        assert!(principals.remove(&derived));
        assert_eq!(
            insert_identity_revocation(&mut identities, &mut principals, pk),
            (false, true)
        );
        assert_eq!(
            insert_identity_revocation(&mut identities, &mut principals, pk),
            (false, false)
        );

        assert!(identities.remove(&pk));
        assert!(principals.remove(&derived));
        identities.replace_file(HashSet::from([pk]));
        principals.replace_file(HashSet::from([derived.clone()]));
        assert_eq!(
            insert_identity_revocation(&mut identities, &mut principals, pk),
            (false, false)
        );
        identities.replace_file(HashSet::new());
        principals.replace_file(HashSet::new());
        assert!(identities.contains(&pk));
        assert!(principals.contains(&derived));
        assert!(identities.remove(&pk));
        assert!(principals.remove(&derived));
        assert!(identities.is_empty() && principals.is_empty());
    }

    #[test]
    fn monotone_clock_and_revocation_file_enforcement() {
        let (_tmp, b, mut h, _sock) = filled("revfile", 1);
        let t1 = b.now_ms();
        let t2 = b.now_ms();
        assert!(t1 > 0 && t2 >= t1, "broker clock must be non-decreasing");

        let rev_path =
            std::env::temp_dir().join(format!("asb_revfile_{}_{}.txt", std::process::id(), t1));
        let pk_hex = "11".repeat(32);
        std::fs::write(
            &rev_path,
            format!("# comment line\n{pk_hex}\ns-revokedsession\n"),
        )
        .unwrap();
        let count = b.load_revocation_file(&rev_path).unwrap();
        let _ = std::fs::remove_file(&rev_path);
        assert_eq!(count, 2);
        let pk = [0x11u8; 32];
        assert!(b.is_identity_revoked(&pk));
        assert!(b.is_principal_revoked(&crate::identity_principal(&pk)));
        assert!(b.is_principal_revoked("s-revokedsession"));
        assert!(b.unrevoke_principal("s-revokedsession"));
        assert!(!b.is_principal_revoked("s-revokedsession"));
        h.shutdown();
    }

    #[cfg(unix)]
    #[test]
    fn credit_windowed_pipelined_publish_and_replicated_watermark_integration() {
        let pid = std::process::id();
        let f_log = format!("/tmp/asb_unit_rep_f_{pid}.log");
        let f_rep = format!("{f_log}.replica");
        let l_log = format!("/tmp/asb_unit_rep_l_{pid}.log");
        let l_sock = format!("/tmp/asb_unit_rep_l_{pid}.sock");
        let _ = std::fs::remove_file(&f_log);
        let _ = std::fs::remove_file(&f_rep);
        let _ = std::fs::remove_file(&l_log);
        let _ = std::fs::remove_file(&l_sock);
        let _tmp = Cleanup(vec![f_log.clone(), f_rep, l_log.clone(), l_sock.clone()]);

        let follower = Broker::open_replica_with(&f_log, Durability::Relaxed).unwrap();
        let mut f_handle = follower.serve_tcp("127.0.0.1:0").unwrap();
        let f_addr = f_handle.tcp_addr().unwrap().to_string();

        let leader = Broker::open_replicated(&l_log, &[f_addr], 1).unwrap();
        let mut l_handle = leader.serve(&l_sock).unwrap();

        let mut client = crate::Client::connect(&l_sock).unwrap();
        let payloads: Vec<Vec<u8>> = (0..128)
            .map(|i| format!("msg-{i:04}").into_bytes())
            .collect();
        let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        let acks = client.publish_pipelined(77, "/t/pipe", &refs, 32).unwrap();
        assert_eq!(acks.len(), 128);
        for (i, &(off, deduped)) in acks.iter().enumerate() {
            assert_eq!(off, i as u64);
            assert!(!deduped);
        }
        assert_eq!(leader.head(), 128);
        assert_eq!(leader.visible_head(), 128);
        assert_eq!(
            leader
                .shared
                .log
                .lock()
                .unwrap()
                .committed_watermark_offset(),
            Offset(128)
        );
        assert_eq!(follower.head(), 128);

        l_handle.shutdown();
        f_handle.shutdown();
    }

    /// A BATCH THAT ROLLED BACK DECIDED NOTHING FOR GOOD. The distinct-subject bound
    /// counts the batch's own new subjects, so a record refused at the bound behind
    /// one that took the producer's last slot was judged against a record that, once
    /// the commit fails, never landed — and resent, it lands. Its refusal is therefore
    /// not known to be permanent (`ERR_REFUSED`), though it keeps its own message.
    #[cfg(unix)]
    #[test]
    fn a_refusal_inside_a_batch_that_rolled_back_is_not_permanent() {
        use crate::store::MAX_SUBJECTS_PER_PRODUCER;
        let log = format!("/tmp/asb_unit_rollback_{}.log", std::process::id());
        let _ = std::fs::remove_file(&log);
        let _tmp = Cleanup(vec![log.clone()]);
        let b = Broker::open(&log).unwrap();
        // Producer 1 one slot short of its bound, in one batch (one fsync).
        {
            let mut l = b.shared.log.lock().unwrap();
            for seq in 0..MAX_SUBJECTS_PER_PRODUCER as u64 - 1 {
                l.stage_publish(1, seq, format!("/t/{seq}"), vec![])
                    .unwrap();
            }
            l.commit_batch().unwrap();
            b.shared.head.store(l.head().0, Ordering::Relaxed);
            b.shared.log_head.store(l.head().0, Ordering::Relaxed);
        }
        let publish = |producer_seq: u64, subject: &str| {
            let (ack, rx) = mpsc::channel();
            let kind = WriteKind::Publish {
                producer_id: 1,
                producer_seq,
                subject: subject.to_string(),
                body: vec![],
            };
            (WriteOp { kind, ack }, rx)
        };

        // `/t/last` takes the last slot; `/t/behind` is refused at the bound for it;
        // then the fsync fails and the batch rolls back.
        b.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        let (last, last_ack) = publish(1 << 20, "/t/last");
        let (behind, behind_ack) = publish((1 << 20) + 1, "/t/behind");
        process_batch(&b.shared, vec![last, behind]);
        let (code, _) = last_ack.recv().unwrap().unwrap_err();
        assert_eq!(code, ERR_REFUSED, "the record the fsync took down");
        let (code, msg) = behind_ack.recv().unwrap().unwrap_err();
        assert!(msg.contains("distinct subjects"), "its own words: {msg}");
        assert_eq!(
            code, ERR_REFUSED,
            "judged against a rolled-back batch: {msg}"
        );

        // Why: the slot it was refused for is free again, so the same record lands.
        b.inject_faults(InjectedFaults::default());
        let (again, again_ack) = publish((1 << 20) + 1, "/t/behind");
        process_batch(&b.shared, vec![again]);
        assert!(again_ack.recv().unwrap().is_ok(), "the resend landed");

        // The control: the same refusal in a batch that COMMITS keeps the code that
        // names it — the producer is now at its bound for real.
        let (over, over_ack) = publish((1 << 20) + 2, "/t/over");
        process_batch(&b.shared, vec![over]);
        let (code, msg) = over_ack.recv().unwrap().unwrap_err();
        assert_eq!(code, ERR_SUBJECT_BOUND, "a committed batch's bound: {msg}");
    }
    /// Cached deliveries keep the full offset, including bit31 and all high bits.
    #[test]
    fn delivery_crc_cache_keeps_the_entire_offset() {
        let root = crate::test_scratch::Scratch::new("crc-key");
        // Unserved broker: owned log and writer only, no listener or daemon process.
        let broker = Broker::open_with(root.path().join("log"), Durability::Relaxed).unwrap();
        let mut stream = Vec::<u8>::new();
        let record = |offset, body: &[u8]| BrokerRecord {
            seq: Offset(offset),
            producer_id: 1,
            producer_seq: 1,
            subject: "/t/cache".into(),
            body: body.to_vec(),
            commit: None,
        };
        let (bit31, high32) = {
            let mut cached = |rec: &BrokerRecord| {
                let mut out = Vec::new();
                queue_delivery_cached(&broker.shared, &mut stream, &mut out, rec).unwrap();
                let expected = astream_wire::Frame::new(encode_response(&Response::Delivery {
                    offset: rec.seq.0,
                    subject: rec.subject.clone(),
                    body: rec.body.clone(),
                }))
                .encode()
                .unwrap();
                out == expected && astream_wire::Frame::decode(&out).is_ok_and(|d| d.is_some())
            };
            let zero = record(0, b"zero");
            let ordinary = record(17, b"ordinary");
            assert!(cached(&zero), "empty cache and offset zero");
            assert!(cached(&zero), "offset zero cache hit");
            assert!(cached(&ordinary), "different-slot cache miss");
            assert!(cached(&ordinary), "ordinary cache hit");
            let bit31 = cached(&record(17 + (1u64 << 31), b"different bit31 record"));
            let high32 = cached(&record(17 + (1u64 << 32), b"different high32 record"));
            (bit31, high32)
        };
        // Counterfactual fork deliveries keep their own CRC and never seed the live cache.
        let live = record(9, b"live");
        let fork = record(9, b"counterfactual");
        let mut fork_out = Vec::new();
        queue_delivery(&mut stream, &mut fork_out, &fork).unwrap();
        let mut live_out = Vec::new();
        queue_delivery_cached(&broker.shared, &mut stream, &mut live_out, &live).unwrap();
        let live_expected = astream_wire::Frame::new(encode_response(&Response::Delivery {
            offset: live.seq.0,
            subject: live.subject.clone(),
            body: live.body.clone(),
        }))
        .encode()
        .unwrap();
        assert_eq!(
            (bit31, high32, live_out == live_expected),
            (true, true, true)
        );
    }

    /// Run `f` on its own thread and wait for it — a one-minute HANG DETECTOR, never a
    /// latency budget: a shutdown that wedges fails here instead of hanging the suite.
    fn within_a_minute<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{what} did not return within a minute"))
    }

    /// A STRANGER listener that takes every connection, as a live server does: accept
    /// and read each until one SENDS a byte — the test's sentinel, dialed once the
    /// shutdown under test has returned — and report how many came before it. A
    /// wakeup dial sends nothing, so its read ends at EOF when shutdown drops it.
    /// Returns the receiver of that count.
    fn stranger<S: Read>(
        mut accept: impl FnMut() -> io::Result<S> + Send + 'static,
    ) -> Receiver<usize> {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut dials = 0;
            loop {
                let mut conn = accept().unwrap();
                if conn.read(&mut [0u8; 1]).unwrap_or(0) == 1 {
                    break;
                }
                dials += 1;
            }
            let _ = tx.send(dials);
        });
        rx
    }

    /// The stranger's count, under the hang detector, once `sentinel` sent its byte.
    fn stranger_took(took: &Receiver<usize>, mut sentinel: impl Write) -> usize {
        sentinel.write_all(b"!").unwrap();
        took.recv_timeout(Duration::from_secs(60))
            .expect("the stranger never saw the sentinel")
    }

    /// A wakeup dial that CONNECTS is no proof it reached THIS acceptor. On macOS/BSD
    /// a socket bound to `127.0.0.1:P` beside a broker's `0.0.0.0:P` — a legal bind:
    /// std, OpenSSH `-L` and most servers set `SO_REUSEADDR` — takes every loopback
    /// connection to P, and loopback is what shutdown dials for a wildcard bind
    /// (`asb serve 0.0.0.0:<port> --tcp --allow-remote`). Here the handle's dial
    /// address is pointed at such a stranger directly: the same end state, without
    /// opening a wildcard listener on the test host. Shutdown redialed the stranger
    /// forever when any connected dial reset its give-up count; it must abandon its own
    /// acceptor after [`ACCEPT_WAKE_ATTEMPTS`] rounds its acceptor never answered.
    #[test]
    fn a_tcp_wake_dial_another_listener_answers_is_not_redialed_forever() {
        let root = crate::test_scratch::Scratch::new("tcp-stranger");
        let b = Broker::open_with(root.path().join("log"), Durability::Relaxed).unwrap();
        let mut h = b.serve_tcp("127.0.0.1:0").unwrap();
        assert!(h.blocks_in_accept());
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let theirs = listener.local_addr().unwrap();
        let took = stranger(move || listener.accept().map(|(s, _)| s));
        let own = match &mut h.endpoint {
            Endpoint::Tcp { dial, .. } => std::mem::replace(dial, theirs),
            #[cfg(unix)]
            Endpoint::Unix { .. } => unreachable!("serve_tcp made a TCP endpoint"),
        };
        within_a_minute("shutdown with a stranger answering its dials", move || {
            h.shutdown()
        });
        let s = b.stats();
        assert_eq!(
            (s.acceptors_abandoned, s.accepted),
            (1, 0),
            "(abandoned, accepted): the acceptor was never reached, and shutdown gave up"
        );
        // The stranger took the dials: at least one, at most the bound. (A dial whose
        // handshake the stranger's side finished only after the sentinel's could only
        // lower the count, so neither end of the range can fail by load.)
        let took = stranger_took(&took, TcpStream::connect(theirs).unwrap());
        assert!(
            (1..=ACCEPT_WAKE_ATTEMPTS as usize).contains(&took),
            "the stranger took {took} wakeup dials"
        );
        // The abandoned acceptor is still parked on its own address; a connection
        // that reaches it finds the cleared flags and lets it leave.
        drop(TcpStream::connect(own).unwrap());
    }

    /// The Unix face of the same rule: a `serve` that a second `serve` on the same path
    /// raced past its bind — the other's stale check met this socket between bind()
    /// and listen(), removed it, and bound its own before this one read the path's
    /// inode — records the OTHER socket's inode, so its names pass every identity check
    /// and its wakeup dials connect, to the other broker. Built here directly: the
    /// listener nameless, the endpoint naming a stranger's socket and a wake name
    /// linked to it. Shutdown must give up after [`ACCEPT_WAKE_ATTEMPTS`] unanswered
    /// rounds, and leave the socket path alone — no dial through it ever reached this
    /// acceptor, so it is the other broker's — while removing the wake name it made.
    #[cfg(unix)]
    #[test]
    fn a_unix_wake_dial_another_listener_answers_is_not_redialed_forever() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let k = SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        // Under /tmp: a Darwin TMPDIR can push a socket path past SUN_LEN.
        let ours = format!("/tmp/asb_unit_wk_{pid}_{k}_a.sock");
        let theirs = format!("/tmp/asb_unit_wk_{pid}_{k}_s.sock");
        let (ours_wake, theirs_wake) = (format!("{ours}.wake"), format!("{theirs}.wake"));
        let _tmp = Cleanup(vec![
            ours.clone(),
            ours_wake.clone(),
            theirs.clone(),
            theirs_wake.clone(),
        ]);
        let root = crate::test_scratch::Scratch::new("unix-stranger");
        let b = Broker::open_with(root.path().join("log"), Durability::Relaxed).unwrap();
        let mut h = b.serve(&ours).unwrap();
        assert!(h.blocks_in_accept());
        let listener = UnixListener::bind(&theirs).unwrap();
        let node = node_at(Path::new(&theirs)).unwrap();
        let took = stranger(move || listener.accept().map(|(s, _)| s));
        std::fs::hard_link(&theirs, &theirs_wake).unwrap();
        std::fs::remove_file(&ours).unwrap();
        std::fs::remove_file(&ours_wake).unwrap();
        h.endpoint = Endpoint::Unix {
            path: PathBuf::from(&theirs),
            node: Some(node),
            wake: Some(PathBuf::from(&theirs_wake)),
        };
        within_a_minute("shutdown with a stranger answering its dials", move || {
            h.shutdown()
        });
        let s = b.stats();
        assert_eq!(
            (s.acceptors_abandoned, s.accepted),
            (1, 0),
            "(abandoned, accepted): the acceptor was never reached, and shutdown gave up"
        );
        assert_eq!(
            node_at(Path::new(&theirs)),
            Some(node),
            "the other broker's socket path survives"
        );
        assert_eq!(
            node_at(Path::new(&theirs_wake)),
            None,
            "the wake name this broker made is removed"
        );
        // A Unix connect is queued on the listener before it returns, and the stranger
        // accepts in that order, so the count is exact: one dial a round, every round
        // unanswered.
        let took = stranger_took(&took, UnixStream::connect(&theirs).unwrap());
        assert_eq!(
            took, ACCEPT_WAKE_ATTEMPTS as usize,
            "wakeup dials the stranger took"
        );
    }

    /// The liveness probe answers at once and leaves the stream as it found it. The
    /// probed end has NO read timeout, so a probe that blocked would park for good
    /// (the hang detector says so). Then that end writes 1 MiB, far past a Unix
    /// socket's buffer, before its peer reads a byte: a stream the probe left
    /// non-blocking would fail that write with `WouldBlock` and close, and the peer's
    /// `read_exact` would see the end of the stream short of 1 MiB. Last, a peer that
    /// closed reads as gone.
    #[cfg(unix)]
    #[test]
    fn the_peer_probe_neither_blocks_nor_leaves_the_stream_non_blocking() {
        const LEN: usize = 1 << 20;
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        let (probed, rx) = mpsc::channel();
        let writer = thread::spawn(move || {
            let _ = probed.send(peer_gone(&mut a));
            // `a` closes as this returns, so a failed write ends `b`'s stream short.
            a.write_all(&vec![7u8; LEN]).map_err(|e| e.kind())
        });
        let gone = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the probe blocked on a socket with nothing to read");
        assert!(!gone, "a connected peer read as gone");
        let mut got = vec![0u8; LEN];
        b.read_exact(&mut got)
            .expect("the probed end stopped writing short of 1 MiB");
        assert!(got.iter().all(|&x| x == 7));
        let wrote = within_a_minute("the 1 MiB write", move || writer.join().unwrap());
        assert_eq!(wrote, Ok(()), "the write after the probe failed");

        let (mut c, d) = UnixStream::pair().unwrap();
        drop(d);
        assert!(peer_gone(&mut c), "a closed peer read as connected");
    }

    /// THE LOG LOCK IS HELD WHILE STAGING AND FREE FOR THE WRITE, read straight off the
    /// mutex with `try_lock`, which never waits: with the writer parked at
    /// `pause_next_batch` (staged, the commit not begun) it is held — the negative
    /// control, and the point a reader still waits out, for staging alone — and with the
    /// writer parked at `pause_next_write` (begun, nothing written) it is free. The
    /// readers that use that freedom are `tests/suite/unlocked_write.rs`.
    #[test]
    fn the_log_lock_is_held_while_staging_and_free_for_the_write() {
        let root = crate::test_scratch::Scratch::new("lock-points");
        let b = Broker::open(root.path().join("log")).unwrap();
        let publish = |seq: u64| {
            let shared = Arc::clone(&b.shared);
            thread::spawn(move || {
                let kind = WriteKind::Publish {
                    producer_id: 1,
                    producer_seq: seq,
                    subject: format!("/t/{seq}"),
                    body: vec![],
                };
                submit_blocking(&shared, kind)
            })
        };
        // Armed one at a time: arming a pause replaces one still armed.
        for held in [true, false] {
            let (pause, seq) = if held {
                (b.pause_next_batch(), 1)
            } else {
                (b.pause_next_write(), 2)
            };
            let parked = publish(seq);
            let pause = within_a_minute("the writer to reach the pause", move || {
                assert!(pause.wait(), "the writer stopped before the pause");
                pause
            });
            let busy = matches!(
                b.shared.log.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            );
            assert_eq!(
                busy,
                held,
                "the log lock at the {} pause",
                if held { "staged" } else { "write" }
            );
            pause.resume();
            let acked = within_a_minute("the parked publish's ack", move || parked.join().unwrap());
            assert_eq!(acked, Ok((seq - 1, false)));
        }
    }

    /// IN THE WRITE WINDOW THE LOG REFUSES EVERY OTHER MUTATION. With the writer parked at the
    /// start of a batch's write (`pause_next_write`: begun, the log lock RELEASED), the log
    /// lock can be taken — and the log itself turns away everything that could touch the
    /// file or the batch on its way to it: staging into the batch, an un-batched append, a
    /// second begin, the clean close and retention all fail with the in-flight refusal,
    /// the head unmoved. This is what a broker retention call would meet if it did not
    /// first take the writer's batch lock (`retention_waits_for_a_batch_being_written` in
    /// `tests/suite/unlocked_write.rs` shows that it waits instead). Once resumed, the batch
    /// commits and the log takes mutations again.
    #[test]
    fn in_the_write_window_the_log_refuses_every_other_mutation() {
        let root = crate::test_scratch::Scratch::new("write-window");
        let b = Broker::open(root.path().join("log")).unwrap();
        let pause = b.pause_next_write();
        let shared = Arc::clone(&b.shared);
        let parked = thread::spawn(move || {
            let kind = WriteKind::Publish {
                producer_id: 1,
                producer_seq: 1,
                subject: "/t/a".to_string(),
                body: b"a".to_vec(),
            };
            submit_blocking(&shared, kind)
        });
        let pause = within_a_minute("the writer to reach the paused write", move || {
            assert!(pause.wait(), "the writer stopped before the paused write");
            pause
        });

        let shared = Arc::clone(&b.shared);
        let refusals = within_a_minute("the log lock in the write window", move || {
            let mut log = shared.log.lock().unwrap();
            let mut said: Vec<(&str, String)> = Vec::new();
            match log.stage_publish(2, 1, "/t/b".to_string(), vec![]) {
                Err(StageErr::Io(e)) => said.push(("stage", e.to_string())),
                other => panic!("staging into the batch in flight: {other:?}"),
            }
            let e = log.publish(3, 1, "/t/c".to_string(), vec![]).unwrap_err();
            said.push(("un-batched publish", e.to_string()));
            let e = log.commit_begin().err().expect("a second begin");
            said.push(("second begin", e.to_string()));
            let e = log.close_clean().unwrap_err();
            said.push(("clean close", e.to_string()));
            #[cfg(feature = "retention")]
            {
                let e = log.retain_before(Offset(1)).unwrap_err();
                said.push(("retention", e.to_string()));
            }
            (log.head().0, said)
        });
        let (head, said) = refusals;
        assert_eq!(head, 0, "the batch in flight is not promoted");
        for (what, msg) in &said {
            assert!(
                msg.contains("a batch is being written outside the log lock"),
                "{what} in the write window: {msg}"
            );
        }

        pause.resume();
        let acked = within_a_minute("the parked publish's ack", move || parked.join().unwrap());
        assert_eq!(acked, Ok((0, false)));
        let mut log = b.shared.log.lock().unwrap();
        assert_eq!(log.head().0, 1, "committed");
        assert_eq!(
            log.stage_publish(2, 1, "/t/b".to_string(), vec![]).unwrap(),
            (Offset(1), false),
            "the log stages again once the batch has ended"
        );
        log.abort_batch();
    }

    /// RETENTION AFTER A WRITER PANIC FAILS, AND NEVER HANGS. A writer that panics inside
    /// a batch dies holding the batch lock and poisons it; a retention call after that
    /// panics, as it did when it met the writer's poisoned LOG lock — it does not wait
    /// forever on a lock its own `try_lock` already took (`batch_settled`). The panic
    /// is a thread standing in for the writer: one that takes the batch lock and dies
    /// with it, the way `process_batch` would if a `lock().unwrap()` inside it panicked.
    /// Every call is under the one-minute hang detector, and each retention call's
    /// panic is caught in its own thread, so the broker itself is dropped normally.
    #[cfg(feature = "retention")]
    #[test]
    fn retention_after_a_writer_panic_fails_and_never_hangs() {
        let root = crate::test_scratch::Scratch::new("batch-poisoned");
        let b = Arc::new(Broker::open(root.path().join("log")).unwrap());
        let shared = Arc::clone(&b.shared);
        let died = thread::spawn(move || {
            let _batch = shared.batch.lock().unwrap();
            panic!("the stand-in writer dies inside its batch");
        })
        .join();
        assert!(died.is_err(), "the stand-in writer did not panic");
        assert!(b.shared.batch.is_poisoned(), "the batch lock is poisoned");

        for carrying_state in [false, true] {
            let b2 = Arc::clone(&b);
            let failed = within_a_minute("retention after the writer panicked", move || {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if carrying_state {
                        b2.retain_before_carrying_state(0)
                    } else {
                        b2.retain_before(0)
                    }
                }))
                .is_err()
            });
            assert!(
                failed,
                "retention (carrying state: {carrying_state}) returned past a dead writer"
            );
        }
        assert_eq!(b.stats().retention_waits, 0, "nothing waited");
    }

    /// OFF THE ACK PATH (6E-1a): a broker's compaction copies the kept suffix with no
    /// lock held, so a publish acks while the copy runs, and the finish copies what was
    /// acked meanwhile. The compaction's seam holds the copy at its start
    /// (`compact::gate`); with it held, three publishes go through the writer and each
    /// acks under the one-minute hang detector. Released, the compaction drops six, and
    /// a reopen reads offsets 6 to 12, the three among them. The seam's meter shows the
    /// copy's two flushes: the unlocked one covers the suffix as it stood at the begin,
    /// and the one the finish makes under the locks covers only the three acked during
    /// the copy. NEGATIVE CONTROLS: a compaction holding the batch lock across its copy
    /// fails this (no ack until the copy ends, which the gate never lets it); one whose
    /// unlocked copy does not flush makes its finish flush the whole suffix under the
    /// locks, and fails the meter's assertion.
    #[cfg(feature = "retention")]
    #[test]
    fn a_publish_acks_while_a_compaction_copies() {
        let root = crate::test_scratch::Scratch::new("compact-off-ack");
        let path = root.path().join("log");
        let b = Arc::new(Broker::open(&path).unwrap());
        let shared = Arc::clone(&b.shared);
        let publish = move |seq: u64| {
            let shared = Arc::clone(&shared);
            thread::spawn(move || {
                let kind = WriteKind::Publish {
                    producer_id: 1,
                    producer_seq: seq,
                    subject: "/t/x".to_string(),
                    body: format!("m{seq}").into_bytes(),
                };
                submit_blocking(&shared, kind)
            })
        };
        for seq in 0..10 {
            let acked = publish(seq);
            let acked = within_a_minute("a seed publish", move || acked.join().unwrap());
            assert_eq!(acked, Ok((seq, false)));
        }
        let (gate, held) = crate::compact::gate();
        let meter = Arc::new(crate::compact::Meter::default());
        b.shared
            .log
            .lock()
            .unwrap()
            .arm_compact_seam(crate::compact::Seam {
                gate: Some(gate),
                meter: Some(Arc::clone(&meter)),
                ..crate::compact::Seam::default()
            });
        let len = |p: &std::path::Path| std::fs::metadata(p).unwrap().len();
        let b2 = Arc::clone(&b);
        let compaction = thread::spawn(move || b2.retain_before(6));
        let held = within_a_minute("the compaction's copy to reach the gate", move || {
            assert!(held.wait(), "the compaction ended before its copy");
            held
        });
        // The begin is done and nothing was appended since: the log's length at the begin.
        let begun = len(&path);
        for seq in 10..13 {
            let parked = publish(seq);
            let acked = within_a_minute("a publish's ack while the copy is held", move || {
                parked.join().unwrap()
            });
            assert_eq!(acked, Ok((seq, false)), "acked while the copy is held");
        }
        let taken = len(&path) - begun;
        held.resume();
        let dropped = within_a_minute("the compaction", move || compaction.join().unwrap());
        assert_eq!(dropped.unwrap(), 6);
        {
            let log = b.shared.log.lock().unwrap();
            assert_eq!((log.base(), log.head()), (Offset(6), Offset(13)));
        }
        let kept = len(&path);
        assert_eq!(
            *meter.flushes.lock().unwrap(),
            vec![kept - taken, taken],
            "the unlocked copy flushes the suffix as it stood; the locked finish flushes only \
             what was acked during the copy"
        );
        drop(b);
        let log = BrokerLog::open(&path).unwrap();
        let kept: Vec<(u64, Vec<u8>)> = log
            .read_from(Offset::ZERO)
            .iter()
            .map(|r| (r.seq.0, r.body.clone()))
            .collect();
        let want: Vec<(u64, Vec<u8>)> = (6..13u64)
            .map(|i| (i, format!("m{i}").into_bytes()))
            .collect();
        assert_eq!(
            kept, want,
            "the records acked during the copy are in the file"
        );
    }

    /// ONE COMPACTION AT A TIME (6E-1a): `Shared::compaction` is held across a compaction's
    /// begin, its unlocked copy and its finish, so a second retention call waits for the
    /// first to end and then compacts the log the first left. The first is held at its
    /// copy's start (`compact::gate`), where the lock must read taken; the second is
    /// started, the first released and joined, and the second must then reach the gate
    /// itself, be released and compact from the first's base. Every wait is a one-minute
    /// hang detector. NEGATIVE CONTROL: a compaction that does not take the lock fails
    /// the lock's assertion (and, two of them overlapping, the second's begin replaces the
    /// first's `<log>.kept`, which the first's finish renames over the log: a log that
    /// refuses to open).
    #[cfg(feature = "retention")]
    #[test]
    fn compactions_on_one_broker_run_one_at_a_time() {
        let root = crate::test_scratch::Scratch::new("compact-one-at-a-time");
        let path = root.path().join("log");
        let b = Arc::new(Broker::open(&path).unwrap());
        for seq in 0..10u64 {
            let shared = Arc::clone(&b.shared);
            let acked = within_a_minute("a seed publish", move || {
                let kind = WriteKind::Publish {
                    producer_id: 1,
                    producer_seq: seq,
                    subject: "/t/x".to_string(),
                    body: format!("m{seq}").into_bytes(),
                };
                submit_blocking(&shared, kind)
            });
            assert_eq!(acked, Ok((seq, false)));
        }
        let (gate, held) = crate::compact::gate();
        b.shared
            .log
            .lock()
            .unwrap()
            .arm_compact_seam(crate::compact::Seam {
                gate: Some(gate),
                ..crate::compact::Seam::default()
            });
        let b1 = Arc::clone(&b);
        let first = thread::spawn(move || b1.retain_before(4));
        let held = within_a_minute("the first compaction's copy to reach the gate", move || {
            assert!(held.wait(), "the first compaction ended before its copy");
            held
        });
        assert!(
            matches!(
                b.shared.compaction.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "a compaction's copy runs holding the compaction lock"
        );
        let b2 = Arc::clone(&b);
        let second = thread::spawn(move || b2.retain_before(8));
        held.resume();
        let first = within_a_minute("the first compaction", move || first.join().unwrap());
        assert_eq!(first.unwrap(), 4);
        let held = within_a_minute(
            "the second compaction's copy to reach the gate",
            move || {
                assert!(held.wait(), "the second compaction ended before its copy");
                held
            },
        );
        held.resume();
        let second = within_a_minute("the second compaction", move || second.join().unwrap());
        assert_eq!(
            second.unwrap(),
            4,
            "the second compacts from the first's base"
        );
        {
            let log = b.shared.log.lock().unwrap();
            assert_eq!((log.base(), log.head()), (Offset(8), Offset(10)));
        }
        drop(b);
        let log = BrokerLog::open(&path).unwrap();
        let kept: Vec<(u64, Vec<u8>)> = log
            .read_from(Offset::ZERO)
            .iter()
            .map(|r| (r.seq.0, r.body.clone()))
            .collect();
        let want: Vec<(u64, Vec<u8>)> = (8..10u64)
            .map(|i| (i, format!("m{i}").into_bytes()))
            .collect();
        assert_eq!(kept, want);
    }

    /// THE WRITER'S AUTOMATIC COMPACTION (`set_max_retained_records`) never runs inside a
    /// retention call: while one holds the compaction lock — as it does across its
    /// unlocked copy (`compactions_on_one_broker_run_one_at_a_time`) — a batch over the
    /// cap is acked and leaves the base where it was, and the first batch after the lock
    /// is free compacts. Deterministic: the writer runs a batch's automatic compaction
    /// before it takes the next batch, so a later publish's ack orders the check after
    /// it. NEGATIVE CONTROL: an automatic compaction that does not take the lock moves
    /// the base while the call holds it (and so fails a real call's finish).
    #[cfg(feature = "retention")]
    #[test]
    fn an_automatic_compaction_waits_out_a_retention_call() {
        let root = crate::test_scratch::Scratch::new("compact-auto-one-at-a-time");
        let path = root.path().join("log");
        let b = Arc::new(Broker::open(&path).unwrap());
        let publish = |seq: u64| {
            let shared = Arc::clone(&b.shared);
            let acked = within_a_minute("a publish", move || {
                let kind = WriteKind::Publish {
                    producer_id: 1,
                    producer_seq: seq,
                    subject: "/t/x".to_string(),
                    body: format!("m{seq}").into_bytes(),
                };
                submit_blocking(&shared, kind)
            });
            assert_eq!(acked, Ok((seq, false)));
        };
        for seq in 0..10u64 {
            publish(seq);
        }
        let one = b.shared.compaction.lock().unwrap();
        b.set_max_retained_records(2);
        publish(10);
        publish(11);
        assert_eq!(
            b.shared.log.lock().unwrap().base(),
            Offset::ZERO,
            "an automatic compaction ran while a retention call held the compaction lock"
        );
        drop(one);
        publish(12);
        publish(13);
        let base = b.shared.log.lock().unwrap().base();
        assert!(
            base > Offset::ZERO,
            "the first batch after the lock is free compacts (base {base:?})"
        );
        assert_eq!(b.base(), base.0);
    }

    /// A LOG THAT LATCHES TO STRICT KEEPS MARKING WHAT IT MADE DURABLE. A tail that
    /// attached while the log served Ordered — like a connection whose `Hello` said so —
    /// may hold back what it is delivered until a durable mark covers it (aterm's
    /// notifier pages only those), and nothing on its stream says the tier changed. So
    /// when the device refuses a barrier while serving, the batch it refused is flushed
    /// in full (and every batch after it), and the tail is still sent a durable mark for
    /// each batch it was delivered — not only the idle mark an interval later, an hour
    /// here, which a busy subscription never reaches. Before this, the latch cleared the
    /// flag the tail loop sends durable marks under, and this read waited out its hang
    /// detector. Apple only: elsewhere Ordered is served Strict from the open, and
    /// nothing latches.
    #[cfg(unix)]
    #[test]
    fn a_latched_log_keeps_marking_what_it_made_durable() {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let root = crate::test_scratch::Scratch::new("latch-marks");
        let sock = format!(
            "/tmp/asb_unit_latch_{}_{}.sock",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let _ = std::fs::remove_file(&sock);
        let _tmp = Cleanup(vec![sock.clone()]);
        let b = Broker::open_opts(
            root.path().join("log"),
            OpenOptions::new(Durability::Ordered { horizon: None }),
            || Ok(Prepared::Nothing),
        )
        .unwrap();
        if b.served_durability().served != crate::Tier::Ordered {
            // Only where the barrier does not exist at all: on Apple this log is served
            // Ordered, or the test proves nothing and says so.
            assert_eq!(b.served_durability().reason, crate::ServedReason::NotApple);
            return;
        }
        b.set_idle_mark(Duration::from_secs(3600));
        let _h = b.serve(&sock).unwrap();
        let mut sub = crate::Client::connect(&sock)
            .unwrap()
            .subscribe(0, "/o/>")
            .unwrap();
        // A hang detector, never a budget: the mark this test waits for must come.
        sub.set_read_timeout(Some(Duration::from_secs(60))).unwrap();
        assert!(
            matches!(
                sub.recv_event().unwrap(),
                Some(crate::Event::Mark { next: 0, .. })
            ),
            "the catch-up mark"
        );
        // Read until `off` is delivered and a durable mark covers it, in either order
        // (the mark follows the delivery on one stream, but read both ways round).
        fn delivered_and_marked(sub: &mut crate::Subscription<UnixStream>, off: u64) {
            let (mut delivered, mut marked) = (false, false);
            while !delivered || !marked {
                match sub.recv_event().unwrap().expect("the broker stays up") {
                    crate::Event::Delivery { offset, .. } => {
                        assert_eq!(offset, off);
                        delivered = true;
                    }
                    crate::Event::Mark { durable, .. } => {
                        marked = durable.is_some_and(|d| d > off);
                    }
                }
            }
        }
        let mut c = crate::Client::connect(&sock).unwrap();
        // Served Ordered: the first batch is barriered, not durable, and its delivery is
        // not followed by a durable mark.
        let (first, _) = c.publish(1, 1, "/o/a", b"one").unwrap();
        assert!(b.durable_head() <= first, "barriered, not flushed in full");
        assert!(matches!(
            sub.recv_event().unwrap(),
            Some(crate::Event::Delivery { offset, .. }) if offset == first
        ));
        // The device refuses the next barrier: the log latches, and that batch — and
        // with it the first — is flushed in full before its ack.
        b.shared
            .log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refuse_barrier = true;
        let (second, _) = c.publish(1, 2, "/o/b", b"two").unwrap();
        let served = b.served_durability();
        assert_eq!(
            (served.served, served.reason),
            (
                crate::Tier::Strict,
                crate::ServedReason::LatchedAfterBarrierError
            )
        );
        assert!(b.durable_head() > second, "the latch flushed it in full");
        delivered_and_marked(&mut sub, second);
        // And every batch after the latch, flushed in full like a Strict one's.
        let (third, _) = c.publish(1, 3, "/o/c", b"three").unwrap();
        delivered_and_marked(&mut sub, third);
    }

    /// The held head's tests, deterministic: ops submitted straight to the writer, each
    /// with its own ack channel, so what step 3 sent is IN the channel by the time the
    /// writer stands at `pause_next_flush` — `try_recv` there is exact, not a race with
    /// the flush. Relaxed (the escalation table holds the head on every platform), and
    /// Ordered where it is served.
    mod held_head {
        use super::*;
        use crate::{Escalate, Escalation};
        use std::sync::mpsc::TryRecvError;

        const MINUTE: Duration = Duration::from_secs(60); // a hang detector, never a budget

        fn escalating(tag: &str, durability: Durability) -> (crate::test_scratch::Scratch, Broker) {
            let root = crate::test_scratch::Scratch::new(tag);
            let mut o = OpenOptions::new(durability);
            o.escalation = Escalation::new([Escalate::Subject("/e/>".into())]);
            let b =
                Broker::open_opts(root.path().join("log"), o, || Ok(Prepared::Nothing)).unwrap();
            (root, b)
        }

        fn submit(b: &Broker, kind: WriteKind) -> Receiver<AckResult> {
            let (tx, rx) = mpsc::channel();
            b.shared
                .writes
                .send(Queued::Op(WriteOp { kind, ack: tx }))
                .unwrap();
            rx
        }

        fn publish(producer_id: u64, producer_seq: u64, subject: &str) -> WriteKind {
            WriteKind::Publish {
                producer_id,
                producer_seq,
                subject: subject.to_string(),
                body: b"x".to_vec(),
            }
        }

        /// The tiers whose head an escalated record holds on this platform.
        fn holding() -> Vec<Durability> {
            let mut tiers = vec![Durability::Relaxed];
            if cfg!(target_vendor = "apple") {
                tiers.push(Durability::Ordered { horizon: None });
            }
            tiers
        }

        /// In the window between step 3 and the full flush — the window the held head is
        /// FOR — the op before the escalated record is answered, and the escalated op and
        /// the one after it are NOT: the visible head stands at the escalated record and
        /// the durable head below it. Then the flush answers them and releases the head.
        /// (Asserted after an ack arrives instead, a broker that acked first and flushed a
        /// moment later passes whenever the test is slower than the flush.)
        #[test]
        fn nothing_the_head_holds_is_answered_before_the_flush() {
            for durability in holding() {
                let (_root, b) = escalating("held-acks", durability);
                assert_ne!(b.served_durability().served, crate::Tier::Strict);
                // Hold the writer on a first batch, so the next three ops form ONE batch.
                let gate = b.pause_next_batch();
                let first = submit(&b, publish(1, 1, "/o/a"));
                assert!(gate.wait_at_most(MINUTE), "the first batch staged");
                let plain = submit(&b, publish(2, 1, "/o/b"));
                let esc = submit(&b, publish(2, 2, "/e/b"));
                let after = submit(&b, publish(2, 3, "/o/c"));
                let flush = b.pause_next_flush();
                gate.resume();
                assert_eq!(first.recv_timeout(MINUTE).unwrap(), Ok((0, false)));
                assert!(
                    flush.wait_at_most(MINUTE),
                    "{durability:?}: a flush is owed"
                );
                assert_eq!(plain.try_recv(), Ok(Ok((1, false))), "{durability:?}");
                assert_eq!(esc.try_recv(), Err(TryRecvError::Empty), "{durability:?}");
                assert_eq!(after.try_recv(), Err(TryRecvError::Empty), "{durability:?}");
                assert_eq!(
                    b.visible_head(),
                    2,
                    "{durability:?}: held at the escalated record"
                );
                assert!(
                    b.durable_head() <= 2,
                    "{durability:?}: nothing above it is durable"
                );
                flush.resume();
                assert_eq!(esc.recv_timeout(MINUTE).unwrap(), Ok((2, false)));
                assert_eq!(after.recv_timeout(MINUTE).unwrap(), Ok((3, false)));
                assert_eq!(
                    (b.visible_head(), b.durable_head()),
                    (4, 4),
                    "{durability:?}"
                );
            }
        }

        /// A WILL REGISTRATION in a held batch is answered by its own commit — at step 3,
        /// not after the flush — so a flush that then FAILS cannot tell the connection
        /// "will was not persisted" while the log keeps the `/a/will` record (which the
        /// next open would fire: a goodbye for a producer told it registered nothing).
        /// The escalated publish beside it is refused "outcome unknown", as it must be.
        #[test]
        fn a_will_registration_in_a_held_batch_is_answered_by_its_commit() {
            for durability in holding() {
                let (_root, b) = escalating("held-will", durability);
                // Every full flush fails; Relaxed's batch issues none and Ordered's issues
                // a barrier, so only the held head's flush meets the fault.
                b.inject_faults(InjectedFaults {
                    sync: true,
                    truncate: false,
                });
                let gate = b.pause_next_batch();
                let first = submit(&b, publish(1, 1, "/o/a"));
                assert!(gate.wait_at_most(MINUTE), "the first batch staged");
                let esc = submit(&b, publish(1, 2, "/e/a"));
                let will = submit(
                    &b,
                    WriteKind::WillRegister {
                        conn_id: None,
                        will: WillRecord {
                            producer_id: 7,
                            producer_seq: 1,
                            subject: "/w/gone".to_string(),
                            body: b"bye".to_vec(),
                        },
                    },
                );
                gate.resume();
                assert_eq!(first.recv_timeout(MINUTE).unwrap(), Ok((0, false)));
                let refused = esc.recv_timeout(MINUTE).unwrap().unwrap_err();
                assert_eq!(refused.0, ERR_REFUSED);
                assert!(
                    refused.1.contains("durability flush failed"),
                    "{}",
                    refused.1
                );
                assert_eq!(
                    will.recv_timeout(MINUTE).unwrap(),
                    Ok((2, false)),
                    "{durability:?}: the will is on the log, and the connection is told so"
                );
            }
        }
    }

    /// The writer's stop ([`stop_writer`]), driven straight through its queue so what is
    /// queued around the stop is exact, not a race with shutdown.
    mod writer_stop {
        use super::*;

        const MINUTE: Duration = Duration::from_secs(60); // a hang detector, never a budget

        fn submit(b: &Broker, producer_seq: u64) -> Receiver<AckResult> {
            let (tx, rx) = mpsc::channel();
            let kind = WriteKind::Publish {
                producer_id: 1,
                producer_seq,
                subject: format!("/s/{producer_seq}"),
                body: b"x".to_vec(),
            };
            b.shared
                .writes
                .send(Queued::Op(WriteOp { kind, ack: tx }))
                .unwrap();
            rx
        }

        /// THE STOP ENDS THE WRITER AFTER EVERYTHING QUEUED BEFORE IT AND BESIDE IT.
        /// With the writer held inside a batch, an op, the stop and an op that raced
        /// shutdown in behind it are queued; once the batch goes on, all three ops are
        /// committed and answered at their offsets, and the writer thread returns — a
        /// join under a one-minute hang detector — having woken on no timer. A writer
        /// that left at the stop would drop the op queued before it in the same drain
        /// (its ack `Disconnected`); one that committed up to the stop and left, the op
        /// behind it.
        #[test]
        fn a_stopped_writer_commits_what_is_queued_then_returns() {
            let root = crate::test_scratch::Scratch::new("writer-stop");
            let b = Broker::open(root.path().join("log")).unwrap();
            let gate = b.pause_next_batch();
            let first = submit(&b, 1);
            assert!(gate.wait_at_most(MINUTE), "the first batch staged");
            let before = submit(&b, 2);
            b.shared.running.store(false, Ordering::Relaxed);
            b.shared.writes.send(Queued::Stop).unwrap();
            let after = submit(&b, 3);
            let writer = b
                .shared
                .writer
                .lock()
                .unwrap()
                .take()
                .expect("the writer runs");
            gate.resume();
            assert!(
                within_a_minute("the stopped writer", move || writer.join().is_ok()),
                "the writer panicked"
            );
            assert_eq!(first.try_recv(), Ok(Ok((0, false))));
            assert_eq!(
                before.try_recv(),
                Ok(Ok((1, false))),
                "queued before the stop"
            );
            assert_eq!(
                after.try_recv(),
                Ok(Ok((2, false))),
                "queued after the stop"
            );
            assert_eq!(b.stats().writer_wakeups, 0, "the writer woke on a timer");
        }
    }

    /// THE POISON IS READ WITHOUT THE LOG LOCK. A failed rollback poisons a Strict log;
    /// then the writer is parked inside a batch, HOLDING the log lock — read straight off
    /// the mutex with `try_lock`, the NEGATIVE CONTROL: a read that took the lock would
    /// wait here until the batch resumes. Meanwhile `poisoned`, `flush_counts` and
    /// `wait_poisoned` all answer, and so does a fresh connection's `Hello`, whose health
    /// entry names the same poison — each under a one-minute hang detector, never a
    /// latency budget. Before this, `poisoned` took the log lock, which the writer holds
    /// across a rollback's truncate and a failed flush's identity I/O: the look waited
    /// out the very failure it was looking for.
    #[cfg(unix)]
    #[test]
    fn the_poison_and_the_flush_counts_are_read_without_the_log_lock() {
        let root = crate::test_scratch::Scratch::new("vitals-unlocked");
        let sock = root.path().join("sock");
        let b = Arc::new(Broker::open(root.path().join("log")).unwrap());
        let _h = b.serve(&sock).unwrap();
        let mut c = crate::Client::connect(&sock).unwrap();
        c.publish(1, 1, "/o/a", b"before").unwrap();
        b.inject_faults(InjectedFaults {
            sync: true,
            truncate: true,
        });
        c.publish(1, 2, "/o/a", b"orphan").unwrap_err();
        b.inject_faults(InjectedFaults::default());
        let why = b.poisoned().expect("the failed rollback poisoned the log");

        let pause = b.pause_next_batch();
        c.send_publish(1, 3, "/o/a", b"parks the writer").unwrap();
        let pause = within_a_minute("the writer to reach the pause", move || {
            assert!(pause.wait(), "the writer stopped before the pause");
            pause
        });
        assert!(
            matches!(
                b.shared.log.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "the parked writer holds the log lock"
        );
        let reader = Arc::clone(&b);
        let (poisoned, failed, waited, told) =
            within_a_minute("the vitals, read beside a held log lock", move || {
                let mut fresh = crate::Client::connect(&sock).unwrap();
                fresh.hello().unwrap();
                (
                    reader.poisoned(),
                    reader.flush_counts().failed,
                    reader.wait_poisoned(Duration::from_secs(3600)),
                    fresh.broker_health().cloned(),
                )
            });
        assert_eq!(poisoned.as_deref(), Some(why.as_str()));
        assert_eq!(failed, 1, "the rollback's failed flush is counted");
        assert_eq!(waited.as_deref(), Some(why.as_str()), "no wait: poisoned");
        let told = told.expect("this broker's Hello carries the health entry");
        assert_eq!(told.poisoned.as_deref(), Some(why.as_str()));
        assert_eq!(told.flushes.failed, 1);
        pause.resume();
        let err = c.recv_publish_ack().unwrap_err();
        assert!(
            crate::broker_refusal(&err).is_some_and(crate::BrokerRefusal::is_poisoned),
            "{err}"
        );
    }

    /// Spin until `cond` holds — a one-minute HANG DETECTOR, never a latency budget.
    fn until_a_minute(what: &str, mut cond: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !cond() {
            assert!(Instant::now() < deadline, "{what}: never happened");
            thread::yield_now();
        }
    }

    /// A `Hello` THAT NAMES THE POISON NAMES THE LATCH WITH IT. On Relaxed with `/e/>`
    /// escalated (a full flush the table owes, on every platform), an escalated
    /// publish's full flush fails: the log latches to Strict and is poisoned, in one
    /// publication. The test HOLDS the copy `Hello` answers from (`Shared::hello`)
    /// across the failure, so the writer cannot refresh it — and the copy still says
    /// Relaxed once the poison is out (the window is real: the NEGATIVE CONTROL), while
    /// the answer assembled from it and the vitals (`hello_answer`) says poisoned AND
    /// served Strict (`LatchedAfterFlushFailure`). Before, the tier came from the copy
    /// alone, and one answer told the poison beside the tier from before the latch.
    #[cfg(unix)]
    #[test]
    fn a_hello_that_names_the_poison_names_the_latch_with_it() {
        let root = crate::test_scratch::Scratch::new("hello-latch");
        let sock = root.path().join("sock");
        let mut o = OpenOptions::new(Durability::Relaxed);
        o.escalation = crate::Escalation::new([crate::Escalate::Subject("/e/>".into())]);
        let b = Arc::new(
            Broker::open_opts(root.path().join("log"), o, || Ok(Prepared::Nothing)).unwrap(),
        );
        let _h = b.serve(&sock).unwrap();
        let mut c = crate::Client::connect(&sock).unwrap();
        c.publish(1, 1, "/o/a", b"page cache").unwrap();
        b.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        let held = b.shared.hello.lock().unwrap();
        let (tx, refused) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(c.publish(1, 2, "/e/a", b"a person's word").is_err());
        });
        let waiter = Arc::clone(&b);
        let why = within_a_minute("the poison", move || {
            waiter.wait_poisoned(Duration::from_secs(3600))
        })
        .expect("the failed full flush poisoned the log");
        assert_eq!(
            held.ext.durability.map(|s| s.served),
            Some(crate::proto::Tier::Relaxed),
            "the copy is not refreshed yet"
        );
        let told = hello_answer(held.clone(), &b.shared.vitals);
        drop(held);
        let health = told.ext.health.expect("the health entry");
        assert_eq!(health.poisoned.as_deref(), Some(why.as_str()));
        let served = told.ext.durability.expect("the tier served");
        assert_eq!(
            (served.requested, served.served, served.reason),
            (
                crate::proto::Tier::Relaxed,
                crate::proto::Tier::Strict,
                crate::proto::ServedReason::LatchedAfterFlushFailure
            ),
            "the poison is told beside the latched tier"
        );
        assert!(
            refused.recv_timeout(Duration::from_secs(60)).unwrap(),
            "the escalated publish is refused"
        );
        assert_eq!(b.served_durability(), served);
    }

    /// `wait_poisoned` SLEEPS ON THE POISON AND ON THE STOP. A waiter given an hour is
    /// seen PARKED first (the vitals' `parked` seam, counted under their lock before it
    /// sleeps, so what the test does next is something it must be woken for); then a
    /// failed rollback poisons the log, and it wakes with the reason. On a second,
    /// healthy broker a parked waiter wakes with `None` when the last listener shuts
    /// down. Each under a one-minute hang detector against its hour: a stop that forgot
    /// to wake the vitals leaves its waiter asleep, and fails here (a waiter that has
    /// not parked yet reads the stop without being woken, which is why it must be seen
    /// parked). NEGATIVE CONTROL: on a healthy broker a zero timeout returns `None` at
    /// once.
    #[cfg(unix)]
    #[test]
    fn wait_poisoned_wakes_at_the_poison_and_at_the_stop() {
        let parked_waiter = |b: &Arc<Broker>| {
            let w = Arc::clone(b);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(w.wait_poisoned(Duration::from_secs(3600)));
            });
            until_a_minute("the waiter parks", || {
                b.shared.vitals.parked.load(Ordering::SeqCst) == 1
            });
            rx
        };

        let root = crate::test_scratch::Scratch::new("wait-poisoned");
        let sock = root.path().join("sock");
        let b = Arc::new(Broker::open(root.path().join("log")).unwrap());
        let _h = b.serve(&sock).unwrap();
        assert_eq!(b.wait_poisoned(Duration::ZERO), None, "healthy: no poison");
        let mut c = crate::Client::connect(&sock).unwrap();
        c.publish(1, 1, "/o/a", b"before").unwrap();
        let rx = parked_waiter(&b);
        b.inject_faults(InjectedFaults {
            sync: true,
            truncate: true,
        });
        c.publish(1, 2, "/o/a", b"orphan").unwrap_err();
        let woke = rx
            .recv_timeout(Duration::from_secs(60))
            .expect("the poison woke the waiter");
        assert!(woke.is_some());
        assert_eq!(woke, b.poisoned());

        let root = crate::test_scratch::Scratch::new("wait-stopped");
        let b = Arc::new(Broker::open(root.path().join("log")).unwrap());
        let mut h = b.serve(root.path().join("sock")).unwrap();
        let rx = parked_waiter(&b);
        h.shutdown();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(60))
                .expect("the stop woke the waiter"),
            None
        );
    }

    /// A POISON THAT LANDS AFTER A SNAPSHOT IS TAKEN IS WRITTEN AT ONCE, not a period
    /// later. `keep_health` with an hour's period writes its first snapshot — healthy —
    /// and the test holds it there, before its thread starts (the thread's slot, held);
    /// a failed rollback then poisons the log, and the thread is let go. It must name
    /// the poison under a one-minute hang detector, against its hour: the snapshot it
    /// last wrote did not name it. Before, the thread looked at the poison itself before
    /// its first wait, found it set, took it as written, and slept the hour while the
    /// sidecar said `poisoned=no` of a log that refused every write. A periodic write
    /// has the same window: the poison landing while it creates, writes and renames the
    /// file — slow on exactly the failing disk that poisons a log.
    #[cfg(unix)]
    #[test]
    fn a_poison_after_the_snapshot_is_written_at_once_not_a_period_later() {
        let root = crate::test_scratch::Scratch::new("health-race");
        let log = root.path().join("log");
        let sock = root.path().join("sock");
        let b = Arc::new(Broker::open(&log).unwrap());
        let _h = b.serve(&sock).unwrap();
        let mut c = crate::Client::connect(&sock).unwrap();
        c.publish(1, 1, "/o/a", b"before").unwrap();

        let slot = b.shared.health_thread.lock().unwrap();
        let keeper = Arc::clone(&b);
        let (tx, kept) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(keeper.keep_health(Duration::from_secs(3600)).is_ok());
        });
        let read = |log: &Path| crate::health::read_health(log).unwrap();
        until_a_minute("the first snapshot is published", || read(&log).is_some());
        assert_eq!(read(&log).unwrap().health.poisoned, None, "healthy");

        b.inject_faults(InjectedFaults {
            sync: true,
            truncate: true,
        });
        c.publish(1, 2, "/o/a", b"orphan").unwrap_err();
        let why = b.poisoned().expect("the failed rollback poisoned the log");
        drop(slot);
        assert!(kept.recv_timeout(Duration::from_secs(60)).unwrap());
        until_a_minute("the sidecar names the poison, an hour early", || {
            read(&log).unwrap().health.poisoned.as_deref() == Some(why.as_str())
        });
    }

    #[cfg(unix)]
    #[test]
    fn pipelined_multi_follower_replication_confirms_across_three_replicas() {
        let root = crate::test_scratch::Scratch::new("pipelined-multi-replica");
        let f1_log = root.path().join("f1.log");
        let f2_log = root.path().join("f2.log");
        let f3_log = root.path().join("f3.log");
        let l_log = root.path().join("leader.log");
        let l_sock = root.path().join("leader.sock");

        let f1 = Broker::open_replica_with(&f1_log, Durability::Relaxed).unwrap();
        let f2 = Broker::open_replica_with(&f2_log, Durability::Relaxed).unwrap();
        let f3 = Broker::open_replica_with(&f3_log, Durability::Relaxed).unwrap();
        let mut h1 = f1.serve_tcp("127.0.0.1:0").unwrap();
        let mut h2 = f2.serve_tcp("127.0.0.1:0").unwrap();
        let mut h3 = f3.serve_tcp("127.0.0.1:0").unwrap();
        let addrs = vec![
            h1.tcp_addr().unwrap().to_string(),
            h2.tcp_addr().unwrap().to_string(),
            h3.tcp_addr().unwrap().to_string(),
        ];

        let leader = Broker::open_replicated(&l_log, &addrs, 3).unwrap();
        let mut lh = leader.serve(&l_sock).unwrap();

        let mut client = crate::Client::connect(&l_sock).unwrap();
        let payloads: Vec<Vec<u8>> = (0..160)
            .map(|i| format!("multi-{i:04}").into_bytes())
            .collect();
        let refs: Vec<&[u8]> = payloads.iter().map(Vec::as_slice).collect();
        let acks = client.publish_pipelined(99, "/t/multi", &refs, 64).unwrap();
        assert_eq!(acks.len(), 160);
        assert_eq!(leader.head(), 160);
        assert_eq!(leader.visible_head(), 160);
        assert_eq!(f1.head(), 160);
        assert_eq!(f2.head(), 160);
        assert_eq!(f3.head(), 160);

        lh.shutdown();
        h1.shutdown();
        h2.shutdown();
        h3.shutdown();
    }

    #[cfg(all(unix, feature = "retention"))]
    #[test]
    fn automatic_state_carrying_compaction_caps_retained_records_and_preserves_state() {
        let root = crate::test_scratch::Scratch::new("auto-retain");
        let log_path = root.path().join("auto.log");
        let sock_path = root.path().join("auto.sock");

        let b = Broker::open_with(&log_path, Durability::Relaxed).unwrap();
        b.set_max_retained_records(20);
        assert_eq!(b.max_retained_records(), 20);
        let mut h = b.serve(&sock_path).unwrap();

        let mut c = crate::Client::connect(&sock_path).unwrap();
        // Publish a quiet subject at offset 0 that will fall below the compaction cut.
        c.publish(1, 1, "/t/quiet", b"sticky-state").unwrap();
        // Publish 40 noisy records so automatic compaction triggers repeatedly.
        for seq in 2..=41 {
            c.publish(1, seq, "/t/noisy", b"payload").unwrap();
        }
        c.commit("/g/consumers", 35).unwrap();

        let stats = b.stats();
        assert!(
            b.base() > 0 && stats.log.base.0 == b.base(),
            "automatic compaction must advance base (got base={})",
            b.base()
        );
        assert!(
            stats.log.retained_records <= 20,
            "retained_records={} exceeded cap 20",
            stats.log.retained_records
        );

        // Verify quiet subject's Last record survived compaction via `<log>.state`.
        let (rows, _) = c.last("/t/quiet", "", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].1, "/t/quiet");
        assert_eq!(rows[0].2, b"sticky-state");

        h.shutdown();
    }
}
