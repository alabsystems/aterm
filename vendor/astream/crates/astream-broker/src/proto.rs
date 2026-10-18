//! The broker wire protocol: each message is exactly one `astream_wire::Frame`
//! (inheriting CRC + the 16 MiB cap). The frame payload is a tiny tagged codec —
//! `| PROTO_VERSION:u8 | tag:u8 | body |`, every variable `u32` length
//! bounds-checked before slicing, the same fail-closed discipline as the engine
//! envelope. `read_frame`/`write_frame` are generic over `Read`/`Write`, so the
//! codec is unit-testable in memory with no socket.
//!
//! # Refusal codes
//!
//! `Response::Error::code` is the CLASS of a refusal; the message says why in words,
//! and is the same whatever the code. A client branches on the code — through
//! [`crate::client::BrokerRefusal`], which the client puts inside the `io::Error` a
//! refused request returns.
//!
//! | code | name                   | the same request, resent unchanged             |
//! |------|------------------------|------------------------------------------------|
//! | 2    | [`ERR_BAD_SUBJECT`]    | is refused again: the subject is not a subject |
//! | 3    | [`ERR_BAD_FILTER`]     | is refused again: the filter is not a filter   |
//! | 5    | [`ERR_REFUSED`]        | may land: NOT known to be permanent            |
//! | 6    | [`ERR_TOO_MANY_CONNS`] | may be served on a later connection            |
//! | 7    | [`ERR_RECORD_REFUSED`] | is refused again, on the request's own terms   |
//! | 8    | [`ERR_UNAUTHORIZED`]   | is refused again on this connection's grants   |
//! | 9    | [`ERR_PROTOCOL`]       | is refused again: another protocol; it closes  |
//! | 10   | [`ERR_SUBJECT_BOUND`]  | lands only once the subject enters the index   |
//!
//! Codes 2, 3, 7, 8 and 9 are PERMANENT for the request that drew them: sent again,
//! unchanged, on the same connection, it is refused again, so a retry is a loop.
//! Code 5 is the fail-safe reading. A broker older than codes 7 and 8 answered every
//! refusal but 2, 3 and 6 with 5, permanent ones included, so a client reads 5 as
//! "not known to be permanent" — it may retry — and never as "will succeed". An
//! unknown code reads as 5. (Code 4 once meant a malformed request and is not
//! reused.)
//!
//! Code 10 is NOT permanent, and it is not 5 either: it names one condition, a
//! producer at its distinct-subject budget asking for one subject more, so a client
//! can tell it from every other refusal and decide by what it knows. The broker
//! cannot know whether another producer will ever introduce the subject — after
//! which the identical request is an update and lands — but the producer often can:
//! one that is the only writer of its subjects (its own lanes) may drop the record
//! for good, while one that shares a subject may wait for it. A broker older than
//! code 10 answers it with 5.
//!
//! One code-8 refusal is told apart by its WORDS, not by a code of its own: an
//! expired capability's message starts [`CAP_EXPIRED_PREFIX`], and
//! [`BrokerRefusal::is_expired`](crate::client::BrokerRefusal::is_expired) reads it.
//! So is one code-5 refusal: every write a POISONED log refuses starts
//! [`LOG_POISONED_PREFIX`], and
//! [`BrokerRefusal::is_poisoned`](crate::client::BrokerRefusal::is_poisoned) reads it.
//!
//! # The one layout every version keeps
//!
//! A refusal is `[version:u8][0x83][code:u8][len:u32 LE][msg]` under EVERY
//! `PROTO_VERSION` — protocol 1 wrote exactly this, 2 does, and a later version must
//! too — and a decoder reads nothing after `msg`. Everything else on the wire may
//! change with the version byte; this may not. It is what lets a broker answer a peer
//! that speaks ANOTHER protocol at all: [`encode_protocol_refusal`] stamps this layout
//! with the PEER's version byte, so the peer's own decoder reads it, whichever version
//! that is.
//!
//! # What the wire says about durability
//!
//! Three things, all additive, none spending a request tag, a protocol version or an
//! error code:
//!
//! - **The tier served**: tag 0x01 of the `Hello` answer's extension block
//!   ([`NonceExt::durability`], [`ServedInfo`]) — the tier asked for, the tier in force
//!   and why they differ, the Ordered horizon and the escalation table's size. A status
//!   line on any host reads the tier from the broker, never from how it was started. It
//!   can change in a running broker in one direction only: a log that latches to Strict
//!   (a barrier refused, a full flush failed) answers `served = Strict` from then on.
//! - **The durable head**: [`Response::Mark`]'s optional `durable` trailer, on EVERY
//!   `Mark` a broker of this build sends — every record below it is on stable media by
//!   a full flush. It is the in-band proof a reader on another host needs: a `Fetch` head
//!   query (`max = 0`) answers "is offset `o` durable?" with `durable > o`. An older
//!   broker sends none; it serves Strict or Relaxed, never Ordered, and the one before
//!   Ordered says which in its `Hello` block: on Strict read `None` as "durable =
//!   visible", on Relaxed as "nothing acked since the open (or a clean close) is
//!   durable". A broker older still sends no block and may serve Relaxed unannounced:
//!   "durable = visible" holds there only for a reader that knows its broker strict. In
//!   process the same value is [`Broker::durable_head`](crate::Broker::durable_head).
//! - **Durable marks**: on a broker that SERVES Ordered, a subscriber that asked for
//!   idle marks (`Request::IdleMarks`, which `Client::subscribe` negotiates) gets one
//!   extra `Mark` when a full flush covers records it was delivered and not yet told are
//!   durable — at most one per such flush. Not on Strict (the durable head is the
//!   visible head after every batch, so a mark per delivery would say nothing) and not
//!   on Relaxed (it does not move between opens).
//!
//! An escalated record (the broker's escalation table; see
//! [`Escalation`](crate::Escalation)) never needs any of these to be safe: the broker
//! holds the visible head at it until its full flush has succeeded, so no reader is ever
//! delivered, and no producer acked, an escalated record that a power loss can take.

use astream_wire::frame::{MAGIC as FRAME_MAGIC, VERSION as FRAME_VERSION};
use astream_wire::{crc32_ieee, Frame, FrameError, HEADER_SIZE, MAX_PAYLOAD_LEN};
use std::io::{self, Read, Write};

use crate::logid::{Lineage, LogIdentity};

/// Protocol version (the payload's first byte).
pub const PROTO_VERSION: u8 = 2;

/// Refused, and NOT known to be permanent: a write that failed to append, commit or
/// fsync, a batch another record's I/O failure aborted, a quorum not reached, a will
/// the log could not persist, a broker stopping, a request this broker cannot decode
/// — and ANY refusal decided inside a batch that then rolled back, which was judged
/// against records that never landed (a subject-bound refusal included: the batch's
/// own new subjects it counted never landed). Resending may land. A broker that
/// predates [`ERR_RECORD_REFUSED`] and [`ERR_UNAUTHORIZED`] sent this code for every
/// refusal but 2, 3 and 6, and one that predates [`ERR_SUBJECT_BOUND`] sends it for a
/// producer at its distinct-subject budget, so it is also what a refusal of unknown
/// permanence looks like.
pub const ERR_REFUSED: u8 = 5;

/// A subject the request names is not a valid subject (permanent).
pub const ERR_BAD_SUBJECT: u8 = 2;

/// A filter the request names is not a valid filter (permanent).
pub const ERR_BAD_FILTER: u8 = 3;

/// The broker is at its connection cap (`MAX_CONNS`): it answered this connection
/// with the refusal and closed it. Not about the request — a later connection may be
/// served.
pub const ERR_TOO_MANY_CONNS: u8 = 6;

/// The request is refused ON ITS OWN TERMS, whatever capability carried it: a record
/// too large for the log, a reserved (broker-internal) subject, a will at a sequence
/// in the reserved ack half, a name past the length bound, a second will under
/// another producer id on one connection. Resent unchanged, it is refused again
/// (permanent).
pub const ERR_RECORD_REFUSED: u8 = 7;

/// The connection's capabilities do not authorize the request (the message starts
/// `unauthorized:`): no attached grant covers it, the grant that did has expired, a
/// publish names a producer id its grant does not bind, or an `Attach` was refused —
/// a proof that does not verify, no `Hello` first, a full keyring, a producer id
/// bound to another principal — or a guarded connection stayed unattached past its
/// deadline. Resent unchanged on this connection it is refused again (permanent); a
/// different capability is a different request.
///
/// One of those causes is told apart from the rest: a capability whose `exp=` has
/// passed answers with a message that starts [`CAP_EXPIRED_PREFIX`].
pub const ERR_UNAUTHORIZED: u8 = 8;

/// How an EXPIRED capability's [`ERR_UNAUTHORIZED`] refusal begins, word for word.
/// The broker sends it at all three places an expiry is decided: at the `Attach`
/// door, on a request every grant that covered has lapsed for, and when a stream
/// ends at its grants' deadline. No other refusal starts with it, and every refusal
/// for an expired capability does.
///
/// An expiry needs a different remedy from every other refusal of code 8: the
/// capability is genuine, its time is up, and minting it again with a later `exp=`
/// fixes it. A forged or tampered grant needs a new secret or a new capability, and a
/// request outside the grants needs a wider one. Code 8 is one class, so the
/// difference is carried in the words, and astream owns both the words and the
/// client that reads them:
/// [`BrokerRefusal::is_expired`](crate::client::BrokerRefusal::is_expired) is the
/// test. Adding a code instead would have changed the wire, which this does not.
///
/// A broker older than this constant sent the same words at the attach and at a
/// stream's deadline. On a request whose grants had lapsed it said "does not grant",
/// so against such a broker that one refusal reads as not expired.
pub const CAP_EXPIRED_PREFIX: &str = "unauthorized: capability expired";

/// How every write a POISONED log refuses begins, word for word: an [`ERR_REFUSED`]
/// refusal of a publish, a commit, a will, a binding or a flush the writer answers
/// while the log is poisoned ([`BrokerLog::poisoned`](crate::BrokerLog::poisoned)),
/// followed by the reason in parentheses. No other refusal starts with it.
///
/// A poisoned log is a refusal of a kind no resend on this broker cures: a full flush
/// failed with acked or held records no full flush covers, or a rollback could not put
/// the file back, and every write is refused until the log is REOPENED — a new open,
/// which only the embedder can make (exit, and let a supervisor start the broker
/// again). Code 5 is right for it, because to the producer it is transient: the same
/// record lands once the log is reopened. But a producer that retries a code-5
/// refusal on the same connection waits for ever, and one that reads code 5 as "the
/// broker is answering" reports a broker as working while it takes nothing. So the
/// condition is carried in the words, which astream owns, and
/// [`BrokerRefusal::is_poisoned`](crate::client::BrokerRefusal::is_poisoned) is the
/// test: a producer that sees it knows the connection is worth nothing until the
/// broker restarts. Adding a code instead would have changed the wire, which this
/// does not.
///
/// The op whose own flush POISONED the log (an escalated publish, a held commit, a
/// `flush_durable`) is answered with what happened to it — "outcome unknown, resend" —
/// not with this; its resend is answered with this. A broker older than this constant
/// answered the first op of a batch on a poisoned log with `append failed: log
/// poisoned (…)` and every later one with `batch aborted (prior append I/O error)`, so
/// against it this is `false`: the fail-safe reading, under which a producer retries.
pub const LOG_POISONED_PREFIX: &str = "log poisoned";

/// The request is in ANOTHER astream protocol: its version byte is not this broker's
/// [`PROTO_VERSION`]. The broker answers it once, with [`encode_protocol_refusal`] —
/// the refusal layout every version keeps, stamped with the PEER's version byte so the
/// peer can read it — and closes the connection, because nothing more the peer sends
/// can be decoded here. Permanent: only a client of the same protocol is served. This
/// crate's client reads it — it names the broker's version — and any reply stamped
/// with another version as a [`ProtocolMismatch`](crate::client::ProtocolMismatch)
/// naming both versions, not as a [`BrokerRefusal`](crate::client::BrokerRefusal).
pub const ERR_PROTOCOL: u8 = 9;

/// The producer already holds its whole distinct-subject budget
/// ([`MAX_SUBJECTS_PER_PRODUCER`](crate::store::MAX_SUBJECTS_PER_PRODUCER), counting
/// the subjects its registered wills have reserved), and this publish, transaction or
/// will names a subject NEW to the last-value index. Nothing was appended.
///
/// NOT permanent. A producer at the budget can still UPDATE a subject the index
/// holds, whoever introduced it, so the identical request lands once another
/// producer introduces this subject; it also lands once the producer's own count
/// falls. What it is NOT is a transient write failure: on a broker that nothing else
/// changes, resending it is refused again. So a client that knows no other producer
/// ever writes the subject — a producer's own lanes — may treat it as final for that
/// record, and one that shares the subject may hold the record for the subject to
/// appear. [`Request::Budget`] reads the count and the budget.
///
/// A refusal of this kind inside a batch that then rolled back is [`ERR_REFUSED`]
/// instead: the subjects it was counted against never landed. A broker that predates
/// this code sends [`ERR_REFUSED`].
pub const ERR_SUBJECT_BOUND: u8 = 10;

const TAG_PUBLISH: u8 = 0x01;
const TAG_SUBSCRIBE: u8 = 0x02;
const TAG_FORK_SUBSCRIBE: u8 = 0x03;
const TAG_COMMIT: u8 = 0x04;
const TAG_PROCESS: u8 = 0x05;
const TAG_SUBSCRIBE_GROUP: u8 = 0x06;
const TAG_ATTACH: u8 = 0x07;
const TAG_REPLICATE: u8 = 0x08;
const TAG_LAST: u8 = 0x09;
const TAG_FETCH: u8 = 0x0A;
const TAG_WILL: u8 = 0x0B;
const TAG_HELLO: u8 = 0x0C;
const TAG_KEEPALIVE: u8 = 0x0D;
const TAG_IDLE_MARKS: u8 = 0x0E;
const TAG_BUDGET: u8 = 0x0F;
const TAG_ACK: u8 = 0x81;
const TAG_DELIVERY: u8 = 0x82;
const TAG_ERROR: u8 = 0x83;
const TAG_MARK: u8 = 0x84;
const TAG_NONCE: u8 = 0x85;
const TAG_BUDGET_REPLY: u8 = 0x86;

// The Clean island, out of line: only a Trust compiler lexes it, so it mounts under
// `cfg(clean_islands)` (build.rs) and a stock compiler never opens the file.
#[cfg(clean_islands)]
#[path = "proto_island.rs"]
mod proto_island;
#[cfg(not(clean_islands))]
pub use crate::twins::{
    committed_watermark, compact_floor_monotone, credit_horizon_monotone, epoch_advance_monotone,
    fenced_ack_select, group_commit_monotone, high_water_monotone, replica_ack_monotone,
    window_gate_send,
};
#[cfg(clean_islands)]
pub use proto_island::{
    committed_watermark, compact_floor_monotone, credit_horizon_monotone, epoch_advance_monotone,
    fenced_ack_select, group_commit_monotone, high_water_monotone, replica_ack_monotone,
    window_gate_send,
};

/// 5-parameter epoch-fenced replication watermark kernel:
/// rejects stale-epoch follower acks (`req_epoch < leader_epoch`), otherwise
/// advances `cur_ack` monotonically to `max(cur_ack, cand_ack)` and clamps the
/// committed watermark at `min(primary_head, next_ack)`.
///
/// Composes the 4-parameter Clean-origin [`fenced_ack_select`] with
/// [`committed_watermark`] and [`replica_ack_monotone`]
/// (`TlaEpochWatermarkReplication::fenced_replica_watermark_le_head`).
#[must_use]
pub const fn fenced_replica_watermark(
    leader_epoch: u64,
    req_epoch: u64,
    primary_head: u64,
    cur_ack: u64,
    cand_ack: u64,
) -> u64 {
    committed_watermark(
        primary_head,
        fenced_ack_select(
            req_epoch,
            leader_epoch,
            cur_ack,
            replica_ack_monotone(cur_ack, cand_ack),
        ),
    )
}

/// 4-parameter credit-windowed send advance kernel:
/// verifies well-formedness (`ack_seq <= sent_seq`) and available credit
/// (`sent_seq < credit_horizon`), then advances `sent_seq` toward `cand_seq`
/// clamped by `credit_horizon`.
///
/// Composes the 4-parameter Clean-origin [`window_gate_send`] with
/// [`credit_horizon_monotone`] and [`committed_watermark`]
/// (`TlaCreditFlowWindow::windowed_send_advance_ge_sent`).
#[must_use]
pub const fn windowed_send_advance(
    sent_seq: u64,
    ack_seq: u64,
    credit_horizon: u64,
    cand_seq: u64,
) -> u64 {
    if ack_seq <= sent_seq {
        window_gate_send(
            sent_seq,
            credit_horizon,
            credit_horizon_monotone(sent_seq, committed_watermark(credit_horizon, cand_seq)),
            sent_seq,
        )
    } else {
        sent_seq
    }
}

contract_fn! {
    /// Kernel-certified incarnation fence predicate `will_seq < high_water`, used by
    /// [`crate::store::BrokerLog`] when deciding whether a pending last will is fenced
    /// by a later normal publish from the same producer (`TlaWillFencing`).
    #[must_use]
    pub fn will_is_fenced(high_water: u64, will_seq: u64) -> bool
    clauses [ensures result == (will_seq < high_water)]
    {
        will_seq < high_water
    }
}

contract_fn! {
    /// Kernel-certified replication epoch fence predicate `req_epoch < cur_epoch`,
    /// used by [`crate::store::BrokerLog`] and [`crate::broker`] to reject stale
    /// primary replication frames (`TlaEpochWatermarkReplication`).
    #[must_use]
    pub fn epoch_is_fenced(cur_epoch: u64, req_epoch: u64) -> bool
    clauses [ensures result == (req_epoch < cur_epoch)]
    {
        req_epoch < cur_epoch
    }
}

contract_fn! {
    /// Kernel-certified committed-offset visibility predicate `offset < committed_upto`,
    /// used by [`crate::store::BrokerLog::is_offset_replicated_committed`] and
    /// [`crate::store::BrokerLog::fetch_replicated_committed`] (`TlaEpochWatermarkReplication`).
    #[must_use]
    pub fn offset_is_committed(committed_upto: u64, offset: u64) -> bool
    clauses [ensures result == (offset < committed_upto)]
    {
        offset < committed_upto
    }
}

contract_fn! {
    /// Kernel-certified pipelined flow-control window guard `next_seq < horizon`,
    /// used by [`crate::client::CreditWindow`] when pipelining frames over high-RTT links
    /// (`TlaCreditFlowWindow`).
    #[must_use]
    pub fn can_send_in_window(horizon: u64, next_seq: u64) -> bool
    clauses [ensures result == (next_seq < horizon)]
    {
        next_seq < horizon
    }
}

/// A client → broker request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Append `body` to `subject`, deduped by `(producer_id, producer_seq)`.
    Publish {
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// Stream every record matching `filter` from `from_offset`, then tail live.
    Subscribe { from_offset: u64, filter: String },
    /// COUNTERFACTUAL fork-delivery: stream the alternate timeline that results from
    /// forking the log at `fork_at` and swapping that one record for
    /// `(replacement_subject, replacement_body)`, filtered by `filter`. The live log
    /// is untouched (a sandbox view); delivery is the frozen counterfactual snapshot.
    ForkSubscribe {
        fork_at: u64,
        replacement_subject: String,
        replacement_body: Vec<u8>,
        filter: String,
    },
    /// Durably advance consumer `group`'s committed offset to `upto` (a pure commit).
    Commit { group: String, upto: u64 },
    /// The read-process-write TRANSACTION: append `out_body` to `out_subject` AND
    /// commit `group` to `upto` atomically (one durable record), deduped by
    /// `(producer_id, producer_seq)` — exactly-once processing.
    ProcessAndProduce {
        producer_id: u64,
        producer_seq: u64,
        out_subject: String,
        out_body: Vec<u8>,
        group: String,
        upto: u64,
    },
    /// Subscribe as consumer `group`: deliver matching records from the group's
    /// committed offset (broker-enforced resume) — the newest commit not held back for a
    /// full flush, at what the tier promises of it (durable on Strict, and on Ordered and
    /// Relaxed when the escalation table names the group; on the Replicated tier, one a
    /// quorum may not hold yet) — then tail live.
    SubscribeGroup { group: String, filter: String },
    /// Add a capability to this connection's KEYRING. `grant` is the signed grant
    /// string (`[rw|ro][,p=<principal>]:<filter>`, or a bare filter for the
    /// read-write unbound form); `proof` is a PROOF OF POSSESSION —
    /// `HMAC-SHA256(tag, nonce ‖ grant)` over the nonce this connection got from
    /// [`Request::Hello`] — so the capability's tag never crosses the wire and a
    /// captured `Attach` frame cannot be replayed onto another connection.
    ///
    /// It APPENDS (a repeated grant string replaces; the ring is bounded), and it is
    /// ACKNOWLEDGED with a [`Response::Mark`] — a rejected attach is visible at once
    /// rather than only at the next request. An unguarded broker acknowledges it and
    /// enforces nothing.
    Attach { grant: String, proof: Vec<u8> },
    /// REPLICATION (leader → follower): append this record at EXACTLY leader offset
    /// `seq`, with its producer key, subject, body, and any consumer-group commit
    /// annotation — so a follower's log is an identical prefix of the leader's
    /// (commit records and annotations included). Idempotent: a record already held
    /// at `seq` acks `deduped = true`; a gap, or a different record at `seq`, is
    /// refused (never silently overwritten). Acked like a publish.
    Replicate {
        seq: u64,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
        /// `Some((group, upto))` for a commit-carrying record.
        commit: Option<(String, u64)>,
        /// The LEADER's [`LogIdentity`], which a replica adopts as its own once the
        /// record commits and is the replica's newest — the record's offset is the
        /// leader's, so a client that fails over keeps its offsets. An OPTIONAL TRAILER after `commit`: a leader
        /// that predates log identity sends none (`None`), and a follower that
        /// predates it never reads the bytes (its decoder stops after `commit`).
        leader: Option<LogIdentity>,
    },
    /// LAST-VALUE (retained state): deliver the most recent record of EVERY subject
    /// matching `filter` whose subject sorts strictly after `after` (`""` for the
    /// first page), at most `max` of them, in ascending SUBJECT order — then a
    /// [`Response::Mark`] carrying the head the page was read with. Non-terminal: the
    /// connection stays usable, so `Subscribe { from_offset: mark.next, filter }` on
    /// the same connection tails on gap-free from THAT page's snapshot. Each page of a
    /// PAGED query is read at its own, later head, so a paged reader tails from the
    /// FIRST page's `next` and folds newest-wins across the pages — tailing from the
    /// last page's `next` skips whatever superseded a value an earlier page reported.
    ///
    /// BOTH bounds are the BROKER's, not the client's: `max` is clamped to
    /// `LAST_PAGE_MAX` and the scan of the subject index is cut off after
    /// `LAST_SCAN_MAX` entries VISITED, matched or not. So the page may be shorter
    /// than `max` for a reason that is not "the answer ended", and the closing
    /// `Mark`'s `resume` is the cursor that continues it: page until `resume` is
    /// empty, passing it back as `after`, or the answer is silently truncated.
    Last {
        filter: String,
        after: String,
        max: u32,
    },
    /// BOUNDED, NON-TERMINAL read: deliver at most `max` records matching `filter`
    /// with offset >= `from_offset`, scanning at most `FETCH_SCAN_MAX` records — then
    /// a [`Response::Mark`] whose `next` is the offset after the LAST SCANNED record,
    /// so a sparse filter still advances the cursor instead of re-scanning. `max = 0`
    /// delivers nothing and is the head query. Unlike `Subscribe` it never tails and
    /// never ends the connection.
    Fetch {
        from_offset: u64,
        filter: String,
        max: u32,
    },
    /// REGISTER A LAST WILL: the record the broker appends ON THIS CONNECTION'S
    /// BEHALF when the connection ends, for any reason. Persisted first (a hidden
    /// `/a/will` record) and acknowledged with a [`Response::Mark`]; one per
    /// connection, a second replacing the first — under the SAME producer id, which is
    /// the only shape the durable record can express, so a second `Will` naming a
    /// DIFFERENT producer id is refused rather than silently leaving two wills on the
    /// log for the next broker open to fire.
    ///
    /// It fires as an ORDINARY publish under `(producer_id, producer_seq)`, so it is
    /// deduped like any other — a graceful goodbye that publishes the same key first
    /// makes the will a no-op — and it is FENCED: it appends nothing if a record with
    /// a HIGHER `producer_seq` from the same producer has since landed, which is how
    /// a reconnected producer structurally suppresses its previous incarnation's will.
    Will {
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// Open the capability handshake: ask for this connection's fresh
    /// [`Response::Nonce`], which an [`Attach`](Request::Attach)'s proof is computed
    /// over. Always allowed (it grants nothing); a guarded broker refuses an `Attach`
    /// with no preceding `Hello`.
    Hello,
    /// KEEP THIS CONNECTION ALIVE: append nothing and answer a [`Response::Mark`]
    /// carrying the head — and, on a TCP connection, hold it from now on to the
    /// broker's keepalive deadline (`KEEPALIVE_DEADLINE`, `Broker::set_keepalive_deadline`):
    /// a connection that then goes that long with nothing read from it is ended
    /// exactly as a closed one is, its will fired and its slot freed. That is how a
    /// request/reply peer that vanished without a FIN is noticed at all; the broker
    /// writes to such a connection only to answer it, so silence is all it ever sees.
    ///
    /// THE FIRST ONE ARMS THE DEADLINE, so a client that never sends one is never
    /// cut. A Unix-socket connection is answered and never held: its peer cannot
    /// vanish without its kernel closing the socket. A broker that predates the verb
    /// answers `Error 5 "malformed request"` and keeps the connection. On a guarded
    /// broker it needs a capability attached (it grants nothing, but the `Mark` names
    /// the log's head). A wire frame, never a log record.
    Keepalive,
    /// Opt in to catch-up and idle [`Response::Mark`] frames on this connection's
    /// next `Subscribe` or `SubscribeGroup`. Answered with a `Mark` before the
    /// stream handoff; append nothing. A broker that predates this verb answers
    /// `Error 5 "malformed request"` and leaves the connection usable, so a client
    /// falls back to that broker's existing tail behavior. No read deadline is
    /// armed here.
    ///
    /// Without this request a tail sends deliveries only: older clients skip marks
    /// inside `recv`, and unsolicited idle marks would keep resetting their socket
    /// timeout forever. On a guarded broker any unexpired capability permits this
    /// request (the acknowledgement names the head), as for `Keepalive`.
    IdleMarks,
    /// READ A PRODUCER'S DISTINCT-SUBJECT BUDGET: answered with a
    /// [`Response::Budget`] — how many subjects of the last-value index are charged to
    /// `producer_id`, how many its registered wills hold in reserve, and the budget
    /// both count against. Appends nothing and changes nothing.
    ///
    /// The count is the one the broker refuses by ([`ERR_SUBJECT_BOUND`]), read from
    /// the durable index after this connection's pending acks: a subject is charged to
    /// the producer whose record first put it in the index, and never to a producer
    /// that only updated it. It is per LOG (one shard of a sharded broker is one log).
    ///
    /// On a guarded broker it is answered only to a connection that could PUBLISH as
    /// `producer_id`: its ring holds a genuine, unexpired read-write grant that is
    /// bound to that producer (`p=<principal>` deriving it) or unbound. A broker that
    /// predates the verb answers `Error 5 "malformed request"` and keeps the
    /// connection. A wire frame, never a log record.
    Budget { producer_id: u64 },
}

/// A broker → client response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// A publish landed (or deduped) at `offset`.
    PublishAck { offset: u64, deduped: bool },
    /// One delivered record.
    Delivery {
        offset: u64,
        subject: String,
        body: Vec<u8>,
    },
    /// The request was refused: `code` is its class (the module docs' table of
    /// refusal codes) and `msg` says why in words. Its layout is the one every
    /// protocol version keeps (the module docs).
    Error { code: u8, msg: String },
    /// The end of a bounded, NON-TERMINAL read: `next` is the offset to resume from
    /// (a `Subscribe`/`Fetch` cursor) and `head` is the subscriber-visible head the
    /// answer was read with. Also the acknowledgement of an `Attach`.
    ///
    /// `resume` is the SUBJECT cursor of a [`Request::Last`] page: pass it back as
    /// `after` for the next page, and an EMPTY `resume` means the scan reached the end
    /// of the filter's range — this was the last page. It is empty on every other
    /// verb's `Mark` (`Fetch` pages by offset through `next`; `Attach` and `Will`
    /// carry no cursor at all).
    ///
    /// `resume` is not an optional trailer. A broker that predates the field stamps
    /// payload byte 0 with its own `PROTO_VERSION`, and [`decode_response`] refuses
    /// the frame at that byte, so a `Mark` that ends after `head` is a current peer's
    /// frame cut short: it decodes as malformed like every other short body (read as
    /// `resume = ""` it would report "this was the last page" for a truncated answer).
    ///
    /// `durable` IS an optional trailer, after `resume`: the broker's DURABLE head when
    /// it wrote the `Mark` — every record below it is on stable media by a full flush
    /// (never above `head`). A broker of this build writes it on EVERY `Mark`; an older
    /// client never checked where a `Mark` body ends and ignores it, and an older broker
    /// sends none (`None`): it serves Strict or Relaxed, never Ordered, and a reader
    /// reads which from its `Hello` block where it sends one — on Strict "durable =
    /// visible", on Relaxed nothing acked since its open (see the module docs). A body that ends one to seven bytes after `resume` is a frame
    /// cut short (malformed); bytes after the eight are a later trailer, skipped. On a
    /// broker that serves [`Tier::Ordered`], a subscriber that asked for idle marks also
    /// gets a DURABLE mark when a full flush covers records it was delivered.
    Mark {
        next: u64,
        head: u64,
        resume: String,
        durable: Option<u64>,
    },
    /// This connection's fresh nonce (32 bytes), the answer to
    /// [`Request::Hello`]. An `Attach`'s proof is `HMAC-SHA256(tag, nonce ‖ grant)`
    /// over it, which binds the attach to THIS connection.
    ///
    /// `log` is the [`LogIdentity`] of the log the broker serves: a client that kept
    /// offsets from an earlier connection compares it before it uses one. An
    /// OPTIONAL TRAILER after the nonce, so the frame stays readable both ways: a
    /// broker that predates log identity sends none (`None` — nothing known either
    /// way), and a client that predates it never reads the bytes (its decoder stops
    /// after the nonce).
    ///
    /// `ext` is the EXTENSION BLOCK after the identity: what the broker says about the
    /// log beside its name — the tier it serves ([`NonceExt::durability`]) and where the
    /// log's generations agree ([`NonceExt::lineage`]). Present only when `log` is, and
    /// additive both ways: an older client never reads past the identity, and an older
    /// broker sends none (both fields `None`). See [`NonceExt`] for the layout. The WIRE
    /// is additive; the variant is not: code that builds a `Response::Nonce` literal (a
    /// test's stand-in broker) names the field — `ext: NonceExt::default()` says
    /// nothing, as a broker before the block did.
    Nonce {
        nonce: Vec<u8>,
        log: Option<LogIdentity>,
        ext: NonceExt,
    },
    /// The answer to [`Request::Budget`]: `subjects` index entries are charged to the
    /// producer, its registered wills hold `reserved` more, and a publish that would
    /// add a subject past `subjects + reserved == max` is refused
    /// ([`ERR_SUBJECT_BOUND`]).
    Budget {
        subjects: u64,
        reserved: u64,
        max: u64,
    },
}

/// A durability tier as the `Hello` block (and `<log>.id`) codes it: 0 Strict,
/// 1 Ordered, 2 Relaxed. A code this build does not know is kept as [`Tier::Other`].
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// An ack is on stable media.
    Strict,
    /// An ack is ordered on the device (a broker that serves it is newer than this
    /// build's tiers; reported as told).
    Ordered,
    /// An ack is in the page cache.
    Relaxed,
    /// A code this build does not know.
    Other(u8),
}

impl Tier {
    fn code(self) -> u8 {
        match self {
            Tier::Strict => 0,
            Tier::Ordered => 1,
            Tier::Relaxed => 2,
            Tier::Other(c) => c,
        }
    }

    fn from_code(c: u8) -> Tier {
        match c {
            0 => Tier::Strict,
            1 => Tier::Ordered,
            2 => Tier::Relaxed,
            c => Tier::Other(c),
        }
    }
}

/// Why a broker serves a tier other than the one it was asked for.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServedReason {
    /// It serves what it was asked for.
    None,
    /// The ordering-only flush exists only on Apple.
    NotApple,
    /// The log is not on the boot volume.
    NotBootVolume,
    /// The device refused the barrier when the log was opened.
    BarrierRefused,
    /// A barrier failed while serving, so the log latched to Strict.
    LatchedAfterBarrierError,
    /// A full flush failed while serving: the log latched to Strict, and refuses every
    /// write until it is reopened (the records that flush was to cover may be lost).
    LatchedAfterFlushFailure,
    /// A code this build does not know.
    Other(u8),
}

impl ServedReason {
    fn code(self) -> u8 {
        match self {
            ServedReason::None => 0,
            ServedReason::NotApple => 1,
            ServedReason::NotBootVolume => 2,
            ServedReason::BarrierRefused => 3,
            ServedReason::LatchedAfterBarrierError => 4,
            ServedReason::LatchedAfterFlushFailure => 5,
            ServedReason::Other(c) => c,
        }
    }

    fn from_code(c: u8) -> ServedReason {
        match c {
            0 => ServedReason::None,
            1 => ServedReason::NotApple,
            2 => ServedReason::NotBootVolume,
            3 => ServedReason::BarrierRefused,
            4 => ServedReason::LatchedAfterBarrierError,
            5 => ServedReason::LatchedAfterFlushFailure,
            c => ServedReason::Other(c),
        }
    }
}

/// What a broker says about the durability it serves: the `Hello` block's tag 0x01, so
/// a status line or a doctor on any host reads the tier in force from the broker itself
/// and never infers it from an argv.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServedInfo {
    /// The tier the broker was opened at.
    pub requested: Tier,
    /// The tier in force.
    pub served: Tier,
    /// Why they differ ([`ServedReason::None`] when they do not).
    pub reason: ServedReason,
    /// The Ordered HORIZON asked for, in milliseconds rounded UP (a sub-millisecond
    /// horizon reads 1, never 0): the writer issues a full flush once this long has passed
    /// since the first ack no full flush covers, after the batch in hand — so the oldest
    /// such ack is at most this plus one batch plus one full flush old. `Some(0)`: a full
    /// flush after every batch. `None` for `Ordered { horizon: None }` — NO bound: only
    /// an escalation, a `flush_durable` and a clean close flush in full, so a power loss
    /// can take every batch since the last of them — and `None` whenever the tier asked
    /// for is not Ordered (Strict flushes every batch; Relaxed keeps no horizon).
    ///
    /// The two Ordered cases that are opposite promises — no bound, and a flush after
    /// every batch — never read alike: the wire carries `u32::MAX` for no horizon and
    /// saturates a real one at `u32::MAX - 1` (49 days).
    pub horizon_ms: Option<u32>,
    /// How many escalation rules the broker holds (records it flushes in full).
    pub rules: u16,
}

/// The wire's `horizon_ms` for "no horizon" ([`ServedInfo::horizon_ms`] `None`).
const HORIZON_NONE: u32 = u32::MAX;

impl ServedInfo {
    /// The horizon `h` as [`horizon_ms`](Self::horizon_ms) reports it: milliseconds
    /// rounded up, saturating below the wire's "none".
    #[must_use]
    pub fn horizon_ms_of(h: std::time::Duration) -> u32 {
        u32::try_from(h.as_nanos().div_ceil(1_000_000))
            .unwrap_or(HORIZON_NONE)
            .min(HORIZON_NONE - 1)
    }
}

/// The `Nonce` answer's EXTENSION BLOCK: after the identity trailer, `(tag u8, len u16
/// LE, bytes)*` to the end of the body. A tag a reader does not know is skipped by its
/// length; an entry cut short is a malformed frame. It is sent only with an identity,
/// and, like the identity, before authentication: tiers, generations and offsets are
/// not secrets.
///
/// | tag  | entry      | bytes |
/// |------|------------|-------|
/// | 0x01 | durability | `requested u8, served u8, reason u8, horizon_ms u32 LE, rules u16 LE` ([`ServedInfo`]) |
/// | 0x02 | lineage    | `n u8`, then `n × (generation u64 LE, common u64 LE)` ([`Lineage`]) |
/// | 0x03 | reserved   | the request set a broker announces (the subject-retire design) |
///
/// No request tag, no protocol version and no error code are spent on it: a broker
/// that predates the block sends a frame that ends at the identity, which reads here
/// as both fields `None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NonceExt {
    /// Tag 0x01: the tier the broker serves.
    pub durability: Option<ServedInfo>,
    /// Tag 0x02: where the log's generations agree.
    pub lineage: Option<Lineage>,
}

/// The extension-block tags ([`NonceExt`]).
const EXT_DURABILITY: u8 = 0x01;
const EXT_LINEAGE: u8 = 0x02;

impl NonceExt {
    fn put(&self, p: &mut Vec<u8>) {
        let mut entry = |tag: u8, body: &[u8]| {
            p.push(tag);
            p.extend_from_slice(&(body.len() as u16).to_le_bytes());
            p.extend_from_slice(body);
        };
        if let Some(d) = &self.durability {
            let mut b = vec![d.requested.code(), d.served.code(), d.reason.code()];
            b.extend_from_slice(&d.horizon_ms.unwrap_or(HORIZON_NONE).to_le_bytes());
            b.extend_from_slice(&d.rules.to_le_bytes());
            entry(EXT_DURABILITY, &b);
        }
        if let Some(l) = &self.lineage {
            let mut b = Vec::new();
            l.put(&mut b);
            entry(EXT_LINEAGE, &b);
        }
    }

    /// Read the block from `b` (the bytes after the identity) to its end; `None` for an
    /// entry cut short, or a known entry whose bytes are not its layout.
    fn read(mut b: &[u8]) -> Option<NonceExt> {
        let mut ext = NonceExt::default();
        while let Some((&tag, rest)) = b.split_first() {
            let len = u16::from_le_bytes(rest.get(..2)?.try_into().ok()?) as usize;
            let end = 2usize.checked_add(len)?;
            let body = rest.get(2..end)?;
            b = rest.get(end..)?;
            match tag {
                EXT_DURABILITY => {
                    let [req, served, reason, h0, h1, h2, h3, r0, r1] = *body else {
                        return None;
                    };
                    let requested = Tier::from_code(req);
                    let horizon = u32::from_le_bytes([h0, h1, h2, h3]);
                    ext.durability = Some(ServedInfo {
                        requested,
                        served: Tier::from_code(served),
                        reason: ServedReason::from_code(reason),
                        // A horizon belongs to Ordered alone; the broker that predates
                        // Ordered sent 0 for Strict and Relaxed, which is no horizon.
                        horizon_ms: (requested == Tier::Ordered && horizon != HORIZON_NONE)
                            .then_some(horizon),
                        rules: u16::from_le_bytes([r0, r1]),
                    });
                }
                EXT_LINEAGE => ext.lineage = Some(Lineage::from_wire(body)?),
                // Unknown (0x03 included): skipped by its length.
                _ => {}
            }
        }
        Some(ext)
    }
}

fn put_str(p: &mut Vec<u8>, s: &str) {
    p.extend_from_slice(&(s.len() as u32).to_le_bytes());
    p.extend_from_slice(s.as_bytes());
}
fn put_bytes(p: &mut Vec<u8>, b: &[u8]) {
    p.extend_from_slice(&(b.len() as u32).to_le_bytes());
    p.extend_from_slice(b);
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}
impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let end = self.i.checked_add(1)?;
        let v = *self.b.get(self.i)?;
        self.i = end;
        Some(v)
    }
    fn u64(&mut self) -> Option<u64> {
        let end = self.i.checked_add(8)?;
        let v = u64::from_le_bytes(self.b.get(self.i..end)?.try_into().ok()?);
        self.i = end;
        Some(v)
    }
    fn u32(&mut self) -> Option<usize> {
        let end = self.i.checked_add(4)?;
        let v = u32::from_le_bytes(self.b.get(self.i..end)?.try_into().ok()?) as usize;
        self.i = end;
        Some(v)
    }
    fn take(&mut self, n: usize) -> Option<Vec<u8>> {
        let end = self.i.checked_add(n)?;
        let v = self.b.get(self.i..end)?.to_vec();
        self.i = end;
        Some(v)
    }
    fn string(&mut self) -> Option<String> {
        let n = self.u32()?;
        String::from_utf8(self.take(n)?).ok()
    }
    fn bytes(&mut self) -> Option<Vec<u8>> {
        let n = self.u32()?;
        self.take(n)
    }
    /// An OPTIONAL [`LogIdentity`] trailer: `Some(None)` when the body ends here (a
    /// peer that predates it), `Some(Some(_))` when it follows — and `None`
    /// (malformed) for a body that ends INSIDE one, which is a frame cut short, never
    /// "no identity". Bytes after it are left unread, the room the next trailer takes.
    fn identity_trailer(&mut self) -> Option<Option<LogIdentity>> {
        if self.i == self.b.len() {
            return Some(None);
        }
        let end = self.i.checked_add(LogIdentity::WIRE_LEN)?;
        let ident = LogIdentity::from_wire(self.b.get(self.i..)?)?;
        self.i = end;
        Some(Some(ident))
    }
}

/// Append an optional identity trailer: nothing for `None`, so a frame without one is
/// byte-identical to the frame a peer that predates log identity sends.
fn put_identity(p: &mut Vec<u8>, ident: Option<&LogIdentity>) {
    if let Some(ident) = ident {
        ident.put(p);
    }
}

/// Encode a request to a frame payload.
pub fn encode_request(req: &Request) -> Vec<u8> {
    let mut p = vec![PROTO_VERSION, 0];
    match req {
        Request::Publish {
            producer_id,
            producer_seq,
            subject,
            body,
        } => {
            p[1] = TAG_PUBLISH;
            p.extend_from_slice(&producer_id.to_le_bytes());
            p.extend_from_slice(&producer_seq.to_le_bytes());
            put_str(&mut p, subject);
            put_bytes(&mut p, body);
        }
        Request::Subscribe {
            from_offset,
            filter,
        } => {
            p[1] = TAG_SUBSCRIBE;
            p.extend_from_slice(&from_offset.to_le_bytes());
            put_str(&mut p, filter);
        }
        Request::ForkSubscribe {
            fork_at,
            replacement_subject,
            replacement_body,
            filter,
        } => {
            p[1] = TAG_FORK_SUBSCRIBE;
            p.extend_from_slice(&fork_at.to_le_bytes());
            put_str(&mut p, replacement_subject);
            put_bytes(&mut p, replacement_body);
            put_str(&mut p, filter);
        }
        Request::Commit { group, upto } => {
            p[1] = TAG_COMMIT;
            put_str(&mut p, group);
            p.extend_from_slice(&upto.to_le_bytes());
        }
        Request::ProcessAndProduce {
            producer_id,
            producer_seq,
            out_subject,
            out_body,
            group,
            upto,
        } => {
            p[1] = TAG_PROCESS;
            p.extend_from_slice(&producer_id.to_le_bytes());
            p.extend_from_slice(&producer_seq.to_le_bytes());
            put_str(&mut p, out_subject);
            put_bytes(&mut p, out_body);
            put_str(&mut p, group);
            p.extend_from_slice(&upto.to_le_bytes());
        }
        Request::SubscribeGroup { group, filter } => {
            p[1] = TAG_SUBSCRIBE_GROUP;
            put_str(&mut p, group);
            put_str(&mut p, filter);
        }
        Request::Attach { grant, proof } => {
            p[1] = TAG_ATTACH;
            put_str(&mut p, grant);
            put_bytes(&mut p, proof);
        }
        Request::Will {
            producer_id,
            producer_seq,
            subject,
            body,
        } => {
            p[1] = TAG_WILL;
            p.extend_from_slice(&producer_id.to_le_bytes());
            p.extend_from_slice(&producer_seq.to_le_bytes());
            put_str(&mut p, subject);
            put_bytes(&mut p, body);
        }
        Request::Hello => p[1] = TAG_HELLO,
        Request::Keepalive => p[1] = TAG_KEEPALIVE,
        Request::IdleMarks => p[1] = TAG_IDLE_MARKS,
        Request::Budget { producer_id } => {
            p[1] = TAG_BUDGET;
            p.extend_from_slice(&producer_id.to_le_bytes());
        }
        Request::Replicate {
            seq,
            producer_id,
            producer_seq,
            subject,
            body,
            commit,
            leader,
        } => {
            return encode_replicate(
                *seq,
                *producer_id,
                *producer_seq,
                subject,
                body,
                commit.as_ref().map(|(g, u)| (g.as_str(), *u)),
                leader.as_ref(),
            );
        }
        Request::Last { filter, after, max } => {
            p[1] = TAG_LAST;
            put_str(&mut p, filter);
            put_str(&mut p, after);
            p.extend_from_slice(&max.to_le_bytes());
        }
        Request::Fetch {
            from_offset,
            filter,
            max,
        } => {
            p[1] = TAG_FETCH;
            p.extend_from_slice(&from_offset.to_le_bytes());
            put_str(&mut p, filter);
            p.extend_from_slice(&max.to_le_bytes());
        }
    }
    p
}

/// Encode a `Replicate` payload directly from BORROWED fields — the leader's
/// replication path ships shared `Arc`'d log records without cloning subject/body
/// into a `Request` first. Byte-identical to `encode_request(&Request::Replicate {..})`.
/// `leader` is the identity trailer (`None` writes the frame a leader that predates
/// log identity sends).
pub fn encode_replicate(
    seq: u64,
    producer_id: u64,
    producer_seq: u64,
    subject: &str,
    body: &[u8],
    commit: Option<(&str, u64)>,
    leader: Option<&LogIdentity>,
) -> Vec<u8> {
    let mut p = Vec::new();
    p.extend_from_slice(&[PROTO_VERSION, TAG_REPLICATE]);
    p.extend_from_slice(&seq.to_le_bytes());
    p.extend_from_slice(&producer_id.to_le_bytes());
    p.extend_from_slice(&producer_seq.to_le_bytes());
    put_str(&mut p, subject);
    put_bytes(&mut p, body);
    match commit {
        None => p.push(0),
        Some((group, upto)) => {
            p.push(1);
            put_str(&mut p, group);
            p.extend_from_slice(&upto.to_le_bytes());
        }
    }
    put_identity(&mut p, leader);
    p
}

/// Decode a request from a frame payload (bounds-checked; `None` on malformed).
pub fn decode_request(p: &[u8]) -> Option<Request> {
    let mut r = Reader { b: p, i: 0 };
    if r.u8()? != PROTO_VERSION {
        return None;
    }
    match r.u8()? {
        TAG_PUBLISH => Some(Request::Publish {
            producer_id: r.u64()?,
            producer_seq: r.u64()?,
            subject: r.string()?,
            body: r.bytes()?,
        }),
        TAG_SUBSCRIBE => Some(Request::Subscribe {
            from_offset: r.u64()?,
            filter: r.string()?,
        }),
        TAG_FORK_SUBSCRIBE => Some(Request::ForkSubscribe {
            fork_at: r.u64()?,
            replacement_subject: r.string()?,
            replacement_body: r.bytes()?,
            filter: r.string()?,
        }),
        TAG_COMMIT => Some(Request::Commit {
            group: r.string()?,
            upto: r.u64()?,
        }),
        TAG_PROCESS => Some(Request::ProcessAndProduce {
            producer_id: r.u64()?,
            producer_seq: r.u64()?,
            out_subject: r.string()?,
            out_body: r.bytes()?,
            group: r.string()?,
            upto: r.u64()?,
        }),
        TAG_SUBSCRIBE_GROUP => Some(Request::SubscribeGroup {
            group: r.string()?,
            filter: r.string()?,
        }),
        TAG_ATTACH => Some(Request::Attach {
            grant: r.string()?,
            proof: r.bytes()?,
        }),
        TAG_WILL => Some(Request::Will {
            producer_id: r.u64()?,
            producer_seq: r.u64()?,
            subject: r.string()?,
            body: r.bytes()?,
        }),
        TAG_HELLO => Some(Request::Hello),
        TAG_KEEPALIVE => Some(Request::Keepalive),
        TAG_IDLE_MARKS => Some(Request::IdleMarks),
        TAG_BUDGET => Some(Request::Budget {
            producer_id: r.u64()?,
        }),
        TAG_REPLICATE => {
            let seq = r.u64()?;
            let producer_id = r.u64()?;
            let producer_seq = r.u64()?;
            let subject = r.string()?;
            let body = r.bytes()?;
            let commit = match r.u8()? {
                0 => None,
                1 => Some((r.string()?, r.u64()?)),
                _ => return None,
            };
            Some(Request::Replicate {
                seq,
                producer_id,
                producer_seq,
                subject,
                body,
                commit,
                leader: r.identity_trailer()?,
            })
        }
        TAG_LAST => Some(Request::Last {
            filter: r.string()?,
            after: r.string()?,
            max: r.u32()? as u32,
        }),
        TAG_FETCH => Some(Request::Fetch {
            from_offset: r.u64()?,
            filter: r.string()?,
            max: r.u32()? as u32,
        }),
        _ => None,
    }
}

/// Encode a response to a frame payload.
pub fn encode_response(resp: &Response) -> Vec<u8> {
    let mut p = vec![PROTO_VERSION, 0];
    match resp {
        Response::PublishAck { offset, deduped } => {
            p[1] = TAG_ACK;
            p.extend_from_slice(&offset.to_le_bytes());
            p.push(*deduped as u8);
        }
        Response::Delivery {
            offset,
            subject,
            body,
        } => {
            p[1] = TAG_DELIVERY;
            p.extend_from_slice(&offset.to_le_bytes());
            put_str(&mut p, subject);
            put_bytes(&mut p, body);
        }
        Response::Error { code, msg } => {
            p[1] = TAG_ERROR;
            p.push(*code);
            put_str(&mut p, msg);
        }
        Response::Mark {
            next,
            head,
            resume,
            durable,
        } => {
            p[1] = TAG_MARK;
            p.extend_from_slice(&next.to_le_bytes());
            p.extend_from_slice(&head.to_le_bytes());
            put_str(&mut p, resume);
            if let Some(d) = durable {
                p.extend_from_slice(&d.to_le_bytes());
            }
        }
        Response::Nonce { nonce, log, ext } => {
            p[1] = TAG_NONCE;
            put_bytes(&mut p, nonce);
            put_identity(&mut p, log.as_ref());
            // The block rides only behind an identity: a frame without one stays the
            // frame a broker that predates log identity sends.
            if log.is_some() {
                ext.put(&mut p);
            }
        }
        Response::Budget {
            subjects,
            reserved,
            max,
        } => {
            p[1] = TAG_BUDGET_REPLY;
            p.extend_from_slice(&subjects.to_le_bytes());
            p.extend_from_slice(&reserved.to_le_bytes());
            p.extend_from_slice(&max.to_le_bytes());
        }
    }
    p
}

/// Encode a `Delivery` payload directly from BORROWED fields — the egress hot path.
/// Byte-identical to `encode_response(&Response::Delivery { offset, subject, body })`,
/// but it does not construct/own a `Response`, so a shared `Arc`'d log record's subject
/// and body are read by reference (no deep copy on the read path; the only copy is the
/// one unavoidable assembly into the outgoing frame).
pub fn encode_delivery(offset: u64, subject: &str, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::new();
    put_delivery(&mut p, offset, subject, body);
    p
}

fn put_delivery(p: &mut Vec<u8>, offset: u64, subject: &str, body: &[u8]) {
    p.extend_from_slice(&[PROTO_VERSION, TAG_DELIVERY]);
    p.extend_from_slice(&offset.to_le_bytes());
    put_str(p, subject);
    put_bytes(p, body);
}

fn put_publish(p: &mut Vec<u8>, producer_id: u64, producer_seq: u64, subject: &str, body: &[u8]) {
    p.extend_from_slice(&[PROTO_VERSION, TAG_PUBLISH]);
    p.extend_from_slice(&producer_id.to_le_bytes());
    p.extend_from_slice(&producer_seq.to_le_bytes());
    put_str(p, subject);
    put_bytes(p, body);
}

fn encode_header_bytes(len32: u32, crc: u32) -> [u8; HEADER_SIZE] {
    let l = len32.to_le_bytes();
    let c = crc.to_le_bytes();
    [
        FRAME_MAGIC[0],
        FRAME_MAGIC[1],
        FRAME_VERSION,
        0,
        l[0],
        l[1],
        l[2],
        l[3],
        c[0],
        c[1],
        c[2],
        c[3],
    ]
}

/// Append one framed `Publish` request to `out`, assembled in place: `subject` and
/// `body` are copied once into `out` instead of allocating a `String`, a `Vec<u8>`,
/// a request payload buffer, and a framed buffer.
pub(crate) fn append_publish_frame(
    out: &mut Vec<u8>,
    producer_id: u64,
    producer_seq: u64,
    subject: &str,
    body: &[u8],
) -> io::Result<()> {
    append_framed(out, |p| {
        put_publish(p, producer_id, producer_seq, subject, body)
    })
}

/// Compute the exact wire CRC-32/IEEE of a `Response::Delivery { offset, subject, body }`
/// payload without allocating or copying a temporary payload buffer.
#[must_use]
pub fn compute_delivery_crc(offset: u64, subject: &str, body: &[u8]) -> u32 {
    let mut hdr = [0u8; 14];
    hdr[0] = PROTO_VERSION;
    hdr[1] = TAG_DELIVERY;
    hdr[2..10].copy_from_slice(&offset.to_le_bytes());
    hdr[10..14].copy_from_slice(&(subject.len() as u32).to_le_bytes());
    let crc = astream_wire::crc32_ieee_update(0, &hdr);
    let crc = astream_wire::crc32_ieee_update(crc, subject.as_bytes());
    let crc = astream_wire::crc32_ieee_update(crc, &(body.len() as u32).to_le_bytes());
    astream_wire::crc32_ieee_update(crc, body)
}

/// Append one framed `Delivery` to `out`, assembled in place: the body is copied once,
/// into `out`, instead of into a payload, then a `Frame`, then the encoded frame. A
/// streaming verb gathers several into one buffer and writes them with one call.
pub(crate) fn append_delivery_frame(
    out: &mut Vec<u8>,
    offset: u64,
    subject: &str,
    body: &[u8],
) -> io::Result<()> {
    append_delivery_frame_with_crc(out, offset, subject, body, None).map(|_| ())
}

/// Append one framed `Delivery` to `out`, reusing `precomputed_crc` when `Some` so a
/// 64-subscriber fan-out computes the frame CRC once per record instead of 64 times.
/// Returns the frame's CRC-32/IEEE so the caller can populate a lock-free cache.
pub(crate) fn append_delivery_frame_with_crc(
    out: &mut Vec<u8>,
    offset: u64,
    subject: &str,
    body: &[u8],
    precomputed_crc: Option<u32>,
) -> io::Result<u32> {
    let start = out.len();
    out.extend_from_slice(&[0u8; HEADER_SIZE]);
    let payload_start = out.len();
    put_delivery(out, offset, subject, body);
    let len = out.len().saturating_sub(payload_start);
    let Some(len32) = u32::try_from(len).ok().filter(|_| len <= MAX_PAYLOAD_LEN) else {
        out.truncate(start);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload exceeds cap",
        ));
    };
    let payload_slice = out.get(payload_start..).unwrap_or(&[]);
    let crc = precomputed_crc.unwrap_or_else(|| crc32_ieee(payload_slice));
    if let Some((hdr, _)) = out
        .get_mut(start..)
        .and_then(|s| s.split_first_chunk_mut::<HEADER_SIZE>())
    {
        *hdr = encode_header_bytes(len32, crc);
    }
    Ok(crc)
}

/// Append `payload`, framed, to `out` (one copy). See [`append_delivery_frame`].
pub(crate) fn append_frame(out: &mut Vec<u8>, payload: &[u8]) -> io::Result<()> {
    append_framed(out, |p| p.extend_from_slice(payload))
}

/// Frame, in place, the payload `fill` appends to `out`: the header is reserved first
/// and written once the payload's length and CRC are known. Byte-identical to
/// `Frame::new(payload).encode()`. A payload over the cap leaves `out` as it was and
/// is `InvalidData`.
fn append_framed(out: &mut Vec<u8>, fill: impl FnOnce(&mut Vec<u8>)) -> io::Result<()> {
    let start = out.len();
    out.extend_from_slice(&[0u8; HEADER_SIZE]);
    let payload_start = out.len();
    fill(out);
    let len = out.len().saturating_sub(payload_start);
    let Some(len32) = u32::try_from(len).ok().filter(|_| len <= MAX_PAYLOAD_LEN) else {
        out.truncate(start);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload exceeds cap",
        ));
    };
    let crc = crc32_ieee(out.get(payload_start..).unwrap_or(&[]));
    if let Some((hdr, _)) = out
        .get_mut(start..)
        .and_then(|s| s.split_first_chunk_mut::<HEADER_SIZE>())
    {
        *hdr = encode_header_bytes(len32, crc);
    }
    Ok(())
}

/// The broker's answer to a request in ANOTHER protocol, whose version byte was
/// `peer`: an [`ERR_PROTOCOL`] refusal in the layout every version keeps (the module
/// docs), stamped with `peer` so the peer's own decoder reads it, and carrying this
/// broker's [`PROTO_VERSION`] in the one byte after the message, where no decoder
/// looks — the byte [`foreign_protocol`] reads back.
///
/// Written out byte by byte, not through [`encode_response`]: whatever a later version
/// does to its own encoder, this is the frame a peer of every version must still read.
pub fn encode_protocol_refusal(peer: u8) -> Vec<u8> {
    let msg = format!(
        "protocol mismatch: this broker speaks astream protocol {PROTO_VERSION}, the request \
         came in protocol {peer} (run the same astream release on both ends)"
    );
    let mut p = vec![peer, TAG_ERROR, ERR_PROTOCOL];
    put_str(&mut p, &msg);
    p.push(PROTO_VERSION);
    p
}

/// The astream protocol a broker REPLY says its sender speaks, when that is not this
/// build's [`PROTO_VERSION`]: the version byte of a reply stamped with another one (an
/// older or newer broker answering in its own protocol — "malformed request", which is
/// all a broker that predates [`ERR_PROTOCOL`] can say), or the broker's version that
/// an [`encode_protocol_refusal`] refusal stamped with ours carries after its message.
/// `None` for a reply in this protocol, and for an empty one.
pub fn foreign_protocol(reply: &[u8]) -> Option<u8> {
    foreign_protocol_at(PROTO_VERSION, reply)
}

/// [`foreign_protocol`] as a client of protocol `ours` reads it.
fn foreign_protocol_at(ours: u8, reply: &[u8]) -> Option<u8> {
    let mut r = Reader { b: reply, i: 0 };
    let version = r.u8()?;
    if version != ours {
        return Some(version);
    }
    if r.u8()? != TAG_ERROR || r.u8()? != ERR_PROTOCOL {
        return None;
    }
    let len = r.u32()?;
    r.i = r.i.checked_add(len).filter(|&end| end <= reply.len())?;
    let broker = r.u8()?;
    (r.i == reply.len() && broker != ours).then_some(broker)
}

/// Decode a response from a frame payload (bounds-checked; `None` on malformed).
pub fn decode_response(p: &[u8]) -> Option<Response> {
    let mut r = Reader { b: p, i: 0 };
    if r.u8()? != PROTO_VERSION {
        return None;
    }
    match r.u8()? {
        TAG_ACK => Some(Response::PublishAck {
            offset: r.u64()?,
            deduped: r.u8()? != 0,
        }),
        TAG_DELIVERY => Some(Response::Delivery {
            offset: r.u64()?,
            subject: r.string()?,
            body: r.bytes()?,
        }),
        TAG_ERROR => Some(Response::Error {
            code: r.u8()?,
            msg: r.string()?,
        }),
        TAG_MARK => {
            let next = r.u64()?;
            let head = r.u64()?;
            // NOT optional. An older broker is refused at the version byte above, so a
            // Mark that ends after `head` is a current peer's truncated frame, and an
            // empty `resume` means something specific ("the scan reached the end of the
            // filter's range") that a truncation must not be read as.
            let resume = r.string()?;
            // Optional: an older broker ends the body here. One to seven bytes more is a
            // frame cut short; anything past eight is a later trailer, skipped.
            let durable = match r.b.len().checked_sub(r.i)? {
                0 => None,
                1..=7 => return None,
                _ => Some(r.u64()?),
            };
            Some(Response::Mark {
                next,
                head,
                resume,
                durable,
            })
        }
        TAG_NONCE => {
            let nonce = r.bytes()?;
            let log = r.identity_trailer()?;
            let ext = NonceExt::read(r.b.get(r.i..)?)?;
            Some(Response::Nonce { nonce, log, ext })
        }
        TAG_BUDGET_REPLY => Some(Response::Budget {
            subjects: r.u64()?,
            reserved: r.u64()?,
            max: r.u64()?,
        }),
        _ => None,
    }
}

/// Bytes of a frame's payload read per `read_exact` call: a frame's buffer GROWS as its
/// bytes arrive rather than being allocated at the declared length up front, so a peer
/// that announces a 16 MiB frame and then stalls pins one chunk (plus what it actually
/// sent) until its read times out — not the whole declared length.
#[cfg(test)]
const READ_CHUNK: usize = 64 * 1024;

/// Read exactly one frame's payload from `r` (blocking). `Ok(None)` on a clean EOF
/// (peer closed at a frame boundary); `Err` on a malformed/oversized frame, or
/// `UnexpectedEof` on a frame cut short — in its header or its payload. `read_exact`
/// handles partial reads (it loops until each chunk arrives).
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut hdr = [0u8; HEADER_SIZE];
    // The first read decides between a clean close (nothing at all) and a frame;
    // once any header byte has arrived, a short header is a truncated frame.
    let first = loop {
        match r.read(&mut hdr) {
            Ok(n) => break n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    };
    if first == 0 {
        return Ok(None);
    }
    let Some(hdr_rem) = hdr.get_mut(first..) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid read length",
        ));
    };
    r.read_exact(hdr_rem)?;
    // The header is judged BEFORE the payload it announces is read: a bad magic,
    // version or flags byte, or a length over the cap, is refused on these 12 bytes.
    // A header on its own decodes as "need more bytes" — or, announcing an empty
    // payload, as the whole frame.
    match Frame::decode(&hdr) {
        Ok(None) => {}
        Ok(Some(d)) => return Ok(Some(d.frame.payload)),
        Err(FrameError::TooLarge) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame exceeds cap",
            ))
        }
        Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt frame")),
    }
    let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
    let crc = u32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    // Read straight into the payload buffer (no header copy, no second copy out).
    let mut payload = Vec::new();
    (&mut *r).take(len as u64).read_to_end(&mut payload)?;
    if payload.len() < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "truncated frame",
        ));
    }
    if crc32_ieee(&payload) != crc {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt frame"));
    }
    Ok(Some(payload))
}

/// Frame `payload` and write it to `w` (blocking), in one `write_all`.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let mut out = Vec::new();
    append_frame(&mut out, payload)?;
    w.write_all(&out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let p = Request::Publish {
            producer_id: 9,
            producer_seq: 4,
            subject: "/a/stream/x".into(),
            body: b"hi".to_vec(),
        };
        assert_eq!(decode_request(&encode_request(&p)), Some(p));
        let s = Request::Subscribe {
            from_offset: 7,
            filter: "/a/stream/>".into(),
        };
        assert_eq!(decode_request(&encode_request(&s)), Some(s));
        let b = Request::Budget {
            producer_id: 0x0102_0304_0506_0708,
        };
        assert_eq!(decode_request(&encode_request(&b)), Some(b));
    }

    #[test]
    fn replicate_round_trips_with_and_without_a_commit() {
        let plain = Request::Replicate {
            seq: 11,
            producer_id: 3,
            producer_seq: 4,
            subject: "/a/r/x".into(),
            body: b"m".to_vec(),
            commit: None,
            leader: None,
        };
        assert_eq!(decode_request(&encode_request(&plain)), Some(plain.clone()));
        let with_commit = Request::Replicate {
            seq: 11,
            producer_id: 3,
            producer_seq: 4,
            subject: "/a/r/x".into(),
            body: b"m".to_vec(),
            commit: Some(("g1".to_string(), 10)),
            leader: None,
        };
        let bytes = encode_request(&with_commit);
        assert_eq!(decode_request(&bytes), Some(with_commit));
        // The borrowed-field encoder is byte-identical to the Request encoder.
        assert_eq!(
            encode_replicate(11, 3, 4, "/a/r/x", b"m", Some(("g1", 10)), None),
            bytes
        );
        // An unknown commit tag byte is malformed, not misread.
        let mut bad = encode_replicate(1, 1, 1, "/a/x", b"", None, None);
        *bad.last_mut().unwrap() = 2;
        assert_eq!(decode_request(&bad), None);
    }

    /// A reader that hands out at most `step` bytes per call — the shape of a socket
    /// delivering a large frame in pieces.
    struct Trickle<'a> {
        bytes: &'a [u8],
        step: usize,
    }
    impl std::io::Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.bytes.len().min(self.step).min(buf.len());
            buf[..n].copy_from_slice(&self.bytes[..n]);
            self.bytes = &self.bytes[n..];
            Ok(n)
        }
    }

    /// A frame larger than one read chunk is assembled chunk by chunk (from a reader
    /// that trickles it) byte-identically, and a frame cut short mid-payload is
    /// `UnexpectedEof`, never a partial payload.
    #[test]
    fn large_frames_are_read_in_chunks_and_a_short_one_is_unexpected_eof() {
        let payload: Vec<u8> = (0..(3 * READ_CHUNK + 17))
            .map(|i| (i % 251) as u8)
            .collect();
        let mut wire = Vec::new();
        write_frame(&mut wire, &payload).unwrap();
        let mut trickle = Trickle {
            bytes: &wire,
            step: 1000,
        };
        assert_eq!(read_frame(&mut trickle).unwrap(), Some(payload));
        assert_eq!(
            read_frame(&mut trickle).unwrap(),
            None,
            "clean EOF after the frame"
        );
        let mut short = Trickle {
            bytes: &wire[..wire.len() - 5],
            step: 4096,
        };
        assert_eq!(
            read_frame(&mut short).unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    /// Only a stream that ends exactly at a frame boundary is a clean close. One cut
    /// short INSIDE the 12-byte header is `UnexpectedEof`, like one cut inside its
    /// payload — a subscriber must not read a broker that died mid-header as a
    /// stream that ended.
    #[test]
    fn a_frame_cut_inside_its_header_is_unexpected_eof() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"payload").unwrap();
        for cut in 1..HEADER_SIZE {
            let mut short = Trickle {
                bytes: &wire[..cut],
                step: 4096,
            };
            assert_eq!(
                read_frame(&mut short).map_err(|e| e.kind()),
                Err(io::ErrorKind::UnexpectedEof),
                "header cut after {cut} bytes"
            );
        }
        let mut empty = Trickle {
            bytes: &[],
            step: 4096,
        };
        assert_eq!(read_frame(&mut empty).unwrap(), None);
    }

    /// The in-place framer is byte-identical to the wire crate's `Frame::encode`, a
    /// framed delivery to `encode_delivery` framed, and an over-cap payload is refused
    /// leaving the buffer as it was.
    #[test]
    fn in_place_framing_matches_frame_encode() {
        for payload in [&b""[..], b"x", &[7u8; 70_000]] {
            let mut out = b"prefix".to_vec();
            append_frame(&mut out, payload).unwrap();
            assert_eq!(&out[..6], b"prefix");
            assert_eq!(out[6..], Frame::new(payload.to_vec()).encode().unwrap()[..]);
        }
        let mut out = Vec::new();
        append_delivery_frame(&mut out, 9, "/a/x", b"body").unwrap();
        assert_eq!(
            out,
            Frame::new(encode_delivery(9, "/a/x", b"body"))
                .encode()
                .unwrap()
        );
        let mut out = b"kept".to_vec();
        let big = vec![0u8; MAX_PAYLOAD_LEN + 1];
        assert_eq!(
            append_frame(&mut out, &big).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(out, b"kept");
    }

    /// A header that announces a frame the reader must refuse is refused on the header
    /// alone: the reader never waits for (or buffers) the payload it announces.
    #[test]
    fn a_bad_header_is_refused_before_its_payload_is_read() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &[1u8; 32]).unwrap();
        // Bad magic, over-cap length: only the 12 header bytes are available.
        let mut bad_magic = wire[..HEADER_SIZE].to_vec();
        bad_magic[0] ^= 0xFF;
        let mut over_cap = wire[..HEADER_SIZE].to_vec();
        over_cap[4..8].copy_from_slice(&(MAX_PAYLOAD_LEN as u32 + 1).to_le_bytes());
        for hdr in [bad_magic, over_cap] {
            let mut r = Trickle {
                bytes: &hdr,
                step: 4096,
            };
            assert_eq!(
                read_frame(&mut r).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
        // A flipped payload byte is still caught by the CRC.
        let mut corrupt = wire.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        let mut r = Trickle {
            bytes: &corrupt,
            step: 4096,
        };
        assert_eq!(
            read_frame(&mut r).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn responses_round_trip() {
        let a = Response::PublishAck {
            offset: 3,
            deduped: true,
        };
        assert_eq!(decode_response(&encode_response(&a)), Some(a));
        let d = Response::Delivery {
            offset: 5,
            subject: "/a/stream/x".into(),
            body: b"body".to_vec(),
        };
        assert_eq!(decode_response(&encode_response(&d)), Some(d));
        // Three u64s of one shape: distinct values, so a swapped field cannot pass.
        let b = Response::Budget {
            subjects: 4095,
            reserved: 1,
            max: 4096,
        };
        assert_eq!(decode_response(&encode_response(&b)), Some(b));
    }

    #[test]
    fn frame_round_trip_in_memory_and_clean_eof() {
        let mut buf = Vec::new();
        write_frame(
            &mut buf,
            &encode_request(&Request::Subscribe {
                from_offset: 0,
                filter: "/a/>".into(),
            }),
        )
        .unwrap();
        let mut cur = std::io::Cursor::new(buf);
        let payload = read_frame(&mut cur).unwrap().unwrap();
        assert!(matches!(
            decode_request(&payload),
            Some(Request::Subscribe { .. })
        ));
        // No more bytes => clean EOF.
        assert_eq!(read_frame(&mut cur).unwrap(), None);
    }

    /// PROTOCOL ECONOMY, PINNED. The whole fabric —
    /// retained values, bounded reads, a last will, a proof-of-possession attach, an
    /// exactly-once inbox, request/reply, barriers, presence, negotiated liveness and a
    /// producer's view of its own subject budget — rides on FIFTEEN request tags and SIX
    /// response tags. This test is the budget: it enumerates every byte and fails if a
    /// sixteenth request tag or a seventh response tag ever decodes, and it pins each
    /// tag's NUMBER, so a renumbering that would silently re-interpret an old client's
    /// frames breaks the build instead. `Keepalive` at 0x0D, `IdleMarks` at 0x0E,
    /// `Budget` at 0x0F and `Budget`'s reply at 0x86 took numbers no earlier version
    /// decoded: an older broker refuses either request as malformed and keeps the
    /// connection, and never sends the reply. Additions, not reinterpretations; no
    /// version change.
    #[test]
    fn the_tag_budget_is_fifteen_requests_and_six_responses() {
        // Version 2: `Attach` kept its tag (0x07) and its wire SHAPE (a str then a
        // byte string) while its MEANING changed (`{grant, proof}`, answered with a
        // Mark), so under an unchanged version byte a version-1 Attach would decode
        // CLEANLY into the new variant. The byte is checked on every decode, which
        // turns that silent reinterpretation into a refusal. The stored format is
        // unaffected.
        assert_eq!(
            PROTO_VERSION, 2,
            "PROTO_VERSION moved to 2 with Attach's redefinition"
        );
        assert_eq!(
            crate::brecord::BREC_VERSION,
            2,
            "the stored record format stays 2: nothing here is an on-disk change"
        );

        // Every tag's NUMBER, pinned against the variant that carries it.
        let request_tags: [(u8, Request); 15] = [
            (
                0x01,
                Request::Publish {
                    producer_id: 0,
                    producer_seq: 0,
                    subject: String::new(),
                    body: Vec::new(),
                },
            ),
            (
                0x02,
                Request::Subscribe {
                    from_offset: 0,
                    filter: String::new(),
                },
            ),
            (
                0x03,
                Request::ForkSubscribe {
                    fork_at: 0,
                    replacement_subject: String::new(),
                    replacement_body: Vec::new(),
                    filter: String::new(),
                },
            ),
            (
                0x04,
                Request::Commit {
                    group: String::new(),
                    upto: 0,
                },
            ),
            (
                0x05,
                Request::ProcessAndProduce {
                    producer_id: 0,
                    producer_seq: 0,
                    out_subject: String::new(),
                    out_body: Vec::new(),
                    group: String::new(),
                    upto: 0,
                },
            ),
            (
                0x06,
                Request::SubscribeGroup {
                    group: String::new(),
                    filter: String::new(),
                },
            ),
            (
                0x07,
                Request::Attach {
                    grant: String::new(),
                    proof: Vec::new(),
                },
            ),
            (
                0x08,
                Request::Replicate {
                    seq: 0,
                    producer_id: 0,
                    producer_seq: 0,
                    subject: String::new(),
                    body: Vec::new(),
                    commit: None,
                    leader: None,
                },
            ),
            (
                0x09,
                Request::Last {
                    filter: String::new(),
                    after: String::new(),
                    max: 0,
                },
            ),
            (
                0x0A,
                Request::Fetch {
                    from_offset: 0,
                    filter: String::new(),
                    max: 0,
                },
            ),
            (
                0x0B,
                Request::Will {
                    producer_id: 0,
                    producer_seq: 0,
                    subject: String::new(),
                    body: Vec::new(),
                },
            ),
            (0x0C, Request::Hello),
            (0x0D, Request::Keepalive),
            (0x0E, Request::IdleMarks),
            (0x0F, Request::Budget { producer_id: 0 }),
        ];
        for (tag, req) in &request_tags {
            let bytes = encode_request(req);
            assert_eq!(bytes[1], *tag, "tag number moved for {req:?}");
            assert_eq!(
                decode_request(&bytes).as_ref(),
                Some(req),
                "round trip for tag {tag:#04x}"
            );
        }

        let response_tags: [(u8, Response); 6] = [
            (
                0x81,
                Response::PublishAck {
                    offset: 0,
                    deduped: false,
                },
            ),
            (
                0x82,
                Response::Delivery {
                    offset: 0,
                    subject: String::new(),
                    body: Vec::new(),
                },
            ),
            (
                0x83,
                Response::Error {
                    code: 0,
                    msg: String::new(),
                },
            ),
            (
                0x84,
                Response::Mark {
                    next: 0,
                    head: 0,
                    resume: String::new(),
                    durable: Some(0),
                },
            ),
            (
                0x85,
                Response::Nonce {
                    nonce: Vec::new(),
                    log: None,
                    ext: NonceExt::default(),
                },
            ),
            (
                0x86,
                Response::Budget {
                    subjects: 0,
                    reserved: 0,
                    max: 0,
                },
            ),
        ];
        for (tag, resp) in &response_tags {
            let bytes = encode_response(resp);
            assert_eq!(bytes[1], *tag, "tag number moved for {resp:?}");
            assert_eq!(
                decode_response(&bytes).as_ref(),
                Some(resp),
                "round trip for tag {tag:#04x}"
            );
        }

        // THE BUDGET: sweep every byte. A zero-filled body satisfies every declared
        // tag's parser (empty strings, empty byte vectors, a `None` commit), so a tag
        // decodes here IF AND ONLY IF it is declared — an undeclared byte is `None`,
        // which is what keeps an old broker's answer to a new client `Error 5,
        // "malformed request"` and nothing else.
        let mut body = vec![PROTO_VERSION, 0];
        body.extend_from_slice(&[0u8; 64]);
        let declared_requests: Vec<u8> = request_tags.iter().map(|(t, _)| *t).collect();
        let declared_responses: Vec<u8> = response_tags.iter().map(|(t, _)| *t).collect();
        let mut decodable_requests = Vec::new();
        let mut decodable_responses = Vec::new();
        for tag in 0u8..=255 {
            body[1] = tag;
            if decode_request(&body).is_some() {
                decodable_requests.push(tag);
            }
            if decode_response(&body).is_some() {
                decodable_responses.push(tag);
            }
        }
        assert_eq!(
            decodable_requests, declared_requests,
            "exactly fifteen request tags decode, and no other byte does"
        );
        assert_eq!(
            decodable_responses, declared_responses,
            "exactly six response tags decode, and no other byte does"
        );

        // And the version byte still gates every tag, response tags included: the
        // right tag under the wrong version decodes to nothing at all. The body is
        // the same zero-filled one the sweep just proved every declared tag's parser
        // accepts, so what refuses these frames is the version byte — not a body that
        // ran out early.
        body[0] = PROTO_VERSION + 1;
        for tag in &declared_requests {
            body[1] = *tag;
            assert_eq!(
                decode_request(&body),
                None,
                "request tag {tag:#04x} decoded under a wrong version byte"
            );
        }
        for tag in &declared_responses {
            body[1] = *tag;
            assert_eq!(
                decode_response(&body),
                None,
                "response tag {tag:#04x} decoded under a wrong version byte"
            );
        }
    }

    #[test]
    fn malformed_payload_decodes_to_none() {
        assert_eq!(decode_request(&[]), None);
        assert_eq!(decode_request(&[PROTO_VERSION, TAG_PUBLISH, 1, 2]), None);
        assert_eq!(decode_request(&[9, TAG_PUBLISH]), None); // bad version
    }

    /// A `Mark` that stops after `head` is MALFORMED, not an older broker's frame: an
    /// older broker is refused at the version byte, so tolerating the short form would
    /// only read a current peer's truncated page as `resume = ""` — "the scan reached
    /// the end of the filter's range".
    #[test]
    fn a_mark_truncated_after_head_is_malformed() {
        let full = encode_response(&Response::Mark {
            next: 4,
            head: 9,
            resume: "/f/x".to_string(),
            durable: None,
        });
        assert_eq!(
            decode_response(&full),
            Some(Response::Mark {
                next: 4,
                head: 9,
                resume: "/f/x".to_string(),
                durable: None,
            })
        );
        // The 18-byte prefix: version, tag, next, head, and nothing more.
        assert_eq!(
            decode_response(&full[..18]),
            None,
            "a short Mark is malformed"
        );
        // An older broker does not get this far: its version byte is refused.
        let mut older = full.clone();
        older[0] = PROTO_VERSION - 1;
        assert_eq!(decode_response(&older), None);
    }

    /// The `Mark` decoder of the release before `Mark.durable`, frozen here as it stood
    /// (next, head, resume — and no check of where the body ends): what an OLDER client
    /// runs on a `Mark` this build's broker sends.
    fn older_client_mark(p: &[u8]) -> Option<(u64, u64, String)> {
        let mut r = Reader { b: p, i: 0 };
        if r.u8()? != PROTO_VERSION || r.u8()? != TAG_MARK {
            return None;
        }
        Some((r.u64()?, r.u64()?, r.string()?))
    }

    /// `Mark.durable` is an ADDITIVE trailer both ways: an older client reads a `Mark`
    /// carrying it as the same `(next, head, resume)` (it never checked where the body
    /// ends); this build reads an older broker's `Mark`, which ends at `resume`, as
    /// `durable: None`. One to seven bytes after `resume` is a frame cut short; bytes past
    /// the eight are a later trailer, skipped.
    #[test]
    fn a_marks_durable_trailer_is_additive_both_ways() {
        let with = Response::Mark {
            next: 4,
            head: 9,
            resume: "/f/x".to_string(),
            durable: Some(7),
        };
        let bytes = encode_response(&with);
        assert_eq!(
            older_client_mark(&bytes),
            Some((4, 9, "/f/x".to_string())),
            "an older client reads a new broker's Mark"
        );
        assert_eq!(decode_response(&bytes), Some(with.clone()));
        // An older broker's Mark: the same bytes without the trailer.
        let without = &bytes[..bytes.len() - 8];
        let Some(Response::Mark { durable, .. }) = decode_response(without) else {
            panic!("an older broker's Mark decodes")
        };
        assert_eq!(
            durable, None,
            "an older broker says nothing about durability"
        );
        for cut in 1..8 {
            assert_eq!(
                decode_response(&bytes[..bytes.len() - cut]),
                None,
                "a trailer cut {cut} bytes short is malformed"
            );
        }
        let mut later = bytes.clone();
        later.extend_from_slice(&[0xAB; 5]);
        assert_eq!(
            decode_response(&later),
            Some(with),
            "a later trailer is skipped"
        );
    }

    /// The log-identity trailers are ADDITIVE, so neither side has to be upgraded
    /// first. A frame without one is byte for byte the frame a peer that predates log
    /// identity sends, and reads as `None`. A frame with one STARTS with exactly those
    /// bytes, which is all such a peer's decoder reads (it never checked where a body
    /// ends). A body that ends INSIDE one is malformed, never "no identity"; bytes past
    /// one are left unread for the next trailer.
    #[test]
    fn identity_trailers_are_additive_in_both_directions() {
        let ident = LogIdentity {
            id: [0xab; 16],
            generation: 7,
        };
        let nonce = |log| Response::Nonce {
            nonce: vec![1; 32],
            log,
            ext: NonceExt::default(),
        };
        let mut older = vec![PROTO_VERSION, TAG_NONCE];
        older.extend_from_slice(&32u32.to_le_bytes());
        older.extend_from_slice(&[1; 32]);
        let bare = encode_response(&nonce(None));
        let with = encode_response(&nonce(Some(ident)));
        assert_eq!(bare, older, "no identity: the older peer's frame exactly");
        assert_eq!(
            with[..older.len()],
            older[..],
            "the older frame is a prefix"
        );
        assert_eq!(with.len(), older.len() + LogIdentity::WIRE_LEN);
        assert_eq!(decode_response(&bare), Some(nonce(None)));
        assert_eq!(decode_response(&with), Some(nonce(Some(ident))));
        for cut in 1..LogIdentity::WIRE_LEN {
            assert_eq!(
                decode_response(&with[..older.len() + cut]),
                None,
                "cut {cut}"
            );
        }
        // Behind the identity comes the extension block: an entry of an unknown tag is
        // skipped by its length, one cut short is a malformed frame.
        let mut longer = with.clone();
        longer.extend_from_slice(&[0x7f, 2, 0, 0xaa, 0xbb]);
        assert_eq!(decode_response(&longer), Some(nonce(Some(ident))));
        longer.push(9);
        assert_eq!(decode_response(&longer), None, "an entry cut short");

        let replicate = |leader| Request::Replicate {
            seq: 3,
            producer_id: 4,
            producer_seq: 5,
            subject: "/a/x".into(),
            body: b"b".to_vec(),
            commit: Some(("g".into(), 2)),
            leader,
        };
        let bare = encode_request(&replicate(None));
        let with = encode_request(&replicate(Some(ident)));
        assert_eq!(
            bare,
            encode_replicate(3, 4, 5, "/a/x", b"b", Some(("g", 2)), None)
        );
        assert_eq!(
            with,
            encode_replicate(3, 4, 5, "/a/x", b"b", Some(("g", 2)), Some(&ident))
        );
        assert_eq!(with[..bare.len()], bare[..], "the older frame is a prefix");
        assert_eq!(decode_request(&bare), Some(replicate(None)));
        assert_eq!(decode_request(&with), Some(replicate(Some(ident))));
        for cut in 1..LogIdentity::WIRE_LEN {
            assert_eq!(decode_request(&with[..bare.len() + cut]), None, "cut {cut}");
        }
    }

    /// THE REFUSAL A PEER OF ANY VERSION READS, pinned byte by byte:
    /// The `Nonce` extension block: the served tier (0x01) and the lineage (0x02) round
    /// trip; the frame without them is byte-identical to the identity-only frame, which
    /// is a prefix of the frame with them — exactly what an OLDER client reads, stopping
    /// after the identity; an unknown tag (0x03, reserved for the retire design's
    /// request set) is skipped; a known entry whose bytes are not its layout, or a block
    /// with no identity before it, is malformed or absent.
    #[test]
    fn the_nonce_extension_block_is_additive_and_skips_unknown_tags() {
        let ident = LogIdentity {
            id: [0xcd; 16],
            generation: 3,
        };
        let ext = NonceExt {
            durability: Some(ServedInfo {
                requested: Tier::Relaxed,
                served: Tier::Relaxed,
                reason: ServedReason::None,
                horizon_ms: None,
                rules: 0,
            }),
            lineage: Some(Lineage::default().step(3, 41)),
        };
        let nonce = |log, ext| Response::Nonce {
            nonce: vec![2; 32],
            log,
            ext,
        };
        let plain = encode_response(&nonce(Some(ident), NonceExt::default()));
        let full = encode_response(&nonce(Some(ident), ext.clone()));
        assert_eq!(
            full[..plain.len()],
            plain[..],
            "an older client's frame is a prefix"
        );
        assert_eq!(
            decode_response(&full),
            Some(nonce(Some(ident), ext.clone()))
        );
        // With no identity no block is sent, whatever the caller passed.
        assert_eq!(
            encode_response(&nonce(None, ext.clone())),
            encode_response(&nonce(None, NonceExt::default()))
        );
        // A future tier and reason are kept as told, not refused.
        let mut future = full.clone();
        let at = plain.len() + 3;
        future[at] = 9;
        future[at + 2] = 77;
        match decode_response(&future) {
            Some(Response::Nonce { ext, .. }) => {
                let d = ext.durability.unwrap();
                assert_eq!(
                    (d.requested, d.reason),
                    (Tier::Other(9), ServedReason::Other(77))
                );
            }
            other => panic!("{other:?}"),
        }
        // The reserved tag 0x03 is skipped.
        let mut reserved = full.clone();
        reserved.extend_from_slice(&[0x03, 1, 0, 0xff]);
        assert_eq!(
            decode_response(&reserved),
            Some(nonce(Some(ident), ext.clone()))
        );
        // A durability entry one byte short is malformed.
        let mut short = plain.clone();
        short.extend_from_slice(&[0x01, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(decode_response(&short), None);
    }

    /// THE HORIZON TELLS NO BOUND FROM A FLUSH AFTER EVERY BATCH. An Ordered broker with
    /// no horizon (a power loss can take every batch since the last escalation) and one
    /// with a zero horizon (a full flush after every batch) are opposite promises, and
    /// both used to read `horizon_ms = 0` — the value Strict sent too. Now none reads
    /// `None`, zero reads `Some(0)`, a sub-millisecond horizon rounds UP to `Some(1)`
    /// (never down to the every-batch promise), and a horizon on a tier other than
    /// Ordered is no horizon whatever the wire carried (the broker before Ordered sent 0
    /// for Strict and Relaxed).
    #[test]
    fn the_hello_horizon_tells_none_from_zero() {
        use std::time::Duration;
        let ident = LogIdentity {
            id: [0xcd; 16],
            generation: 0,
        };
        let told = |requested: Tier, horizon_ms: Option<u32>| {
            let ext = NonceExt {
                durability: Some(ServedInfo {
                    requested,
                    served: requested,
                    reason: ServedReason::None,
                    horizon_ms,
                    rules: 1,
                }),
                lineage: None,
            };
            let frame = encode_response(&Response::Nonce {
                nonce: vec![2; 32],
                log: Some(ident),
                ext,
            });
            match decode_response(&frame) {
                Some(Response::Nonce { ext, .. }) => ext.durability.unwrap().horizon_ms,
                other => panic!("{other:?}"),
            }
        };
        assert_eq!(told(Tier::Ordered, None), None, "no horizon");
        assert_eq!(told(Tier::Ordered, Some(0)), Some(0), "every batch");
        assert_eq!(told(Tier::Ordered, Some(1000)), Some(1000));
        assert_ne!(told(Tier::Ordered, None), told(Tier::Ordered, Some(0)));
        assert_eq!(told(Tier::Strict, Some(0)), None, "not Ordered: no horizon");
        assert_eq!(told(Tier::Relaxed, Some(0)), None);
        assert_eq!(ServedInfo::horizon_ms_of(Duration::ZERO), 0);
        assert_eq!(ServedInfo::horizon_ms_of(Duration::from_nanos(1)), 1);
        assert_eq!(ServedInfo::horizon_ms_of(Duration::from_micros(999)), 1);
        assert_eq!(ServedInfo::horizon_ms_of(Duration::from_millis(1000)), 1000);
        assert_eq!(ServedInfo::horizon_ms_of(Duration::from_micros(1001)), 2);
        assert_eq!(
            ServedInfo::horizon_ms_of(Duration::MAX),
            u32::MAX - 1,
            "a real horizon never reads as none"
        );
    }

    /// `[peer][0x83][ERR_PROTOCOL][len:u32 LE][msg][PROTO_VERSION]`. Under the peer's own
    /// version byte it is the refusal layout every version keeps, so the peer's decoder
    /// reads `Error { code: 9 }`; the byte after the message, which no decoder reads, is
    /// how the peer learns which protocol this broker speaks.
    #[test]
    fn a_protocol_refusal_is_readable_by_the_peer_it_refuses() {
        for peer in [0, PROTO_VERSION - 1, PROTO_VERSION + 1, u8::MAX] {
            let refusal = encode_protocol_refusal(peer);
            assert_eq!(refusal[..3], [peer, 0x83, ERR_PROTOCOL], "peer {peer}");
            let len = u32::from_le_bytes(refusal[3..7].try_into().unwrap()) as usize;
            assert_eq!(
                refusal.len(),
                7 + len + 1,
                "peer {peer}: the message, then one byte"
            );
            assert_eq!(refusal[refusal.len() - 1], PROTO_VERSION, "peer {peer}");
            // The decoder this build has is the protocol-2 one, so the frozen layout is
            // checked through it: the same bytes under its version byte are a refusal.
            let mut restamped = refusal.clone();
            restamped[0] = PROTO_VERSION;
            assert!(
                matches!(
                    decode_response(&restamped),
                    Some(Response::Error {
                        code: ERR_PROTOCOL,
                        ..
                    })
                ),
                "peer {peer}"
            );
            // And the peer, reading it in its own protocol, learns this broker's.
            assert_eq!(
                foreign_protocol_at(peer, &refusal),
                Some(PROTO_VERSION),
                "peer {peer}"
            );
        }
    }

    /// The two ways a reply says the broker speaks another protocol — stamped with
    /// another version (all a broker that predates `ERR_PROTOCOL` can send: its
    /// "malformed request", in its own protocol), or a protocol refusal in ours naming
    /// the broker's — and everything that is not one.
    #[test]
    fn foreign_protocol_reads_a_broker_in_another_protocol() {
        let stamped = |resp: &Response, version: u8| {
            let mut p = encode_response(resp);
            p[0] = version;
            p
        };
        let malformed = Response::Error {
            code: ERR_REFUSED,
            msg: "malformed request".into(),
        };
        let mark = Response::Mark {
            next: 1,
            head: 2,
            resume: String::new(),
            durable: None,
        };
        assert_eq!(
            foreign_protocol(&stamped(&malformed, PROTO_VERSION - 1)),
            Some(PROTO_VERSION - 1)
        );
        assert_eq!(
            foreign_protocol(&stamped(&mark, PROTO_VERSION + 1)),
            Some(PROTO_VERSION + 1)
        );
        // A newer broker's refusal, stamped with ours.
        let refusal_naming = |broker: u8| {
            let mut p = vec![PROTO_VERSION, TAG_ERROR, ERR_PROTOCOL];
            put_str(&mut p, "from a newer broker");
            p.push(broker);
            p
        };
        assert_eq!(
            foreign_protocol(&refusal_naming(PROTO_VERSION + 1)),
            Some(PROTO_VERSION + 1)
        );

        let mut two_after = refusal_naming(PROTO_VERSION + 1);
        two_after.push(0);
        for (what, reply) in [
            ("a refusal in ours", encode_response(&malformed)),
            ("a mark in ours", encode_response(&mark)),
            (
                "ERR_PROTOCOL naming no version",
                encode_response(&Response::Error {
                    code: ERR_PROTOCOL,
                    msg: String::new(),
                }),
            ),
            ("one naming ours", refusal_naming(PROTO_VERSION)),
            ("two bytes after the message", two_after),
            ("nothing", Vec::new()),
        ] {
            assert_eq!(foreign_protocol(&reply), None, "{what}");
        }
    }
}
