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
//! one that is the only writer of its subjects (its own lanes) may retire the record
//! for good, while one that shares a subject may wait for it. A broker older than
//! code 10 answers it with 5.
//!
//! One code-8 refusal is told apart by its WORDS, not by a code of its own: an
//! expired capability's message starts [`CAP_EXPIRED_PREFIX`], and
//! [`BrokerRefusal::is_expired`](crate::client::BrokerRefusal::is_expired) reads it.
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

use astream_wire::frame::{MAGIC as FRAME_MAGIC, VERSION as FRAME_VERSION};
use astream_wire::{crc32_ieee, Frame, FrameError, HEADER_SIZE, MAX_PAYLOAD_LEN};
use std::io::{self, Read, Write};

use crate::logid::LogIdentity;

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
    /// durable committed offset (broker-enforced resume), then tail live.
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
    Mark {
        next: u64,
        head: u64,
        resume: String,
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
    Nonce {
        nonce: Vec<u8>,
        log: Option<LogIdentity>,
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
        let v = *self.b.get(self.i)?;
        self.i += 1;
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
        let ident = LogIdentity::from_wire(self.b.get(self.i..)?)?;
        self.i += LogIdentity::WIRE_LEN;
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
    let mut p = vec![PROTO_VERSION, TAG_REPLICATE];
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
        Response::Mark { next, head, resume } => {
            p[1] = TAG_MARK;
            p.extend_from_slice(&next.to_le_bytes());
            p.extend_from_slice(&head.to_le_bytes());
            put_str(&mut p, resume);
        }
        Response::Nonce { nonce, log } => {
            p[1] = TAG_NONCE;
            put_bytes(&mut p, nonce);
            put_identity(&mut p, log.as_ref());
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
    let mut p = Vec::with_capacity(2 + 8 + 4 + subject.len() + 4 + body.len());
    put_delivery(&mut p, offset, subject, body);
    p
}

fn put_delivery(p: &mut Vec<u8>, offset: u64, subject: &str, body: &[u8]) {
    p.extend_from_slice(&[PROTO_VERSION, TAG_DELIVERY]);
    p.extend_from_slice(&offset.to_le_bytes());
    put_str(p, subject);
    put_bytes(p, body);
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
    append_framed(out, |p| put_delivery(p, offset, subject, body))
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
    out.resize(start + HEADER_SIZE, 0);
    fill(out);
    let len = out.len() - start - HEADER_SIZE;
    let Some(len32) = u32::try_from(len).ok().filter(|_| len <= MAX_PAYLOAD_LEN) else {
        out.truncate(start);
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "payload exceeds cap",
        ));
    };
    let crc = crc32_ieee(&out[start + HEADER_SIZE..]);
    let hdr = &mut out[start..start + HEADER_SIZE];
    hdr[..2].copy_from_slice(&FRAME_MAGIC);
    hdr[2] = FRAME_VERSION;
    hdr[3] = 0; // flags: none defined
    hdr[4..8].copy_from_slice(&len32.to_le_bytes());
    hdr[8..].copy_from_slice(&crc.to_le_bytes());
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
        TAG_MARK => Some(Response::Mark {
            next: r.u64()?,
            head: r.u64()?,
            // NOT optional. An older broker is refused at the version byte above, so a
            // Mark that ends after `head` is a current peer's truncated frame, and an
            // empty `resume` means something specific ("the scan reached the end of the
            // filter's range") that a truncation must not be read as.
            resume: r.string()?,
        }),
        TAG_NONCE => Some(Response::Nonce {
            nonce: r.bytes()?,
            log: r.identity_trailer()?,
        }),
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
    r.read_exact(&mut hdr[first..])?;
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
    let mut payload = Vec::with_capacity(len.min(READ_CHUNK));
    while payload.len() < len {
        let start = payload.len();
        payload.resize(start + (len - start).min(READ_CHUNK), 0);
        r.read_exact(&mut payload[start..])?;
    }
    if crc32_ieee(&payload) != crc {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "corrupt frame"));
    }
    Ok(Some(payload))
}

/// Frame `payload` and write it to `w` (blocking), in one `write_all`.
pub fn write_frame(w: &mut impl Write, payload: &[u8]) -> io::Result<()> {
    let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
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
                },
            ),
            (
                0x85,
                Response::Nonce {
                    nonce: Vec::new(),
                    log: None,
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
        });
        assert_eq!(
            decode_response(&full),
            Some(Response::Mark {
                next: 4,
                head: 9,
                resume: "/f/x".to_string()
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
        let mut longer = with.clone();
        longer.push(9);
        assert_eq!(decode_response(&longer), Some(nonce(Some(ident))));

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
