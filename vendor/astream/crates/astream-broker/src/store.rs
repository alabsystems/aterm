//! `BrokerLog` — the broker's durable message log: a TIERED (Strict / Ordered / Relaxed)
//! group-commit log with exactly-once ingest and durable consumer-group commits.
//!
//! Ports the atomic-append rollback and recover-on-open discipline from
//! `astream_engine::store`/`durable` (longest intact in-order prefix) and tightens
//! it: only a TORN TAIL — an incomplete trailing frame, or a zero-filled tail, the
//! signatures of an interrupted write — is truncated on open. A complete-but-corrupt
//! frame, an older record format (`BREC_VERSION`) this build no longer reads, or an
//! offset gap FOLLOWED BY MORE BYTES is corruption, and [`BrokerLog::open_with`]
//! refuses to open rather than silently truncating what may be acked data
//! ([`BrokerLog::open_repair`] is the explicit, data-losing repair). A record in a
//! NEWER format is neither: every open refuses it by name (`ErrorKind::Unsupported`),
//! and no repair touches it. Adds exactly-once **ingest**: a `(producer_id,
//! producer_seq)` idempotency key maps to the original `Offset`, so a re-sent publish
//! is NOT re-appended and returns the offset it first got — the precise pair, NOT the
//! engine's monotonic high-water (which would wrongly reject a producer legitimately
//! re-sending an old seq after a crash). The dedup map and the offset spine are
//! rebuilt from the stored bytes on open, so exactly-once survives a broker restart.
//!
//! The broker's hot path is GROUP COMMIT (`stage_*` + `BrokerLog::commit_batch`):
//! a batch of records is written, then ONE full flush (Strict), ONE ordering barrier
//! (Ordered) or nothing (Relaxed) covers it — and a batch holding a record the
//! escalation table names owes a full flush its caller issues outside the lock, the
//! visible head held at that record until it has (`Committed`). The per-record
//! `publish` / `commit` / `process_and_produce` methods are the un-batched equivalents
//! with the same tier semantics (an escalated record — or a resend of one whose original
//! no full flush covers — flushed in full before they return). An open log holds an
//! EXCLUSIVE advisory lock on its file, so two brokers can never serve one log.
//!
//! Every batch ends in a SEAL — its record count, after its last record's last field
//! — and every other record of it says the batch CONTINUES (`BREC_VERSION`); every
//! open records a DURABLE FLOOR in `<log>.id` after its own fsync (a clean close raises
//! it to the head). Together they let recovery judge damage above the floor by the
//! batches: a torn batch is cut at its start, so the log is always a prefix of whole
//! units, and only damage below the floor or behind a later complete unit — never a
//! torn write — makes the open refuse (`recover.rs`). Records with no mark at all (an
//! older build's, appended after a revert) are units of their own. A log no sealing
//! build has opened is judged by the rules above, verbatim. Every
//! read and write of the file and its identity sidecar goes through the storage seam
//! (`storage.rs`), which is how the crash simulation (`crash_sim.rs`) runs this exact
//! code against a device that loses power after any operation.

use crate::brecord::{BrokerRecord, GroupCommit, Mark};
use crate::logid::{self, Prior};
use crate::recover::{self, Classified};
use crate::storage::{Backing, Dir};
use astream_wire::{Filter, Frame, FrameError, Offset, Subject};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::fs::{File, OpenOptions as FsOpenOptions};
use std::io::{self, Write};
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(clean_islands)]
#[path = "store_island.rs"]
mod store_island;
#[cfg(not(clean_islands))]
pub use crate::twins::store_head_advance;
#[cfg(clean_islands)]
pub use store_island::store_head_advance;

contract_fn! {
    /// Verified check that a producer sequence number lies in the reserved ack half
    /// (`ack_base <= seq`), linked via `clean-kernel-defeq` to `store_ack_seq_in_reserved_half_isl`.
    #[must_use]
    pub const fn store_ack_seq_in_reserved_half(seq: u64, ack_base: u64) -> bool
    clauses [ensures result == (ack_base <= seq)]
    {
        ack_base <= seq
    }
}

/// Reserved subject for pure (no-output) consumer-group commit records. A wildcard
/// data filter (e.g. `/a/>` or `/a/*`) WOULD match it, so the broker's delivery
/// paths skip this subject explicitly (filter-independently): commit records consume
/// offsets but are never delivered to data subscribers. It is broker-INTERNAL: a
/// publish / process_and_produce / fork replacement on this subject is refused
/// (`StageErr::Reserved`), so the only records that carry it are commit records.
pub const COMMIT_SUBJECT: &str = "/a/commit";

/// Reserved subject for a connection's registered LAST WILL (the record the broker
/// appends on that connection's behalf when it ends). Hidden exactly like
/// [`COMMIT_SUBJECT`]: excluded from dedup, from the [`BrokerLog::last_matching`]
/// index, and from every delivery path, and refused to a client publish by name.
pub const WILL_SUBJECT: &str = "/a/will";

/// Reserved subject for the producer-id BINDING table (`producer id -> principal`,
/// appended when a bound capability first attaches). Hidden exactly like
/// [`COMMIT_SUBJECT`].
pub const BIND_SUBJECT: &str = "/a/bind";

/// Reserved subject for durable SUBJECT RETIREMENT (`producer_id` + UTF-8 target
/// subject): removes a retired subject (e.g. an exited `aterm` session's `presence`
/// and `ev` faces) from the last-value index (`last` / `carried_last`) and decrements
/// `subjects_per_producer` both live and on rebuild across restarts and compaction.
pub const RETIRE_SUBJECT: &str = "/a/retire";

/// Every broker-INTERNAL subject, in one place: a record on one of these consumes an
/// offset but is never delivered, never enters the dedup map, the per-producer
/// high-water map or the last-value index, and is refused to a client publish by
/// name. Kept as one predicate so a new hidden kind cannot be added to the store and
/// forgotten on a delivery path (e.g. commit records reaching a wildcard subscriber).
#[must_use]
pub fn is_hidden_subject(subject: &str) -> bool {
    matches!(
        subject,
        COMMIT_SUBJECT | WILL_SUBJECT | BIND_SUBJECT | RETIRE_SUBJECT
    )
}

/// The top half of the producer sequence space, RESERVED for acks.
///
/// An ack derives its `producer_seq` from the OFFSET it acknowledges, which is what
/// makes a retry idempotent, and that derivation has to live somewhere both faces of
/// this crate agree on: the library helper [`crate::ack`] and the `asb ack` CLI verb
/// produce records for the same logical operation, and a bridge will use BOTH. If the
/// two derived different keys for one `(producer_id, offset)`, the broker's dedup would
/// not recognise a retry that crossed faces and the ack would append twice. So the
/// derivation is `ACK_SEQ_BASE | offset` and it is a WIRE CONTRACT, read from this one
/// constant by both.
///
/// It is defined HERE, in the log, and re-exported by the client, because the
/// reservation is a property of the LOG and not a convention of a client: an ack's
/// sequence is derived from an offset, so it is NOT an incarnation, and the last-will
/// fence must not read it as one. `ACK_SEQ_BASE | offset` is above every
/// incarnation-derived sequence a producer will ever publish, so folding acks into the
/// fence's high water would put the fence above every will that producer could register
/// and suppress its goodbye for good. The log keeps the halves apart in both
/// directions: acks never enter [`BrokerLog::producer_high_water`], and a will offered
/// a sequence in this half is REFUSED at registration (`stage_will_register`) rather
/// than accepted and left permanently unfenceable.
pub const ACK_SEQ_BASE: u64 = 1 << 63;

/// A connection's registered LAST WILL: the record the broker appends on that
/// connection's behalf when it ends.
///
/// Persisted as a hidden [`WILL_SUBJECT`] record whose producer fields carry the
/// will's own `(producer_id, producer_seq)` — which is exactly why hidden subjects
/// stay OUT of the dedup map: a will record that entered it would dedup away the very
/// publish it exists to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WillRecord {
    /// The producer key the will publishes under (and is deduped by).
    pub producer_id: u64,
    /// The sequence the will publishes under, and the value the FENCE compares
    /// against: a later record from the same producer suppresses it.
    pub producer_seq: u64,
    /// The subject the will publishes to.
    pub subject: String,
    /// The body the will publishes.
    pub body: Vec<u8>,
}

const fn encode_u32_le_bytes(v: u32) -> [u8; 4] {
    [v as u8, (v >> 8) as u8, (v >> 16) as u8, (v >> 24) as u8]
}

#[cfg_attr(
    not(any(feature = "retention", feature = "anti-rollback", feature = "at-rest")),
    allow(dead_code)
)]
const fn encode_u64_le_bytes(v: u64) -> [u8; 8] {
    [
        v as u8,
        (v >> 8) as u8,
        (v >> 16) as u8,
        (v >> 24) as u8,
        (v >> 32) as u8,
        (v >> 40) as u8,
        (v >> 48) as u8,
        (v >> 56) as u8,
    ]
}

const fn decode_u32_le_bytes(b: &[u8; 4]) -> u32 {
    (b[0] as u32) | ((b[1] as u32) << 8) | ((b[2] as u32) << 16) | ((b[3] as u32) << 24)
}

#[cfg_attr(not(feature = "retention"), allow(dead_code))]
const fn decode_u64_le_bytes(b: &[u8; 8]) -> u64 {
    (b[0] as u64)
        | ((b[1] as u64) << 8)
        | ((b[2] as u64) << 16)
        | ((b[3] as u64) << 24)
        | ((b[4] as u64) << 32)
        | ((b[5] as u64) << 40)
        | ((b[6] as u64) << 48)
        | ((b[7] as u64) << 56)
}

const fn decode_will_subject_len(body: &[u8]) -> Option<(usize, &[u8])> {
    let &[b0, b1, b2, b3, ref rest @ ..] = body else {
        return None;
    };
    Some((decode_u32_le_bytes(&[b0, b1, b2, b3]) as usize, rest))
}

impl WillRecord {
    /// The stored body of a `/a/will` record: `u32 LE subject length ‖ subject ‖ body`.
    /// The producer key rides in the record's own producer fields.
    fn encode_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&encode_u32_le_bytes(self.subject.len() as u32));
        out.extend_from_slice(self.subject.as_bytes());
        out.extend_from_slice(&self.body);
        out
    }

    /// Decode a stored `/a/will` record; `None` on anything malformed (a will the
    /// broker cannot read is one it does not fire).
    fn from_record(rec: &BrokerRecord) -> Option<WillRecord> {
        let (len, rest) = decode_will_subject_len(&rec.body)?;
        let subject_bytes = rest.get(..len)?;
        let body_bytes = rest.get(len..)?;
        let Ok(subject_str) = core::str::from_utf8(subject_bytes) else {
            return None;
        };
        Some(WillRecord {
            producer_id: rec.producer_id,
            producer_seq: rec.producer_seq,
            subject: subject_str.to_owned(),
            body: body_bytes.to_vec(),
        })
    }
}

/// Cap on how many DISTINCT subjects one producer may create, counted in the
/// last-value index. Distinct subjects grow with history (one presence row per
/// session ever spawned, one ack per member per barrier), and each costs a permanent
/// index entry; a publish that would exceed this is refused. Per PRODUCER because a
/// guarded broker binds a producer id to a capability's principal.
///
/// Enforced on EVERY append path — the staged (group-commit) one the broker's writer
/// uses, and the un-batched [`BrokerLog::publish`] /
/// [`BrokerLog::process_and_produce`] an embedder of the re-exported type calls —
/// from the one predicate, so the two cannot disagree.
pub const MAX_SUBJECTS_PER_PRODUCER: usize = 4096;

/// The longest literal prefix every subject a filter can match must start with: the
/// filter's leading literal segments, terminated by `/`, up to (not including) its
/// first `*` or `>`. `/f/F/pub/*/*/presence` -> `/f/F/pub/`; `/x/>` -> `/x/`; `/>` ->
/// `/`. A wildcard-free filter yields the filter itself — a longer subject that
/// starts with it is still rejected by `Filter::matches`, so the prefix is only ever
/// a range BOUND, never the match.
fn literal_prefix(filter: &Filter) -> &str {
    let s = filter.as_str();
    let Some(tail) = s.get(1..) else {
        return s;
    };
    let mut pos = 1usize;
    for seg in tail.split('/') {
        if seg == "*" || seg == ">" {
            return s.get(..pos).unwrap_or(s);
        }
        pos = pos.saturating_add(seg.len()).saturating_add(1);
    }
    s
}

/// Apply a durable [`RETIRE_SUBJECT`] record's target (`target` is either an exact
/// [`Subject`] or a wildcard [`Filter`] such as `"/f/local/pub/n-1/s1/>"`) to `last`,
/// `carried_last`, and `subjects_per_producer`.
fn apply_retire_target(
    last: &mut BTreeMap<String, Offset>,
    carried_last: &mut BTreeMap<String, Arc<BrokerRecord>>,
    subjects_per_producer: &mut HashMap<u64, usize>,
    producer_id: u64,
    target: &str,
) {
    let mut removed = 0usize;
    if Subject::new(target).is_ok() {
        carried_last.remove(target);
        if last.remove(target).is_some() {
            removed = 1;
        }
    } else if let Ok(filt) = Filter::new(target) {
        let prefix = literal_prefix(&filt);
        let mut matching = Vec::new();
        for (subj, _) in last.range::<str, _>((Bound::Included(prefix), Bound::Unbounded)) {
            if !subj.starts_with(prefix) {
                break;
            }
            if filt.matches_str(subj) {
                matching.push(subj.clone());
            }
        }
        for subj in matching {
            carried_last.remove(&subj);
            if last.remove(&subj).is_some() {
                removed = removed.saturating_add(1);
            }
        }
    }
    if removed > 0 {
        if let Some(c) = subjects_per_producer.get_mut(&producer_id) {
            *c = c.saturating_sub(removed);
            if *c == 0 {
                subjects_per_producer.remove(&producer_id);
            }
        }
    }
}

/// Operational snapshot of a [`BrokerLog`]'s retained state and indexes (`REQUIREMENTS` R5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogStats {
    /// Lowest retained offset (`0` if never compacted).
    pub base: Offset,
    /// Next offset to be assigned (exclusive durable head).
    pub head: Offset,
    /// Number of retained records in memory and on disk (`head - base`).
    pub retained_records: usize,
    /// Total byte length of the retained on-disk log frames.
    pub retained_bytes: usize,
    /// Number of distinct visible subjects in the last-value index (`Last`).
    pub distinct_subjects: usize,
    /// Number of durably bound producer IDs (`/a/bind`).
    pub bound_producers: usize,
    /// Number of active pending last wills (`/a/will` that have neither fired nor been fenced).
    pub pending_wills: usize,
    /// Number of consumer groups with a durable committed offset.
    pub group_commits: usize,
    /// Durability tier this log was opened at.
    pub durability: Durability,
    /// True when this log is a replication follower target (`<log>.replica`).
    pub is_replica: bool,
    /// True when an unrecoverable I/O error has poisoned the log until reopen.
    pub poisoned: bool,
}

/// A durable, single-writer message log with exactly-once ingest and durable
/// consumer-group commit offsets.
pub struct BrokerLog {
    /// The log file, through the storage seam (`storage.rs`): a real file, or under
    /// `cfg(test)` the crash simulation's device.
    file: Backing,
    /// A batch is being written and flushed OUTSIDE the log's lock: set by
    /// [`commit_begin`](Self::commit_begin), cleared by [`commit_end`](Self::commit_end).
    /// While it is set, every other mutation is refused (`check_poisoned`): the staged
    /// batch is on its way to the file through the file's own descriptor
    /// ([`BatchOut`](crate::storage::BatchOut)), and nothing may stage into it, append
    /// beside it, rewrite the file under it or close the log over it.
    writing: bool,
    /// Where the identity sidecar is read and replaced, through the same seam.
    dir: Dir,
    /// Where the newest stored frame starts (`None` with no record): what a clean
    /// close fingerprints, without reading the whole log back.
    newest_at: Option<u64>,
    /// Set by [`close_clean`](Self::close_clean): the sidecar says this log stopped
    /// cleanly at its head, so nothing more may be appended under that claim.
    closed: bool,
    /// Byte length of the DURABLE prefix on disk (recovered prefix + everything a
    /// successful batch write/fsync has committed). Staged bytes live in `staged_bytes`
    /// until `commit_batch` writes them; a failed write or fsync truncates the file back
    /// to `durable_len`. Raw framed bytes are NOT duplicated in RAM alongside `records`.
    durable_len: u64,
    /// Cached 16-byte authentication tag of the first retained record's frame (`[0; 16]`
    /// when the log is empty), used by `anti-rollback` watermark updates without holding
    /// the raw on-disk byte stream in RAM.
    #[cfg_attr(not(feature = "anti-rollback"), allow(dead_code))]
    log_id: [u8; 16],
    /// Committed records, each behind an `Arc` so the egress catch-up (`read_from`)
    /// hands subscribers cheap shared handles instead of deep-copying subject+body.
    records: Vec<Arc<BrokerRecord>>,
    /// The lowest offset still retained (records below it were pruned by retention).
    /// `0` for a never-retained log; recovered from the `<log>.base` sidecar. Kept
    /// records keep their ABSOLUTE offsets — record `i` in `records` has seq
    /// `base + i` — so a consumer's offsets stay meaningful across retention.
    base: Offset,
    next: Offset,
    dedup: HashMap<(u64, u64), Offset>,
    /// LAST-VALUE index: subject -> the offset of that subject's most recent durable
    /// record. Ordered, so a query seeks to a filter's literal prefix in `O(log n)` and
    /// then scans forward under a bound the caller supplies, rather than walking the
    /// whole log — or, for a filter whose matches are sparse inside that prefix, the
    /// whole index. Hidden subjects are excluded, so a retained-state query can never
    /// surface one.
    last: BTreeMap<String, Offset>,
    /// How many distinct subjects each producer has created in `last` (the bound
    /// [`MAX_SUBJECTS_PER_PRODUCER`] is checked against this).
    subjects_per_producer: HashMap<u64, usize>,
    /// Subjects RESERVED by registered wills that have not landed yet, per producer:
    /// a will's TARGET subject, held from the moment its `/a/will` record commits
    /// until a record on that subject lands under the same producer. Counted against
    /// [`MAX_SUBJECTS_PER_PRODUCER`] beside `subjects_per_producer`, because a will
    /// FIRING is exempt from the bound. Without the reservation the check at
    /// registration reads the same count for every live connection, so N connections
    /// registering wills for N DISTINCT new subjects all pass it, and the firings then
    /// take the producer one subject past the cap per connection.
    ///
    /// Rebuilt on open from the log's own `/a/will` records (see
    /// [`pending_wills`](Self::pending_wills)), so a reservation survives a restart.
    ///
    /// DELIBERATELY CONSERVATIVE, always in the direction that keeps the cap. A
    /// reservation is released when a record on that subject lands under that
    /// producer, and NOT when the will is superseded by a replacement for a different
    /// subject, fenced by a later record, or beaten to the subject by another
    /// producer; and while a publish to a reserved subject is STAGED it is counted
    /// both as the batch's pending new subject and as the reservation, until the batch
    /// commits. Each of those holds one extra unit of one producer's budget — never
    /// more than the budget itself, since a registration that would exceed it is
    /// refused — and the next open rebuilds the set from the wills that would actually
    /// fire, at most one per producer. The cost: a producer that registers wills on
    /// many connections for many subjects reaches its cap sooner than its landed
    /// subjects alone would say. The alternative, releasing a reservation a live
    /// connection's will could still use, would let the firings overshoot the cap.
    will_reserved: HashMap<u64, HashSet<String>>,
    /// Per-producer HIGH WATER: the largest `producer_seq` that producer has landed,
    /// over the visible (non-hidden) records. A will FIRES only if nothing above its
    /// own sequence has since landed from the same producer — so a reconnected
    /// producer that publishes at a higher sequence suppresses its previous
    /// incarnation's will structurally, with no reader-side fold.
    high_water: HashMap<u64, u64>,
    /// Pending wills carried across compaction by [`Self::retain_before_carrying_state`]
    /// in the `<log>.state` sidecar when their `/a/will` records below `base` were
    /// pruned. A later `/a/will` record in `records` for the same producer supersedes
    /// the carried entry, and a later publish or firing fences/dedups it.
    carried_wills: HashMap<u64, (Offset, WillRecord)>,
    /// Latest records of quiet subjects whose newest record was below `base` when
    /// [`Self::retain_before_carrying_state`] compacted the log, carried in `<log>.state`
    /// so `Last` queries still answer them across compaction and reopen (`REQUIREMENTS` R4).
    carried_last: BTreeMap<String, Arc<BrokerRecord>>,
    /// The path this log was opened from: the log's own name, the stem the replica
    /// marker beside it is derived from, and what the retention compaction rewrites
    /// (with its base sidecar).
    path: PathBuf,
    /// THIS LOG IS A REPLICA: it took a leader's record while it was still empty, or an
    /// operator declared it one. Durable, in the `<log>.replica` marker beside the
    /// file, because the property must survive the restart it exists to make safe. A
    /// replica's log is the leader's spine and NOTHING may be appended to it on this
    /// broker's own initiative — in particular no will may FIRE here (see
    /// [`stage_will_fire`](Self::stage_will_fire)); its wills fire on the leader and
    /// arrive by replication like every other record.
    replica: bool,
    /// WHICH log this is, and which incarnation of its offsets ([`LogIdentity`]),
    /// with what the open that settled it saw. Durable in the `<log>.id` sidecar,
    /// settled on every open before it returns; on a replica, its leader's.
    ///
    /// [`LogIdentity`]: crate::LogIdentity
    identity: crate::logid::Sidecar,
    /// Set once THIS open refused a replicated record because the log already holds
    /// a DIFFERENT record at its offset: the log is not its leader's spine there, so
    /// it does not take its leader's identity again before a reopen settles it anew
    /// ([`adopt_identity`](Self::adopt_identity)).
    diverged: bool,
    /// The producer-id BINDING table: `producer id -> (principal, the offset of the
    /// /a/bind record that bound it)`. Rebuilt on open from those records, so a
    /// binding survives a broker restart. It is belt-and-braces on top of the 64-bit
    /// second-preimage cost of `producer_id_of`: two DIFFERENT principals that derive
    /// one id cannot both attach.
    bind: HashMap<u64, (String, Offset)>,
    /// Per-group last fully-processed offset (resume is `upto + 1`). Rebuilt from
    /// the records' `commit` annotations on open, so it survives a broker restart.
    group_commits: HashMap<String, u64>,
    /// Records staged into the in-flight batch, with their encoded frames
    /// (`staged_bytes`) and an in-batch dedup view. They reach the file and become
    /// durable (promoted into `records`/`dedup`/`group_commits`/`mirror`, advancing
    /// `next`) only when [`commit_batch`](Self::commit_batch) writes and fsyncs them —
    /// this is group commit: one write and one fsync amortized over a whole batch,
    /// with the SAME Strict durability contract (ack ⟹ fsync'd).
    staged: Vec<BrokerRecord>,
    staged_bytes: Vec<u8>,
    /// Where the last staged frame starts in `staged_bytes`: the record, staged with the
    /// continues mark like every other, that [`commit_batch`](Self::commit_batch)
    /// re-encodes with the batch seal.
    staged_last_at: usize,
    /// The crash simulation's negative control: the writer before the batch marks,
    /// which marks nothing.
    #[cfg(test)]
    pub(crate) unsealed_writer: bool,
    staged_dedup: HashMap<(u64, u64), Offset>,
    /// Subjects the in-flight batch would ADD to `last`, per producer — so the
    /// distinct-subject bound counts a batch's own new subjects too and one batch
    /// cannot overshoot it.
    staged_new_subjects: HashMap<u64, HashSet<String>>,
    /// Bindings the in-flight batch would add, so two `Attach`es inside one batch
    /// see each other's binding instead of both appending.
    staged_bind: HashMap<u64, (String, Offset)>,
    /// What an `ack` promises (see [`Durability`]): the tier ASKED for. Strict fsyncs
    /// each batch; Ordered issues an ordering barrier; Relaxed skips the fsync (ack on
    /// page-cache write).
    durability: Durability,
    /// The tier SERVED, and why it differs from `durability`: what decides each batch's
    /// flush ([`flush_owed`](Self::flush_owed)). Decided by the open; lowered to Strict
    /// when a barrier is refused or a full flush fails while it serves, never raised.
    served: Served,
    /// The escalation table, parsed ([`OpenOptions::escalation`]).
    rules: crate::escalation::Rules,
    /// The DURABLE HEAD: every record below it is on stable media by a full flush this
    /// process saw succeed — the open's own, every Strict batch's, an escalation's, the
    /// horizon's, `flush_durable`'s, `sync_full`'s, the clean close's. A barrier never
    /// moves it (an ordered record is not yet a durable one).
    durable_head: u64,
    /// The first PROMOTED record a full flush still owes before any reader may see it —
    /// an escalated record, or one behind it, of a batch whose full flush has not
    /// succeeded yet. The visible head is held here: `held_from.unwrap_or(next)`. If
    /// that flush fails it stays held for the rest of the open, which the failure
    /// poisons ([`flush_failed_lossy`](Self::flush_failed_lossy)).
    held_from: Option<u64>,
    /// The group commits (`group`, `upto`) of promoted records at or above the held head,
    /// in log order: they reach `group_commits` — and so steer
    /// [`group_start`](Self::group_start) — only when the flush that releases the head
    /// succeeds. A commit the `Group` rule escalated (a person's inbox read mark) is
    /// neither visible nor durable while held, and a second reader of the group must not
    /// resume past mail on its strength: the commit may yet be refused "outcome unknown",
    /// or lost to a power loss inside the window.
    held_commits: Vec<(String, u64)>,
    /// The first escalated record the in-flight batch staged.
    staged_escalated: Option<u64>,
    /// Original offsets of resent escalated ops the in-flight batch answered from
    /// dedup, whose originals no full flush covers yet ([`Committed::held`]).
    staged_held: Vec<u64>,
    /// [`FlushCounts`].
    counts: FlushCounts,
    /// A test's stand-in for a device that refuses the barrier while serving (the latch).
    #[cfg(test)]
    pub(crate) refuse_barrier: bool,
    /// `Some(reason)` once the file or its sidecars no longer provably match this
    /// state: a failed batch could not be rolled back (the truncate itself failed), a
    /// retention rewrite failed half-way, or the anti-rollback watermark could not be
    /// advanced over promoted records. Every further mutation is refused until the log
    /// is reopened (the recovery scan re-establishes the prefix). Without this, after
    /// a failed rollback the next batch would append records with the SAME seqs after
    /// the orphaned suffix, and recovery would resurrect the rolled-back records and
    /// drop the acked ones.
    poisoned: Option<String>,
    /// The documented fault-injection seam ([`inject_faults`](Self::inject_faults)).
    faults: InjectedFaults,
    /// Encrypt-at-rest key. `Some` seals every record's payload on disk under
    /// XChaCha20-Poly1305 (bound to its log index as AAD); `None` writes plaintext
    /// frames. Set only by the `open_encrypted*` constructors. The key is never
    /// written to the log and is zeroized on drop.
    at_rest_key: Option<[u8; 32]>,
    /// Anti-rollback: when `Some(path)`, an authenticated monotonic head watermark is
    /// maintained in the sidecar at `path` after every durable commit, and checked on
    /// open — so a write-capable attacker cannot silently truncate the log to an
    /// earlier state (per-record sealing alone leaves each surviving record valid).
    /// Requires an at-rest key.
    #[cfg_attr(not(feature = "anti-rollback"), allow(dead_code))]
    head_watermark: Option<PathBuf>,
    /// Monotone leader replication epoch (`Astream.Broker.TlaEpochWatermarkReplication`).
    /// Any replication request from a stale epoch (`req_epoch < replication_epoch`) is
    /// fenced (`crate::proto::epoch_is_fenced`).
    replication_epoch: u64,
    /// Monotone follower/quorum-acknowledged offset (`Astream.Broker.TlaEpochWatermarkReplication`).
    /// The committed watermark is `committed_watermark(head, replica_ack_upto)` = `min(head, replica_ack_upto)`.
    replica_ack_upto: Offset,
}

impl Drop for BrokerLog {
    fn drop(&mut self) {
        // Unlock explicitly before close(): `close(2)` only releases a `flock(2)` when
        // the kernel open-file-description refcount reaches zero, so a concurrent
        // `fork`/`posix_spawn` in another thread briefly holds `fg_count > 1` until its
        // `execve` closes `FD_CLOEXEC` descriptors. `flock(LOCK_UN)` releases the lock on
        // the open file description immediately regardless of `fg_count`.
        self.file.unlock();
        if let Some(ref mut key) = self.at_rest_key {
            key.fill(0);
            std::hint::black_box(key);
        }
    }
}

/// How `open_inner` treats the anti-rollback head watermark.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AntiRollbackMode {
    /// No watermark maintained or checked (the default, and every non-encrypted path).
    Off,
    /// Require + verify the watermark: refuse if the log head is below it (rollback)
    /// or the sidecar is missing on a non-empty log (deleted).
    #[cfg(feature = "anti-rollback")]
    Verify,
    /// Establish/bootstrap the watermark at the current head (first adoption) without
    /// requiring a prior sidecar.
    #[cfg(feature = "anti-rollback")]
    Init,
}

/// AAD binding the head-watermark sidecar to its purpose (domain separation from the
/// record-sealing AAD, which is a bare u64 index).
#[cfg(feature = "anti-rollback")]
const HW_AAD: &[u8] = b"astream-anti-rollback-v1";

/// The sidecar path for a log at `path`.
#[cfg(feature = "anti-rollback")]
fn watermark_path(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.as_mut_os_string().push(".hw");
    p
}

/// The retention base-offset sidecar path for a log at `path`.
#[cfg(feature = "retention")]
fn base_path(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.as_mut_os_string().push(".base");
    p
}

/// The state-carrying compaction sidecar path (`<log>.state`) for a log at `path`.
fn state_path(path: &Path) -> PathBuf {
    let mut p = path.to_path_buf();
    p.as_mut_os_string().push(".state");
    p
}

/// Domain-separating AAD for sealing `<log>.state` under the log's at-rest key.
#[cfg(all(feature = "retention", feature = "at-rest"))]
const STATE_AAD: &[u8] = b"astream-compacted-state-v1";

#[cfg(feature = "retention")]
const STATE_MAGIC: &[u8; 5] = b"ASST\x01";

/// Durable control state carried across log compaction (`<log>.state`).
#[derive(Default)]
struct CompactedState {
    bind: HashMap<u64, (String, Offset)>,
    carried_wills: HashMap<u64, (Offset, WillRecord)>,
    high_water: HashMap<u64, u64>,
    group_commits: HashMap<String, u64>,
    carried_last: BTreeMap<String, Arc<BrokerRecord>>,
}

#[cfg(feature = "retention")]
const fn decode_state_header_bytes(hdr: &[u8; 13], expected_base: u64) -> Option<u64> {
    let state_base = (hdr[5] as u64)
        | ((hdr[6] as u64) << 8)
        | ((hdr[7] as u64) << 16)
        | ((hdr[8] as u64) << 24)
        | ((hdr[9] as u64) << 32)
        | ((hdr[10] as u64) << 40)
        | ((hdr[11] as u64) << 48)
        | ((hdr[12] as u64) << 56);
    if hdr[0] != b'A'
        || hdr[1] != b'S'
        || hdr[2] != b'S'
        || hdr[3] != b'T'
        || hdr[4] != 0x01
        || state_base > expected_base
    {
        return None;
    }
    Some(state_base)
}

#[cfg(feature = "retention")]
const fn decode_state_header(bytes: &[u8], expected_base: u64) -> Option<(u64, &[u8])> {
    let &[m0, m1, m2, m3, m4, b0, b1, b2, b3, b4, b5, b6, b7, ref rest @ ..] = bytes else {
        return None;
    };
    let Some(state_base) = decode_state_header_bytes(
        &[m0, m1, m2, m3, m4, b0, b1, b2, b3, b4, b5, b6, b7],
        expected_base,
    ) else {
        return None;
    };
    Some((state_base, rest))
}

#[cfg(feature = "retention")]
fn state_read_u32(b: &[u8], p: &mut usize) -> Option<usize> {
    let Some(end) = p.checked_add(4) else {
        return None;
    };
    let Some(slice) = b.get(*p..end) else {
        return None;
    };
    let Some((chunk, _)) = slice.split_first_chunk::<4>() else {
        return None;
    };
    *p = end;
    Some(decode_u32_le_bytes(chunk) as usize)
}

#[cfg(feature = "retention")]
fn state_read_u64(b: &[u8], p: &mut usize) -> Option<u64> {
    let Some(end) = p.checked_add(8) else {
        return None;
    };
    let Some(slice) = b.get(*p..end) else {
        return None;
    };
    let Some((chunk, _)) = slice.split_first_chunk::<8>() else {
        return None;
    };
    *p = end;
    Some(decode_u64_le_bytes(chunk))
}

#[cfg(feature = "retention")]
fn state_read_str(b: &[u8], p: &mut usize) -> Option<String> {
    let len = state_read_u32(b, p)?;
    let end = p.checked_add(len)?;
    let raw = b.get(*p..end)?;
    let Ok(s) = core::str::from_utf8(raw) else {
        return None;
    };
    *p = end;
    Some(s.to_owned())
}

#[cfg(feature = "retention")]
impl CompactedState {
    fn encode(&self, base: u64) -> Result<Vec<u8>, FrameError> {
        let mut out = Vec::new();
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&encode_u64_le_bytes(base));

        let Ok(bind_len) = u32::try_from(self.bind.len()) else {
            return Err(FrameError::TooLarge);
        };
        out.extend_from_slice(&encode_u32_le_bytes(bind_len));
        for (&pid, (principal, off)) in &self.bind {
            let Ok(plen) = u32::try_from(principal.len()) else {
                return Err(FrameError::TooLarge);
            };
            out.extend_from_slice(&encode_u64_le_bytes(pid));
            out.extend_from_slice(&encode_u64_le_bytes(off.0));
            out.extend_from_slice(&encode_u32_le_bytes(plen));
            out.extend_from_slice(principal.as_bytes());
        }

        let Ok(will_len) = u32::try_from(self.carried_wills.len()) else {
            return Err(FrameError::TooLarge);
        };
        out.extend_from_slice(&encode_u32_le_bytes(will_len));
        for (off, w) in self.carried_wills.values() {
            let Ok(slen) = u32::try_from(w.subject.len()) else {
                return Err(FrameError::TooLarge);
            };
            let Ok(blen) = u32::try_from(w.body.len()) else {
                return Err(FrameError::TooLarge);
            };
            out.extend_from_slice(&encode_u64_le_bytes(off.0));
            out.extend_from_slice(&encode_u64_le_bytes(w.producer_id));
            out.extend_from_slice(&encode_u64_le_bytes(w.producer_seq));
            out.extend_from_slice(&encode_u32_le_bytes(slen));
            out.extend_from_slice(w.subject.as_bytes());
            out.extend_from_slice(&encode_u32_le_bytes(blen));
            out.extend_from_slice(&w.body);
        }

        let Ok(hw_len) = u32::try_from(self.high_water.len()) else {
            return Err(FrameError::TooLarge);
        };
        out.extend_from_slice(&encode_u32_le_bytes(hw_len));
        for (&pid, &hw) in &self.high_water {
            out.extend_from_slice(&encode_u64_le_bytes(pid));
            out.extend_from_slice(&encode_u64_le_bytes(hw));
        }

        let Ok(gc_len) = u32::try_from(self.group_commits.len()) else {
            return Err(FrameError::TooLarge);
        };
        out.extend_from_slice(&encode_u32_le_bytes(gc_len));
        for (group, &upto) in &self.group_commits {
            let Ok(glen) = u32::try_from(group.len()) else {
                return Err(FrameError::TooLarge);
            };
            out.extend_from_slice(&encode_u64_le_bytes(upto));
            out.extend_from_slice(&encode_u32_le_bytes(glen));
            out.extend_from_slice(group.as_bytes());
        }

        let Ok(cl_len) = u32::try_from(self.carried_last.len()) else {
            return Err(FrameError::TooLarge);
        };
        out.extend_from_slice(&encode_u32_le_bytes(cl_len));
        for rec in self.carried_last.values() {
            let rbytes = rec.encode_payload()?;
            let Ok(rlen) = u32::try_from(rbytes.len()) else {
                return Err(FrameError::TooLarge);
            };
            out.extend_from_slice(&encode_u32_le_bytes(rlen));
            out.extend_from_slice(&rbytes);
        }
        Ok(out)
    }

    fn decode(bytes: &[u8], expected_base: u64) -> Option<CompactedState> {
        let (_state_base, _) = decode_state_header(bytes, expected_base)?;
        let mut pos = 13usize;

        let bind_count = state_read_u32(bytes, &mut pos)?;
        let mut bind = HashMap::new();
        for _ in 0..bind_count {
            let pid = state_read_u64(bytes, &mut pos)?;
            let off = Offset(state_read_u64(bytes, &mut pos)?);
            let principal = state_read_str(bytes, &mut pos)?;
            bind.insert(pid, (principal, off));
        }

        let will_count = state_read_u32(bytes, &mut pos)?;
        let mut carried_wills = HashMap::new();
        for _ in 0..will_count {
            let off = Offset(state_read_u64(bytes, &mut pos)?);
            let producer_id = state_read_u64(bytes, &mut pos)?;
            let producer_seq = state_read_u64(bytes, &mut pos)?;
            let subject = state_read_str(bytes, &mut pos)?;
            let blen = state_read_u32(bytes, &mut pos)?;
            let bend = pos.checked_add(blen)?;
            let body = bytes.get(pos..bend)?.to_vec();
            pos = bend;
            carried_wills.insert(
                producer_id,
                (
                    off,
                    WillRecord {
                        producer_id,
                        producer_seq,
                        subject,
                        body,
                    },
                ),
            );
        }

        let hw_count = state_read_u32(bytes, &mut pos)?;
        let mut high_water = HashMap::new();
        for _ in 0..hw_count {
            let pid = state_read_u64(bytes, &mut pos)?;
            let hw = state_read_u64(bytes, &mut pos)?;
            high_water.insert(pid, hw);
        }

        let gc_count = state_read_u32(bytes, &mut pos)?;
        let mut group_commits = HashMap::new();
        for _ in 0..gc_count {
            let upto = state_read_u64(bytes, &mut pos)?;
            let group = state_read_str(bytes, &mut pos)?;
            group_commits.insert(group, upto);
        }

        let mut carried_last = BTreeMap::new();
        if pos < bytes.len() {
            let cl_count = state_read_u32(bytes, &mut pos)?;
            for _ in 0..cl_count {
                let rlen = state_read_u32(bytes, &mut pos)?;
                let rend = pos.checked_add(rlen)?;
                let rbytes = bytes.get(pos..rend)?;
                pos = rend;
                let rec = BrokerRecord::from_payload(rbytes)?;
                if rec.seq.0 >= expected_base || is_hidden_subject(&rec.subject) {
                    return None;
                }
                carried_last.insert(rec.subject.clone(), Arc::new(rec));
            }
        }

        if pos != bytes.len() {
            return None;
        }
        Some(CompactedState {
            bind,
            carried_wills,
            high_water,
            group_commits,
            carried_last,
        })
    }
}

#[cfg(all(feature = "retention", feature = "at-rest"))]
fn seal_state_payload(key: Option<&[u8; 32]>, payload: Vec<u8>) -> Vec<u8> {
    match key {
        Some(k) => astream_aead::seal(k, STATE_AAD, &payload),
        None => payload,
    }
}
#[cfg(all(feature = "retention", not(feature = "at-rest")))]
fn seal_state_payload(_key: Option<&[u8; 32]>, payload: Vec<u8>) -> Vec<u8> {
    payload
}

#[cfg(all(feature = "retention", feature = "at-rest"))]
fn open_state_payload(key: Option<&[u8; 32]>, payload: Vec<u8>) -> Option<Vec<u8>> {
    match key {
        Some(k) => {
            let Ok(v) = astream_aead::open(k, STATE_AAD, &payload) else {
                return None;
            };
            Some(v)
        }
        None => Some(payload),
    }
}
#[cfg(all(feature = "retention", not(feature = "at-rest")))]
fn open_state_payload(_key: Option<&[u8; 32]>, payload: Vec<u8>) -> Option<Vec<u8>> {
    Some(payload)
}

/// Remove a file at `path` (shared `&Path` boundary).
pub(crate) fn remove_file_path(path: &Path) -> io::Result<()> {
    std::fs::remove_file(path)
}

/// Rename `from` to `to` (shared `&Path` boundary).
pub(crate) fn rename_path(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Open `path` with `opts` (shared `&Path` boundary).
pub(crate) fn open_with_options(opts: &FsOpenOptions, path: &Path) -> io::Result<File> {
    opts.open(path)
}

#[cfg(feature = "retention")]
fn write_state(
    path: &Path,
    key: Option<&[u8; 32]>,
    base: u64,
    state: &CompactedState,
) -> io::Result<()> {
    let Ok(raw) = state.encode(base) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retention: state too large",
        ));
    };
    let payload = seal_state_payload(key, raw);
    let Ok(framed) = Frame::new(payload).encode() else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retention: state too large",
        ));
    };
    let sp = state_path(path);
    let mut tmp = sp.clone();
    tmp.as_mut_os_string().push(".tmp");
    {
        let mut f = create_fresh(&tmp, &mut FsOpenOptions::new())?;
        f.write_all(&framed)?;
        f.sync_all()?;
    }
    rename_path(&tmp, &sp)?;
    sync_parent_dir(&sp)?;
    Ok(())
}

/// Open only an observed regular state sidecar, without following its final symlink.
/// Absence is returned only by the initial entry lookup; later open/read failures refuse.
#[cfg(feature = "retention")]
fn open_state_file(path: &Path) -> io::Result<Option<File>> {
    let entry = match std::fs::symlink_metadata(path) {
        Ok(entry) => entry,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !entry.is_file() || entry.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "retention: state sidecar is not a bounded regular file",
        ));
    }
    #[cfg(not(any(
        target_os = "macos",
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "retention: safe state-file admission is unavailable on this platform",
    ));
    #[cfg(any(
        target_os = "macos",
        windows,
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    ))]
    {
        let mut opts = FsOpenOptions::new();
        opts.read(true);
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(0x4 | 0x100); // Darwin O_NONBLOCK | O_NOFOLLOW.
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(0x800 | 0x20000); // Linux x86_64 O_NONBLOCK | O_NOFOLLOW.
        }
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(0x800 | 0x8000); // Linux AArch64 O_NONBLOCK | O_NOFOLLOW.
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            opts.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT.
        }
        let file = open_with_options(&opts, path)?;
        let meta = file.metadata()?;
        let cap =
            (astream_wire::HEADER_SIZE as u64).saturating_add(astream_wire::MAX_PAYLOAD_LEN as u64);
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > cap {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retention: state sidecar is not a bounded regular file",
            ));
        }
        Ok(Some(file))
    }
}

fn read_state(
    path: &Path,
    key: Option<&[u8; 32]>,
    expected_base: u64,
) -> io::Result<CompactedState> {
    #[cfg(feature = "retention")]
    {
        use std::io::Read;
        let sp = state_path(path);
        let Some(mut file) = open_state_file(&sp)? else {
            return Ok(CompactedState::default());
        };
        // The shared frame reader checks the header before buffering at most its payload cap.
        // Exactly one extra byte checks EOF, even if this file grew after the metadata check.
        let Some(payload) = crate::proto::read_frame(&mut file)? else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retention: empty state sidecar",
            ));
        };
        let mut extra = [0u8; 1];
        if file.read(&mut extra)? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retention: trailing state sidecar bytes",
            ));
        }
        let Some(pt) = open_state_payload(key, payload) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retention: state sidecar failed to authenticate (wrong key or tampered)",
            ));
        };
        let Some(decoded) = CompactedState::decode(&pt, expected_base) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "retention: malformed or mismatched state sidecar",
            ));
        };
        Ok(decoded)
    }
    #[cfg(not(feature = "retention"))]
    {
        let _ = (path, key, expected_base);
        Ok(CompactedState::default())
    }
}

#[cfg(feature = "retention")]
const fn decode_base_bytes(b: &[u8]) -> Option<Offset> {
    let &[b0, b1, b2, b3, b4, b5, b6, b7, ref rest @ ..] = b else {
        return None;
    };
    if !rest.is_empty() {
        return None;
    }
    Some(Offset(decode_u64_le_bytes(&[
        b0, b1, b2, b3, b4, b5, b6, b7,
    ])))
}

/// Read the persisted retention base offset (`0` / `Offset::ZERO` if none). A base
/// that does not match the recovered records is caught by scan's `seq == base + i`
/// check (a mismatch is a Corrupt tail), so a tampered base cannot silently reindex.
fn read_base(path: &Path) -> io::Result<Offset> {
    #[cfg(feature = "retention")]
    {
        match std::fs::read(base_path(path)) {
            Ok(b) => match decode_base_bytes(&b) {
                Some(off) => Ok(off),
                None => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "retention: malformed base offset",
                )),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Offset::ZERO),
            Err(e) => Err(e),
        }
    }
    #[cfg(not(feature = "retention"))]
    {
        let _ = path;
        Ok(Offset::ZERO)
    }
}

/// Create `path` as a NEW file, first removing whatever entry holds the name. The
/// log's sidecars are written at fixed names beside it, and `create_new` (O_EXCL)
/// never follows a symlink: a link planted at one of those names by someone who can
/// write the log directory cannot redirect the write onto another file.
pub(crate) fn create_fresh(path: &Path, opts: &mut FsOpenOptions) -> io::Result<File> {
    match remove_file_path(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    open_with_options(opts.write(true).create_new(true), path)
}

/// Durably persist the retention base offset via tmp-file rename.
#[cfg(feature = "retention")]
fn write_base(path: &Path, base: u64) -> io::Result<()> {
    let bp = base_path(path);
    let mut tmp = bp.clone();
    tmp.as_mut_os_string().push(".tmp");
    {
        let mut f = create_fresh(&tmp, &mut FsOpenOptions::new())?;
        f.write_all(&encode_u64_le_bytes(base))?;
        f.sync_all()?;
    }
    rename_path(&tmp, &bp)?;
    sync_parent_dir(&bp)?;
    Ok(())
}

/// A per-log identity derived from the log's own sealed content: the authentication
/// tag of the FIRST record's sealed frame. It is unique per log (the first record's
/// nonce is random), survives truncation (record 0 always remains), and cannot be
/// produced for a given log without the key. Binding it into the watermark stops a
/// key-less attacker from splicing a DIFFERENT same-key log's watermark onto this one
/// (a watermark from log B carries log B's identity). `[0; 16]` for an empty log.
#[cfg_attr(not(feature = "anti-rollback"), allow(dead_code))]
fn log_identity(durable_frames: &[u8]) -> [u8; 16] {
    if let Ok(Some(d)) = Frame::decode(durable_frames) {
        if let Some((_, tail)) = d.frame.payload.split_last_chunk::<16>() {
            return *tail;
        }
    }
    [0u8; 16]
}

#[cfg(feature = "anti-rollback")]
const fn decode_watermark_bytes(raw: &[u8; 32]) -> ([u8; 16], u64, u64) {
    let id = [
        raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7], raw[8], raw[9], raw[10],
        raw[11], raw[12], raw[13], raw[14], raw[15],
    ];
    let base = (raw[16] as u64)
        | ((raw[17] as u64) << 8)
        | ((raw[18] as u64) << 16)
        | ((raw[19] as u64) << 24)
        | ((raw[20] as u64) << 32)
        | ((raw[21] as u64) << 40)
        | ((raw[22] as u64) << 48)
        | ((raw[23] as u64) << 56);
    let head = (raw[24] as u64)
        | ((raw[25] as u64) << 8)
        | ((raw[26] as u64) << 16)
        | ((raw[27] as u64) << 24)
        | ((raw[28] as u64) << 32)
        | ((raw[29] as u64) << 40)
        | ((raw[30] as u64) << 48)
        | ((raw[31] as u64) << 56);
    (id, base, head)
}

#[cfg(feature = "anti-rollback")]
const fn decode_watermark_plaintext(pt: &[u8]) -> Option<([u8; 16], u64, u64)> {
    let &[i0, i1, i2, i3, i4, i5, i6, i7, i8, i9, i10, i11, i12, i13, i14, i15, b0, b1, b2, b3, b4, b5, b6, b7, h0, h1, h2, h3, h4, h5, h6, h7, ref rest @ ..] =
        pt
    else {
        return None;
    };
    if !rest.is_empty() {
        return None;
    }
    Some(decode_watermark_bytes(&[
        i0, i1, i2, i3, i4, i5, i6, i7, i8, i9, i10, i11, i12, i13, i14, i15, b0, b1, b2, b3, b4,
        b5, b6, b7, h0, h1, h2, h3, h4, h5, h6, h7,
    ]))
}

/// Read + authenticate the persisted watermark, returning `(log_identity, base, head)`.
/// `Ok(None)` if the sidecar does not exist; `Err` if it exists but fails to
/// authenticate (tampered or wrong key).
#[cfg(feature = "anti-rollback")]
fn read_watermark(path: &Path, key: &[u8; 32]) -> io::Result<Option<([u8; 16], u64, u64)>> {
    match std::fs::read(path) {
        Ok(bytes) => {
            let Ok(pt) = astream_aead::open(key, HW_AAD, &bytes) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "anti-rollback: the head-watermark sidecar failed to authenticate (tampered or wrong key)",
                ));
            };
            let Some((id, base, head)) = decode_watermark_plaintext(&pt) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "anti-rollback: malformed watermark",
                ));
            };
            Ok(Some((id, base, head)))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Durably (re)write the authenticated watermark to `(log_id, base, head)` via a
/// tmp-file rename, so a crash never leaves a torn watermark. Binding `base` too
/// keeps the (unauthenticated) retention base sidecar from being tampered under an
/// anti-rollback log. Monotone by construction (the caller only advances head).
#[cfg(feature = "anti-rollback")]
fn write_watermark(
    path: &Path,
    key: &[u8; 32],
    log_id: &[u8; 16],
    base: u64,
    head: u64,
) -> io::Result<()> {
    let mut pt = Vec::with_capacity(32);
    pt.extend_from_slice(log_id);
    pt.extend_from_slice(&encode_u64_le_bytes(base));
    pt.extend_from_slice(&encode_u64_le_bytes(head));
    let sealed = astream_aead::seal(key, HW_AAD, &pt);
    let mut tmp = path.to_path_buf();
    tmp.as_mut_os_string().push(".tmp");
    {
        let mut f = create_fresh(&tmp, &mut FsOpenOptions::new())?;
        f.write_all(&sealed)?;
        f.sync_all()?;
    }
    rename_path(&tmp, path)?;
    sync_parent_dir(path)?;
    Ok(())
}

/// Seal a record's payload for disk under the at-rest key (bound to its log index
/// as AAD so a record cannot be moved/duplicated within the log), or pass it
/// through when no key is set.
#[cfg(feature = "at-rest")]
fn seal_disk_payload(key: Option<&[u8; 32]>, index: u64, payload: Vec<u8>) -> Vec<u8> {
    match key {
        Some(k) => astream_aead::seal(k, &encode_u64_le_bytes(index), &payload),
        None => payload,
    }
}
#[cfg(not(feature = "at-rest"))]
fn seal_disk_payload(_key: Option<&[u8; 32]>, _index: u64, payload: Vec<u8>) -> Vec<u8> {
    payload
}

/// Open a disk record's payload: `Some(plaintext)` on success, `None` if a key is
/// set and the payload fails to authenticate (wrong key or a tampered record).
#[cfg(feature = "at-rest")]
pub(crate) fn open_disk_payload(
    key: Option<&[u8; 32]>,
    index: u64,
    frame_payload: Vec<u8>,
) -> Option<Vec<u8>> {
    match key {
        Some(k) => match astream_aead::open(k, &encode_u64_le_bytes(index), &frame_payload) {
            Ok(p) => Some(p),
            Err(_) => None,
        },
        None => Some(frame_payload),
    }
}
#[cfg(not(feature = "at-rest"))]
pub(crate) fn open_disk_payload(
    _key: Option<&[u8; 32]>,
    _index: u64,
    frame_payload: Vec<u8>,
) -> Option<Vec<u8>> {
    Some(frame_payload)
}

/// A point on the durability dial — what an `ack` promises. Orthogonal to the
/// determinism dial; every setting is benchmarkable and its guarantee is stated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Durability {
    /// `ack` ⟹ the batch is on stable media (Apple: `F_FULLFSYNC`; elsewhere
    /// `fdatasync`). A power loss loses nothing acked. The default and the strictest
    /// tier (stricter than the usual-suspects' defaults).
    ///
    /// The batch in flight when the power failed was never acked; a sealing build cuts
    /// it at its start at the next open, and moves the generation (with lineage) only
    /// when that cut removed written bytes — a record of the batch, or a whole frame of
    /// it, which the bytes cannot tell from rot in an acked record. A process crash in
    /// the boot the run served in (a host that names its boot, the log on the boot
    /// volume) moves nothing even then: a batch the dead process left half-written was
    /// its own unfinished write. Once a sealing build has opened the log (its `<log>.id`
    /// records the durable floor), the open does not refuse for a torn batch — with one
    /// residual: a record whose body carries a whole record frame planted at a place a
    /// record of the torn batch could hold, which a resync cannot tell from a later
    /// write (see `recover.rs`; such a log refuses, untouched, never loses records).
    /// Damage below that floor, or behind a later complete unit, is no torn write and
    /// still REFUSES (`InvalidData`, the file untouched) until an operator repairs the
    /// log: `asb repair <log>` ([`BrokerLog::open_repair`]), or for a log sealed at rest
    /// `BrokerLog::open_encrypted_repair` with its key. A log no sealing build has
    /// opened yet is judged by the rules that predate the floor, which refuse a torn
    /// batch whose bytes the open cannot tell from damage.
    #[default]
    Strict,
    /// `ack` ⟹ the batch is ORDERED on the device (Apple: `F_BARRIERFSYNC`): every write
    /// issued before the barrier reaches stable media before any write issued after it,
    /// without the ack waiting for the media to hold either — a fraction of the cost of
    /// [`Strict`](Self::Strict)'s full flush, and a smaller tax on the volume's other
    /// writers, though not none. Measured on one Mac under shared load (the fsync study,
    /// 2026-10-16): the flush alone p50 0.29 ms (n = 26,000) against 4.3 ms (n = 42,000),
    /// a 64-byte append with its flush p50 0.14 ms against 4.3–7.3 ms (n = 6,000 each);
    /// beside a barrier loop a neighbour's 64-byte page-cache write took p50 11–80 µs and
    /// p95 52–671 µs over three passes of 2,000 (alone 3–5 µs and 5–18 µs; beside a
    /// full-flush loop 169–214 µs and 4.2–5.2 ms). The full flushes this tier still issues
    /// — one per escalated batch, and about one per horizon under traffic — carry a full
    /// flush's whole tax.
    ///
    /// After a power loss the log is a prefix of whole acked batches. It holds every batch
    /// a full flush covered:
    ///
    /// - every record the open's escalation table names
    ///   ([`OpenOptions::escalation`]), flushed in full BEFORE it is visible to any
    ///   reader or acked — the records a person would miss;
    /// - every batch older than `horizon` plus one batch and one full flush: the writer
    ///   issues a full flush no EARLIER than `horizon` after the first ack no full flush
    ///   covers — after the batch in hand when it falls due, or when idle, and never
    ///   inside the log's lock — so that ack can be up to the horizon, the batch the
    ///   writer was in, and the flush itself old before it is on stable media;
    /// - everything before a clean close.
    ///
    /// It may lack newer batches. When it may have lost an acked batch, the next open
    /// moves the generation by one and records where the generations agree
    /// ([`LogIdentity::continues`](crate::LogIdentity::continues)). A process crash
    /// (`kill -9`) loses nothing: the kernel that holds the page cache runs on. It moves
    /// nothing either, but where the open cannot know it was only a process crash: a run
    /// that served in a boot it could not name ([`Boot::Unknown`], or a kernel that will
    /// not say) moves the generation at any unclean reopen, and so does a run a failed
    /// full flush recorded `Lossy` — the safe direction both times. Once a sealing build
    /// has opened the log, the open never refuses after a power loss; damage below the
    /// last open's durable floor, or behind a later sealed batch, still refuses, because
    /// it is not a torn write.
    ///
    /// A full flush that FAILS (an escalation's, the horizon's, `flush_durable`'s) poisons
    /// the open: the records it was to cover stay unseen and their ops are answered
    /// "outcome unknown", the run is recorded `Lossy`, and every write is refused until
    /// the log is reopened — a later flush that succeeds says nothing about what the
    /// failed one was to cover ([`BrokerLog::poisoned`]).
    ///
    /// Served as [`Strict`](Self::Strict) where the device cannot promise the order: any
    /// platform but Apple (Linux has no ordering-only flush — `fdatasync` empties the
    /// drive's cache), a log file off the boot volume (a device that may lose power apart
    /// from the kernel, and whose barrier honouring is unknown), a descriptor the barrier
    /// is refused on at open, or one it is refused on later (the log latches to Strict for
    /// the rest of the open, saying so once). Never weaker. What a broker serves, and why,
    /// is [`BrokerLog::served`] and the `Hello` block.
    ///
    /// `horizon: None`: only escalations, [`flush_durable`](crate::Broker::flush_durable)
    /// (or [`BrokerLog::sync_full`]) and a clean close flush in full, so NO age bounds the
    /// loss: a power loss can take every batch since the last of them. It is what a test
    /// that wants durability to depend on nothing but the code under it asks for, and what
    /// `Hello` reports as no horizon (`horizon_ms: None`, never `Some(0)`).
    /// [`Durability::ordered`] is the one-second horizon. A [`BrokerLog`] used directly
    /// (no broker, no writer thread) has no horizon at all.
    Ordered {
        /// How long after the first ack no full flush covers the writer issues one (see
        /// above for the bound that gives); `None` for no horizon.
        horizon: Option<std::time::Duration>,
    },
    /// `ack` ⟹ the batch is written to the OS page cache (NO `fsync`). Lowest latency /
    /// highest throughput (no fsync on the hot path), comparable to Redis
    /// `appendfsync=no` / Kafka `acks=1`. The honest move is surfacing this choice, not
    /// hiding a durability difference inside a benchmark — and saying what it costs:
    ///
    /// - A PROCESS crash (`kill -9`, a panic, an abort) loses nothing acked: the bytes
    ///   are already in the kernel, which writes them back whether or not the process
    ///   lives. A batch the dead writer left half-written (Linux stops a write between
    ///   pages for a fatal signal) ends the file inside that batch, which the next open
    ///   cuts at its start — it was never acked — and moves nothing for, in the same
    ///   boot; that open's own `fsync` makes every record it recovers durable.
    /// - A POWER loss (a kernel panic, a forced power-off, a watchdog reset) may lose
    ///   any batch no full flush covered — every batch since the last open or clean
    ///   close — in any order: the device may persist the page cache's blocks in any
    ///   order, and the file's length apart from them. The next open, in its new boot,
    ///   keeps the longest intact prefix above the last durable floor (the open's, or a
    ///   clean close's), cuts the rest, and moves the log's
    ///   [`LogIdentity`](crate::LogIdentity) generation with lineage, so a client that
    ///   kept offsets resumes where the log continues
    ///   ([`LogIdentity::continues`](crate::LogIdentity::continues)) instead of trusting
    ///   offsets the lost records gave up. Damage below the floor refuses (`InvalidData`,
    ///   the file untouched) until the explicit repair. 'Crash-consistent regardless' was
    ///   withdrawn: it held for a process crash only.
    ///
    /// The rule rests on the boot session (`astream-sys`): it knows a power loss by the
    /// reboot it implies. A run that stopped cleanly ([`BrokerLog::close_clean`], which
    /// the broker runs at shutdown) moves nothing in any boot. Where the boot cannot be
    /// named, or the log is off the boot volume, every unclean stop moves the
    /// generation, a process crash included — the price of not knowing.
    ///
    /// The escalation table ([`OpenOptions::escalation`]) applies here too: a record it
    /// names is flushed in full before it is visible or acked, and a full flush of the
    /// log's file writes every dirty page of it, so that record and everything before it
    /// survive a power loss — the one exception to "any batch, in any order".
    Relaxed,
}

/// The default [`Durability::Ordered`] horizon: one second. A sparse burst pays at most
/// one full flush for it (the horizon runs from the first ack no full flush covers), and
/// under continuous traffic it costs about one full flush a second — measured at 2–3 per
/// 3 s, about 0.4 % of the device's time, against 28–30 at 100 ms; the 100 ms horizon
/// showed no p50 benefit.
pub const ORDERED_HORIZON: std::time::Duration = std::time::Duration::from_secs(1);

impl Durability {
    /// [`Durability::Ordered`] at the default [`ORDERED_HORIZON`].
    #[must_use]
    pub const fn ordered() -> Durability {
        Durability::Ordered {
            horizon: Some(ORDERED_HORIZON),
        }
    }

    /// The tier as the `Hello` block and `<log>.id` code it.
    pub(crate) const fn tier(self) -> crate::proto::Tier {
        match self {
            Durability::Strict => crate::proto::Tier::Strict,
            Durability::Ordered { .. } => crate::proto::Tier::Ordered,
            Durability::Relaxed => crate::proto::Tier::Relaxed,
        }
    }

    /// The horizon an [`Ordered`](Self::Ordered) log's writer keeps; `None` for any other
    /// tier, and for `Ordered { horizon: None }`.
    pub(crate) const fn horizon(self) -> Option<std::time::Duration> {
        match self {
            Durability::Ordered { horizon } => horizon,
            Durability::Strict | Durability::Relaxed => None,
        }
    }
}

/// How many flushes a log has issued since it opened, by kind: what the counting tests
/// and the fabric bench read ([`BrokerLog::flush_counts`],
/// [`Broker::flush_counts`](crate::Broker::flush_counts)). The open's own flush and a
/// clean close's are not counted; everything the write path issues is, a flush that
/// failed included (in `failed`, not in `barrier` or `full`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FlushCounts {
    /// Ordering-only flushes (`F_BARRIERFSYNC`) that succeeded: one per Ordered batch.
    pub barrier: u64,
    /// Full flushes (`F_FULLFSYNC` on Apple, `fdatasync` elsewhere) that succeeded: one
    /// per Strict batch, and on the weaker tiers one per escalated batch (or batch
    /// answering a resent escalated op whose original none covered), horizon,
    /// `flush_durable` or `sync_full` that found something to cover.
    pub full: u64,
    /// Full flushes ASKED FOR — a horizon, a `flush_durable`, a `sync_full` — and skipped,
    /// because a full flush already covered everything they would have. A resent
    /// escalated op whose original a full flush already covers asks for none, so it is
    /// not counted here: the dedup answers it with nothing owed.
    pub skipped: u64,
    /// Flushes of either kind that returned an error: each fails its batch, and one that
    /// leaves acked or held records uncovered poisons the log until it is reopened
    /// ([`BrokerLog::poisoned`]).
    pub failed: u64,
}

/// The tier a log serves NOW, and why it differs from the one asked for: decided once by
/// the open, and lowered to Strict (never raised) when a barrier or a full flush fails
/// while it serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Served {
    tier: crate::proto::Tier,
    reason: crate::proto::ServedReason,
}

/// What [`BrokerLog::commit_batch`] tells the writer to do once the batch is on the file:
/// a PURE value, the one the broker's writer acts on and the crash simulation marks acks
/// by, so the two cannot disagree about what was acked when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Committed {
    /// The head readers may see now: the first record a full flush still owes (an
    /// escalated record, held back until it is durable), or the log's head when none is
    /// owed. An op is acked now only if its record is below it.
    pub(crate) visible_now: Offset,
    /// `Some(head)` when a full flush is owed before the rest may be shown or acked: the
    /// batch holds an escalated record no full flush covers, or a resend of one.
    pub(crate) owe_full: Option<Offset>,
    /// The original offsets of RESENT escalated ops (dedup hits) whose originals no full
    /// flush covers yet: their acks wait for the flush even when the original is already
    /// visible (an older record a resend now names as one a person would miss).
    pub(crate) held: Vec<u64>,
}

impl Committed {
    /// Whether the op whose record is at `off` (`deduped`: a resend that matched an
    /// existing record) may be acked before the owed full flush.
    pub(crate) fn acks_now(&self, off: u64, deduped: bool) -> bool {
        off < self.visible_now.0 && !(deduped && self.held.contains(&off))
    }
}

/// Which boot this open is in, for the unclean-stop rule (see
/// [`LogIdentity`](crate::LogIdentity)'s module docs): a weak-tier run that did not stop
/// cleanly moves the generation at the first open in ANOTHER boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Boot {
    /// Ask the kernel ([`astream_sys::boot_session`]). The default.
    #[default]
    Detect,
    /// This boot is the one with this id — what a test uses to stand in a reboot. The
    /// all-zero id means unknown.
    Known([u8; 16]),
    /// This open cannot name its boot: every unclean weak-tier stop moves the
    /// generation, a process crash included (the safe direction).
    Unknown,
}

impl Boot {
    fn resolve(self) -> Option<[u8; 16]> {
        match self {
            Boot::Detect => astream_sys::boot_session(),
            Boot::Known(id) => (id != [0u8; 16]).then_some(id),
            Boot::Unknown => None,
        }
    }
}

/// Whether the log FILE is on the BOOT VOLUME — the device that loses power only
/// together with the kernel, so a process crash in the same boot cannot lose what its
/// page cache holds. Off it (a removable or network disk can vanish while the kernel
/// runs), every unclean weak-tier stop moves the generation, and Ordered is served
/// Strict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Volume {
    /// Compare the OPENED file's device (`st_dev`, from `fstat`) with `/`'s (and, on
    /// Apple, `/System/Volumes/Data`'s). The default.
    #[default]
    Detect,
    /// On the boot volume — what a test asserts.
    Boot,
    /// Off it — what a test uses to stand in a removable disk.
    Other,
}

impl Volume {
    fn resolve(self, file: &Backing) -> bool {
        match self {
            Volume::Detect => match file {
                Backing::Os(f) => on_boot_volume(f),
                #[cfg(test)]
                Backing::Sim(_) => true,
            },
            Volume::Boot => true,
            Volume::Other => false,
        }
    }
}

/// Whether the open log file shares a device with the boot volume.
///
/// THE FILE'S OWN DEVICE, never its directory's: the log is opened following symlinks,
/// so `~/fabric/bus.log` can be a link into `/Volumes/USB` while `~/fabric` is on the
/// data volume. Judged by the directory, that log was taken for one on the boot volume
/// — served Ordered on a drive whose barrier honouring is unknown, and spared the
/// unclean bump for a same-boot reopen after the drive lost power on its own (a pulled
/// cable), when the batches acked since its last full flush are gone and new publishes
/// would reuse their offsets under an unchanged identity. `fstat` on the descriptor the
/// log is written through cannot be pointed elsewhere.
#[cfg(unix)]
fn on_boot_volume(file: &File) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(here) = file.metadata() else {
        return false;
    };
    let roots: &[&str] = if cfg!(target_vendor = "apple") {
        // APFS splits the boot volume into the read-only system volume (`/`) and the
        // data volume (`/System/Volumes/Data`, where `~` and `/private/tmp` live); both
        // sit on the one internal device.
        &["/", "/System/Volumes/Data"]
    } else {
        &["/"]
    };
    let here_dev = here.dev();
    for &r in roots {
        if let Ok(m) = std::fs::metadata(r) {
            if m.dev() == here_dev {
                return true;
            }
        }
    }
    false
}

#[cfg(not(unix))]
fn on_boot_volume(_file: &File) -> bool {
    false
}

/// What an embedder's `prepare` (run under the log's own lock, before recovery) found:
/// see [`BrokerLog::open_opts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prepared {
    /// Nothing for the log's identity.
    Nothing,
    /// The embedder knows the last run may have lost acked records (its own unclean
    /// mark, say) — the reason, in words. ORed into the open's one decision: the
    /// generation moves by one however many verdicts agree.
    Unclean(String),
}

/// How [`BrokerLog::open_opts`] (and [`Broker::open_opts`](crate::Broker::open_opts))
/// open a log. Every other constructor is a shorthand for it. Build one with
/// [`new`](Self::new) and set the fields that differ from the defaults.
#[non_exhaustive]
#[derive(Clone, Default)]
pub struct OpenOptions {
    /// What an ack promises ([`Durability`]).
    pub durability: Durability,
    /// The records flushed in full before they are visible or acked, on a tier whose
    /// ordinary ack does not wait for a full flush ([`Durability::Ordered`],
    /// [`Durability::Relaxed`]; under [`Durability::Strict`] every batch is). Empty by
    /// default. A rule that does not parse refuses the open (`InvalidInput`), before the
    /// file is created or `prepare` runs. See [`Escalation`](crate::Escalation).
    pub escalation: crate::Escalation,
    /// Which boot this open is in ([`Boot::Detect`] unless a test stands one in).
    pub boot: Boot,
    /// Whether the log is on the boot volume ([`Volume::Detect`] unless a test says).
    pub volume: Volume,
    /// `Some(secret)`: a broker opened with these options enforces capabilities, as
    /// [`Broker::open_guarded`](crate::Broker::open_guarded) does (an empty secret is
    /// refused). Ignored by [`BrokerLog::open_opts`].
    #[cfg(feature = "cap")]
    pub guard: Option<Vec<u8>>,
    /// The crash simulation's recovery mutation (a negative control).
    #[cfg(test)]
    pub(crate) mutation: Mutation,
}

impl OpenOptions {
    /// Options for `durability`, everything else at its default (detect the boot and
    /// the volume; no guard).
    #[must_use]
    pub fn new(durability: Durability) -> OpenOptions {
        OpenOptions {
            durability,
            ..OpenOptions::default()
        }
    }

    fn mutation(&self) -> Mutation {
        #[cfg(test)]
        {
            self.mutation
        }
        #[cfg(not(test))]
        {
            Mutation::Off
        }
    }
}

impl fmt::Debug for OpenOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("OpenOptions");
        d.field("durability", &self.durability)
            .field("escalation", &self.escalation)
            .field("boot", &self.boot)
            .field("volume", &self.volume);
        // The capability secret is a key: never printed.
        #[cfg(feature = "cap")]
        {
            let guard_str = match self.guard {
                Some(_) => Some("<secret>"),
                None => None,
            };
            d.field("guard", &guard_str);
        }
        d.finish()
    }
}

/// The crash simulation's RECOVERY MUTATIONS: each re-introduces one bug the recovery
/// rules exist to prevent, so the simulation can show its properties catch it (the
/// negative controls of `crash_sim.rs`). Only ever `Off` outside this crate's own unit
/// tests — the field that sets one is `cfg(test)` — which is the `InjectedFaults`
/// precedent, narrowed because nothing outside the crate may arm one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Mutation {
    #[default]
    Off,
    /// NC5: write phase 2 (the floor at the head) BEFORE the open's flush, the order
    /// that predates the floor — records a dead writer left only in the page cache
    /// then count as durable.
    FloorBeforeFlush,
    /// NC6: cut at any damage, with no floor check and no later-seal refusal.
    CutAtAnyDamage,
    /// NC8: cut a Relaxed log at its first damage with no unclean verdict (everything
    /// else of that rule kept: what the cut removes still moves the generation).
    RelaxedCutWithoutVerdict,
    /// NC9: record lineage even for a log found shorter than the last open saw.
    LineageOnShorter,
    /// NC11: no empty-frame rule (an empty frame reads as a payload that does not
    /// parse, as before the rule).
    NoEmptyFrameRule,
    /// The baseline: recover as if no floor were known (the rules that predate it).
    IgnoreFloor,
}

impl Mutation {
    pub(crate) const fn floor_before_flush(self) -> bool {
        matches!(self, Mutation::FloorBeforeFlush)
    }
    pub(crate) const fn cut_at_any_damage(self) -> bool {
        matches!(self, Mutation::CutAtAnyDamage)
    }
    pub(crate) const fn relaxed_cut_without_verdict(self) -> bool {
        matches!(self, Mutation::RelaxedCutWithoutVerdict)
    }
    pub(crate) const fn lineage_on_shorter(self) -> bool {
        matches!(self, Mutation::LineageOnShorter)
    }
    pub(crate) const fn no_empty_frame_rule(self) -> bool {
        matches!(self, Mutation::NoEmptyFrameRule)
    }
    pub(crate) const fn ignore_floor(self) -> bool {
        matches!(self, Mutation::IgnoreFloor)
    }
}

/// A DOCUMENTED fault-injection seam: make the log's fsync and/or its rollback
/// truncate fail, so the failed-fsync rollback and the failed-rollback POISON paths
/// can be witnessed by the claim tests without a real disk fault. Integration tests
/// link the library without `cfg(test)`, so this is always compiled; it is inert
/// unless armed, and nothing in the broker arms it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InjectedFaults {
    /// Every FULL `fsync` of the log fails: a Strict batch's (rolled back, nothing
    /// acked), and on the weaker tiers the full flush an escalated record, the horizon or
    /// `flush_durable` asks for (the run is recorded `Lossy` and the log poisoned until it is
    /// reopened: `BrokerLog::full_flush_failed`).
    /// An [`Ordered`](Durability::Ordered) batch's ordering-only barrier is no full flush
    /// and still succeeds, which is how a test reaches the failure AFTER the barrier.
    pub sync: bool,
    /// Every rollback truncate fails (the log poisons itself).
    pub truncate: bool,
}

/// Why staging a record into the current batch failed.
#[derive(Debug)]
pub(crate) enum StageErr {
    /// The encoded record exceeds the frame limit — an op-level error; the file is
    /// untouched, so the batch can still commit its other records.
    TooLarge,
    /// The subject is one of the broker-internal hidden subjects — an op-level error;
    /// a client cannot append a record there (it would bypass dedup and never be
    /// delivered). Carries the subject so the refusal names it.
    Reserved(String),
    /// This producer already owns [`MAX_SUBJECTS_PER_PRODUCER`] distinct subjects and
    /// the record would create one more — an op-level error; the file is untouched.
    SubjectBound(String),
    /// A will did not fire: a record with a HIGHER `producer_seq` from the same
    /// producer has landed since it was registered. Not a failure — the intended
    /// outcome of the fence.
    Fenced(String),
    /// A producer id is already bound to a DIFFERENT principal — an op-level error;
    /// the file is untouched and the attach is refused.
    Collision(String),
    /// A replicated record does not extend this log's spine (a gap, or a different
    /// record already held at that offset) — an op-level error naming the offset.
    Diverged(String),
    /// A will was offered a `producer_seq` in the reserved ack half
    /// ([`ACK_SEQ_BASE`]) — an op-level error refused at REGISTRATION, where the
    /// client is still listening; the file is untouched.
    SeqReserved(String),
    /// A replicated HIDDEN record (`/a/will`, `/a/bind`) was offered to a log this
    /// broker OWNS — an op-level error; the file is untouched. Decided in the writer,
    /// under the lock that appends, because "whose log is this" is only settled there:
    /// carries the whole message, which is the same wording the connection thread's
    /// early refusal uses.
    ReservedReplica(String),
    /// The log is a REPLICA and the op would have appended a record of this broker's
    /// OWN making (a will firing at open). Not a failure — the intended outcome of
    /// the leader/replica split; the file is untouched.
    Replica(String),
    /// An I/O error while staging (the replica marker could not be written) — the
    /// batch is poisoned and must be aborted. Also raised for every mutation once the
    /// log is [poisoned](BrokerLog#poison).
    Io(io::Error),
}

/// The one wording every path uses to refuse a hidden subject, so the CLI, the
/// library and the wire all say the same thing.
pub(crate) fn reserved_msg(subject: &str) -> String {
    format!("reserved subject {subject}: broker-internal (consumer-group commits, wills and producer bindings are the broker's own records)")
}

/// The PURE COMMIT shape: a leader's own `/a/commit` record, which carries a group
/// commit and nothing else (no producer key, no body). The one hidden record shape a
/// client-reachable `Replicate` may carry onto any log — it appends no publish and
/// grants nothing `Request::Commit` does not already grant.
fn is_pure_commit(rec: &BrokerRecord) -> bool {
    rec.subject == COMMIT_SUBJECT
        && rec.producer_id == 0
        && rec.producer_seq == 0
        && rec.body.is_empty()
        && rec.commit.is_some()
}

/// The one wording for a replicated HIDDEN record offered to a log this broker owns,
/// used by the connection thread's early refusal and by the writer's own check under
/// the lock, so the two cannot say different things about the same frame.
pub(crate) fn replicated_reserved_msg(subject: &str) -> String {
    format!(
        "reserved subject {subject}: this broker OWNS its log, and only a replication \
         target takes a replicated {subject} record (a follower seeded from a copied log \
         is declared with Broker::open_replica)"
    )
}

fn reserved_err(subject: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reserved_msg(subject))
}

/// The one wording for a mutation a POISONED log refuses ([`BrokerLog::poisoned`]):
/// the library's own error (every mutation's gate) and the writer's refusal of each op
/// of a batch on the poisoned log say the same thing, and both start
/// [`LOG_POISONED_PREFIX`](crate::proto::LOG_POISONED_PREFIX), which
/// [`BrokerRefusal::is_poisoned`](crate::BrokerRefusal::is_poisoned) reads.
pub(crate) fn poisoned_refusal(reason: &str) -> String {
    format!(
        "{} ({reason}); reopen it to recover",
        crate::proto::LOG_POISONED_PREFIX
    )
}

/// The one wording for the distinct-subject bound.
pub(crate) fn subject_bound_msg(producer_id: u64) -> String {
    format!(
        "producer {producer_id} already holds {MAX_SUBJECTS_PER_PRODUCER} distinct subjects \
         (MAX_SUBJECTS_PER_PRODUCER): a new subject would grow the last-value index without bound"
    )
}

/// Take the exclusive advisory lock on the log file, failing `AlreadyExists` if
/// another open `BrokerLog` (in this or any process) holds it. A filesystem without
/// advisory locks is tolerated (the lock is a guard, not the durability mechanism).
/// The OS releases the lock when the process dies, so a crashed broker never leaves a
/// stale lock behind (unlike a lock FILE would).
// `File::try_lock` / `unlock` are std-stable since 1.89, the workspace's declared
// `rust-version`; the toolchain itself is Trust (rust-toolchain.toml).
fn lock_exclusive(file: &File, path: &Path) -> io::Result<()> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(std::fs::TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "log {} is locked by another open BrokerLog (a broker already serves it)",
                path.display()
            ),
        )),
        Err(std::fs::TryLockError::Error(e)) if e.kind() == io::ErrorKind::Unsupported => Ok(()),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

/// What the replica marker file holds — prose for an operator who finds it, since
/// only its EXISTENCE is load-bearing.
const REPLICA_MARKER_NOTE: &[u8] = b"astream-broker: this log is a REPLICATION TARGET.\n\
    Its records are a leader's spine; this broker appends nothing of its own to it\n\
    (in particular it does not fire the wills the log holds when it opens).\n";

/// The REPLICA MARKER beside a log: `<log>.replica`, an empty-but-for-a-comment file
/// whose mere EXISTENCE records that this log is a replication TARGET — that it took
/// its first replicated record through [`stage_replica`](BrokerLog::stage_replica)
/// while it was still empty, or that an operator declared it one.
///
/// It cannot live IN the log: every byte of the log is the leader's spine, so a
/// marker record would be exactly the divergence the marker exists to prevent.
///
/// THE UNDO IS THE FILE. There is no verb that un-declares a replica, because the
/// declaration is the file: delete `<log>.replica` and the next open reads a log this
/// broker owns again, which fires the wills it holds. That is the whole of the repair
/// for a follower declared by mistake, and it is what the stderr line
/// [`mark_replica`](BrokerLog::mark_replica) prints on the first write points at.
fn replica_marker_path(path: &Path) -> std::path::PathBuf {
    let mut p = path.to_path_buf();
    p.as_mut_os_string().push(".replica");
    p
}

/// fsync the directory that names `path` (a just-created log, or a sidecar just
/// created or renamed into place), so the directory entry — not only the file's data
/// — survives a power loss. `sync_data` on the file alone never persists the entry
/// that names it.
#[cfg(unix)]
pub(crate) fn sync_parent_dir(path: &Path) -> io::Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let res = match File::open(parent) {
        Ok(d) => d.sync_all(),
        Err(e) => Err(e),
    };
    match res {
        Ok(()) => Ok(()),
        // The directory entry simply cannot be fsync'd here, through no fault of the
        // log: a filesystem that refuses to fsync a directory handle (Unsupported /
        // InvalidInput), or a directory this process may write but not OPEN — a
        // write-only 0300 log directory, where opening the log itself is allowed.
        // The file's own bytes are still fsync'd; there is nothing more we can do
        // about the entry, and FAILING here would refuse a log that opens fine.
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}
#[cfg(not(unix))]
pub(crate) fn sync_parent_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

/// Where an open's bytes live: the real file at a path, or (tests) the crash
/// simulation's device, which the same open runs against unchanged.
pub(crate) enum Storage<'a> {
    Os(&'a Path),
    #[cfg(test)]
    Sim(crate::crash_sim::Sim),
}

/// The served-tier code `<log>.id` carries for `tier`: what the NEXT open reads to decide
/// whether this run could have lost an acked record to a power loss.
const fn served_code(tier: crate::proto::Tier) -> u8 {
    match tier {
        crate::proto::Tier::Strict => logid::SERVED_STRICT,
        crate::proto::Tier::Ordered => logid::SERVED_ORDERED,
        // Relaxed, and (unreachable: a log serves only its own three) a code this build
        // does not know, which reads as the weakest tier.
        _ => logid::SERVED_RELAXED,
    }
}

/// The tier an open SERVES when asked for `durability` — decided once, after the open's
/// full flush, and never weaker than asked:
///
/// | asked | Apple, log on the boot volume, barrier probe Ok | Apple, anything else | not Apple |
/// |---|---|---|---|
/// | Strict | Strict | Strict | Strict |
/// | Ordered | Ordered | Strict, with a reason | Strict (`NotApple`) |
/// | Relaxed | Relaxed | Relaxed | Relaxed |
///
/// The boot volume matters because the same-boot rule trusts the page cache of a device
/// that loses power only with the kernel, and because a barrier on a disk that is not
/// Apple's own is not known to be honoured. The PROBE is one barrier on the log's own
/// descriptor: a filesystem or device that refuses it answers here, at open, rather than
/// at the first ack. (The simulated device models a barrier on every platform.)
fn decide_served(durability: Durability, boot_volume: bool, file: &Backing) -> Served {
    use crate::proto::{ServedReason, Tier};
    let asked = durability.tier();
    if asked != Tier::Ordered {
        return Served {
            tier: asked,
            reason: ServedReason::None,
        };
    }
    if !file.barrier_exists() {
        return Served {
            tier: Tier::Strict,
            reason: ServedReason::NotApple,
        };
    }
    if !boot_volume {
        return Served {
            tier: Tier::Strict,
            reason: ServedReason::NotBootVolume,
        };
    }
    match file.barrier() {
        Ok(()) => Served {
            tier: Tier::Ordered,
            reason: ServedReason::None,
        },
        Err(e) => {
            eprintln!(
                "astream-broker: the barrier was refused at open ({e}); this log serves \
                 Strict, never weaker than asked"
            );
            Served {
                tier: Tier::Strict,
                reason: ServedReason::BarrierRefused,
            }
        }
    }
}

/// The flush a commit owes on top of its write, by the tier SERVED
/// ([`BrokerLog::flush_owed`]): chosen under the log's lock as a batch begins, issued by
/// the batch's write outside it ([`BatchWrite::run`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// A full flush before the ack (Strict).
    Full,
    /// An ordering barrier before the ack (Ordered).
    Barrier,
    /// Nothing before the ack (Relaxed).
    Nothing,
}

/// The seal a batch of `count` records ends with — `None` for a count no `u32` holds,
/// which no batch reaches (every staged record is held in memory until the commit),
/// and which the commit refuses whole rather than write a batch no seal closes.
fn batch_seal(count: usize) -> Option<Mark> {
    let Ok(n) = u32::try_from(count) else {
        return None;
    };
    Some(Mark::Seal(n))
}

/// What a flush did ([`issue_flush`]), for the log to count in [`FlushCounts`] and act
/// on under its lock ([`BrokerLog::settle_issued`]). The device work needs no lock and a
/// batch does it without one ([`BatchWrite::run`]); the counts and the latch are the
/// log's state, so they wait for the lock.
#[derive(Debug, Default)]
struct Issued {
    /// A full flush returned `Ok`: the write is DURABLE.
    full: bool,
    /// An ordering barrier returned `Ok`.
    barrier: bool,
    /// The flush failed.
    failed: bool,
    /// The device refused the barrier ([`astream_sys::barrier_refused`]), with why: the
    /// log latches to Strict, and the write was flushed in full instead.
    refused: Option<String>,
}

/// Issue `flush` through `out` after a write, recording what it did in `issued`.
/// `sync_fault` is the fault-injection seam's failed fsync and `refuse_barrier` the test
/// seam's refusing device, both as they stood when the write began. `Ok(true)` when the
/// write is now DURABLE (a full flush covered it); a refused barrier falls back to a
/// full flush here, before the ack, so a device that refuses one never acks a write it
/// did not order.
fn issue_flush(
    out: &crate::storage::BatchOut,
    flush: Flush,
    sync_fault: bool,
    refuse_barrier: bool,
    issued: &mut Issued,
) -> io::Result<bool> {
    match flush {
        Flush::Nothing => Ok(false),
        Flush::Full => {
            let res = if sync_fault {
                Err(io::Error::other("injected fsync fault"))
            } else {
                out.sync_data()
            };
            if let Err(e) = res {
                issued.failed = true;
                return Err(e);
            }
            issued.full = true;
            Ok(true)
        }
        Flush::Barrier => {
            let res = if refuse_barrier {
                Err(io::Error::from(io::ErrorKind::Unsupported))
            } else {
                out.barrier()
            };
            match res {
                Ok(()) => {
                    issued.barrier = true;
                    Ok(false)
                }
                Err(e) if refuse_barrier || astream_sys::barrier_refused(&e) => {
                    issued.refused = Some(e.to_string());
                    issue_flush(out, Flush::Full, sync_fault, refuse_barrier, issued)
                }
                Err(e) => {
                    issued.failed = true;
                    Err(e)
                }
            }
        }
    }
}

/// What [`BrokerLog::commit_begin`] hands the writer.
pub(crate) enum Begun {
    /// Nothing staged: nothing to write or flush, and the commit's answer now — which can
    /// still owe a full flush (a resent escalated op whose original no full flush
    /// covers). The log's lock never needs to be let go for it.
    Empty(Committed),
    /// A batch to write and flush with the log's lock RELEASED ([`BatchWrite::run`]),
    /// then settle under it ([`BrokerLog::commit_end`]).
    Write(BatchWrite),
}

/// A staged batch on its way to the file with the log's lock RELEASED: its bytes as
/// written (sealed), the file's own descriptor ([`BatchOut`](crate::storage::BatchOut)),
/// the flush its tier owes, and the fault-injection and refusing-device seams as they
/// stood when the batch began. Made under the lock by [`BrokerLog::commit_begin`],
/// [`run`](Self::run) without it, settled under it again by [`BrokerLog::commit_end`],
/// which promotes the batch only if `run` returned `Ok`, and counts what its flush did
/// ([`Issued`]).
///
/// WHY THE WRITE AND THE FLUSH LEAVE THE LOCK. The broker's log `Mutex` is the one every
/// reader takes: a `Fetch` page, a `Last` round, a tail copying the record it was just
/// woken for. While the writer held it across `write` and `F_FULLFSYNC`, each of them
/// waited out most of an fsync whenever a Strict writer was busy — a 4-row `Fetch`
/// beside one serial Strict publisher p50 3.37 ms (0.010 ms idle), and a subscriber
/// woken by one batch lost the lock to the NEXT queued batch and received its record a
/// whole fsync after the ack (recv minus ack p50 4.21 ms; publish → recv 8.59 ms). With
/// the write and the flush outside the lock those were 0.029 ms, 0.005 ms and 4.30 ms,
/// and the writer's total lock hold over a fabric bench run fell from about 2.4 s to
/// about 11 ms (cost study B2, the checker's re-measurement, load 45-57). The lock now
/// covers staging, the seal and the promotion — memory work — and never the device:
/// not a Strict batch's full flush, not an Ordered batch's barrier (nor the full flush a
/// refused barrier falls back to), and, as before, not the full flush an escalated
/// record owes (`process_batch` takes a [`FlushHandle`](crate::storage::FlushHandle)
/// for that one).
///
/// MEASURED on this change against the commit before it (`broker_unlocked_write_bench`,
/// 6 interleaved runs per arm, n = 1200 per series, load 3.6-8.2): beside one serial
/// Strict publisher a 4-row `Fetch` went from p50/p95 2.010/3.952 ms to 0.014/0.078 ms
/// and a 4-row `Last` page from 2.386/4.128 to 0.014/0.081; with a second batch queued
/// 1 ms behind, a delivery's recv minus ack went from p50/p95 4.257/6.058 ms to
/// 0.004/0.024 and publish → recv from 7.796/11.229 to 3.566/5.194. `Hello`, which
/// takes no log lock, is the control (0.012 against 0.010). aterm's fabric burst (300
/// posts, 12 interleaved pairs, load 8-22) is neutral: 200.6 against 211.5 msg/s
/// nearest-rank p50, paired median -3.9 msg/s, because its posts are still published
/// one at a time, one fsync each, and no reader there waits behind a queued batch.
///
/// WHAT IT DOES NOT MOVE. The staged records are promoted only by `commit_end`, after
/// the flush returned, so nothing becomes visible or ack-able before it is durable (or
/// ordered) at its tier; the held head, the durable head and every count move there
/// too. Retention, the clean close and every other mutation refuse while the batch is
/// in flight (`staged` non-empty, `writing` set), and the broker's own retention calls
/// wait for it on the writer's batch lock instead. The anti-rollback watermark is still
/// advanced under the lock in `commit_end` (a sidecar write per batch, on the
/// encrypted, verified logs only), and its pre-bind in `commit_begin` (once per log).
pub(crate) struct BatchWrite {
    out: crate::storage::BatchOut,
    bytes: Vec<u8>,
    flush: Flush,
    faults: InjectedFaults,
    refuse_barrier: bool,
    issued: Issued,
}

impl BatchWrite {
    /// Write the batch in ONE call, then issue the flush its tier owes ([`issue_flush`]):
    /// the only part of a commit that waits on the device, and the part that runs
    /// without the log's lock. The injected fsync fault ([`InjectedFaults::sync`]) fails
    /// a full flush HERE, as a real one would, so a failed flush reaches the rollback
    /// through the same halves a real one does.
    pub(crate) fn run(&mut self) -> io::Result<()> {
        self.out.write_all(&self.bytes)?;
        let _ = issue_flush(
            &self.out,
            self.flush,
            self.faults.sync,
            self.refuse_barrier,
            &mut self.issued,
        )?;
        Ok(())
    }
}

/// How an operator's stderr line names a settled identity's lineage.
fn lineage_note(settled: &logid::Settled) -> String {
    match settled.lineage.steps().last() {
        Some(&(generation, common)) if generation == settled.identity.generation => {
            format!(" (offsets below {common} name the same records as before)")
        }
        _ => " (a reader keeping offsets re-reads the log)".to_string(),
    }
}

/// Open (creating if absent) and exclusively lock the log file, then fsync its
/// directory, returning whether this call created the file. `sync_dir` is
/// [`sync_parent_dir`], injectable so a test can see it.
fn open_log_file(path: &Path, sync_dir: fn(&Path) -> io::Result<()>) -> io::Result<(File, bool)> {
    let (file, created) = match open_with_options(
        FsOpenOptions::new().read(true).write(true).create_new(true),
        path,
    ) {
        Ok(f) => (f, true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (
            open_with_options(FsOpenOptions::new().read(true).write(true), path)?,
            false,
        ),
        Err(e) => return Err(e),
    };
    lock_exclusive(&file, path)?;
    // On EVERY open, not only the one that creates the file: that open may have died
    // (or failed) before its directory fsync, and a record acked into a file whose
    // entry is not durable can vanish with it on a power loss.
    sync_dir(path)?;
    Ok((file, created))
}

impl BrokerLog {
    /// Open (creating if absent) the log at `path` with Strict durability.
    pub fn open(path: impl AsRef<Path>) -> io::Result<BrokerLog> {
        Self::open_with(path, Durability::Strict)
    }

    /// Open (creating if absent) the log at `path` at the given [`Durability`],
    /// recovering the longest intact, in-order prefix and rebuilding the offset spine +
    /// dedup map from it. (Recovery is identical for both tiers — both keep a clean
    /// prefix; the tier only changes whether an `ack` waits for `fsync`.)
    ///
    /// Recovery distinguishes a TORN TAIL from CORRUPTION: an incomplete trailing
    /// frame or a zero-filled tail (an interrupted write) is truncated, durably. A
    /// complete frame that does not decode (bad CRC/magic/version), a record whose
    /// payload does not parse (an older `BREC_VERSION` this build no longer reads, or a
    /// foreign file), or an offset gap, when MORE BYTES FOLLOW or the frame is itself
    /// complete, is corruption: this returns `InvalidData` naming the offset and byte,
    /// and leaves the file UNTOUCHED. Use [`open_repair`](Self::open_repair) to
    /// truncate at that point explicitly.
    ///
    /// A record in a NEWER format than this build reads (its version byte above
    /// [`BREC_VERSION`](crate::BREC_VERSION)) is not corruption: a newer astream wrote
    /// it. This returns `Unsupported`, saying so, and `open_repair` refuses it the same
    /// way — the file is untouched either way.
    ///
    /// Settles the log's [`identity`](Self::identity) and records it in `<log>.id`
    /// before returning; an open that cannot write that sidecar fails.
    ///
    /// Takes an exclusive advisory lock on the file: a second open of the same log
    /// (this or another process) fails `AlreadyExists` until the first is closed. On a
    /// fresh file the parent directory is fsync'd, so the entry naming the log survives
    /// a power loss along with the data later fsync'd into it.
    pub fn open_with(path: impl AsRef<Path>, durability: Durability) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_inner(
            path.as_ref(),
            durability,
            false,
            None,
            AntiRollbackMode::Off,
        )?;
        Ok(log)
    }

    /// Prepare embedder sidecars after taking the log's own file lock and
    /// syncing its directory, before recovery reads or changes any log state.
    /// The same owned file stays locked through recovery and the returned log.
    pub(crate) fn open_with_prepare(
        path: impl AsRef<Path>,
        durability: Durability,
        prepare: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<BrokerLog> {
        Self::open_opts(path, OpenOptions::new(durability), || {
            prepare()?;
            Ok(Prepared::Nothing)
        })
    }

    /// Open (creating if absent) the log at `path` as `opts` say — the one open every
    /// other constructor is a shorthand for (at an empty-escalation, detected-boot,
    /// detected-volume default).
    ///
    /// `prepare` runs once, after the file is opened, exclusively locked and its
    /// directory synced, and before recovery or the identity sidecar is read; its
    /// [`Prepared`] answer feeds the identity rules (an embedder's own unclean verdict
    /// moves the generation, once, together with astream's). It must not change the log
    /// file itself; its own sidecars it changes crash-safely, and an error it returns
    /// propagates unchanged, releasing the lock.
    ///
    /// Recovery and the identity rules are [`open_with`](Self::open_with)'s, plus what
    /// a known durable floor allows (the floor is recorded by every open of this build,
    /// after its flush, and raised by [`close_clean`](Self::close_clean)): a power loss
    /// does not make the open refuse a log this build has opened before — a torn batch is
    /// cut at its start, or, after an unclean Relaxed stop, at its first damage — while
    /// damage below the floor, or behind a later complete unit, still refuses, because no
    /// torn write leaves it. (One residual refuses a torn batch: a record frame planted
    /// in a body at a place a record of the batch could hold — see [`Durability::Strict`].)
    /// See [`Durability`] for what each tier's power loss costs.
    pub fn open_opts(
        path: impl AsRef<Path>,
        opts: OpenOptions,
        prepare: impl FnOnce() -> io::Result<Prepared>,
    ) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_core(
            Storage::Os(path.as_ref()),
            &opts,
            false,
            None,
            AntiRollbackMode::Off,
            prepare,
        )?;
        Ok(log)
    }

    /// Open (creating if absent) an ENCRYPT-AT-REST log: every record's payload is
    /// sealed on disk under `key` (XChaCha20-Poly1305, random nonce, log-index AAD),
    /// so a reader of the file recovers nothing without it. A wrong key or a tampered
    /// record is a corrupt-tail error (refuse to open), never a silent truncation —
    /// encrypted data is never discarded. Strict durability.
    #[cfg(feature = "at-rest")]
    pub fn open_encrypted(path: impl AsRef<Path>, key: [u8; 32]) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_inner(
            path.as_ref(),
            Durability::Strict,
            false,
            Some(key),
            AntiRollbackMode::Off,
        )?;
        Ok(log)
    }

    /// [`open_encrypted`](Self::open_encrypted) at a chosen [`Durability`].
    #[cfg(feature = "at-rest")]
    pub fn open_encrypted_with(
        path: impl AsRef<Path>,
        durability: Durability,
        key: [u8; 32],
    ) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_inner(
            path.as_ref(),
            durability,
            false,
            Some(key),
            AntiRollbackMode::Off,
        )?;
        Ok(log)
    }

    /// Like [`open_with`](Self::open_with) but TRUNCATES at a corruption point instead
    /// of refusing to open, returning the log and the number of bytes discarded (the
    /// corrupt record and everything after it — possibly acked data; this is the
    /// explicit, operator-invoked repair, never the default). A clean log or a merely
    /// torn tail opens exactly as `open_with` would.
    ///
    /// A repair that truncates records advances the log's
    /// [`identity`](Self::identity) generation (durably, before the first byte is
    /// cut): later appends reuse the cut offsets, so every offset a client persisted
    /// before the repair is meaningless after it. A record in a NEWER format is not
    /// repaired: it is refused `Unsupported`, exactly as by `open_with`.
    ///
    /// KEYLESS: it reads every record as plaintext, so on a log sealed at rest the very
    /// first record — intact, its CRC holding — does not parse. Cutting there would
    /// discard every record of the log, the authentic ones before any damage included,
    /// so a log whose FIRST record is intact but not a plaintext record is refused
    /// (`InvalidData`, the file untouched): repair a sealed log with
    /// `open_encrypted_repair` and its key.
    pub fn open_repair(
        path: impl AsRef<Path>,
        durability: Durability,
    ) -> io::Result<(BrokerLog, u64)> {
        Self::open_inner(path.as_ref(), durability, true, None, AntiRollbackMode::Off)
    }

    /// [`open_repair`](Self::open_repair) for a log sealed at rest, under its `key`.
    /// The scan authenticates every record, so the cut lands at the first DAMAGE — a
    /// hole a power loss left, a frame that does not decode — and every authentic
    /// record before it is kept; the bytes discarded are returned. A cut advances the
    /// generation durably before the first byte goes, as `open_repair`'s does.
    ///
    /// It never cuts at a record that fails to AUTHENTICATE. That frame is intact (its
    /// CRC holds), so no crash or power loss made it: it is a wrong key — which would
    /// otherwise cut the whole log away from its first record — or a record rewritten
    /// by someone without the key. Either is refused `InvalidData` with the file
    /// untouched, exactly as by [`open_encrypted`](Self::open_encrypted); a log
    /// tampered with is restored from a backup.
    #[cfg(feature = "at-rest")]
    pub fn open_encrypted_repair(
        path: impl AsRef<Path>,
        durability: Durability,
        key: [u8; 32],
    ) -> io::Result<(BrokerLog, u64)> {
        Self::open_inner(
            path.as_ref(),
            durability,
            true,
            Some(key),
            AntiRollbackMode::Off,
        )
    }

    /// Open an ENCRYPT-AT-REST log with ANTI-ROLLBACK: an authenticated monotonic head
    /// watermark (a `<log>.hw` sidecar) is maintained after every durable commit and
    /// verified on open. A log truncated/rolled back below the watermark — or a
    /// watermark sidecar missing (deleted) on a non-empty log — is REFUSED, so a
    /// write-capable attacker cannot silently revert the log to an earlier state
    /// without the key. Use [`open_encrypted_verified_init`](Self::open_encrypted_verified_init)
    /// ONCE to establish the watermark the first time anti-rollback is enabled.
    #[cfg(feature = "anti-rollback")]
    pub fn open_encrypted_verified(path: impl AsRef<Path>, key: [u8; 32]) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_inner(
            path.as_ref(),
            Durability::Strict,
            false,
            Some(key),
            AntiRollbackMode::Verify,
        )?;
        Ok(log)
    }

    /// Bootstrap [`open_encrypted_verified`](Self::open_encrypted_verified): establish
    /// the head watermark at the current head without requiring a prior sidecar (first
    /// adoption). This one open cannot detect a rollback that predates the watermark.
    #[cfg(feature = "anti-rollback")]
    pub fn open_encrypted_verified_init(
        path: impl AsRef<Path>,
        key: [u8; 32],
    ) -> io::Result<BrokerLog> {
        let (log, _) = Self::open_inner(
            path.as_ref(),
            Durability::Strict,
            false,
            Some(key),
            AntiRollbackMode::Init,
        )?;
        Ok(log)
    }

    /// Verify (or establish) the anti-rollback head watermark on open, returning the
    /// sidecar path to maintain (or `None` when anti-rollback is off).
    #[cfg(feature = "anti-rollback")]
    fn open_watermark(
        mode: AntiRollbackMode,
        path: &Path,
        key: Option<&[u8; 32]>,
        durable: &[u8],
        base: u64,
        head: u64,
    ) -> io::Result<Option<PathBuf>> {
        if mode == AntiRollbackMode::Off {
            return Ok(None);
        }
        let Some(key) = key else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "anti-rollback requires an at-rest key",
            ));
        };
        let hw = watermark_path(path);
        let log_id = log_identity(durable);
        match read_watermark(&hw, key)? {
            Some((persisted_id, persisted_base, persisted_head)) => {
                if head < persisted_head {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "anti-rollback: log head {head} is BELOW the persisted watermark \
                             {persisted_head} — the log was truncated or rolled back"
                        ),
                    ));
                }
                // Bind to THIS log. A non-empty log's watermark always carries this
                // log's identity — the first commit binds it before writing a byte
                // (`prebind_watermark`) — so a watermark spliced from a DIFFERENT
                // same-key log (its own identity), or an unbound zero one, is refused:
                // a key-less attacker cannot mint a matching one. A log AHEAD of its own
                // bound watermark is only the crash lag advanced below.
                //
                // An EMPTY log has no identity. It may sit under an unbound watermark,
                // or under a pre-bind whose records never landed — at exactly this base
                // and head. A bound watermark any other empty log sits under is a log
                // truncated away with its base sidecar moved up to hide it.
                let bound = if head > base {
                    persisted_id == log_id
                } else {
                    persisted_id == [0u8; 16] || (persisted_base, persisted_head) == (base, head)
                };
                if !bound {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "anti-rollback: the watermark does not belong to this log \
                         (foreign or stale watermark / cross-log splice)",
                    ));
                }
                // The authenticated watermark also pins the retention base, so a
                // tampered (unauthenticated) base sidecar cannot silently reindex the
                // log under anti-rollback. A base BELOW the watermark's is a rollback;
                // ahead is only the benign crash-lag advanced below.
                if base < persisted_base {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "anti-rollback: retention base {base} is BELOW the persisted \
                             base {persisted_base} — the base sidecar was rolled back or tampered"
                        ),
                    ));
                }
                // The log committed/compacted past the last watermark write (a crash
                // between the record/base durability and the watermark update): advance.
                if head > persisted_head || base > persisted_base {
                    write_watermark(&hw, key, &log_id, base, head)?;
                }
            }
            None => {
                if mode == AntiRollbackMode::Verify && head > 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "anti-rollback: the head-watermark sidecar is missing on a non-empty log \
                         (deleted?) — re-establish it with open_encrypted_verified_init",
                    ));
                }
                write_watermark(&hw, key, &log_id, base, head)?;
            }
        }
        Ok(Some(hw))
    }

    #[cfg(not(feature = "anti-rollback"))]
    fn open_watermark(
        _mode: AntiRollbackMode,
        _path: &Path,
        _key: Option<&[u8; 32]>,
        _durable: &[u8],
        _base: u64,
        _head: u64,
    ) -> io::Result<Option<PathBuf>> {
        Ok(None)
    }

    /// Advance the anti-rollback head watermark to the current head after a durable
    /// commit. FAIL-CLOSED: the write is propagated, so a batch is not acked unless its
    /// head is durably covered by the watermark — the invariant `persisted >= every
    /// acked head` is what makes a below-watermark head on open a sound rollback signal.
    /// (A no-op when anti-rollback is off.)
    #[cfg(feature = "anti-rollback")]
    fn advance_watermark(&self) -> io::Result<()> {
        if let (Some(hw), Some(key)) = (&self.head_watermark, self.at_rest_key.as_ref()) {
            write_watermark(hw, key, &self.log_id, self.base.0, self.next.0)?;
        }
        Ok(())
    }

    #[cfg(not(feature = "anti-rollback"))]
    fn advance_watermark(&self) -> io::Result<()> {
        Ok(())
    }

    /// Before the first record of an EMPTY log is written, bind the watermark to the
    /// identity that record will give the log (at the unchanged head). Otherwise a
    /// crash between that record's fsync and the watermark update after it leaves a
    /// non-empty log under an unbound watermark, which a verified open must refuse
    /// (it is also what a watermark spliced from a fresh log looks like) — where
    /// this way it is only a log ahead of its own watermark, the ordinary crash lag.
    /// `first_frames` are the frames about to be written. A no-op on a non-empty log,
    /// whose identity is fixed, or when anti-rollback is off.
    #[cfg(feature = "anti-rollback")]
    fn prebind_watermark(&self, first_frames: &[u8]) -> io::Result<()> {
        if let (true, Some(hw), Some(key)) = (
            self.durable_len == 0,
            &self.head_watermark,
            self.at_rest_key.as_ref(),
        ) {
            let log_id = log_identity(first_frames);
            write_watermark(hw, key, &log_id, self.base.0, self.next.0)?;
        }
        Ok(())
    }

    #[cfg(not(feature = "anti-rollback"))]
    fn prebind_watermark(&self, _first_frames: &[u8]) -> io::Result<()> {
        Ok(())
    }

    /// [`advance_watermark`](Self::advance_watermark) over records that were just
    /// promoted. They are already durable and visible, so a failure cannot be undone
    /// by a rollback: it POISONS the log instead. Otherwise a retry of the same key
    /// would be acked as a dedup hit the watermark does not cover — or, on the
    /// un-batched path, which records the key in dedup only after this returns, be
    /// appended a second time. A reopen re-establishes the watermark.
    fn cover_head(&mut self) -> io::Result<()> {
        if let Err(e) = self.advance_watermark() {
            self.poisoned = Some(format!("anti-rollback watermark update failed: {e}"));
            return Err(e);
        }
        Ok(())
    }

    fn prepare_nothing() -> io::Result<Prepared> {
        Ok(Prepared::Nothing)
    }

    fn open_inner(
        path: &Path,
        durability: Durability,
        repair: bool,
        at_rest_key: Option<[u8; 32]>,
        anti_rollback: AntiRollbackMode,
    ) -> io::Result<(BrokerLog, u64)> {
        Self::open_core(
            Storage::Os(path),
            &OpenOptions::new(durability),
            repair,
            at_rest_key,
            anti_rollback,
            Self::prepare_nothing,
        )
    }

    /// THE open, every constructor's: lock the file, run the embedder's `prepare`,
    /// recover, settle the identity, and record it around the flush.
    ///
    /// 1. Lock the file and run `prepare` (before anything is read).
    /// 2. Read the identity sidecar: the durable floor, the last run's tier, boot and
    ///    state, and so this open's UNCLEAN verdict.
    /// 3. Scan and classify ([`recover::classify`]): keep, cut, or refuse.
    /// 4. Settle the identity and lineage ([`logid::settle`]).
    /// 5. PHASE 1, only before a cut that removes written bytes (or a repair's cut):
    ///    record the moved identity durably, so the cut cannot outlive its
    ///    announcement.
    /// 6. Cut, then flush the file in full — even when nothing was cut: a writer that
    ///    died between a batch's write and its flush leaves complete records it never
    ///    acked, only in the page cache; recovery accepts them, so they must be durable
    ///    before this process serves them or acks a retry as their dedup hit.
    /// 7. PHASE 2, after the flush: the run is `Open` at this tier and boot, and the log
    ///    is durable through its head — the floor. Written AFTER the flush on purpose:
    ///    written before it (the order that predates the floor), records the dead writer
    ///    left only in the page cache would count as durable, and a power loss before
    ///    the flush would leave damage below a floor that promised none (the crash
    ///    simulation's NC5). Not written when it would say what the sidecar already
    ///    does.
    pub(crate) fn open_core(
        storage: Storage<'_>,
        opts: &OpenOptions,
        repair: bool,
        at_rest_key: Option<[u8; 32]>,
        anti_rollback: AntiRollbackMode,
        prepare: impl FnOnce() -> io::Result<Prepared>,
    ) -> io::Result<(BrokerLog, u64)> {
        let durability = opts.durability;
        let mutation = opts.mutation();
        // Anti-rollback rests on one invariant: the persisted watermark covers every
        // acked head (`advance_watermark`), so a head BELOW it on open can only be a
        // rollback. Only Strict keeps that true across a power loss. Any weaker tier
        // acks a batch before it is on stable media while the watermark sidecar is
        // rewritten durably after it, so an ordinary power loss leaves the log below a
        // watermark that did persist — and the verified open then refuses an honest
        // loss as an attack, with no repair that keeps the check. (It would also pay the
        // watermark's own durable rewrite per batch, the cost the weaker tier exists to
        // skip.) The verified constructors hard-code Strict today; this keeps a future
        // one honest, and refuses before the file is created or anything is prepared.
        if anti_rollback != AntiRollbackMode::Off && durability != Durability::Strict {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "anti-rollback requires Durability::Strict, not {durability:?}: its \
                     watermark must cover every acked head, and a {durability:?} ack can be \
                     lost to a power loss the watermark would then refuse as a rollback"
                ),
            ));
        }
        // The escalation table is checked here, before the file is created or anything
        // is prepared: a rule that does not parse is a caller's mistake, refused by name.
        let rules = opts.escalation.compile()?;
        let (mut file, dir, created) = match storage {
            Storage::Os(path) => {
                let (file, created) = open_log_file(path, sync_parent_dir)?;
                (
                    Backing::from_file(file),
                    Dir::Os(path.to_path_buf()),
                    created,
                )
            }
            #[cfg(test)]
            Storage::Sim(sim) => {
                let created = sim.open_file();
                (Backing::Sim(sim.clone()), Dir::Sim(sim), created)
            }
        };
        let path = dir.log_path().to_path_buf();
        let prepared = prepare()?;
        // A log that ever took a replicated record is a replica FOREVER, across every
        // constructor: the marker beside it is read before a single record is decoded.
        // A log WE just created carries none of that history, so a marker left beside
        // the name by a deleted predecessor is stale and is cleared rather than
        // inherited. (The simulated log has no replica marker, base or state sidecar.)
        let replica = dir.is_os() && {
            let marker = replica_marker_path(&path);
            if created {
                let _ = remove_file_path(&marker);
                let _ = remove_file_path(&state_path(&path));
            }
            marker.exists()
        };
        let mut existing = Vec::new();
        file.read_to_end(&mut existing)?;
        let (base, compacted) = if dir.is_os() {
            let base = read_base(&path)?;
            (base, read_state(&path, at_rest_key.as_ref(), base.0)?)
        } else {
            (Offset::ZERO, CompactedState::default())
        };
        let prior = if created {
            Prior::Absent
        } else {
            logid::read(&dir)?
        };
        let boot = opts.boot.resolve();
        let boot_volume = opts.volume.resolve(&file);
        let floor = if mutation.ignore_floor() {
            None
        } else {
            prior.floor(base.0)
        };
        let unclean = match &prepared {
            Prepared::Unclean(why) => prior.unclean(Some(why), boot),
            Prepared::Nothing => prior.unclean(None, boot),
        };
        let scanned = scan_from(&existing, at_rest_key.as_ref(), base.0, floor, mutation);
        let classified = recover::classify(
            &scanned,
            &recover::Judge {
                bytes: &existing,
                key: at_rest_key.as_ref(),
                base: base.0,
                floor,
                unclean_relaxed: unclean.is_some() && prior.served_relaxed() && floor.is_some(),
                prior_relaxed: prior.served_relaxed(),
                same_boot: prior.same_boot(boot, boot_volume),
                mutation,
            },
        );
        let (keep, kept_records, written, repair_cut) = match classified {
            Classified::Keep {
                keep,
                records,
                written,
            } => (keep, records, written, false),
            Classified::Corrupt {
                offset,
                at_byte,
                reason,
                repair_at,
            } => {
                Self::refuse_or_repair(
                    &path,
                    &existing,
                    (repair, at_rest_key.is_some()),
                    (offset, at_byte),
                    &reason,
                    repair_at,
                )?;
                (at_byte, scanned.records.len(), true, true)
            }
            // A record in a NEWER format than this build reads was WRITTEN, not damaged:
            // its payload passed the frame CRC (and, at rest, authentication) before its
            // version byte was read. It is refused by name on EVERY open, `open_repair`
            // included — truncating it would destroy records the release that wrote them
            // reads — and the file is left untouched.
            Classified::Newer {
                offset,
                at_byte,
                version,
            } => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!(
                        "{}: the record at offset {offset} (byte {at_byte}) is in record format \
                         v{version}, written by a newer astream; this build reads up to v{}: \
                         upgrade astream to open this log (the file is untouched, and no repair \
                         applies)",
                        path.display(),
                        crate::brecord::BREC_VERSION
                    ),
                ));
            }
        };
        let pre_cut_head = base.0.saturating_add(scanned.records.len() as u64);
        let valid_len = scanned.valid_len;
        let mut records = scanned.records;
        records.truncate(kept_records);
        let kept_head = base.0.saturating_add(records.len() as u64);
        let dropped = existing.len().saturating_sub(keep) as u64;
        let (serving, served) = Self::settle_and_flush_open(
            &mut file,
            &dir,
            &path,
            &prior,
            &existing,
            valid_len,
            keep,
            base.0,
            pre_cut_head,
            kept_head,
            dropped,
            replica,
            unclean.as_deref(),
            written,
            repair_cut,
            mutation,
            durability,
            boot,
            boot_volume,
        )?;
        existing.truncate(keep);
        let durable_len = keep as u64;
        let log_id = log_identity(&existing);
        let newest_at = match logid::last_frame_start(&existing) {
            Some(at) => Some(at as u64),
            None => None,
        };
        let next = Offset(base.0.saturating_add(records.len() as u64));
        // Anti-rollback: verify (or establish) the authenticated head watermark against
        // the recovered head, refusing a log that was truncated below it.
        let head_watermark = Self::open_watermark(
            anti_rollback,
            &path,
            at_rest_key.as_ref(),
            &existing,
            base.0,
            next.0,
        )?;
        drop(existing);
        let (
            records,
            dedup,
            last,
            subjects_per_producer,
            high_water,
            carried_wills,
            carried_last,
            bind,
            group_commits,
        ) = Self::rebuild_open_indices(compacted, records);
        let mut log = BrokerLog {
            file,
            writing: false,
            dir,
            path,
            replica,
            identity: serving,
            diverged: false,
            durable_len,
            newest_at,
            log_id,
            records,
            base,
            next,
            dedup,
            last,
            subjects_per_producer,
            will_reserved: HashMap::new(),
            high_water,
            carried_wills,
            carried_last,
            bind,
            group_commits,
            staged: Vec::new(),
            staged_bytes: Vec::new(),
            staged_last_at: 0,
            #[cfg(test)]
            unsealed_writer: false,
            staged_dedup: HashMap::new(),
            staged_new_subjects: HashMap::new(),
            staged_bind: HashMap::new(),
            durability,
            served,
            rules,
            // Everything recovered was flushed in full above.
            durable_head: next.0,
            held_from: None,
            held_commits: Vec::new(),
            staged_escalated: None,
            staged_held: Vec::new(),
            counts: FlushCounts::default(),
            #[cfg(test)]
            refuse_barrier: false,
            poisoned: None,
            closed: false,
            faults: InjectedFaults::default(),
            at_rest_key,
            head_watermark,
            replication_epoch: 0,
            replica_ack_upto: base,
        };
        // The recovery scan rebuilt the landed subjects; this rebuilds the ones the
        // log's still-pending wills have SPOKEN FOR. Both halves of the bound are
        // recovered from the bytes, so a broker restart cannot hand a producer back a
        // budget its registered wills already hold.
        log.reserve_pending_will_subjects();
        Ok((log, dropped))
    }

    #[allow(clippy::too_many_arguments)]
    fn settle_and_flush_open(
        file: &mut Backing,
        dir: &Dir,
        path: &Path,
        prior: &Prior,
        existing: &[u8],
        valid_len: usize,
        keep: usize,
        base: u64,
        pre_cut_head: u64,
        kept_head: u64,
        dropped: u64,
        replica: bool,
        unclean: Option<&str>,
        written: bool,
        repair_cut: bool,
        mutation: Mutation,
        durability: Durability,
        boot: Option<[u8; 16]>,
        boot_volume: bool,
    ) -> io::Result<(logid::Sidecar, Served)> {
        let pre_cut_slice = existing.get(..valid_len).unwrap_or(existing);
        let kept_slice = existing.get(..keep).unwrap_or(existing);
        let settled = logid::settle(
            prior,
            &logid::Facts {
                replica,
                pre_cut: pre_cut_slice,
                pre_cut_head,
                kept: kept_slice,
                base,
                kept_head,
                unclean: unclean.is_some(),
                cut_records: written && !repair_cut && dropped > 0,
                repair_cut,
                lineage_on_shorter: mutation.lineage_on_shorter(),
                minted: logid::minted_id(path),
            },
        );
        if dir.is_os() {
            let why = unclean.unwrap_or("");
            if !why.is_empty() && prior.is_recorded() {
                eprintln!(
                    "astream-broker: {}: the last run that served it may have lost acked \
                     records ({why}); its identity is now {}{}",
                    path.display(),
                    settled.identity,
                    lineage_note(&settled)
                );
            }
            // A recovery cut that may remove a record a client was told about is said
            // too: the operator learns of it here, not from a reader that forgot.
            if written && !repair_cut && dropped > 0 {
                eprintln!(
                    "astream-broker: {}: cut {dropped} bytes from offset {kept_head}, a torn \
                     batch the recovery cannot tell from damage to a record a client may have \
                     seen; its identity is now {}{}",
                    path.display(),
                    settled.identity,
                    lineage_note(&settled)
                );
            }
        }
        // The tier is recorded as ASKED until the open has decided what it serves (after
        // its flush, below): phase 1 records the LAST run's tier when there was one, and a
        // weak tier recorded where Strict is then served is the safe direction for an
        // open that dies before phase 2.
        let mut record = logid::Record {
            settled: &settled,
            base,
            head: kept_head,
            kept: kept_slice,
            served: served_code(durability.tier()),
            boot,
            boot_volume,
        };
        let phase1 = Self::flush_open_phase1(
            file,
            dir,
            prior,
            &record,
            dropped,
            keep,
            written,
            repair_cut,
            unclean.is_some(),
            mutation,
        );
        phase1.and_then(|()| {
            // The tier SERVED, decided now that everything recovered is durable (the probe is
            // a barrier on the log's own descriptor), and recorded in phase 2: the next open
            // reads from it whether this run's acks could be lost to a power loss.
            let served = decide_served(durability, boot_volume, file);
            record.served = served_code(served.tier);
            // PHASE 2: the floor rises to the head only now that the head is durable.
            Self::finish_serving_open(dir, prior, &record, mutation, served)
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn flush_open_phase1(
        file: &mut Backing,
        dir: &Dir,
        prior: &Prior,
        record: &logid::Record<'_>,
        dropped: u64,
        keep: usize,
        written: bool,
        repair_cut: bool,
        unclean: bool,
        mutation: Mutation,
    ) -> io::Result<()> {
        // PHASE 1: the moved identity is durable before the bytes that justify it go.
        let res = if dropped > 0 && (written || repair_cut) {
            let cutting = record.cutting(prior, unclean);
            logid::write(dir, &cutting)
        } else {
            Ok(())
        };
        let res = if mutation.floor_before_flush() {
            res.and_then(|()| Self::write_serving_if_changed(dir, prior, record))
        } else {
            res
        };
        let res = if dropped > 0 {
            res.and_then(|()| file.resize_file(keep as u64))
        } else {
            res
        };
        let res = res.and_then(|()| file.sync_all());
        res.and_then(|()| file.seek_end())
    }

    fn write_serving_if_changed(
        dir: &Dir,
        prior: &Prior,
        record: &logid::Record<'_>,
    ) -> io::Result<()> {
        let early = record.serving();
        if logid::unchanged(prior, &early) {
            Ok(())
        } else {
            logid::write(dir, &early)
        }
    }

    fn finish_serving_open(
        dir: &Dir,
        prior: &Prior,
        record: &logid::Record<'_>,
        mutation: Mutation,
        served: Served,
    ) -> io::Result<(logid::Sidecar, Served)> {
        let serving = record.serving();
        let res = if !mutation.floor_before_flush() && !logid::unchanged(prior, &serving) {
            logid::write(dir, &serving)
        } else {
            Ok(())
        };
        res.map(|()| (serving, served))
    }

    #[allow(clippy::type_complexity)]
    fn rebuild_open_indices(
        compacted: CompactedState,
        records: Vec<BrokerRecord>,
    ) -> (
        Vec<Arc<BrokerRecord>>,
        HashMap<(u64, u64), Offset>,
        BTreeMap<String, Offset>,
        HashMap<u64, usize>,
        HashMap<u64, u64>,
        HashMap<u64, (Offset, WillRecord)>,
        BTreeMap<String, Arc<BrokerRecord>>,
        HashMap<u64, (String, Offset)>,
        HashMap<String, u64>,
    ) {
        let mut dedup = HashMap::new();
        let mut group_commits: HashMap<String, u64> = compacted.group_commits;
        let mut last: BTreeMap<String, Offset> = BTreeMap::new();
        let mut subjects_per_producer: HashMap<u64, usize> = HashMap::new();
        let mut bind: HashMap<u64, (String, Offset)> = compacted.bind;
        let mut high_water: HashMap<u64, u64> = compacted.high_water;
        let carried_wills: HashMap<u64, (Offset, WillRecord)> = compacted.carried_wills;
        let mut carried_last: BTreeMap<String, Arc<BrokerRecord>> = compacted.carried_last;
        for (subj, rec) in &carried_last {
            if last.insert(subj.clone(), rec.seq).is_none() {
                let c = subjects_per_producer.entry(rec.producer_id).or_insert(0);
                *c = c.saturating_add(1);
            }
        }
        for r in &records {
            if r.subject == BIND_SUBJECT {
                if let Ok(principal) = std::str::from_utf8(&r.body) {
                    if !bind.contains_key(&r.producer_id) {
                        bind.insert(r.producer_id, (principal.to_string(), r.seq));
                    }
                }
            }
            if r.subject == RETIRE_SUBJECT {
                if let Ok(target) = std::str::from_utf8(&r.body) {
                    apply_retire_target(
                        &mut last,
                        &mut carried_last,
                        &mut subjects_per_producer,
                        r.producer_id,
                        target,
                    );
                }
            }
            // The broker's own hidden records (commits, wills, producer bindings) are
            // not produces: they never enter the dedup map and never enter the
            // last-value index. Rebuilding both here is what makes exactly-once ingest
            // AND retained state survive a restart.
            if !is_hidden_subject(&r.subject) {
                dedup.insert((r.producer_id, r.producer_seq), r.seq);
                carried_last.remove(&r.subject);
                if last.insert(r.subject.clone(), r.seq).is_none() {
                    let c = subjects_per_producer.entry(r.producer_id).or_insert(0);
                    *c = c.saturating_add(1);
                }
                // The reserved ack half never enters the fence's high water — the
                // same rule `index_last` applies, applied here, so the fence a
                // restart rebuilds is the fence the log had before it.
                if r.producer_seq < ACK_SEQ_BASE {
                    let hw = high_water.entry(r.producer_id).or_insert(r.producer_seq);
                    *hw = crate::proto::high_water_monotone(*hw, r.producer_seq);
                }
            }
            if let Some(c) = &r.commit {
                let e = group_commits.entry(c.group.clone()).or_insert(0);
                *e = crate::proto::group_commit_monotone(*e, c.upto);
            }
        }
        // Wrap the recovered records in Arc for the shared egress read path (the dedup +
        // group_commits above were already rebuilt from the borrowed records).
        let mut shared_records = Vec::new();
        for rec in records {
            shared_records.push(Arc::new(rec));
        }
        (
            shared_records,
            dedup,
            last,
            subjects_per_producer,
            high_water,
            carried_wills,
            carried_last,
            bind,
            group_commits,
        )
    }

    /// A refused classification: the error the open returns, or — on an explicit
    /// repair, where the point may be cut — `Ok`, the caller cutting there.
    fn refuse_or_repair(
        path: &Path,
        existing: &[u8],
        (repair, keyed): (bool, bool),
        (offset, at_byte): (u64, usize),
        reason: &str,
        cut: RepairAt,
    ) -> io::Result<()> {
        // The way out the refusal names is the one that WORKS on this log. The keyless
        // repair on a sealed log would cut it to nothing (it refuses, see
        // `RepairAt::NeedsKey`), so a keyed open names the keyed repair; and a point no
        // repair may cut names none.
        let way_out = match (cut, keyed) {
            (RepairAt::Cut, false) => {
                "`asb repair <log>`, or BrokerLog::open_repair, truncates them explicitly"
            }
            (RepairAt::Cut, true) => {
                "BrokerLog::open_encrypted_repair, with this key, truncates them \
                 explicitly; the keyless `asb repair` refuses a log sealed at rest"
            }
            (RepairAt::WrongKey, _) => {
                "no repair cuts it: an intact record that fails to authenticate is a \
                 wrong key or a rewrite, never a crash; check the key, or restore the \
                 log from a backup"
            }
            (RepairAt::NeedsKey, _) => {
                "a log sealed at rest opens with its key (BrokerLog::open_encrypted) \
                 and is repaired only with it (BrokerLog::open_encrypted_repair); the \
                 keyless repair refuses it"
            }
        };
        if !repair {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "corrupt record at offset {offset} (byte {at_byte} of {}): {reason}; \
                     refusing to open — the {} bytes from there may be acked data \
                     ({way_out})",
                    existing.len(),
                    existing.len().saturating_sub(at_byte)
                ),
            ));
        }
        // A repair cuts DAMAGE. At an intact frame this open cannot read, the cut would
        // discard records that were written whole — the entire log, when the key is
        // wrong or missing — so the repair refuses before the identity is touched or a
        // byte is cut.
        if cut != RepairAt::Cut {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{}: not repaired, nothing was cut: the record at offset {offset} \
                     (byte {at_byte}) is intact but unreadable here: {reason} ({way_out})",
                    path.display()
                ),
            ));
        }
        Ok(())
    }

    /// Re-establish the will-subject reservations after a recovery scan: every will the
    /// log still holds ([`pending_wills`](Self::pending_wills) — at most one per
    /// producer, the one a broker open would actually fire) holds its target subject
    /// unless that subject is already an index entry.
    fn reserve_pending_will_subjects(&mut self) {
        for will in self.pending_wills() {
            self.reserve_will_subject(&will);
        }
    }

    /// Hold `will`'s TARGET subject against its producer's distinct-subject budget,
    /// unless that subject is already in the last-value index (where it is counted
    /// already, and where a firing adds no entry). Idempotent.
    fn reserve_will_subject(&mut self, will: &WillRecord) {
        if is_hidden_subject(&will.subject) || self.last.contains_key(&will.subject) {
            return;
        }
        self.will_reserved
            .entry(will.producer_id)
            .or_default()
            .insert(will.subject.clone());
    }

    /// CLOSE the log cleanly: flush it in full (`F_FULLFSYNC` on Apple), then record
    /// in `<log>.id` that this run stopped at its head with everything below on stable
    /// media — `Closed`, the durable floor raised to the head. What it buys:
    ///
    /// - a reboot after a clean stop keeps the identity, whatever the tier — the next
    ///   open finds nothing a power loss could have taken;
    /// - rot anywhere in a cleanly closed log is refused, never cut (all of it is below
    ///   the floor).
    ///
    /// The broker runs it when its last listener shuts down (after the writer has
    /// joined) and when an unserved `Broker` drops; a direct `BrokerLog` user calls it
    /// as the log's last act. Afterwards every mutation is refused until the log is
    /// reopened: the sidecar now promises nothing was appended. A poisoned log, or one
    /// with a batch staged, is not closed (`Err`), and a flush or sidecar error leaves
    /// the record `Open` — so the next open in another boot moves the generation once,
    /// the safe direction.
    pub fn close_clean(&mut self) -> io::Result<()> {
        self.check_poisoned()?;
        if !self.staged.is_empty() {
            return Err(io::Error::other(
                "close_clean: a batch is staged; commit or abort it first",
            ));
        }
        self.file.sync_data()?;
        self.durable_head = self.next.0;
        self.held_from = None;
        // A run a failed full flush marked `Lossy` stays so: the device may have dropped
        // what that flush was to cover, and a later flush that succeeds says nothing
        // about it — so the next open moves the generation whatever the boot. (Belt and
        // braces: the failed flush also poisoned this open, so `check_poisoned` above
        // refuses first; were that ever to change, the record must still stay `Lossy`.)
        if self.identity.state == logid::State::Lossy {
            self.closed = true;
            return Ok(());
        }
        let newest = match self.newest_at {
            Some(at) => {
                let Ok(len) = usize::try_from(self.durable_len.saturating_sub(at)) else {
                    return Err(io::Error::other(
                        "close_clean: the newest frame is too long",
                    ));
                };
                self.file.read_range(at, len)?
            }
            None => Vec::new(),
        };
        let closed = self.identity.closed(self.base.0, self.next.0, &newest);
        logid::write(&self.dir, &closed)?;
        self.identity = closed;
        self.closed = true;
        Ok(())
    }

    /// [`close_clean`](Self::close_clean) at shutdown: said on stderr when it fails,
    /// since nothing else is left to report to.
    pub(crate) fn close_clean_at_shutdown(&mut self) {
        if self.poisoned.is_some() || self.closed {
            return;
        }
        if let Err(e) = self.close_clean() {
            eprintln!(
                "astream-broker: {} was not closed cleanly ({e}); its next open in another \
                 boot moves its generation once",
                self.path.display()
            );
        }
    }

    /// The [`Lineage`](crate::Lineage) told beside the [`identity`](Self::identity):
    /// where its generations agree.
    pub const fn lineage(&self) -> &crate::Lineage {
        self.identity.lineage()
    }

    /// The tier this open serves, with the one it was asked for and why they differ —
    /// what the answer to `Hello` reports ([`ServedInfo`](crate::ServedInfo)). `served`
    /// can fall to Strict while the log serves (a barrier refused, a full flush failed),
    /// never rise. `horizon_ms` is the Ordered horizon asked for, rounded up (`None`: no
    /// horizon, or not Ordered — see [`ServedInfo::horizon_ms`](crate::ServedInfo));
    /// `rules` the escalation table's size.
    pub fn served(&self) -> crate::ServedInfo {
        let horizon_ms = match self.durability.horizon() {
            Some(d) => Some(crate::ServedInfo::horizon_ms_of(d)),
            None => None,
        };
        crate::ServedInfo {
            requested: self.durability.tier(),
            served: self.served.tier,
            reason: self.served.reason,
            horizon_ms,
            rules: self.rules.count(),
        }
    }

    /// The DURABLE HEAD: every record below this offset is on stable media by a full
    /// flush this process saw succeed (see [`Durability`]). On a Strict log it is the
    /// head after every batch; on Ordered and Relaxed it moves at the open, at each full
    /// flush (an escalation, the horizon, [`sync_full`](Self::sync_full)) and at a clean
    /// close — never at a barrier.
    pub const fn durable_head(&self) -> Offset {
        Offset(self.durable_head)
    }

    /// How many flushes this log has issued since it opened ([`FlushCounts`]).
    pub const fn flush_counts(&self) -> FlushCounts {
        self.counts
    }

    /// The head readers may see: the log's head, or — while a full flush is still owed
    /// for an escalated record — that record's offset (the HELD head).
    pub(crate) const fn visible_head(&self) -> Offset {
        Offset(match self.held_from {
            Some(h) => h,
            None => self.next.0,
        })
    }

    /// The horizon the broker's writer keeps for this log: the Ordered horizon asked
    /// for, while the log still SERVES Ordered (a log latched to Strict flushes every
    /// batch in full, so it has nothing left to bound).
    pub(crate) const fn horizon(&self) -> Option<std::time::Duration> {
        if matches!(self.served.tier, crate::proto::Tier::Ordered) {
            self.durability.horizon()
        } else {
            None
        }
    }

    /// FLUSH IN FULL NOW, unless a full flush already covers the head (counted as
    /// skipped): the durable head after it. For a [`BrokerLog`] used directly, which has
    /// no writer thread and so no horizon — on an Ordered or Relaxed log nothing but this,
    /// an escalated record and [`close_clean`](Self::close_clean) makes an ack durable.
    /// (The broker's [`flush_durable`](crate::Broker::flush_durable) is the same flush,
    /// issued by its writer outside the log's lock.) A failure is the escalation's
    /// (`full_flush_failed`, private): the run is recorded `Lossy` and
    /// the log refuses every write until it is reopened.
    pub fn sync_full(&mut self) -> io::Result<Offset> {
        self.check_poisoned()?;
        let target = self.next.0;
        if self.durable_head >= target {
            self.counts.skipped = self.counts.skipped.saturating_add(1);
            return Ok(self.durable_head());
        }
        match self.flush_handle().full() {
            Ok(()) => {
                self.full_flush_done(target);
                Ok(self.durable_head())
            }
            Err(e) => {
                self.full_flush_failed(&e);
                Err(e)
            }
        }
    }

    /// A handle that flushes this log in full outside its lock (see
    /// [`FlushHandle`](crate::storage::FlushHandle)), carrying the fault-injection seam's
    /// failed fsync as it stands now. Taking one cannot fail: it shares the log's own
    /// descriptor ([`Backing::flush_handle`]).
    pub(crate) fn flush_handle(&self) -> crate::storage::FlushHandle {
        self.file.flush_handle(self.faults.sync)
    }

    /// Whether a full flush is owed for everything below `target`: `false` (counted as
    /// skipped) when one already covers it — and `false`, uncounted, on a poisoned log,
    /// where no flush can make anything durable for this open
    /// ([`full_flush_failed`](Self::full_flush_failed)).
    pub(crate) const fn full_flush_owed(&mut self, target: u64) -> bool {
        if self.poisoned.is_some() {
            return false;
        }
        if self.durable_head >= target {
            self.counts.skipped = self.counts.skipped.saturating_add(1);
            return false;
        }
        true
    }

    /// A full flush that covers every record below `target` SUCCEEDED (taken outside the
    /// lock through [`flush_handle`](Self::flush_handle), or by
    /// [`sync_full`](Self::sync_full)): the durable head rises to it, and every record a
    /// held head kept back below it may now be shown and acked. Never on a poisoned log:
    /// after a failed full flush a later one that succeeds says nothing about what the
    /// failed one was to cover ([`full_flush_failed`](Self::full_flush_failed)).
    pub(crate) fn full_flush_done(&mut self, target: u64) {
        if self.poisoned.is_some() {
            return;
        }
        self.counts.full = self.counts.full.saturating_add(1);
        self.durable_head = self.durable_head.max(target);
        if let Some(h) = self.held_from {
            if h < self.durable_head {
                self.held_from = None;
                self.release_held_commits();
            }
        }
    }

    /// A full flush FAILED (counted in [`FlushCounts::failed`]). Nothing acked can be
    /// un-acked, and the device may have lost what the flush was to cover:
    /// [`flush_failed_lossy`](Self::flush_failed_lossy).
    pub(crate) fn full_flush_failed(&mut self, e: &io::Error) {
        self.counts.failed = self.counts.failed.saturating_add(1);
        self.flush_failed_lossy(e);
    }

    /// A flush failed while PROMOTED records exist that no full flush has covered — an
    /// escalation's, the horizon's, `flush_durable`'s, `sync_full`'s, or a batch's own
    /// flush on a log whose earlier acks rode barriers or the page cache. The device may
    /// have dropped any of those records, so:
    ///
    /// 1. the run is recorded `Lossy` in `<log>.id` (best effort: said on stderr when
    ///    even that fails), so the next open moves the generation whatever the boot;
    /// 2. the log LATCHES to Strict, which `Hello` reports
    ///    ([`ServedReason::LatchedAfterFlushFailure`](crate::ServedReason));
    /// 3. the log is POISONED: every write is refused until it is reopened, and the
    ///    durable head and the held head stay where they were — the records a held batch
    ///    kept back stay unseen, their ops answered "outcome unknown".
    ///
    /// WHY POISON, and not a resend that dedups to its original and a later flush that
    /// covers it. A later flush that SUCCEEDS says nothing about the records the failed
    /// one was to cover. On Apple the `F_BARRIERFSYNC` before it has already sent their
    /// pages to the drive and left them clean in the page cache; if the `F_FULLFSYNC`
    /// failed because the drive dropped its cache (a reset), the next one has nothing
    /// dirty to rewrite and returns 0. On Linux a failed `fdatasync` marks the pages it
    /// failed to write clean and reports the error once, so the retry returns 0 without
    /// writing them — PostgreSQL's "fsyncgate", which it answered with a PANIC. Taking
    /// that 0 as covering them would show the escalated records, mark them durable in
    /// every `Mark`, and ack the resend that dedups to one — records that may be on no
    /// media, gone at the next power loss under an identity a reader trusted. Nor may the
    /// log append past them: a power loss would leave a hole behind later sealed
    /// batches, which recovery refuses (it is not a torn tail), so every ack written
    /// after the failure — a latched Strict batch's included — would be lost to the
    /// repair that cuts at the hole. Refusing every write until a reopen keeps the
    /// uncertain records the log's tail: after a power loss recovery cuts them as a torn
    /// tail above the floor, and the `Lossy` record moves the generation at the next open
    /// whatever the boot, so a reader re-bases instead of trusting offsets the failed
    /// flush may have taken. HONEST BOUNDARY: a reopen in the same boot reads back from
    /// the page cache whatever the failed flush left clean but unwritten, and its own
    /// flush has nothing dirty to rewrite either; its floor then covers those bytes, and a
    /// later power loss that takes them is REFUSED at the open after it (damage below the
    /// floor) — a refusal and `asb repair`, never a silent loss.
    fn flush_failed_lossy(&mut self, e: &io::Error) {
        if self.identity.state != logid::State::Lossy {
            let lossy = self.identity.lossy();
            match logid::write(&self.dir, &lossy) {
                Ok(()) => self.identity = lossy,
                Err(w) => eprintln!(
                    "astream-broker: {}: could not record a failed flush in its identity \
                     sidecar ({w}); the next open in another boot still moves its generation",
                    self.path.display()
                ),
            }
        }
        if self.served.tier != crate::proto::Tier::Strict {
            self.served = Served {
                tier: crate::proto::Tier::Strict,
                reason: crate::proto::ServedReason::LatchedAfterFlushFailure,
            };
        }
        if self.poisoned.is_none() {
            eprintln!(
                "astream-broker: {}: a flush failed ({e}) while records no full flush covers \
                 were acked or held; they may be lost, so the log refuses every write until it \
                 is reopened, and that open moves its generation",
                self.path.display()
            );
            self.poisoned = Some(format!(
                "a flush failed ({e}) with records no full flush covers: the device may have \
                 dropped them, and nothing appended after them could be promised"
            ));
        }
    }

    /// Serve Strict from now on, saying why once.
    fn latch_strict(&mut self, reason: crate::proto::ServedReason, why: &str) {
        if self.served.tier == crate::proto::Tier::Strict {
            return;
        }
        eprintln!(
            "astream-broker: {}: {why}; it serves Strict for the rest of this open (every \
             batch flushed in full before its ack)",
            self.path.display()
        );
        self.served = Served {
            tier: crate::proto::Tier::Strict,
            reason,
        };
    }

    /// The flush a commit owes before its ack, by the tier served.
    const fn flush_owed(&self) -> Flush {
        match self.served.tier {
            crate::proto::Tier::Ordered => Flush::Barrier,
            crate::proto::Tier::Relaxed => Flush::Nothing,
            _ => Flush::Full,
        }
    }

    /// Issue `flush` after a write made under the lock (the un-batched append), counting
    /// it (a failure too, in [`FlushCounts::failed`]); on a barrier the device refuses
    /// ([`astream_sys::barrier_refused`]) the log latches to Strict and the write is
    /// flushed in full instead, before its ack. `Ok(true)` when the write is now
    /// DURABLE (a full flush covered it). The same device work a batch does with the
    /// lock released ([`issue_flush`]), settled at once ([`settle_issued`]).
    ///
    /// [`settle_issued`]: Self::settle_issued
    fn issue(&mut self, flush: Flush) -> io::Result<bool> {
        let mut issued = Issued::default();
        let res = issue_flush(
            &self.file.batch_out(),
            flush,
            self.faults.sync,
            self.refuses_barrier(),
            &mut issued,
        );
        self.settle_issued(issued);
        res
    }

    /// Count what a flush did, and latch the log to Strict if the device refused its
    /// barrier ([`Issued`]): the log's own state, so under its lock, whether the flush
    /// itself ran under it ([`issue`](Self::issue)) or without it
    /// ([`commit_end`](Self::commit_end), after [`BatchWrite::run`]).
    fn settle_issued(&mut self, issued: Issued) {
        let Issued {
            full,
            barrier,
            failed,
            refused,
        } = issued;
        if let Some(why) = refused {
            self.latch_strict(
                crate::proto::ServedReason::LatchedAfterBarrierError,
                &format!("the device refused a barrier ({why})"),
            );
        }
        self.counts.full = self.counts.full.saturating_add(u64::from(full));
        self.counts.barrier = self.counts.barrier.saturating_add(u64::from(barrier));
        self.counts.failed = self.counts.failed.saturating_add(u64::from(failed));
    }

    /// The test seam standing in for a device that refuses the barrier while serving
    /// (`refuse_barrier`); never outside a test.
    const fn refuses_barrier(&self) -> bool {
        #[cfg(test)]
        let refused = self.refuse_barrier;
        #[cfg(not(test))]
        let refused = false;
        refused
    }

    /// The commit's answer when nothing more is written: what readers may see, and
    /// whether a full flush is owed (see [`Committed`]).
    fn committed_now(&mut self) -> Committed {
        let mut held = Vec::new();
        for o in std::mem::take(&mut self.staged_held) {
            if o >= self.durable_head {
                held.push(o);
            }
        }
        let owe = (self.held_from.is_some() || !held.is_empty()) && self.durable_head < self.next.0;
        Committed {
            visible_now: Offset(self.held_from.unwrap_or(self.next.0)),
            owe_full: owe.then_some(self.next),
            held,
        }
    }

    /// Whether a record on `subject` by `producer_id`, carrying a commit of `group`,
    /// is escalated ([`Escalation`](crate::Escalation)): its producer's principal is the
    /// binding in force, a bind staged earlier in this batch included.
    fn escalates(&self, subject: &str, producer_id: u64, group: Option<&str>) -> bool {
        if self.rules.is_empty() {
            return false;
        }
        let principal = match self.binding(producer_id) {
            Some((p, _)) => Some(p),
            None => None,
        };
        self.rules.matches(subject, principal, group)
    }

    /// The un-batched path's [`note_dedup`](Self::note_dedup): a RESENT op answered from
    /// dedup with `original`, when the op is escalated and no full flush covers its
    /// original yet, is flushed in full before the call returns ([`sync_full`]) — the
    /// promise an escalated record keeps on this path (`append_frame`), which a resend
    /// keeps too: through the broker the same resend's ack waits for the flush its batch
    /// owes. Without it, an ordinary original resent on an escalated subject would be
    /// answered "landed" while a power loss can still take it.
    ///
    /// [`sync_full`]: Self::sync_full
    fn cover_resent(
        &mut self,
        original: Offset,
        subject: &str,
        producer_id: u64,
        group: Option<&str>,
    ) -> io::Result<()> {
        if original.0 >= self.durable_head && self.escalates(subject, producer_id, group) {
            self.sync_full()?;
        }
        Ok(())
    }

    /// A RESENT op answered from dedup with `original`: when the op is escalated and no
    /// full flush covers its original yet, its ack waits for one (the batch owes it even
    /// with nothing staged).
    fn note_dedup(
        &mut self,
        original: Offset,
        subject: &str,
        producer_id: u64,
        group: Option<&str>,
    ) {
        if original.0 >= self.durable_head && self.escalates(subject, producer_id, group) {
            self.staged_held.push(original.0);
        }
    }

    /// Release the exclusive file lock early (the broker calls this at shutdown, once
    /// its writer has exited, so a successor can open the log while connection threads
    /// that still hold the old log's `Arc` are being reaped). Dropping the log also
    /// releases it.
    pub(crate) fn unlock(&self) {
        self.file.unlock();
    }

    /// Arm (or clear) the [`InjectedFaults`] seam. See its docs — a test hook, inert
    /// by default.
    pub const fn inject_faults(&mut self, faults: InjectedFaults) {
        self.faults = faults;
    }

    /// `Some(reason)` once the log refuses every further mutation until it is reopened
    /// (see the `poisoned` field): a failed batch whose rollback could not put the file
    /// back, a full flush that failed with acked or held records no full flush covers
    /// ([`InjectedFaults::sync`]), or an anti-rollback watermark that could not follow
    /// promoted records. Reopen to recover. Every write the broker's writer refuses
    /// meanwhile says why in a message that starts
    /// [`LOG_POISONED_PREFIX`](crate::proto::LOG_POISONED_PREFIX)
    /// (`poisoned_refusal`).
    pub fn poisoned(&self) -> Option<&str> {
        self.poisoned.as_deref()
    }

    /// Whether this log is a REPLICA — a replication TARGET whose contents are a
    /// leader's spine rather than this broker's own. True from the moment an EMPTY log
    /// accepts its first replicated record (or [`declare_replica`](Self::declare_replica)
    /// is called), and durable from then on: the `<log>.replica` marker beside the file
    /// is read on every open, and removing that file is the only undo.
    ///
    /// The broker asks this before doing anything on its OWN initiative to the log —
    /// re-firing the wills it holds at open, above all. A replica that fired the
    /// leader's wills locally would append a record the leader does not have, which
    /// permanently diverges it and fences its link.
    pub const fn is_replica(&self) -> bool {
        self.replica
    }

    /// WHICH log this is, and which incarnation of its offsets — settled (and made
    /// durable in `<log>.id`) by the open, or adopted from a leader by a replica. A
    /// client that persisted an offset under any other identity must not apply it
    /// here: see [`LogIdentity`](crate::LogIdentity).
    pub const fn identity(&self) -> crate::LogIdentity {
        self.identity.identity
    }

    /// Take `leader` as this log's identity, durably — what a REPLICA does with the
    /// identity its leader sends beside each replicated record, so a client that
    /// fails over to it keeps its offsets. `through` is the highest offset of the
    /// committed replicated records that carried it.
    ///
    /// Taken only when those records reach this log's NEWEST record: a leader ships
    /// in order from its confirmed prefix, so every record below `through` was
    /// appended or compared byte for byte first, and the whole log is the leader's.
    /// An echo of a shared PREFIX vouches for nothing above it — a follower that
    /// kept records its leader has since cut (and renumbered as a new generation)
    /// matches the leader below the cut and not above, and would otherwise tell a
    /// client that fails over that the leader's offsets name its own records. And
    /// never in an open that refused a replicated record for holding a different one
    /// at its offset.
    ///
    /// `Ok(false)`, changing nothing, on a log that is not a replica (its records are
    /// its own, so its identity is too), one that already carries `leader`, or one
    /// those two rules leave on its own identity.
    pub(crate) fn adopt_identity(
        &mut self,
        leader: crate::LogIdentity,
        through: Offset,
    ) -> io::Result<bool> {
        if !self.replica
            || self.diverged
            || through.0.checked_add(1) != Some(self.next.0)
            || self.identity.identity == leader
        {
            return Ok(false);
        }
        self.identity = logid::adopt(&self.dir, &self.identity, leader)?;
        Ok(true)
    }

    /// Declare this log a REPLICA — for an operator standing a follower up on a log
    /// seeded by other means (a copy of the leader's). Writes the durable
    /// `<log>.replica` marker; idempotent, and says so on stderr the first time.
    ///
    /// A log that takes its first replicated record while it is still EMPTY declares
    /// itself, so a follower brought up on a fresh log never needs this. A log that
    /// already holds records does NOT: taking a replicated record beside them says
    /// nothing about whose log it is, so a seeded follower has to be declared, and this
    /// (or [`Broker::open_replica`](crate::Broker::open_replica)) is how.
    ///
    /// There is no undeclare. The declaration IS the `<log>.replica` file: remove it
    /// and the next open reads a log this broker owns again.
    pub fn declare_replica(&mut self) -> io::Result<()> {
        self.mark_replica()
    }

    /// Write the durable replica marker (once). Any I/O error is the caller's to
    /// handle: a replicated record whose marker could not be written is REFUSED,
    /// because accepting it would leave a log that is a replica in fact and not in
    /// evidence — the exact state whose next restart forges a `gone`.
    ///
    /// The first write says so on stderr. This is a one-way, durable change to what the
    /// broker will do with the log — no will ever fires here again — so it is not a
    /// thing to happen silently; the line names the marker file, which is also how an
    /// operator undoes a declaration made in error (`rm <log>.replica`).
    fn mark_replica(&mut self) -> io::Result<()> {
        if self.replica {
            return Ok(());
        }
        let marker = replica_marker_path(&self.path);
        let mut f = create_fresh(&marker, &mut FsOpenOptions::new())?;
        f.write_all(REPLICA_MARKER_NOTE)?;
        f.sync_all()?;
        sync_parent_dir(&marker)?;
        self.replica = true;
        eprintln!(
            "astream-broker: log {} is now a REPLICATION TARGET. Its records are a \
             leader's spine; this broker appends nothing of its own to it and fires \
             NONE of the wills it holds, on this open or any later one. Marked by {}; \
             remove that file to undo a declaration made in error.",
            self.path.display(),
            marker.display()
        );
        Ok(())
    }

    /// The next offset to be assigned (== current head == record count).
    pub const fn head(&self) -> Offset {
        self.next
    }

    /// Current leader replication epoch (`Astream.Broker.TlaEpochWatermarkReplication`).
    pub const fn replication_epoch(&self) -> u64 {
        self.replication_epoch
    }

    /// Advance the replication epoch monotonically (`req_epoch >= self.replication_epoch`),
    /// fencing stale leader epochs via [`crate::proto::epoch_is_fenced`] and stepping
    /// via the Clean-origin [`crate::proto::epoch_advance_monotone`].
    pub fn advance_replication_epoch(&mut self, req_epoch: u64) -> io::Result<u64> {
        self.check_poisoned()?;
        if crate::proto::epoch_is_fenced(self.replication_epoch, req_epoch) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "replication epoch {req_epoch} is fenced by current epoch {}",
                    self.replication_epoch
                ),
            ));
        }
        self.replication_epoch =
            crate::proto::epoch_advance_monotone(self.replication_epoch, req_epoch);
        Ok(self.replication_epoch)
    }

    /// Record a follower/quorum-acknowledged offset, advancing `replica_ack_upto`
    /// monotonically via Clean-origin [`crate::proto::replica_ack_monotone`] and
    /// returning the updated [`committed_watermark_offset`](Self::committed_watermark_offset).
    pub const fn record_replica_ack(&mut self, ack_upto: Offset) -> Offset {
        self.replica_ack_upto = Offset(crate::proto::replica_ack_monotone(
            self.replica_ack_upto.0,
            ack_upto.0,
        ));
        self.committed_watermark_offset()
    }

    /// Record an epoch-tagged follower acknowledgement (`req_epoch`, `ack_upto`) via
    /// the 5-parameter Clean-origin [`crate::proto::fenced_replica_watermark`] kernel:
    /// if `req_epoch < self.replication_epoch`, `ack_upto` is fenced and ignored;
    /// otherwise `replica_ack_upto` advances monotonically and the clamped
    /// `min(head, replica_ack_upto)` watermark is returned.
    pub const fn record_fenced_replica_ack(&mut self, req_epoch: u64, ack_upto: Offset) -> Offset {
        let wm = crate::proto::fenced_replica_watermark(
            self.replication_epoch,
            req_epoch,
            self.next.0,
            self.replica_ack_upto.0,
            ack_upto.0,
        );
        if !crate::proto::epoch_is_fenced(self.replication_epoch, req_epoch) {
            self.replica_ack_upto = Offset(crate::proto::replica_ack_monotone(
                self.replica_ack_upto.0,
                ack_upto.0,
            ));
        }
        Offset(wm)
    }

    /// The quorum-committed watermark `min(head, replica_ack_upto)`, computed directly
    /// via the Clean-origin [`crate::proto::committed_watermark`] function from
    /// `Astream.Broker.TlaEpochWatermarkReplication`.
    ///
    /// Satisfies the machine-checked invariant `ReplicationSafetyInv`:
    /// `committed_watermark <= head && committed_watermark <= replica_ack_upto`.
    pub const fn committed_watermark_offset(&self) -> Offset {
        Offset(crate::proto::committed_watermark(
            self.next.0,
            self.replica_ack_upto.0,
        ))
    }

    /// Whether `offset` is strictly below [`committed_watermark_offset`](Self::committed_watermark_offset),
    /// checked via the `#[ensures]`-verified [`crate::proto::offset_is_committed`] guard.
    pub const fn is_offset_replicated_committed(&self, offset: Offset) -> bool {
        crate::proto::offset_is_committed(self.committed_watermark_offset().0, offset.0)
    }

    /// Bounded read restricted to records below [`committed_watermark_offset`](Self::committed_watermark_offset)
    /// (`Astream.Broker.TlaEpochWatermarkReplication`): never surfaces a record that has
    /// not yet been acknowledged by the replica watermark.
    pub fn fetch_replicated_committed(
        &self,
        from: u64,
        filter: &Filter,
        max: usize,
        scan_max: usize,
    ) -> (Vec<Arc<BrokerRecord>>, u64) {
        self.fetch(
            from,
            filter,
            max,
            scan_max,
            self.committed_watermark_offset().0,
        )
    }

    /// Append a replicated record gated on BOTH leader epoch fencing
    /// (`!epoch_is_fenced(self.replication_epoch, req_epoch)`) and gapless offset
    /// preconditions (`rec.seq.0 == expected_offset`), corresponding to `ReplicateStep`
    /// in `Astream.Broker.TlaEpochWatermarkReplication`.
    pub fn replicate_fenced(
        &mut self,
        req_epoch: u64,
        expected_offset: u64,
        rec: BrokerRecord,
    ) -> io::Result<(Offset, bool)> {
        self.check_poisoned()?;
        if !self.staged.is_empty() {
            return Err(io::Error::other(
                "replicate_fenced: a batch is staged; commit or abort it first",
            ));
        }
        if crate::proto::epoch_is_fenced(self.replication_epoch, req_epoch) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "stale leader epoch {req_epoch}: fenced by current epoch {}",
                    self.replication_epoch
                ),
            ));
        }
        if rec.seq.0 != expected_offset {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "replica offset mismatch: expected {expected_offset}, record carries {}",
                    rec.seq.0
                ),
            ));
        }
        self.replication_epoch =
            crate::proto::epoch_advance_monotone(self.replication_epoch, req_epoch);
        let (off, deduped) = match self.stage_replica(rec) {
            Ok(v) => v,
            Err(StageErr::Diverged(msg)) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, msg))
            }
            Err(StageErr::ReservedReplica(msg)) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, msg))
            }
            Err(StageErr::TooLarge) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "message too large",
                ))
            }
            Err(StageErr::Io(e)) => return Err(e),
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "replica refused",
                ))
            }
        };
        if !deduped {
            self.commit_batch()?;
        }
        Ok((off, deduped))
    }

    /// The tier this log was opened at.
    pub const fn durability(&self) -> Durability {
        self.durability
    }

    /// Operational snapshot of this log's retained offsets, byte size, and index
    /// cardinalities (`REQUIREMENTS` R5).
    pub fn stats(&self) -> LogStats {
        let mut pending_wills = 0usize;
        for (_, w) in self.latest_wills() {
            let fenced = match self.high_water.get(&w.producer_id) {
                Some(&hw) => crate::proto::will_is_fenced(hw, w.producer_seq),
                None => false,
            };
            if !self.dedup.contains_key(&(w.producer_id, w.producer_seq)) && !fenced {
                pending_wills = pending_wills.saturating_add(1);
            }
        }
        LogStats {
            base: self.base,
            head: self.next,
            retained_records: self.records.len(),
            retained_bytes: self.durable_len as usize,
            distinct_subjects: self.last.len(),
            bound_producers: self.bind.len(),
            pending_wills,
            group_commits: self.group_commits.len(),
            durability: self.durability,
            is_replica: self.replica,
            poisoned: self.poisoned.is_some(),
        }
    }

    /// Publish a message with exactly-once ingest (the un-batched path: one append,
    /// one fsync at Strict, none at Relaxed). If `(producer_id, producer_seq)` was
    /// already appended, returns `(original_offset, true)` and appends nothing;
    /// otherwise appends at the tier's guarantee and returns `(new_offset, false)`. A
    /// hidden subject is refused (`InvalidInput`), and so is a record that would take
    /// this producer past [`MAX_SUBJECTS_PER_PRODUCER`] distinct subjects — the same
    /// bound the staged path applies, decided in the same place, so the invariant holds
    /// for the TYPE and not only for the wire.
    pub fn publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    ) -> io::Result<(Offset, bool)> {
        self.check_poisoned()?;
        if is_hidden_subject(&subject) {
            return Err(reserved_err(&subject));
        }
        if let Some(&off) = self.dedup.get(&(producer_id, producer_seq)) {
            self.cover_resent(off, &subject, producer_id, None)?;
            return Ok((off, true));
        }
        let seq = self.next;
        let rec = BrokerRecord {
            seq,
            producer_id,
            producer_seq,
            subject,
            body,
            commit: None,
        };
        if self.subject_bound_exceeded(&rec, 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                subject_bound_msg(producer_id),
            ));
        }
        self.append_record(rec)?; // Strict: on disk when this returns Ok; Relaxed: page cache
        self.dedup.insert((producer_id, producer_seq), seq);
        Ok((seq, false))
    }

    /// The offset a consumer group resumes from: `committed_upto + 1`, or
    /// `Offset::ZERO` if the group has never committed — counting no commit a held batch
    /// carries: one takes effect when the full flush that releases it succeeds (see
    /// `held_commits`). It is what the tier promises of the commit record: on Strict
    /// durable, on Ordered and Relaxed durable when the escalation table names the group
    /// (a `Group` rule), otherwise ordered or in the page cache. (The Replicated tier's
    /// quorum gate is the broker's, not the log's: there a commit counts once the leader
    /// has committed it, before a quorum may hold it.)
    pub fn group_start(&self, group: &str) -> Offset {
        match self.group_commits.get(group) {
            Some(&upto) => Offset(upto.saturating_add(1)),
            None => Offset::ZERO,
        }
    }

    /// Take a promoted group commit into `group_commits` (monotone: an older `upto`
    /// never regresses the group).
    fn apply_group_commit(&mut self, group: &str, upto: u64) {
        let e = self.group_commits.entry(group.to_string()).or_insert(0);
        *e = crate::proto::group_commit_monotone(*e, upto);
    }

    /// The held head is released: the group commits it kept back take effect, in log
    /// order.
    fn release_held_commits(&mut self) {
        for (group, upto) in std::mem::take(&mut self.held_commits) {
            self.apply_group_commit(&group, upto);
        }
    }

    /// Advance a consumer group's committed offset (a pure, no-output commit) at the
    /// tier's guarantee. Idempotent + monotone: re-committing an old offset never
    /// regresses the cursor. Appends a commit record on the reserved subject.
    pub fn commit(&mut self, group: String, upto: u64) -> io::Result<Offset> {
        self.check_poisoned()?;
        let seq = self.next;
        let rec = BrokerRecord {
            seq,
            producer_id: 0,
            producer_seq: 0,
            subject: COMMIT_SUBJECT.to_string(),
            body: Vec::new(),
            commit: Some(GroupCommit { group, upto }),
        };
        self.append_record(rec)?;
        Ok(seq)
    }

    /// The read-process-write TRANSACTION: append `out_body` to `out_subject` AND
    /// commit `group` to `upto` in ONE record (one append, one fsync at Strict) —
    /// exactly-once processing. Deduped by `(producer_id, producer_seq)`: a retried
    /// transaction (e.g. after a crash before the ack) is NOT re-appended and the
    /// commit is not re-applied; it returns the original offset. So the output appears
    /// exactly once and the offset is committed exactly once, even across a crash
    /// mid-transaction. A hidden subject is refused as an output, and so is one that
    /// would take this producer past [`MAX_SUBJECTS_PER_PRODUCER`] distinct subjects.
    pub fn process_and_produce(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        out_subject: String,
        out_body: Vec<u8>,
        group: String,
        upto: u64,
    ) -> io::Result<(Offset, bool)> {
        self.check_poisoned()?;
        if is_hidden_subject(&out_subject) {
            return Err(reserved_err(&out_subject));
        }
        if let Some(&off) = self.dedup.get(&(producer_id, producer_seq)) {
            // The commit was already applied at first append.
            self.cover_resent(off, &out_subject, producer_id, Some(&group))?;
            return Ok((off, true));
        }
        let seq = self.next;
        let rec = BrokerRecord {
            seq,
            producer_id,
            producer_seq,
            subject: out_subject,
            body: out_body,
            commit: Some(GroupCommit { group, upto }),
        };
        if self.subject_bound_exceeded(&rec, 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                subject_bound_msg(producer_id),
            ));
        }
        self.append_record(rec)?;
        self.dedup.insert((producer_id, producer_seq), seq);
        Ok((seq, false))
    }

    // ---- group commit: stage many records, then ONE fsync covers the batch ----
    //
    // The writer thread stages each queued op (encoding its bytes into the batch
    // buffer), then commits them all with one write and one flush — `commit_batch`, which
    // the broker runs as its three halves with the log lock released around the write and
    // the flush (`commit_begin`, `BatchWrite::run`, `commit_end`). Durability is
    // unchanged: a staged record is NOT acked, NOT in `dedup`, and NOT visible to
    // subscribers (head does not advance) until the batch's flush has returned. So
    // `ack ⟹ fsync'd` still holds under Strict — the fsync is merely amortized over the
    // batch instead of per message.

    fn check_poisoned(&self) -> io::Result<()> {
        // The one gate every mutation passes, so it is also where a batch in flight
        // outside the lock is respected: until `commit_end` settles it, the file's end
        // and the staged batch belong to that write.
        if self.writing {
            return Err(io::Error::other(
                "a batch is being written outside the log lock; mutate once it commits",
            ));
        }
        if self.closed {
            return Err(io::Error::other(
                "the log was closed cleanly (close_clean); reopen it to write",
            ));
        }
        match &self.poisoned {
            Some(reason) => Err(io::Error::other(poisoned_refusal(reason))),
            None => Ok(()),
        }
    }

    fn check_poisoned_stage(&self) -> Result<(), StageErr> {
        match self.check_poisoned() {
            Ok(()) => Ok(()),
            Err(e) => Err(StageErr::Io(e)),
        }
    }

    /// The offset the next staged record would take (`next + staged.len()`), or `None`
    /// on offset overflow.
    fn next_staged_seq(&self) -> Option<Offset> {
        match self.next.0.checked_add(self.staged.len() as u64) {
            Some(off) => Some(Offset(off)),
            None => None,
        }
    }

    /// The offset a staged-or-durable duplicate of `(producer_id, producer_seq)` maps
    /// to, checking both the durable dedup map and the in-flight batch. (A key is
    /// never in both: staging consults both first, and a commit clears the staged view.)
    fn staged_or_durable_dup(&self, key: (u64, u64)) -> Option<Offset> {
        if let Some(&off) = self.dedup.get(&key) {
            Some(off)
        } else {
            self.staged_dedup.get(&key).copied()
        }
    }

    /// Whether `rec` would create a NEW distinct subject for its producer beyond
    /// [`MAX_SUBJECTS_PER_PRODUCER`], counting the durable index, the subjects that
    /// producer's registered wills have RESERVED (`will_reserved`), and `pending` (the
    /// in-flight batch's own not-yet-promoted new subjects for that producer). The ONE
    /// place the bound is decided, so the staged path and the un-batched
    /// [`publish`](Self::publish) / [`process_and_produce`](Self::process_and_produce)
    /// cannot disagree about it.
    fn subject_bound_exceeded(&self, rec: &BrokerRecord, pending: usize) -> bool {
        if is_hidden_subject(&rec.subject) || self.last.contains_key(&rec.subject) {
            return false;
        }
        // A record on a subject this producer has ALREADY reserved consumes that
        // reservation rather than adding an entry beside it, so it is not counted
        // against itself: a producer at the cap can still publish (and still fire a
        // will) on a subject its own will spoke for.
        let held = match self.will_reserved.get(&rec.producer_id) {
            Some(held_set) => held_set
                .len()
                .saturating_sub(usize::from(held_set.contains(&rec.subject))),
            None => 0,
        };
        let already = pending.saturating_add(held).saturating_add(
            self.subjects_per_producer
                .get(&rec.producer_id)
                .copied()
                .unwrap_or(0),
        );
        already >= MAX_SUBJECTS_PER_PRODUCER
    }

    /// Stage a publish into the current batch (exactly-once ingest). Mirrors
    /// [`publish`](Self::publish) but defers the fsync to [`commit_batch`].
    pub(crate) fn stage_publish(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
    ) -> Result<(Offset, bool), StageErr> {
        self.stage_publish_bounded(producer_id, producer_seq, subject, body, true)
    }

    /// [`stage_publish`](Self::stage_publish) with the distinct-subject bound made
    /// explicit. `enforce_bound: false` is for the ONE record whose subject was already
    /// checked, and answered for, at an earlier verb: a will FIRING (see
    /// [`stage_will_fire`](Self::stage_will_fire)).
    fn stage_publish_bounded(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        subject: String,
        body: Vec<u8>,
        enforce_bound: bool,
    ) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        if is_hidden_subject(&subject) {
            return Err(StageErr::Reserved(subject));
        }
        if let Some(off) = self.staged_or_durable_dup((producer_id, producer_seq)) {
            self.note_dedup(off, &subject, producer_id, None);
            return Ok((off, true));
        }
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        self.stage_record_bounded(
            BrokerRecord {
                seq,
                producer_id,
                producer_seq,
                subject,
                body,
                commit: None,
            },
            enforce_bound,
        )?;
        Ok((seq, false))
    }

    /// Stage a pure consumer-group commit into the current batch (mirrors
    /// [`commit`](Self::commit) but deferred to [`commit_batch`]).
    pub(crate) fn stage_commit(&mut self, group: String, upto: u64) -> Result<Offset, StageErr> {
        self.check_poisoned_stage()?;
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        self.stage_record(BrokerRecord {
            seq,
            producer_id: 0,
            producer_seq: 0,
            subject: COMMIT_SUBJECT.to_string(),
            body: Vec::new(),
            commit: Some(GroupCommit { group, upto }),
        })?;
        Ok(seq)
    }

    /// Stage an atomic read-process-write transaction into the current batch (mirrors
    /// [`process_and_produce`](Self::process_and_produce) but deferred).
    pub(crate) fn stage_process_and_produce(
        &mut self,
        producer_id: u64,
        producer_seq: u64,
        out_subject: String,
        out_body: Vec<u8>,
        group: String,
        upto: u64,
    ) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        if is_hidden_subject(&out_subject) {
            return Err(StageErr::Reserved(out_subject));
        }
        if let Some(off) = self.staged_or_durable_dup((producer_id, producer_seq)) {
            self.note_dedup(off, &out_subject, producer_id, Some(&group));
            return Ok((off, true));
        }
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        self.stage_record(BrokerRecord {
            seq,
            producer_id,
            producer_seq,
            subject: out_subject,
            body: out_body,
            commit: Some(GroupCommit { group, upto }),
        })?;
        Ok((seq, false))
    }

    /// Stage a record REPLICATED from a leader, at the leader's offset: the follower's
    /// log must be an identical prefix of the leader's, so the record either extends
    /// this spine exactly (`rec.seq == next staged seq` → staged, `(seq, false)`), or
    /// is already held here byte-for-byte (`seq` below the head → nothing staged,
    /// `(seq, true)` — idempotent re-shipping), or is refused with
    /// [`StageErr::Diverged`] naming the offset (a gap, or a DIFFERENT record already at
    /// that offset — never silently overwritten or skipped).
    pub(crate) fn stage_replica(&mut self, rec: BrokerRecord) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        // WHOSE LOG IS THIS — decided here, in the writer, in the same critical section
        // that appends the record, because that is the only place the answer cannot
        // change under the decision. The connection thread asks the same question
        // before it queues the op, but it reads the PROMOTED head and then drops the
        // lock: between that read and this staging, this connection's own pipelined
        // records are staged but not promoted, so `head()` still says 0 on a log that
        // is about to hold records of its own. A hidden record admitted on the strength
        // of "still empty" and staged onto an OWNED log is the injection this rule
        // exists to refuse — the broker itself fires that `/a/will` at its next open,
        // as an arbitrary publish to an arbitrary subject under an arbitrary producer
        // id, outside the capability matrix; an `/a/bind` locks a victim principal out
        // of attaching for good.
        //
        // The admission test and the marking are ONE predicate (`declares`),
        // evaluated once, here: a hidden record lands only on a log that is already a
        // replica or is still empty at STAGE time — which is exactly the log that this
        // record declares a replica.
        //
        // A PURE COMMIT is exempt, as it is on the connection thread: it is a leader's
        // own `/a/commit` record, it carries no producer key and no body, and a client
        // that can reach this verb can advance a group cursor with `Commit` anyway.
        let declares = self.next.0 == 0 && self.staged.is_empty();
        if is_hidden_subject(&rec.subject) && !is_pure_commit(&rec) && !(self.replica || declares) {
            return Err(StageErr::ReservedReplica(replicated_reserved_msg(
                &rec.subject,
            )));
        }
        let next = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        if rec.seq.0 < next.0 {
            // Records sit at absolute offsets base..next, indexed base-relative. A
            // record below the retained base was pruned here — we can neither compare
            // nor accept it.
            if rec.seq.0 < self.base.0 {
                return Err(StageErr::Diverged(format!(
                    "replica offset {} predates the retained base {}",
                    rec.seq.0, self.base.0
                )));
            }
            let held: Option<&BrokerRecord> = if rec.seq.0 < self.next.0 {
                let Ok(idx) = usize::try_from(rec.seq.0.saturating_sub(self.base.0)) else {
                    return Err(StageErr::TooLarge);
                };
                match self.records.get(idx) {
                    Some(arc) => Some(arc.as_ref()),
                    None => None,
                }
            } else {
                let Ok(idx) = usize::try_from(rec.seq.0.saturating_sub(self.next.0)) else {
                    return Err(StageErr::TooLarge);
                };
                self.staged.get(idx)
            };
            if held == Some(&rec) {
                // Already held, byte for byte: idempotent re-shipping, and NOTHING is
                // appended. It is also nothing to infer ownership from — this branch
                // is reachable for any record already on the log, so a reader that
                // fetches record 0 and echoes it back reaches it too; marking here
                // would let one such echo, appending not a single byte, disable
                // will-firing for every producer on the broker for good.
                return Ok((rec.seq, true));
            }
            self.diverged = true;
            return Err(StageErr::Diverged(format!(
                "replica diverged at offset {}: a different record is already held there",
                rec.seq.0
            )));
        }
        if rec.seq.0 > next.0 {
            return Err(StageErr::Diverged(format!(
                "replica gap: this log's next offset is {}, the record is {}",
                next.0, rec.seq.0
            )));
        }
        // A log that takes its first replicated record while it is STILL EMPTY is a
        // replica, durably and from here on: it has nothing of its own, so the whole of
        // what it holds is the leader's spine, and this broker appends nothing of its
        // own to it ever again. A marker that cannot be written REFUSES the record — a
        // log that is a replica in fact but not in evidence is exactly the one whose
        // next restart forges its leader's wills.
        //
        // A log that already holds records is NOT converted here. Accepting a record at
        // this log's own next offset says nothing about whose log it is, and the
        // conversion is not a small thing: it disables will-firing for every producer
        // on this broker, for good, with the `<log>.replica` file as the only trace. It
        // may not be inferred from a frame any reader can construct — an ordinary
        // Publish-shaped `Replicate` at the head, or a byte-identical echo of a record
        // already held, which appends nothing at all. A follower seeded from a COPY of
        // the leader's log is DECLARED by its operator
        // ([`declare_replica`](Self::declare_replica), `Broker::open_replica`); that is
        // what the declaration is for, and it is the only thing that can tell a seeded
        // follower from a broker that owns its log.
        //
        // STAGED FIRST, MARKED SECOND. `mark_replica` is one-way, durable and fsynced,
        // and it disables will-firing for every producer on this broker for good, so it
        // may not be written for a record the log does not take: staging can still
        // refuse this record (`TooLarge` — a request frame is accepted at a size whose
        // record payload is not), and a refused record is not one the log "takes". The
        // invariant that a marker which cannot be WRITTEN refuses the record still
        // holds the other way round: a marker I/O error is `StageErr::Io`, which
        // poisons the batch, and `process_batch` then aborts it — this record and every
        // byte of it roll back with it.
        //
        // EXEMPT from the distinct-subject bound: that is the leader's admission
        // policy, already applied to this record, and a follower's own count can
        // differ from the leader's (will reservations are rebuilt only when a log
        // reopens). Refusing here would stall the follower on a committed record.
        let seq = rec.seq;
        self.stage_record_bounded(rec, false)?;
        if declares {
            if let Err(e) = self.mark_replica() {
                return Err(StageErr::Io(e));
            }
        }
        Ok((seq, false))
    }

    /// REGISTER a last will: append the hidden `/a/will` record that makes it durable.
    /// The stored record is strictly LARGER than the record the will would publish, so
    /// a registration that fits guarantees the firing will fit too.
    ///
    /// The DISTINCT-SUBJECT BOUND is checked here, against the subject the will would
    /// publish to — not against `/a/will`, which is hidden and outside the bound
    /// entirely. A registration is a promise that the goodbye will be delivered, and a
    /// will whose subject would be its producer's 4097th distinct one cannot be: the
    /// firing is fire-and-forget, so its refusal reaches nobody, and the `/a/will`
    /// record stays on the log for every later open to re-attempt and re-fail. Refusing
    /// at REGISTRATION is the only point where a client is still listening.
    ///
    /// A registration that passes also RESERVES that subject against the producer's
    /// budget (`will_reserved`, promoted when the batch commits and rebuilt on open),
    /// so the next registration sees it — the firings are exempt from the bound, and
    /// the reservation is what keeps them from overshooting it.
    pub(crate) fn stage_will_register(&mut self, will: &WillRecord) -> Result<Offset, StageErr> {
        self.check_poisoned_stage()?;
        // THE FENCE IS WHAT MAKES A STALE GOODBYE HARMLESS, and it does not reach into
        // the reserved ack half ([`ACK_SEQ_BASE`]): a sequence there is derived from an
        // offset, not from an incarnation, so it never enters the high water. A will
        // registered in that half could therefore never be fenced — the producer could
        // come back, republish `state=live`, and the old goodbye would still fire over
        // it. Refused HERE, at registration, which is the only point where the client
        // is listening (a firing's refusal reaches nobody), so the half being reserved
        // is a fact the log enforces rather than a convention it assumes.
        if will.producer_seq >= ACK_SEQ_BASE {
            return Err(StageErr::SeqReserved(format!(
                "will for producer {} at seq {}: sequences at or above {ACK_SEQ_BASE} are \
                 reserved for acks and lie outside the will fence, so a will registered \
                 there could never be fenced by a later record — register it below",
                will.producer_id, will.producer_seq
            )));
        }
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        let probe = BrokerRecord {
            seq,
            producer_id: will.producer_id,
            producer_seq: will.producer_seq,
            subject: will.subject.clone(),
            body: Vec::new(),
            commit: None,
        };
        let (pending_len, already_queued) = match self.staged_new_subjects.get(&will.producer_id) {
            Some(p) => (p.len(), p.contains(&will.subject)),
            None => (0, false),
        };
        if !already_queued && self.subject_bound_exceeded(&probe, pending_len) {
            return Err(StageErr::SubjectBound(subject_bound_msg(will.producer_id)));
        }
        self.stage_record(BrokerRecord {
            seq,
            producer_id: will.producer_id,
            producer_seq: will.producer_seq,
            subject: WILL_SUBJECT.to_string(),
            body: will.encode_body(),
            commit: None,
        })?;
        // Reserve inside this batch too, so a second registration (or a publish) staged
        // before the commit sees the budget already spent. The batch's own queue is the
        // right place for it: an aborted batch drops the `/a/will` record and this
        // reservation together, and `commit_batch` promotes it into `will_reserved`.
        // Nothing is queued for a subject already indexed or already held — it costs no
        // second index entry, and double-counting it would refuse a legitimate publish.
        let held = match self.will_reserved.get(&will.producer_id) {
            Some(held_set) => held_set.contains(&will.subject),
            None => false,
        };
        if !already_queued && !held && !self.last.contains_key(&will.subject) {
            self.staged_new_subjects
                .entry(will.producer_id)
                .or_default()
                .insert(will.subject.clone());
        }
        Ok(seq)
    }

    /// FIRE a will as an ordinary publish, FENCED: nothing is appended if a record
    /// with a HIGHER `producer_seq` from the same producer has already landed
    /// ([`StageErr::Fenced`]) — a record below [`ACK_SEQ_BASE`], since the fence reads
    /// [`producer_high_water`](Self::producer_high_water) and an ack in the reserved
    /// half is not an incarnation. Otherwise it is a plain [`stage_publish`](Self::stage_publish),
    /// so a will whose key is already on the log dedups to a no-op — an exactly-once
    /// goodbye whether the producer said it itself or the broker said it for them.
    ///
    /// EXEMPT from the distinct-subject bound, because
    /// [`stage_will_register`](Self::stage_will_register) already applied it to this
    /// exact subject and answered the client with it. `Fenced` and `Replica` are
    /// intended outcomes of a will — nothing is owed to anyone when they refuse it —
    /// but `SubjectBound` is not: the firing's ack goes nowhere, so a bound applied
    /// here loses the goodbye silently and forever, re-attempted and re-refused at
    /// every later open. The exemption costs no overshoot: registration RESERVED this
    /// subject against the producer's budget (`will_reserved`) and holds it until this
    /// record lands, so what the firing indexes was already paid for.
    pub(crate) fn stage_will_fire(&mut self, will: WillRecord) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        // NEVER on a replica. The log is the leader's spine: the wills it holds are the
        // leader's, they fire THERE, and firing one here would append a record the
        // leader does not have — a forged death notice for a live node, a log that is
        // no longer a prefix of the leader's, and a link fenced for good.
        if self.replica {
            return Err(StageErr::Replica(format!(
                "will for producer {} is not fired here: this log is a replication \
                 target, so its wills fire on the leader that owns them",
                will.producer_id
            )));
        }
        if let Some(hw) = self.producer_high_water(will.producer_id) {
            if crate::proto::will_is_fenced(hw, will.producer_seq) {
                return Err(StageErr::Fenced(format!(
                    "will for producer {} at seq {} is fenced: seq {hw} has landed since",
                    will.producer_id, will.producer_seq
                )));
            }
        }
        self.stage_publish_bounded(
            will.producer_id,
            will.producer_seq,
            will.subject,
            will.body,
            false,
        )
    }

    /// The largest `producer_seq` BELOW [`ACK_SEQ_BASE`] this producer has landed on a
    /// visible subject, durable or staged — the value the will fence compares against.
    ///
    /// The reserved ack half is excluded on purpose: an ack's sequence is derived from
    /// an offset, not from an incarnation, so it says nothing about whether the
    /// producer has been heard from since it registered its will — and being above
    /// every incarnation-derived sequence, one ack would fence every will that producer
    /// could ever register. See [`ACK_SEQ_BASE`].
    pub fn producer_high_water(&self, producer_id: u64) -> Option<u64> {
        let mut best = self.high_water.get(&producer_id).copied();
        for r in &self.staged {
            if r.producer_id == producer_id
                && !is_hidden_subject(&r.subject)
                && r.producer_seq < ACK_SEQ_BASE
            {
                best = Some(match best {
                    Some(a) => crate::proto::high_water_monotone(a, r.producer_seq),
                    None => r.producer_seq,
                });
            }
        }
        best
    }

    /// Every will the log still holds, at most one per producer — the LAST one that
    /// producer registered, which is what "one per connection, a second replaces"
    /// means once the connection itself is gone — in ascending offset order.
    ///
    /// The set is keyed by PRODUCER, not by connection, and nothing tombstones a
    /// superseded `/a/will` record: replacement is durable exactly because a connection
    /// may hold wills under ONE producer id only (a second under a different id is
    /// refused at the verb), so "the connection's latest" and "that producer's latest"
    /// are the same will.
    ///
    /// HONEST BOUNDARY: a producer that registered wills on TWO live connections at
    /// once has only its most recent one re-fired across a broker restart. Wills that
    /// already fired, and wills a later record fenced, are still returned here: they
    /// append nothing when fired, by dedup and by the fence respectively, which is
    /// what makes re-firing every one of them on open idempotent. On a REPLICA the
    /// caller must not fire any of them at all — see [`is_replica`](Self::is_replica).
    pub fn pending_wills(&self) -> Vec<WillRecord> {
        let latest = self.latest_wills();
        let mut out = Vec::new();
        for (_, w) in latest {
            out.push(w);
        }
        out
    }

    /// [`pending_wills`](Self::pending_wills), each with the offset of its `/a/will`
    /// record (including wills carried across compaction via `<log>.state`).
    fn latest_wills(&self) -> Vec<(Offset, WillRecord)> {
        let mut latest: HashMap<u64, (Offset, WillRecord)> = self.carried_wills.clone();
        for rec in &self.records {
            if rec.subject != WILL_SUBJECT {
                continue;
            }
            if let Some(will) = WillRecord::from_record(rec) {
                latest.insert(rec.producer_id, (rec.seq, will));
            }
        }
        let mut ordered: BTreeMap<Offset, WillRecord> = BTreeMap::new();
        for (_, (off, will)) in latest {
            ordered.insert(off, will);
        }
        let mut out = Vec::new();
        for (off, will) in ordered {
            out.push((off, will));
        }
        out
    }

    /// Pending wills below `min` that have neither fired nor been fenced by a later
    /// publish from the same producer.
    #[cfg(feature = "retention")]
    fn pending_wills_below(&self, min: u64) -> Vec<(Offset, WillRecord)> {
        let mut out = Vec::new();
        for (off, w) in self.latest_wills() {
            if off.0 < min && !self.dedup.contains_key(&(w.producer_id, w.producer_seq)) {
                let fenced = match self.high_water.get(&w.producer_id) {
                    Some(&hw) => crate::proto::will_is_fenced(hw, w.producer_seq),
                    None => false,
                };
                if !fenced {
                    out.push((off, w));
                }
            }
        }
        out
    }

    /// The lowest record below `min` that retention must not drop, with what it
    /// holds. Bindings and wills live ONLY in their records — an open rebuilds both
    /// from the log — so compacting one away loses it at the next open: a bound
    /// producer id free for any principal to attach under, or a goodbye never said.
    /// A will that can no longer append anything is not held: one already fired or
    /// otherwise on the log (its key dedups), fenced by a later record of its producer,
    /// or superseded by a later registration.
    #[cfg(feature = "retention")]
    fn retention_blocker(&self, min: u64) -> Option<(u64, String)> {
        let mut best: Option<(u64, String)> = None;
        for (id, (principal, off)) in &self.bind {
            let better = match &best {
                Some((b, _)) => off.0 < *b,
                None => true,
            };
            if off.0 < min && better {
                let what = format!("producer {id}'s binding to principal {principal:?}");
                best = Some((off.0, what));
            }
        }
        for (off, w) in self.pending_wills_below(min) {
            let better = match &best {
                Some((b, _)) => off.0 < *b,
                None => true,
            };
            if better {
                let what = format!("producer {}'s pending last will", w.producer_id);
                best = Some((off.0, what));
            }
        }
        best
    }

    /// The principal a producer id is bound to (durable or in the in-flight batch),
    /// with the offset of the `/a/bind` record that bound it.
    pub fn binding(&self, producer_id: u64) -> Option<(&str, Offset)> {
        if let Some((p, off)) = self.staged_bind.get(&producer_id) {
            Some((p.as_str(), *off))
        } else if let Some((p, off)) = self.bind.get(&producer_id) {
            Some((p.as_str(), *off))
        } else {
            None
        }
    }

    /// Every DURABLE binding, `(producer id, principal, offset of its /a/bind
    /// record)`: the in-flight batch's are not here until it commits. While this
    /// `BrokerLog` is open the table only grows — promotion never replaces an entry,
    /// the committed tail is never cut back (a rollback cuts only the staged batch),
    /// and retention keeps every entry (it refuses to drop a binding's record or
    /// carries it in `<log>.state`) — so a binding read here once is a binding for as
    /// long as this open lasts. The broker's attach path relies on exactly that (see
    /// `Shared::bindings` in the broker). NOT across opens: an open rebuilds the table
    /// from what IT recovered, and a Relaxed log a power cut shortened, or one
    /// [`open_repair`](Self::open_repair) cut back to a corrupt record, can come back
    /// without a binding an earlier open held.
    pub(crate) fn durable_bindings(&self) -> DurableBindings<'_> {
        DurableBindings {
            inner: self.bind.iter(),
        }
    }

    /// How many [`durable_bindings`](Self::durable_bindings) there are, without
    /// visiting them: a copy that holds this many holds them all, since the table
    /// never loses or changes an entry.
    pub(crate) fn durable_binding_count(&self) -> usize {
        self.bind.len()
    }

    /// BIND a producer id to a principal, appending the hidden `/a/bind` record that
    /// makes the binding durable. Idempotent: re-binding the same principal appends
    /// nothing and returns the original offset with `deduped = true`. A DIFFERENT
    /// principal for an id already bound is [`StageErr::Collision`] — the belt-and-
    /// braces refusal behind `producer_id_of`'s 64-bit second-preimage cost.
    ///
    /// The check and the append happen together under the writer's lock, so two
    /// connections racing to bind colliding ids can never both succeed.
    pub(crate) fn stage_bind(
        &mut self,
        producer_id: u64,
        principal: String,
    ) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        if let Some((held, off)) = self.binding(producer_id) {
            if held == principal {
                return Ok((off, true));
            }
            return Err(StageErr::Collision(format!(
                "producer id collision: {producer_id} is already bound to principal {held:?}, \
                 not {principal:?}"
            )));
        }
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        self.stage_record(BrokerRecord {
            seq,
            producer_id,
            producer_seq: 0,
            subject: BIND_SUBJECT.to_string(),
            body: principal.clone().into_bytes(),
            commit: None,
        })?;
        self.staged_bind.insert(producer_id, (principal, seq));
        Ok((seq, false))
    }

    /// Frame a record for disk, sealing its payload under the at-rest key (bound to
    /// its log index) when one is set. The plaintext path is byte-for-byte
    /// `BrokerRecord::encode`, plus the batch `mark` (see
    /// [`BREC_VERSION`](crate::BREC_VERSION)): the mark rides INSIDE the payload the
    /// at-rest layer seals, so on an encrypted log it is authenticated under the
    /// record's own offset and reveals nothing new.
    fn encode_for_disk(&self, rec: &BrokerRecord, mark: Mark) -> Result<Vec<u8>, FrameError> {
        let payload = seal_disk_payload(
            self.at_rest_key.as_ref(),
            rec.seq.0,
            rec.encode_payload_marked(mark)?,
        );
        Frame::new(payload).encode()
    }

    /// The staged batch's bytes as `commit_batch` writes them (its last record sealed):
    /// what a test that crashes mid-commit puts in the file.
    #[cfg(test)]
    pub(crate) fn staged_bytes_as_written(&self) -> Vec<u8> {
        let mut bytes = self.staged_bytes.clone();
        if let (true, Some(last), Some(seal)) = (
            self.seals_batches(),
            self.staged.last(),
            batch_seal(self.staged.len()),
        ) {
            bytes.truncate(self.staged_last_at);
            bytes.extend_from_slice(
                &self
                    .encode_for_disk(last, seal)
                    .expect("a staged record takes its seal"),
            );
        }
        bytes
    }

    /// Whether this writer marks its batches (always, but for the crash simulation's
    /// negative control, which stands in the writer that predates the marks).
    fn seals_batches(&self) -> bool {
        #[cfg(test)]
        if self.unsealed_writer {
            return false;
        }
        true
    }

    /// The mark a staged record takes: the batch continues past it — its last record is
    /// re-encoded with the seal at commit — or nothing, from the writer before the
    /// marks.
    fn staged_mark(&self) -> Mark {
        if self.seals_batches() {
            Mark::Continues
        } else {
            Mark::Unmarked
        }
    }

    /// Encode `rec` into the in-flight batch (and the in-batch dedup view). Nothing
    /// reaches the file until `commit_batch` writes the batch in one call.
    fn stage_record(&mut self, rec: BrokerRecord) -> Result<(), StageErr> {
        self.stage_record_bounded(rec, true)
    }

    /// [`stage_record`](Self::stage_record), with the distinct-subject bound made
    /// explicit — see [`stage_publish_bounded`](Self::stage_publish_bounded). An
    /// exempt record still COUNTS toward the bound once it is indexed; it is only the
    /// refusal that is skipped.
    fn stage_record_bounded(
        &mut self,
        rec: BrokerRecord,
        enforce_bound: bool,
    ) -> Result<(), StageErr> {
        // The distinct-subject bound is checked BEFORE the record is staged, counting
        // the durable index and this batch's own not-yet-promoted new subjects, so a
        // refusal leaves the batch untouched and one batch cannot overshoot the cap.
        // An EXEMPT record is still counted — it really does add an index entry — but
        // it is never refused. It is queued only once staged: a record refused for
        // size spends none of the budget.
        let (queued_len, queued_has_subject) = match self.staged_new_subjects.get(&rec.producer_id)
        {
            Some(p) => (p.len(), p.contains(&rec.subject)),
            None => (0, false),
        };
        let new_subject = !is_hidden_subject(&rec.subject)
            && !self.last.contains_key(&rec.subject)
            && !queued_has_subject;
        if new_subject && enforce_bound && self.subject_bound_exceeded(&rec, queued_len) {
            return Err(StageErr::SubjectBound(subject_bound_msg(rec.producer_id)));
        }
        let Ok(frame) = self.encode_for_disk(&rec, self.staged_mark()) else {
            return Err(StageErr::TooLarge);
        };
        // ESCALATION, decided here — the one funnel every staged record passes, so a
        // record is escalated whatever verb or client version wrote it: a publish, a
        // transaction, a will firing, a commit (only a Group rule sees a pure commit,
        // which is hidden and producer 0). Only once the record is sure to be staged.
        let group_str = match &rec.commit {
            Some(c) => Some(c.group.as_str()),
            None => None,
        };
        if self.staged_escalated.is_none()
            && self.escalates(&rec.subject, rec.producer_id, group_str)
        {
            self.staged_escalated = Some(rec.seq.0);
        }
        self.staged_last_at = self.staged_bytes.len();
        self.staged_bytes.extend_from_slice(&frame);
        if new_subject {
            self.staged_new_subjects
                .entry(rec.producer_id)
                .or_default()
                .insert(rec.subject.clone());
        }
        if !is_hidden_subject(&rec.subject) {
            self.staged_dedup
                .insert((rec.producer_id, rec.producer_seq), rec.seq);
        }
        self.staged.push(rec);
        Ok(())
    }

    /// Record `rec` in the last-value index (and the per-producer distinct-subject
    /// count) as it becomes durable. Hidden subjects are excluded, so no query and no
    /// bound ever sees one.
    fn index_last(&mut self, rec: &BrokerRecord) {
        if is_hidden_subject(&rec.subject) {
            return;
        }
        // The last-will FENCE asks "has this producer been heard from SINCE it
        // registered?", and a will's sequence is its incarnation's reserved top. An
        // ack's sequence is not an incarnation at all — it is `ACK_SEQ_BASE | offset`,
        // above every incarnation-derived sequence there is — so one ack folded in
        // here would fence every will that producer could ever register, permanently
        // and with nothing logged (the firing is fire-and-forget). The reserved half is
        // excluded, here and in the rebuild on open; a will is refused that half at
        // registration, so nothing the fence has to protect lives there.
        if rec.producer_seq < ACK_SEQ_BASE {
            let hw = self
                .high_water
                .entry(rec.producer_id)
                .or_insert(rec.producer_seq);
            *hw = crate::proto::high_water_monotone(*hw, rec.producer_seq);
        }
        self.carried_last.remove(&rec.subject);
        match self.last.get_mut(&rec.subject) {
            Some(slot) => *slot = rec.seq,
            None => {
                self.last.insert(rec.subject.clone(), rec.seq);
                let count = self
                    .subjects_per_producer
                    .entry(rec.producer_id)
                    .or_insert(0);
                *count = count.saturating_add(1);
            }
        }
        // The subject has landed: any will reservation this producer held for it is
        // discharged — either it is counted in `subjects_per_producer` now, or it was
        // already an index entry. Holding it too would charge the budget twice. This is
        // the will's own firing on the ordinary path, and a producer that publishes the
        // subject itself before it dies on the other.
        if let Some(held) = self.will_reserved.get_mut(&rec.producer_id) {
            if held.remove(&rec.subject) && held.is_empty() {
                self.will_reserved.remove(&rec.producer_id);
            }
        }
    }

    /// Write the whole staged batch in ONE call and fsync it ONCE, then promote every
    /// staged record to the durable state (dedup, group commits, offset spine, mirror)
    /// — making them ack-able and visible to subscribers. An empty batch is a no-op
    /// (no write, no fsync). On a write or fsync error nothing is promoted and the
    /// written suffix is rolled back, so the
    /// on-disk log + mirror stay a clean durable prefix (the atomic-append contract,
    /// at batch granularity); if that rollback itself fails the log POISONS itself
    /// (every later mutation errors until reopen) rather than build on an unknown
    /// on-disk suffix.
    ///
    /// It is the three halves the broker's writer runs with the log's lock RELEASED
    /// around the middle one — [`commit_begin`](Self::commit_begin) (locked: decide the
    /// bytes and the flush), [`BatchWrite::run`] (unlocked: write and flush),
    /// [`commit_end`](Self::commit_end) (locked: promote, or roll back) — run back to
    /// back. A log with no writer thread (a direct user, the crash simulation, the
    /// un-batched replica append) commits through this one call, so the crash
    /// simulation's power losses land on exactly the code the broker's batches run.
    ///
    /// THE FLUSH each commit owes is decided by the tier SERVED ([`flush_owed`]): a full
    /// flush before promoting (Strict, exactly as before the tiers), an ordering barrier
    /// (Ordered), or nothing (Relaxed). A barrier the device refuses latches the log to
    /// Strict and flushes this batch in full instead. NO full flush owed by an ESCALATED
    /// record is issued here: the answer ([`Committed`]) holds the visible head at the
    /// first escalated record and names the full flush owed, which the caller issues
    /// OUTSIDE the log's lock (the broker's writer: `process_batch`) — so a reader asking
    /// for a page never waits on a device flush — and reports with
    /// [`full_flush_done`](Self::full_flush_done) or
    /// [`full_flush_failed`](Self::full_flush_failed). An empty batch writes and flushes
    /// nothing, but can still owe a full flush (a resent escalated op whose original no
    /// full flush covers).
    ///
    /// [`flush_owed`]: Self::flush_owed
    pub(crate) fn commit_batch(&mut self) -> io::Result<Committed> {
        match self.commit_begin()? {
            Begun::Write(mut write) => {
                let res = write.run();
                self.commit_end(write, res)
            }
            Begun::Empty(committed) => Ok(committed),
        }
    }

    /// The LOCKED first half of a commit ([`commit_batch`](Self::commit_batch)):
    /// everything that decides what the batch's bytes are and what flush they owe, so
    /// that the write and the flush can run without the log's lock ([`BatchWrite`]).
    /// Validates the final offset, seals the batch, pre-binds the anti-rollback
    /// watermark of an empty log, picks the flush the tier served owes, and hands out the
    /// staged bytes with the file's own descriptor. [`Begun::Empty`] for an empty batch
    /// (nothing to write, nothing to flush; its answer now). On an error nothing was
    /// written and the batch is discarded, as for a failed write.
    ///
    /// NOTHING IS PROMOTED HERE, and that is what makes releasing the lock after it
    /// safe. The batch's records stay in `staged` — invisible to every reader (the head
    /// is still `next`), absent from dedup, the last-value index, the bindings and the
    /// group commits, and the held and durable heads unmoved — until
    /// [`commit_end`](Self::commit_end) has the write's outcome. And `staged` stays
    /// NON-EMPTY all that while, with `writing` set, so every mutation that could touch
    /// the file or the batch refuses meanwhile (`check_poisoned`; retention and the clean
    /// close check `staged` as well).
    pub(crate) fn commit_begin(&mut self) -> io::Result<Begun> {
        self.check_poisoned()?;
        if self.staged.is_empty() {
            return Ok(Begun::Empty(self.committed_now()));
        }
        // All-or-nothing: validate the final offset BEFORE the fsync / any promotion, so
        // an offset-overflow can never leave a half-promoted batch that process_batch
        // then reports as wholly failed. (Staging already advances offsets via
        // next_staged_seq, so this is a belt-and-suspenders check at u64 saturation.)
        if self.next.0.checked_add(self.staged.len() as u64).is_none() {
            self.abort_batch();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "offset overflow",
            ));
        }
        // SEAL the batch: its last record, staged with the continues mark like every
        // other, is re-encoded with the batch's record count after its last field, so
        // recovery can find where this batch starts and tell a torn batch (cut at its
        // start) from damage behind a later complete one (refuse). One extra encode per
        // batch, inside the same single write, and done BEFORE the anti-rollback
        // pre-bind, which reads the first frame as written (on a one-record batch, the
        // sealed one).
        if self.seals_batches() {
            let sealed = match (batch_seal(self.staged.len()), self.staged.last()) {
                (Some(seal), Some(last)) => Some(self.encode_for_disk(last, seal)),
                _ => None,
            };
            match sealed {
                Some(Ok(frame)) => {
                    self.staged_bytes.truncate(self.staged_last_at);
                    self.staged_bytes.extend_from_slice(&frame);
                }
                // The record staged with a one-byte mark, and the margin in
                // MAX_RECORD_PAYLOAD keeps room for the seal, so this cannot fail; if it
                // ever did, nothing is written and the batch fails whole, as for any
                // other write error — never a batch whose last record says it goes on.
                Some(Err(_)) | None => {
                    self.abort_batch();
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the batch's last record does not take its seal",
                    ));
                }
            }
        }
        // The anti-rollback pre-bind stays under the lock: it must land before a byte of
        // the batch does, it reads the first frame as written, and it runs only for the
        // first batch an EMPTY log ever writes — a sidecar write once per log, never per
        // batch.
        if let Err(e) = self.prebind_watermark(&self.staged_bytes) {
            self.abort_batch();
            return Err(e);
        }
        // Strict: write + fsync the whole batch before promoting (ack ⟹ on disk).
        // Ordered: write + barrier (ack ⟹ ordered on the device). Relaxed: skip the
        // fsync — the write puts the bytes in the page cache, so the ack means
        // page-cache-durable (survives a process crash, not power loss). Chosen here, by
        // the tier served now; issued by `BatchWrite::run`, without the lock.
        self.writing = true;
        Ok(Begun::Write(BatchWrite {
            out: self.file.batch_out(),
            bytes: std::mem::take(&mut self.staged_bytes),
            flush: self.flush_owed(),
            faults: self.faults,
            refuse_barrier: self.refuses_barrier(),
            issued: Issued::default(),
        }))
    }

    /// The LOCKED last half of a commit ([`commit_batch`](Self::commit_batch)), given
    /// the outcome of the batch's write and flush ([`BatchWrite::run`]). It first counts
    /// what the flush did and latches the log to Strict if the device refused its
    /// barrier ([`settle_issued`](Self::settle_issued)). On success it promotes every
    /// staged record to the durable state — the moment they become ack-able, and visible
    /// once the caller publishes the head (or held at the batch's first escalated
    /// record) — and advances the anti-rollback watermark; on failure it rolls the
    /// written suffix back (poisoning the log if even that fails, or if the failed flush
    /// may have taken records acked before it) and promotes nothing. PROMOTION COMES
    /// ONLY AFTER DURABILITY: the outcome handed in is the flush's own, so a record is
    /// promoted only once the flush its tier owes has returned `Ok`.
    pub(crate) fn commit_end(
        &mut self,
        write: BatchWrite,
        res: io::Result<()>,
    ) -> io::Result<Committed> {
        let BatchWrite {
            mut bytes,
            faults,
            issued,
            ..
        } = write;
        self.writing = false;
        let durable = issued.full;
        let flush_failed = issued.failed;
        self.settle_issued(issued);
        if let Err(e) = res {
            bytes.clear();
            self.staged_bytes = bytes;
            // The rollback meets the fault seam the batch BEGAN with, as its flush did
            // (`BatchWrite::faults`): the seam can be re-armed in the write window — the
            // log lock is free there — and what is armed then is for the next batch. A
            // truncate fault armed in the window would otherwise fail this batch's
            // rollback and poison the log.
            let armed = std::mem::replace(&mut self.faults, faults);
            self.abort_batch();
            self.faults = armed;
            // The batch is rolled back, as under Strict before the tiers. But a flush
            // that FAILS on a log whose earlier acks rode barriers or the page cache says
            // the device may have dropped THEM too, and the rollback cannot reach them:
            // that is a failed full flush's case ([`flush_failed_lossy`]). A failed WRITE
            // flushed nothing, and says nothing about them.
            //
            // [`flush_failed_lossy`]: Self::flush_failed_lossy
            if flush_failed && self.durable_head < self.next.0 {
                self.flush_failed_lossy(&e);
            }
            return Err(e);
        }
        if self.durable_len == 0 {
            self.log_id = log_identity(&bytes);
        }
        self.newest_at = Some(self.durable_len.saturating_add(self.staged_last_at as u64));
        self.durable_len = self.durable_len.saturating_add(bytes.len() as u64);
        // The buffer comes back for the next batch to stage into, its capacity kept.
        bytes.clear();
        self.staged_bytes = bytes;
        self.staged_dedup.clear();
        self.staged_new_subjects.clear();
        self.staged_bind.clear();
        // Where the held head will stand: a group commit at or above it takes effect only
        // when a full flush releases it (`held_commits`).
        let hold_at = match (self.held_from, self.staged_escalated) {
            (Some(h), Some(s)) => Some(h.min(s)),
            (h, s) => h.or(s),
        };
        for rec in std::mem::take(&mut self.staged) {
            // A committed `/a/will` record holds its target subject from here on: the
            // staged reservation went out with `staged_new_subjects` above, and this is
            // what an open rebuilds from these same records.
            if rec.subject == WILL_SUBJECT {
                if let Some(will) = WillRecord::from_record(&rec) {
                    self.reserve_will_subject(&will);
                }
            }
            if rec.subject == BIND_SUBJECT {
                if let Ok(principal) = std::str::from_utf8(&rec.body) {
                    if !self.bind.contains_key(&rec.producer_id) {
                        self.bind
                            .insert(rec.producer_id, (principal.to_string(), rec.seq));
                    }
                }
            }
            self.apply_retire(&rec);
            if !is_hidden_subject(&rec.subject) {
                self.dedup
                    .insert((rec.producer_id, rec.producer_seq), rec.seq);
            }
            self.index_last(&rec);
            if let Some(c) = &rec.commit {
                let held = match hold_at {
                    Some(h) => rec.seq.0 >= h,
                    None => false,
                };
                if held {
                    self.held_commits.push((c.group.clone(), c.upto));
                } else {
                    self.apply_group_commit(&c.group, c.upto);
                }
            }
            // Validated above, so this never errors mid-loop (no partial promotion).
            let Some(next_seq) = rec.seq.checked_next() else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "offset overflow",
                ));
            };
            self.next = next_seq;
            self.records.push(Arc::new(rec));
        }
        // The visible head is HELD at the batch's first escalated record until a full
        // flush covers it — unless this commit's own flush was full (Strict, or a
        // refused barrier's fallback), which covers the whole batch.
        if let Some(first) = self.staged_escalated.take() {
            self.held_from = Some(match self.held_from {
                Some(h) => h.min(first),
                None => first,
            });
        }
        if durable {
            self.durable_head = self.next.0;
            self.held_from = None;
            self.release_held_commits();
        }
        // The batch is promoted; advance the anti-rollback watermark to the new head
        // (only ever under Strict, the one tier anti-rollback opens at).
        self.cover_head()?;
        Ok(self.committed_now())
    }

    fn apply_retire(&mut self, rec: &BrokerRecord) {
        if rec.subject != RETIRE_SUBJECT {
            return;
        }
        if let Ok(target) = std::str::from_utf8(&rec.body) {
            apply_retire_target(
                &mut self.last,
                &mut self.carried_last,
                &mut self.subjects_per_producer,
                rec.producer_id,
                target,
            );
        }
    }

    /// Validate a retirement target (either an exact [`Subject`] or a wildcard [`Filter`])
    /// and check whether any matching subject is currently present in `last` (and owned by
    /// `producer_id`).
    fn check_retire_target(&self, producer_id: u64, target: &str) -> Result<bool, StageErr> {
        if is_hidden_subject(target) || target.starts_with("/a/") || target == "/a" {
            return Err(StageErr::Reserved(target.to_string()));
        }
        let Ok(filter) = Filter::new(target) else {
            return Err(StageErr::Collision(format!(
                "bad subject or filter {target:?}"
            )));
        };
        let prefix = literal_prefix(&filter);
        let mut owners: BTreeMap<&str, u64> = BTreeMap::new();
        for (subject, &off) in self
            .last
            .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        {
            if !subject.starts_with(prefix) {
                break;
            }
            if filter.matches_str(subject) {
                let owner_rec = match self.record_at(off.0) {
                    Some(r) => Some(r),
                    None => self.carried_last.get(subject),
                };
                let Some(owner) = owner_rec else {
                    return Err(StageErr::Collision(format!(
                        "subject {subject:?} has no owner record"
                    )));
                };
                owners.insert(subject.as_str(), owner.producer_id);
            }
        }
        // Replay just this target's Last view through the staged prefix, in commit order.
        for record in &self.staged {
            if record.subject == RETIRE_SUBJECT {
                if let Ok(retired) = std::str::from_utf8(&record.body) {
                    if let Ok(retired) = Filter::new(retired) {
                        let mut drop_keys = Vec::new();
                        for &subject in owners.keys() {
                            if retired.matches_str(subject) {
                                drop_keys.push(subject);
                            }
                        }
                        for k in drop_keys {
                            owners.remove(k);
                        }
                    }
                }
            } else if !is_hidden_subject(&record.subject) && filter.matches_str(&record.subject) {
                owners.insert(record.subject.as_str(), record.producer_id);
            }
        }
        for (subject, &owner) in &owners {
            if owner != producer_id {
                return Err(StageErr::Collision(format!(
                    "subject {subject:?} (matched by {target:?}) is owned by producer {owner}, not {producer_id}"
                )));
            }
        }
        Ok(!owners.is_empty())
    }

    /// Durably retire `subject_or_filter` (either an exact [`Subject`] or a wildcard
    /// [`Filter`] such as `"/f/local/pub/n-1/s1/>"`) from the last-value index (`Last`)
    /// and decrement `producer_id`'s distinct-subject count (`subjects_per_producer`),
    /// appending a hidden [`RETIRE_SUBJECT`] record so the reclamation survives a broker
    /// restart and log compaction (`aterm-link` `bridge.rs` §"A NODE'S DISTINCT-SUBJECT BUDGET").
    ///
    /// Returns `Ok(Some(offset))` when at least one matching subject was present in `last`
    /// and retired; `Ok(None)` when no matching subject was present (idempotent no-op,
    /// nothing appended). Refuses with `PermissionDenied` if any matching subject in `last`
    /// is currently held by a different producer.
    pub fn retire_subject(
        &mut self,
        producer_id: u64,
        subject: &str,
    ) -> io::Result<Option<Offset>> {
        self.check_poisoned()?;
        if !self.staged.is_empty() {
            return Err(io::Error::other(
                "retire_subject: a batch is staged; commit or abort it first",
            ));
        }
        let present = match self.check_retire_target(producer_id, subject) {
            Ok(p) => p,
            Err(StageErr::Reserved(s)) => return Err(reserved_err(&s)),
            Err(StageErr::Collision(msg)) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, msg))
            }
            Err(_) => return Err(io::Error::new(io::ErrorKind::InvalidInput, "bad retire")),
        };
        if !present {
            return Ok(None);
        }
        let seq = self.next;
        self.append_record(BrokerRecord {
            seq,
            producer_id,
            producer_seq: 0,
            subject: RETIRE_SUBJECT.to_string(),
            body: subject.as_bytes().to_vec(),
            commit: None,
        })?;
        Ok(Some(seq))
    }

    /// Stage a durable subject or wildcard-filter retirement ([`RETIRE_SUBJECT`]) into the
    /// current group-commit batch. Mirrors [`retire_subject`](Self::retire_subject): returns
    /// `(offset, false)` when staged, `(head, true)` when no matching subject was present in
    /// `last` (idempotent no-op), or `Err` if `subject` is reserved or owned by a different
    /// producer.
    pub(crate) fn stage_retire(
        &mut self,
        producer_id: u64,
        subject: String,
    ) -> Result<(Offset, bool), StageErr> {
        self.check_poisoned_stage()?;
        let present = self.check_retire_target(producer_id, &subject)?;
        if !present {
            let cur = self.next_staged_seq().unwrap_or(self.next);
            return Ok((cur, true));
        }
        let seq = self.next_staged_seq().ok_or(StageErr::TooLarge)?;
        self.stage_record(BrokerRecord {
            seq,
            producer_id,
            producer_seq: 0,
            subject: RETIRE_SUBJECT.to_string(),
            body: subject.into_bytes(),
            commit: None,
        })?;
        Ok((seq, false))
    }

    /// Discard the staged (unsynced) batch, truncating the file back to `durable_len`
    /// so disk stays a clean durable prefix. Used when staging hit an
    /// I/O error, or the batch's write or fsync failed. If the truncate (or the
    /// seek back to the end) fails, the on-disk suffix is unknown: the log is POISONED
    /// and refuses every further mutation until reopened.
    pub(crate) fn abort_batch(&mut self) {
        self.rollback_to(self.durable_len);
        self.staged.clear();
        self.staged_bytes.clear();
        self.staged_dedup.clear();
        self.staged_new_subjects.clear();
        self.staged_bind.clear();
        self.staged_escalated = None;
        self.staged_held.clear();
    }

    /// Truncate the file back to `len` durable bytes and reposition at its end;
    /// poison the log if that cannot be done.
    fn rollback_to(&mut self, len: u64) {
        let res = match self.truncate_to(len) {
            Ok(()) => self.file.seek_end(),
            Err(e) => Err(e),
        };
        if let Err(e) = res {
            self.poisoned = Some(format!(
                "could not roll the staged suffix back to {len} bytes: {e}"
            ));
        }
    }

    fn truncate_to(&self, len: u64) -> io::Result<()> {
        if self.faults.truncate {
            return Err(io::Error::other("injected truncate fault"));
        }
        self.file.resize_file(len)
    }

    /// Encode + atomically append a record, then advance the offset spine and apply
    /// any carried group commit. (Shared by publish/commit/process_and_produce.)
    fn append_record(&mut self, rec: BrokerRecord) -> io::Result<()> {
        // Checked BEFORE any byte is written: a record with no successor offset must
        // never reach the disk.
        let Some(next) = rec.seq.checked_next() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "offset overflow",
            ));
        };
        let mark = if self.seals_batches() {
            Mark::Seal(1)
        } else {
            Mark::Unmarked
        };
        let Ok(frame) = self.encode_for_disk(&rec, mark) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "message too large",
            ));
        };
        let group_str = match &rec.commit {
            Some(c) => Some(c.group.as_str()),
            None => None,
        };
        let escalated = self.escalates(&rec.subject, rec.producer_id, group_str);
        // The watermark is pre-bound before a byte is written. Then the append: on disk
        // when it returns Ok under Strict, and for an escalated record on any tier.
        self.prebind_watermark(&frame)?;
        let durable = self.append_frame(&frame, escalated)?;
        self.apply_retire(&rec);
        self.index_last(&rec);
        if let Some(c) = &rec.commit {
            if self.held_from.is_some() {
                self.held_commits.push((c.group.clone(), c.upto));
            } else {
                self.apply_group_commit(&c.group, c.upto);
            }
        }
        self.next = next;
        self.records.push(Arc::new(rec));
        if durable {
            self.durable_head = self.next.0;
            self.held_from = None;
            self.release_held_commits();
        }
        self.cover_head()
    }

    /// LAST-VALUE query (retained state): the most recent record of every subject
    /// matching `filter` whose subject sorts strictly AFTER `after` (`""` for the
    /// first page), in ascending SUBJECT order, at most `max` of them, and only
    /// records BELOW `visible`. Returns the page and its RESUME CURSOR.
    ///
    /// BOUNDED WORK, like [`fetch`](Self::fetch): the scan of the ordered index starts
    /// at the filter's literal prefix (`O(log n)` to find) and then visits at most
    /// `scan_max` entries — matched or not. Without that bound the cost is
    /// `O(entries under the literal prefix)`, not `O(page)`: a filter whose matches are
    /// SPARSE within its prefix (`/f/F/pub/*/*/ack/<B>` over an index holding one
    /// `ack` subject per member per barrier ever issued) pays a `Subject::new` +
    /// `Filter::matches` for every entry it skips, and the caller holds the log lock
    /// for all of it — the whole-index walk a read-only holder could loop to stall the
    /// writer.
    ///
    /// The cursor is what keeps that bound honest. `Some(subject)` is the `after` to
    /// pass for the next page — the last entry VISITED, matched or not, so a page that
    /// matched nothing still advances (the same reason `fetch`'s `next` counts scanned
    /// records rather than delivered ones). `None` means the scan reached the end of
    /// the filter's prefix range: this was the last page. A caller that ignores it sees
    /// a page silently truncated at `scan_max`.
    ///
    /// The page hands out the same cheap `Arc` handles as
    /// [`read_from`](Self::read_from), so the caller writes them to a socket with the
    /// log lock released.
    ///
    /// `visible` is the SUBSCRIBER-VISIBLE head, which the caller must read under the
    /// same lock as this call. In the Strict/Relaxed tiers it is the durable head; in
    /// the Replicated tier it LAGS it, and a subject whose newest record is at or
    /// above it is OMITTED from the page rather than answered with a record the tail
    /// path would refuse to deliver — the same fail-closed direction as `tail_loop`.
    ///
    /// Hidden subjects are not in the index at all, so no filter can reach one.
    pub fn last_matching(
        &self,
        filter: &Filter,
        after: &str,
        max: usize,
        scan_max: usize,
        visible: u64,
    ) -> (Vec<Arc<BrokerRecord>>, Option<String>) {
        let mut out = Vec::new();
        if max == 0 {
            // The head query: nothing is scanned and nothing is delivered, and there is
            // no page to continue — the only answer is the caller's `Mark`.
            return (out, None);
        }
        let prefix = literal_prefix(filter);
        // `>=`, not `>`: a WILDCARD-FREE filter's literal prefix IS the only subject it
        // can match, so a resume cursor equal to the prefix must EXCLUDE it (with `>`
        // the range would restart at `Included(prefix)` and return that row forever).
        // The empty first-page cursor still takes the `Included` branch ("" < "/").
        let lower: Bound<&str> = if after >= prefix && !after.is_empty() {
            Bound::Excluded(after)
        } else {
            Bound::Included(prefix)
        };
        let mut visited = 0usize;
        for (subject, off) in self.last.range::<str, _>((lower, Bound::Unbounded)) {
            if !subject.starts_with(prefix) {
                break; // past the filter's literal prefix: nothing later can match
            }
            visited = visited.saturating_add(1);
            let matched = off.0 < visible && filter.matches_str(subject);
            if matched {
                let rec = match self.record_at(off.0) {
                    Some(r) => Some(r),
                    None => self.carried_last.get(subject),
                };
                if let Some(rec) = rec {
                    out.push(rec.clone());
                }
            }
            if out.len() >= max || visited >= scan_max {
                // The resume cursor is the last entry VISITED, matched or not.
                return (out, Some(subject.clone()));
            }
        }
        // The range ran out: the answer is complete, so there is nothing to resume.
        (out, None)
    }

    /// BOUNDED READ: at most `max` records matching `filter` with offset >= `from`,
    /// scanning at most `scan_max` records and never at or above `visible`. Returns
    /// the page (cheap `Arc` handles, as [`read_from`](Self::read_from)) and the
    /// offset AFTER the last record SCANNED.
    ///
    /// The scan bound is what makes this a *bounded* read: one request costs
    /// `O(scan_max)` whatever the filter matches, so a sparse filter over a long log
    /// pages instead of holding the log lock for a whole-history walk. Because `next`
    /// counts SCANNED records rather than delivered ones, a page that matched nothing
    /// still advances the cursor — otherwise a sparse reader would loop forever.
    ///
    /// `max == 0` scans nothing and returns `(empty, from)`: the head query, whose
    /// only answer is the `Mark`. Hidden subjects are skipped exactly as on the tail
    /// path, and they are still SCANNED (they consume offsets, so `next` counts them).
    pub fn fetch(
        &self,
        from: u64,
        filter: &Filter,
        max: usize,
        scan_max: usize,
        visible: u64,
    ) -> (Vec<Arc<BrokerRecord>>, u64) {
        let mut out = Vec::new();
        if max == 0 {
            return (out, from);
        }
        // Nothing below the retained base exists to scan: start at the earliest
        // retained record, as `read_from` does, so a pruned range costs no budget.
        let start = from.max(self.base.0);
        let limit = start.saturating_add(scan_max as u64).min(visible);
        let mut next = start;
        let mut off = start;
        while off < limit {
            next = off.saturating_add(1);
            if let Some(rec) = self.record_at(off) {
                if !is_hidden_subject(&rec.subject) && filter.matches_str(&rec.subject) {
                    out.push(rec.clone());
                    if out.len() >= max {
                        break;
                    }
                }
            }
            off = next;
        }
        (out, next)
    }

    /// The retained record at ABSOLUTE offset `off` (records are held base-relative),
    /// or `None` if it was pruned or is not committed.
    fn record_at(&self, off: u64) -> Option<&Arc<BrokerRecord>> {
        let rel = off.checked_sub(self.base.0)?;
        let Ok(i) = usize::try_from(rel) else {
            return None;
        };
        self.records.get(i)
    }

    /// How many DISTINCT subjects `producer_id` has created in the last-value index.
    /// [`MAX_SUBJECTS_PER_PRODUCER`] bounds this PLUS
    /// [`will_reserved_subjects`](Self::will_reserved_subjects), not this alone.
    pub fn subjects_per_producer(&self, producer_id: u64) -> usize {
        self.subjects_per_producer
            .get(&producer_id)
            .copied()
            .unwrap_or(0)
    }

    /// How many subjects `producer_id`'s registered-but-unlanded wills have RESERVED
    /// against its budget — the other half of what [`MAX_SUBJECTS_PER_PRODUCER`]
    /// bounds. Durable: rebuilt from the log's `/a/will` records on open.
    pub fn will_reserved_subjects(&self, producer_id: u64) -> usize {
        self.will_reserved.get(&producer_id).map_or(0, HashSet::len)
    }

    /// The lowest offset still retained (records below it were pruned by retention).
    pub const fn base(&self) -> Offset {
        self.base
    }

    /// Compact the log by dropping records with offset `< min_offset` (retention). Kept
    /// records keep their ABSOLUTE offsets, so a consumer at a surviving offset is
    /// unaffected; one below the new base catches up from the earliest retained.
    /// Returns the number of records dropped.
    ///
    /// REFUSED (`InvalidInput`, naming the offset — the highest floor it can take)
    /// when the dropped prefix holds a producer binding or a will that could still
    /// fire: those live only in their own records unless carried in a `<log>.state`
    /// sidecar (see [`Self::retain_before_carrying_state`]). Group commits are not held
    /// back: after a reopen, a group whose last commit record was dropped resumes from
    /// the base (the earliest retained record), and may re-read records it had already
    /// committed.
    ///
    /// Cold + explicit: no batch may be staged. The kept suffix is rewritten
    /// atomically (tmp + rename); then the base floor (and, under anti-rollback, the
    /// re-identified watermark) is advanced. An in-process failure after the rewrite
    /// POISONS the log. A CRASH in the small window between the log rewrite and the
    /// base/watermark update leaves them disagreeing, which the next open REFUSES
    /// (fail-safe — no acked data is silently dropped); recover by restoring the log
    /// file from a backup (NOT `open_repair`, which is keyless and would truncate an
    /// encrypted log). Do retention on a broker you can restart, off the ack path.
    #[cfg(feature = "retention")]
    pub fn retain_before(&mut self, min_offset: Offset) -> io::Result<u64> {
        self.compact_to(min_offset, false)
    }

    /// Compact the log by dropping records with offset `< min_offset`, carrying all
    /// active producer bindings (`/a/bind`), still-pending wills (`/a/will`), producer
    /// sequence fences, consumer-group commit watermarks, and the latest record of every
    /// quiet subject below `min_offset` (`Last`) in the `<log>.state` sidecar (sealed
    /// under the at-rest key when encrypted, or CRC-framed when plaintext).
    ///
    /// Unlike [`Self::retain_before`], this never stops at an early `/a/bind` or
    /// pending `/a/will` record, and preserves `Last` of quiet subjects across
    /// compaction and restart (`REQUIREMENTS` R4).
    #[cfg(feature = "retention")]
    pub fn retain_before_carrying_state(&mut self, min_offset: Offset) -> io::Result<u64> {
        self.compact_to(min_offset, true)
    }

    #[cfg(feature = "retention")]
    fn compact_to(&mut self, min_offset: Offset, carry_state: bool) -> io::Result<u64> {
        self.check_poisoned()?;
        if !self.staged.is_empty() {
            return Err(io::Error::other(
                "retention: a batch is staged; compact when idle",
            ));
        }
        let min = crate::proto::compact_floor_monotone(min_offset.0.min(self.next.0), self.base.0);
        if min <= self.base.0 {
            return Ok(0);
        }
        if !carry_state {
            if let Some((off, what)) = self.retention_blocker(min) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "retention: the record at offset {off}, below the floor {min}, holds \
                         {what}, which only that record keeps across a restart; the highest \
                         floor retention can take now is {off}"
                    ),
                ));
            }
        }
        let drop_count = min.saturating_sub(self.base.0);
        let mut durable = std::fs::read(&self.path)?;
        durable.truncate(self.durable_len as usize);
        // Byte offset of the first KEPT record in the durable log.
        let mut cut = 0usize;
        for _ in 0..drop_count {
            let Some(tail) = durable.get(cut..) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "retention: could not locate the cut point in the durable log",
                ));
            };
            match Frame::decode(tail) {
                Ok(Some(d)) if d.consumed > 0 => cut = cut.saturating_add(d.consumed),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "retention: could not locate the cut point in the durable log",
                    ))
                }
            }
        }
        let kept: Vec<u8> = match durable.get(cut..) {
            Some(slice) => slice.to_vec(),
            None => Vec::new(),
        };
        drop(durable);

        // When carrying state across compaction, persist `<log>.state` BEFORE replacing
        // the log file so an I/O error writing the sidecar leaves the log untouched.
        let new_carried = if carry_state {
            let mut carried: HashMap<u64, (Offset, WillRecord)> = HashMap::new();
            for (off, w) in self.pending_wills_below(min) {
                carried.insert(w.producer_id, (off, w));
            }
            let mut carried_last: BTreeMap<String, Arc<BrokerRecord>> = BTreeMap::new();
            for (subj, &off) in &self.last {
                if off.0 < min {
                    let rec = match self.record_at(off.0) {
                        Some(r) => Some(r),
                        None => self.carried_last.get(subj),
                    };
                    if let Some(rec) = rec {
                        carried_last.insert(subj.clone(), Arc::clone(rec));
                    }
                }
            }
            let state = CompactedState {
                bind: self.bind.clone(),
                carried_wills: carried.clone(),
                high_water: self.high_water.clone(),
                group_commits: self.group_commits.clone(),
                carried_last: carried_last.clone(),
            };
            write_state(&self.path, self.at_rest_key.as_ref(), min, &state)?;
            Some((carried, carried_last))
        } else {
            None
        };

        // 1) Rewrite the log to the kept suffix, atomically (tmp + rename), then reopen
        //    and re-lock the new file (the old inode is now unlinked).
        let mut tmp = self.path.clone();
        tmp.as_mut_os_string().push(".compact");
        // The copy REPLACES the log, so it carries the log's permissions — and is
        // created with them (then set exactly, past the umask), so the kept records
        // are never readable more widely than the log they came from, even briefly.
        let perms = self.file.os()?.metadata()?.permissions();
        {
            let mut opts = FsOpenOptions::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                opts.mode(perms.mode());
            }
            let mut f = create_fresh(&tmp, &mut opts)?;
            f.set_permissions(perms)?;
            f.write_all(&kept)?;
            f.sync_all()?;
        }
        rename_path(&tmp, &self.path)?;

        // From here the on-disk log is ALREADY compacted, so any failure leaves the
        // in-memory state and the base/watermark sidecars possibly disagreeing with it.
        // Do the remaining durable I/O FIRST, and POISON on any failure — the log then
        // refuses every further op until it is reopened (whereupon a base/log mismatch
        // is a fail-safe REFUSE, never silent loss). `next`/head is unchanged (front prune).
        if let Err(e) = self.finish_retention(min) {
            self.poisoned = Some(format!("retention compaction failed after rewrite: {e}"));
            return Err(e);
        }

        // In-memory state (infallible): re-base + drop pruned records and their
        // still-theirs dedup keys, then re-write the anti-rollback watermark to the NEW
        // first-record identity (also fail-closed -> poison).
        let drain_len = usize::try_from(drop_count)
            .unwrap_or(self.records.len())
            .min(self.records.len());
        let old_records = std::mem::take(&mut self.records);
        for (i, rec) in old_records.into_iter().enumerate() {
            if i < drain_len {
                if rec.subject != COMMIT_SUBJECT
                    && self.dedup.get(&(rec.producer_id, rec.producer_seq)) == Some(&rec.seq)
                {
                    self.dedup.remove(&(rec.producer_id, rec.producer_seq));
                }
            } else {
                self.records.push(rec);
            }
        }
        if let Some((carried_wills, carried_last)) = new_carried {
            self.carried_wills = carried_wills;
            self.carried_last = carried_last;
        } else {
            let mut drop_carried = Vec::new();
            for (k, r) in &self.carried_last {
                if r.seq.0 < min {
                    drop_carried.push(k.clone());
                }
            }
            for k in drop_carried {
                self.carried_last.remove(&k);
            }
        }
        self.durable_len = kept.len() as u64;
        self.newest_at = match self.newest_at {
            Some(at) => at.checked_sub(cut as u64),
            None => None,
        };
        self.log_id = log_identity(&kept);
        self.base = Offset(min);
        if let Err(e) = self.advance_watermark() {
            self.poisoned = Some(format!("retention: watermark update failed: {e}"));
            return Err(e);
        }
        Ok(drop_count)
    }

    /// Reopen + re-lock the freshly-compacted log and durably record the new base
    /// floor. Split out so a failure here is caught and poisons the log.
    #[cfg(feature = "retention")]
    fn finish_retention(&mut self, min: u64) -> io::Result<()> {
        // Make the rename durable, reopen + re-lock the new inode (the old is now
        // unlinked), and record the new base — all inside the caller's poison guard, so
        // an in-process failure at ANY step (including this directory fsync) poisons the
        // log rather than leaving it serving on the dead inode.
        sync_parent_dir(&self.path)?;
        let newf = open_with_options(FsOpenOptions::new().read(true).write(true), &self.path)?;
        lock_exclusive(&newf, &self.path)?;
        self.file = Backing::from_file(newf);
        self.file.seek_end()?;
        write_base(&self.path, min)
    }

    /// Hand out every committed record with `seq >= start` (from the earliest retained
    /// one if `start` was pruned), in order, as cheap `Arc` clones — the egress read
    /// path does NO deep copy of subject/body, so the lock is released (the caller
    /// writes outside it) without paying for the payload bytes.
    pub fn read_from(&self, start: Offset) -> Vec<Arc<BrokerRecord>> {
        self.read_range(start, usize::MAX)
    }

    /// [`read_from`](Self::read_from), BOUNDED: at most `max` records from `start`
    /// (from the earliest retained one if `start` was pruned), in order, as cheap `Arc`
    /// clones. Copying the handles is the whole cost of a read made under the log lock,
    /// and it grows with the suffix: a reader that holds the lock while it copies — a
    /// subscriber catching up, a fork's snapshot — takes a long suffix this way in
    /// pieces, releasing the lock between them.
    pub fn read_range(&self, start: Offset, max: usize) -> Vec<Arc<BrokerRecord>> {
        // Records sit at absolute offsets base..next; map `start` to an index (clamp a
        // below-base request for pruned records to the earliest retained). Clamp in u64
        // space then narrow — a lossy `as usize` would wrap a >2^32 offset on 32-bit.
        let len = self.records.len();
        let from = start.0.saturating_sub(self.base.0).min(len as u64) as usize;
        let to = from.saturating_add(max).min(len);
        match self.records.get(from..to) {
            Some(slice) => slice.to_vec(),
            None => Vec::new(),
        }
    }

    /// Whether any committed record at an offset in `from..to` is one a subscriber can
    /// be delivered: a subject [`is_hidden_subject`] does not name. Pruned offsets hold
    /// nothing. It copies nothing and stops at the first such record, so a range a
    /// publish landed in answers at once; only a range of hidden records is walked to
    /// its end. The broker's writer asks this of each batch's newly visible range, under
    /// the log lock, to decide whether the batch wakes the subscriber tails.
    pub fn any_deliverable(&self, from: Offset, to: Offset) -> bool {
        // Clamped as `read_range` clamps: in u64 space, then narrowed below `len`.
        let len = self.records.len();
        let lo = from.0.saturating_sub(self.base.0).min(len as u64) as usize;
        let hi = to.0.saturating_sub(self.base.0).min(len as u64) as usize;
        let Some(recs) = self.records.get(lo..hi) else {
            return false;
        };
        for r in recs {
            if !is_hidden_subject(&r.subject) {
                return true;
            }
        }
        false
    }

    /// How many retained records a handle OUTSIDE the log still shares: copies a
    /// reader took and has not yet delivered and dropped. For tests of how much a
    /// reader copies out of the log at once.
    #[cfg(test)]
    pub(crate) fn shared_records(&self) -> usize {
        self.records
            .iter()
            .filter(|r| Arc::strong_count(r) > 1)
            .count()
    }

    /// A COUNTERFACTUAL snapshot as SHARED handles: the committed records with the one
    /// at `fork_at` replaced by `replacement` (its `seq` forced to `fork_at`). Only the
    /// `Arc` pointers are cloned (plus the one replacement record), so taking this under
    /// the log lock costs O(records) pointer copies, never O(payload bytes) — the same
    /// zero-copy discipline as [`read_from`](Self::read_from). The live log is
    /// unchanged. An out-of-range `fork_at` returns the records as-is.
    pub fn fork_shared(
        &self,
        fork_at: Offset,
        replacement: BrokerRecord,
    ) -> Vec<Arc<BrokerRecord>> {
        let mut out = self.records.clone();
        // `fork_at` is an ABSOLUTE offset; records are stored base-relative, so a fork
        // at or above the retained base maps to index `fork_at - base` (a fork below
        // the base targets a pruned record and is a no-op).
        if let Some(rel) = fork_at.0.checked_sub(self.base.0) {
            if let Ok(i) = usize::try_from(rel) {
                if let Some(slot) = out.get_mut(i) {
                    let mut r = replacement;
                    r.seq = fork_at;
                    *slot = Arc::new(r);
                }
            }
        }
        out
    }

    /// [`fork_shared`](Self::fork_shared) materialized as OWNED records (a deep copy
    /// of every payload). Kept for callers that want owned data; the broker's
    /// fork-delivery path reads the log in bounded pieces
    /// ([`read_range`](Self::read_range)) and copies no payload at all.
    pub fn fork(&self, fork_at: Offset, replacement: BrokerRecord) -> Vec<BrokerRecord> {
        let shared = self.fork_shared(fork_at, replacement);
        let mut out = Vec::new();
        for a in shared {
            out.push((*a).clone());
        }
        out
    }

    /// Committed byte length of the durable on-disk log prefix (`O(1)`, zero allocation).
    #[must_use]
    pub const fn durable_len(&self) -> u64 {
        self.durable_len
    }

    /// Read the durable on-disk bytes (`0..durable_len`) from the backing log file.
    ///
    /// Unlike holding a duplicate `Vec<u8>` mirror in RAM alongside `records`, this reads
    /// from the OS page cache only when explicitly requested (e.g. inspection/tests),
    /// halving the steady-state per-record RAM footprint of `BrokerLog`.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        let mut buf = self.file.stored(&self.path);
        buf.truncate(self.durable_len as usize);
        buf
    }

    /// Atomic append at the tier's guarantee: rolls a partial frame back on a
    /// write/fsync error so the on-disk log stays a clean prefix of `durable_len`
    /// bytes (poisoning the log if the rollback itself fails). `Ok(true)` when a full
    /// flush covered it.
    ///
    /// The un-batched path has no writer thread and no lock to release, so an
    /// `escalated` record on a weak tier is flushed in full right here, before the call
    /// returns: the same promise the broker's held head keeps (durable before anyone is
    /// told), paid by the caller that holds the log. A failed flush rolls the frame back,
    /// as a failed Strict flush's is; and when earlier acks rode barriers or the page
    /// cache, which the rollback cannot reach, it is a failed full flush's case — the run
    /// recorded `Lossy` and the log poisoned until it is reopened
    /// ([`flush_failed_lossy`](Self::flush_failed_lossy)).
    fn append_frame(&mut self, frame_bytes: &[u8], escalated: bool) -> io::Result<bool> {
        let committed = self.durable_len;
        // Strict fsyncs; Ordered issues a barrier; Relaxed writes to the page cache only
        // (see [`Durability`]).
        let weak = self.served.tier != crate::proto::Tier::Strict;
        let flush = if escalated && weak {
            Flush::Full
        } else {
            self.flush_owed()
        };
        if let Err(e) = self.file.write_all(frame_bytes) {
            self.rollback_to(committed);
            return Err(e);
        }
        let durable = match self.issue(flush) {
            Ok(durable) => durable,
            Err(e) => {
                self.rollback_to(committed);
                if self.durable_head < self.next.0 {
                    self.flush_failed_lossy(&e);
                }
                return Err(e);
            }
        };
        if self.durable_len == 0 {
            self.log_id = log_identity(frame_bytes);
        }
        self.newest_at = Some(self.durable_len);
        self.durable_len = self.durable_len.saturating_add(frame_bytes.len() as u64);
        Ok(durable)
    }
}

pub(crate) struct DurableBindings<'a> {
    inner: std::collections::hash_map::Iter<'a, u64, (String, Offset)>,
}

impl<'a> Iterator for DurableBindings<'a> {
    type Item = (u64, &'a str, Offset);

    fn next(&mut self) -> Option<Self::Item> {
        match self.inner.next() {
            Some((&id, (p, off))) => Some((id, p.as_str(), *off)),
            None => None,
        }
    }
}

/// How a stored log ends, as seen by [`scan_from`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Tail {
    /// Every byte is an intact, in-order record.
    Clean,
    /// The intact prefix is followed by an INCOMPLETE frame with nothing decodable
    /// behind it, or by an all-zero tail — the signatures of a write interrupted by a
    /// crash/power loss. Safe to truncate: no complete frame was ever written there, so
    /// nothing in it was ever acked. (Residual, to the scan: a rotted length field on the
    /// very LAST record also looks like an incomplete frame. Recovery that knows the
    /// durable floor tells them apart by asking the frame's CRC over the bytes to the
    /// end — `recover.rs` — and announces the cut; the rules that predate the floor
    /// cannot.)
    Torn,
    /// The intact prefix is followed by bytes that are present but WRONG: a complete
    /// frame with a bad CRC/magic/version, a record payload that does not parse (an
    /// OLDER record format this build no longer reads, a foreign file), or an offset
    /// gap. Truncating here would destroy every record after it — possibly acked data
    /// — so the default open refuses instead.
    Corrupt {
        offset: u64,
        at_byte: usize,
        reason: String,
        /// Whether the explicit repair may cut here.
        repair_at: RepairAt,
        /// Whether a TORN WRITE can leave this shape: a frame that does not decode, an
        /// empty frame, a frame running past the end with records behind it. A shape
        /// that only a writer can make — a CRC-valid frame that is not the next record,
        /// does not parse, or fails to authenticate — is never one, and is refused by
        /// every open. Recovery that knows the log's durable floor judges a `tear`
        /// shape above it by the batch seals instead (`recover.rs`).
        tear: bool,
    },
    /// The intact prefix is followed by a record in a NEWER format (`version` above
    /// `BREC_VERSION`) — written by a newer astream, not damaged: the frame's CRC
    /// (and, at rest, its authentication) held before the version byte was read.
    /// Neither corruption nor repairable: every open refuses it, by name.
    Newer {
        offset: u64,
        at_byte: usize,
        version: u8,
    },
}

/// What the explicit repair may do at a [`Tail::Corrupt`] point. A repair exists to
/// cut DAMAGE — bytes a crash, a power loss or the media left that are not the frame
/// a writer wrote — and it may also cut an intact record this build has no use for
/// (an older record format, an offset gap): that is what an operator asks of it. What
/// it must never cut is an intact frame that only THIS OPEN cannot read for want of
/// the right key, because then everything behind the cut was written whole and is
/// readable with the key: on a sealed log that cut is the whole log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairAt {
    /// Damage, or an intact record this build does not use: the repair cuts here.
    Cut,
    /// A keyed open: an intact frame (its CRC holds, and it is not empty) failed to
    /// authenticate. No crash or power loss produces one — a torn or zeroed frame
    /// fails its CRC or decodes empty first — so it is a wrong key or a rewrite by
    /// someone without the key, and with a wrong key the very first record fails:
    /// cutting would discard the whole log.
    WrongKey,
    /// A keyless open: the log's FIRST frame is intact but no plaintext record — the
    /// shape every log sealed at rest has without its key (or one in a format this
    /// build does not read). The cut would be at byte 0: the whole log. Mid-log the
    /// same shape keeps `Cut`, since a sealed log never gets past its first record
    /// without the key, and there it is an older record format behind plaintext ones.
    NeedsKey,
}

pub(crate) struct Scan {
    pub(crate) records: Vec<BrokerRecord>,
    /// Byte length of the intact prefix.
    pub(crate) valid_len: usize,
    pub(crate) tail: Tail,
    /// The offset and byte just past the last record of the intact prefix that ENDS A
    /// UNIT — a batch's sealed last record, or one a build that predates the batch marks
    /// wrote whole, on its own — where the batch after it starts; `(base, 0)` when the
    /// prefix holds none.
    pub(crate) seal_end: (u64, usize),
    /// The byte where the record at the durable floor's offset starts, when the floor
    /// is within the intact prefix (its end included).
    pub(crate) floor_at: Option<usize>,
}

/// Whether a complete, parsable record starts ANYWHERE after the first byte of
/// `rest` — the bytes that `Frame::decode` said are an incomplete frame. A write
/// interrupted by a crash leaves nothing decodable behind its partial frame; a frame
/// that is in fact complete but whose LENGTH field rotted (so it "extends past EOF")
/// leaves the later records intact and decodable behind it. Only positions whose
/// magic bytes match are decoded, so this is cheap even over a long tail.
pub(crate) fn later_record_exists(rest: &[u8], key: Option<&[u8; 32]>) -> bool {
    use astream_wire::frame::MAGIC;
    use astream_wire::HEADER_SIZE;
    let mut q = 1usize;
    while let Some(tail) = rest.get(q..) {
        let Some(hdr) = tail.first_chunk::<HEADER_SIZE>() else {
            break;
        };
        if hdr[0] == MAGIC[0] && hdr[1] == MAGIC[1] {
            if let Ok(Some(d)) = Frame::decode(tail) {
                // A complete, CRC-valid frame follows the "past-EOF" one. Plaintext: it
                // must also parse as a record. Encrypted: its payload is ciphertext we
                // cannot parse (and whose index we do not know here), but a 32-bit-CRC
                // -valid frame is strong evidence of a real sealed record, so treat its
                // presence as a rotted length field (corruption), not a torn tail.
                // A newer-format record counts: it was written, so the frame before it
                // is a rotted length, not a torn write to cut.
                //
                // An EMPTY frame never counts, keyed or not. Its CRC is valid only
                // because the CRC-32 of nothing is 0 — any magic and version followed by
                // eight zero bytes passes it — so it is no evidence that anything was
                // written there, and no record or sealed record is ever empty (see
                // `scan`). Without this, a torn tail with such bytes in it read as a
                // rotted length field: a torn write refused as corruption.
                let real = match key {
                    _ if d.frame.payload.is_empty() => false,
                    Some(_) => true,
                    None => {
                        BrokerRecord::from_payload(&d.frame.payload).is_some()
                            || crate::brecord::newer_format(&d.frame.payload, None).is_some()
                    }
                };
                if real {
                    return true;
                }
            }
        }
        q = q.saturating_add(1);
    }
    false
}

/// Scan stored bytes into the longest intact, in-order (`seq == expected`) prefix
/// of records, its byte length, and HOW the bytes end (clean / torn / corrupt — the
/// distinction that decides whether recovery may truncate). Pure and panic-free.
#[cfg(test)]
fn scan(bytes: &[u8], key: Option<&[u8; 32]>, base: u64) -> Scan {
    scan_from(bytes, key, base, None, Mutation::Off)
}

fn all_zero_bytes(rest: &[u8]) -> bool {
    for &b in rest {
        if b != 0 {
            return false;
        }
    }
    true
}

/// [`scan`], noting what the batch seals and the durable `floor` say about the intact
/// prefix ([`Scan::seal_end`], [`Scan::floor_at`]) for the recovery that judges damage
/// by them, and taking the crash simulation's `mutation` (always `Off` outside it).
pub(crate) fn scan_from(
    bytes: &[u8],
    key: Option<&[u8; 32]>,
    base: u64,
    floor: Option<u64>,
    mutation: Mutation,
) -> Scan {
    let mut out: Vec<BrokerRecord> = Vec::new();
    let mut pos = 0usize;
    let mut seal_end = (base, 0usize);
    let mut floor_at = if floor == Some(base) {
        Some(0usize)
    } else {
        None
    };
    let tail = loop {
        let rest = bytes.get(pos..).unwrap_or(&[]);
        if rest.is_empty() {
            break Tail::Clean;
        }
        let expected = base.saturating_add(out.len() as u64);
        match Frame::decode(rest) {
            // An EMPTY frame is never a record: a stored record's payload is at least 34
            // bytes (its 25-byte header, two length prefixes and the commit tag), and one
            // sealed at rest is 40 bytes longer again. It is what a torn HEADER decodes
            // as: `Frame::decode` accepts a zero-length payload and the CRC-32 of nothing
            // is 0, so a header whose first four bytes (magic, version, flags) landed and
            // whose length and CRC fields read back as zeros — `A5 71 01 00` and eight
            // zero bytes — is a CRC-valid frame. It is equally what a record whose bytes
            // after its fourth were zeroed by damage decodes as, and the bytes cannot say
            // which. That is exactly the doubt of a frame that does not decode, so it
            // takes that classification, by name. Checked BEFORE the payload is
            // decrypted: an empty payload fails authentication, and "wrong at-rest key or
            // a tampered record" would send an operator after a key that is fine.
            Ok(Some(d)) if d.frame.payload.is_empty() && !mutation.no_empty_frame_rule() => {
                if all_zero_bytes(rest) {
                    break Tail::Torn;
                }
                break Tail::Corrupt {
                    offset: expected,
                    at_byte: pos,
                    reason: "an empty frame: a torn header, or damage".to_string(),
                    repair_at: RepairAt::Cut,
                    tear: true,
                };
            }
            Ok(Some(d)) => {
                // Decrypt the sealed payload (identity when no key is set); a COMPLETE
                // frame that fails to authenticate is a wrong key or a tampered record
                // — a corrupt tail, never silently discarded, and never cut by a repair
                // either (`RepairAt::WrongKey`).
                let payload = match open_disk_payload(key, expected, d.frame.payload) {
                    Some(p) => p,
                    None => {
                        break Tail::Corrupt {
                            offset: expected,
                            at_byte: pos,
                            reason:
                                "record failed to authenticate (wrong at-rest key or a tampered record)"
                                    .to_string(),
                            repair_at: RepairAt::WrongKey,
                            tear: false,
                        };
                    }
                };
                match BrokerRecord::from_payload_marked(&payload) {
                    // No offset follows u64::MAX, so the log never writes a record
                    // there; refusing one also keeps `base + records` from overflowing.
                    Some((r, _)) if r.seq.0 == u64::MAX => {
                        break Tail::Corrupt {
                            offset: expected,
                            at_byte: pos,
                            reason: "record at offset u64::MAX, which the log never assigns"
                                .to_string(),
                            repair_at: RepairAt::Cut,
                            tear: false,
                        };
                    }
                    Some((r, mark)) if r.seq.0 == expected => {
                        out.push(r);
                        // A decoded frame always consumes >= its header; guard anyway so
                        // a zero-progress decode can never spin.
                        if d.consumed == 0 {
                            break Tail::Corrupt {
                                offset: expected,
                                at_byte: pos,
                                reason: "zero-length frame".to_string(),
                                repair_at: RepairAt::Cut,
                                tear: false,
                            };
                        }
                        pos = pos.saturating_add(d.consumed);
                        if mark.ends_a_unit() {
                            seal_end = (expected.saturating_add(1), pos);
                        }
                        if floor == Some(expected.saturating_add(1)) {
                            floor_at = Some(pos);
                        }
                    }
                    // A CRC-valid frame that is not the next record was WRITTEN that way:
                    // never an interrupted write.
                    Some((r, _)) => {
                        break Tail::Corrupt {
                            offset: expected,
                            at_byte: pos,
                            reason: format!(
                                "offset gap: expected seq {expected}, found {}",
                                r.seq.0
                            ),
                            repair_at: RepairAt::Cut,
                            tear: false,
                        };
                    }
                    None => {
                        if let Some(version) =
                            crate::brecord::newer_format(&payload, Some(expected))
                        {
                            break Tail::Newer {
                                offset: expected,
                                at_byte: pos,
                                version,
                            };
                        }
                        // Keyless, at the log's first byte: the shape of a log sealed at
                        // rest read without its key, whose every record looks like this.
                        // The repair's cut would be the whole log (`RepairAt::NeedsKey`).
                        if key.is_none() && pos == 0 {
                            break Tail::Corrupt {
                                offset: expected,
                                at_byte: pos,
                                reason:
                                    "the log's first record is intact (its CRC holds) but is not a \
                                 plaintext record: a log sealed at rest, read without its key, \
                                 or a record format this build does not read"
                                        .to_string(),
                                repair_at: RepairAt::NeedsKey,
                                tear: false,
                            };
                        }
                        break Tail::Corrupt {
                            offset: expected,
                            at_byte: pos,
                            reason: "record payload does not parse (an older BREC_VERSION, or malformed)"
                                .to_string(),
                            repair_at: RepairAt::Cut,
                            tear: false,
                        };
                    }
                }
            }
            // An incomplete trailing frame is the classic torn write — unless intact
            // records follow it, which means its LENGTH field is what is wrong (a
            // complete frame whose header rotted), i.e. corruption mid-log. (A torn
            // write can leave that shape too, when a later block of its batch landed,
            // which is why it is `tear`: a recovery that knows the floor asks the seals.)
            Ok(None) => {
                if later_record_exists(rest, key) {
                    break Tail::Corrupt {
                        offset: expected,
                        at_byte: pos,
                        reason:
                            "frame claims to extend past the end of the log but intact records \
                         follow it (a corrupt length field, not a torn write)"
                                .to_string(),
                        repair_at: RepairAt::Cut,
                        tear: true,
                    };
                }
                break Tail::Torn;
            }
            // A nonzero, complete-sized bad frame may have been committed before corruption.
            // Trailing zeros alone do not establish an interrupted write. Preserve those bytes
            // and require explicit repair; retain the existing all-zero extent exception.
            Err(e) => {
                if all_zero_bytes(rest) {
                    break Tail::Torn;
                }
                break Tail::Corrupt {
                    offset: expected,
                    at_byte: pos,
                    reason: format!("frame does not decode: {e}"),
                    repair_at: RepairAt::Cut,
                    tear: true,
                };
            }
        }
    };
    Scan {
        records: out,
        valid_len: pos,
        tail,
        seal_end,
        floor_at,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::brecord::BREC_VERSION;
    use std::io::{Read, Seek, SeekFrom};

    /// Removes its paths when dropped, each with every `<path>.*` sidecar beside it
    /// (`.hw`, `.base`, `.replica`, a `.hw.tmp` planted as a directory, …), so a test
    /// leaves nothing behind whether it passes or panics. Bound before the log that
    /// uses the paths, so it drops after it.
    struct Cleanup(Vec<PathBuf>);

    impl Cleanup {
        fn new<P: AsRef<Path>>(paths: &[P]) -> Cleanup {
            Cleanup(paths.iter().map(|p| p.as_ref().to_path_buf()).collect())
        }
    }

    impl Drop for Cleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = std::fs::remove_file(path);
                let _ = std::fs::remove_dir_all(path);
                let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
                    continue;
                };
                let prefix = format!("{}.", name.to_string_lossy());
                for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                    if entry.file_name().to_string_lossy().starts_with(&prefix) {
                        let _ = std::fs::remove_file(entry.path());
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                }
            }
        }
    }

    /// A log path unique to this process and `tag`, and the guard that removes it.
    fn tmp(tag: &str) -> (Cleanup, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "astream_brokerlog_{}_{tag}.log",
            std::process::id()
        ));
        (Cleanup::new(&[&path]), path)
    }

    fn rec(seq: u64, pid: u64, pseq: u64) -> BrokerRecord {
        BrokerRecord {
            seq: Offset(seq),
            producer_id: pid,
            producer_seq: pseq,
            subject: "/a/s/x".to_string(),
            body: format!("b{seq}").into_bytes(),
            commit: None,
        }
    }

    /// A log of `n` publishes (producer 1, seqs 1..=n), closed cleanly.
    fn write_log(path: &Path, n: u64) -> u64 {
        let mut log = BrokerLog::open(path).unwrap();
        for i in 1..=n {
            log.publish(1, i, "/a/s/x".into(), format!("b{i}").into_bytes())
                .unwrap();
        }
        log.bytes().len() as u64
    }

    /// Creating a log in a directory this process may write but not READ must still
    /// open: `sync_parent_dir` cannot fsync the entry there, which is not a reason
    /// to refuse a log whose own bytes are fsync'd (and the file has already been
    /// created by then, so failing leaves a zero-length log behind).
    #[test]
    #[cfg(unix)]
    fn a_write_only_log_directory_still_opens() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "astream_brokerlog_{}_wronly_dir",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let _tmp = Cleanup::new(&[&dir]);
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o300)).unwrap();
        // Running as root (or on a filesystem that ignores the mode) defeats the
        // scenario entirely: the directory is still openable, so there is nothing
        // to assert. Probe rather than guess.
        let unreadable = File::open(&dir).is_err();
        let path = dir.join("x.log");
        let opened = BrokerLog::open(&path);
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&dir);
        if unreadable {
            let mut log = opened.expect("a write-only log directory must not refuse the log");
            assert_eq!(
                log.publish(1, 1, "/a/s/x".into(), b"a".to_vec()).unwrap(),
                (Offset(0), false)
            );
        }
    }

    /// An open that finds the log already present still fsyncs its directory: the
    /// open that created it may have died (or failed) before its own directory
    /// fsync, and records acked into a file whose entry is not durable can vanish
    /// with it on a power loss.
    #[test]
    fn opening_an_existing_log_still_fsyncs_its_directory() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SYNCS: AtomicUsize = AtomicUsize::new(0);
        fn counting(_: &Path) -> io::Result<()> {
            SYNCS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn failing(_: &Path) -> io::Result<()> {
            Err(io::Error::other("injected directory fsync failure"))
        }
        let (_tmp, path) = tmp("dirsync");
        let _ = std::fs::remove_file(&path);
        // The first open creates the file, but its directory fsync fails.
        assert!(open_log_file(&path, failing).is_err());
        assert!(path.exists(), "the file was created");
        // The next open finds the file there and must still fsync the directory.
        let (_file, created) = open_log_file(&path, counting).unwrap();
        assert!(!created);
        assert_eq!(SYNCS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn publish_dedup_and_recover() {
        let (_tmp, path) = tmp("dedup");
        let _ = std::fs::remove_file(&path);
        {
            let mut log = BrokerLog::open(&path).unwrap();
            assert_eq!(
                log.publish(1, 1, "/a/s/x".into(), b"a".to_vec()).unwrap(),
                (Offset(0), false)
            );
            assert_eq!(
                log.publish(1, 2, "/a/s/x".into(), b"b".to_vec()).unwrap(),
                (Offset(1), false)
            );
            // re-send (1,1): original offset, deduped, not re-appended.
            assert_eq!(
                log.publish(1, 1, "/a/s/x".into(), b"a".to_vec()).unwrap(),
                (Offset(0), true)
            );
            assert_eq!(log.head(), Offset(2));
            assert_eq!(log.read_from(Offset(0)).len(), 2);
        }
        // Reopen: records + dedup recovered from disk.
        let mut log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.head(), Offset(2), "recovered the offset spine");
        assert_eq!(log.read_from(Offset(0)).len(), 2);
        assert_eq!(
            log.publish(1, 1, "/a/s/x".into(), b"a".to_vec()).unwrap(),
            (Offset(0), true),
            "dedup survived restart"
        );
    }

    /// A torn tail — a trailing frame cut mid-payload — is truncated on open, durably,
    /// and the intact prefix is recovered whole.
    #[test]
    fn torn_tail_is_truncated_and_prefix_recovered() {
        let (_tmp, path) = tmp("torn");
        let _ = std::fs::remove_file(&path);
        let clean_len = write_log(&path, 5);
        // Append the first 20 bytes of what would be record 5 (a torn write).
        let partial = rec(5, 1, 6).encode().unwrap();
        {
            let mut f = FsOpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&partial[..20]).unwrap();
        }
        assert_eq!(std::fs::metadata(&path).unwrap().len(), clean_len + 20);
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.head(), Offset(5), "the 5 intact records recovered");
        assert_eq!(log.bytes().len() as u64, clean_len);
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            clean_len,
            "the torn bytes were truncated off the file"
        );
        drop(log);

        // A zero-filled tail (the file was extended but the data never landed) is
        // torn too.
        {
            let mut f = FsOpenOptions::new().append(true).open(&path).unwrap();
            f.write_all(&[0u8; 64]).unwrap();
        }
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.head(), Offset(5));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), clean_len);
        drop(log);
    }

    /// A complete-but-corrupt frame in the MIDDLE of the log (a flipped byte in record
    /// 2 of 5) is NOT a torn tail: open refuses (naming the offset) and leaves the file
    /// untouched; only the explicit `open_repair` truncates, reporting what it dropped.
    #[test]
    fn mid_log_corruption_refuses_to_open_and_repair_is_explicit() {
        let (_tmp, path) = tmp("midcorrupt");
        let _ = std::fs::remove_file(&path);
        let clean_len = write_log(&path, 5);
        // Every record here is the same size (each its own sealed batch).
        let one = clean_len / 5;
        // Flip a payload byte inside record 2.
        {
            let mut f = FsOpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            let at = 2 * one + 20;
            f.seek(SeekFrom::Start(at)).unwrap();
            let mut b = [0u8; 1];
            f.read_exact(&mut b).unwrap();
            f.seek(SeekFrom::Start(at)).unwrap();
            f.write_all(&[b[0] ^ 0xFF]).unwrap();
        }
        let err = BrokerLog::open(&path).err().expect("must refuse to open");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(
            msg.contains("offset 2") && msg.contains("open_repair"),
            "names the offset and the repair path: {msg}"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            clean_len,
            "refusing to open must not touch the file"
        );
        // Explicit repair: truncates at record 2, reports the 3 records' bytes dropped.
        let (log, dropped) = BrokerLog::open_repair(&path, Durability::Strict).unwrap();
        assert_eq!(dropped, 3 * one);
        assert_eq!(log.head(), Offset(2));
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 2 * one);
        drop(log);
        let _ = std::fs::remove_file(&path);

        // A rotted LENGTH field mid-log makes record 2 "extend past EOF" — the shape
        // of a torn tail — but records 3 and 4 are intact behind it, so it is refused
        // as corruption too (a torn write leaves nothing decodable behind it).
        let clean_len = write_log(&path, 5);
        {
            let mut f = FsOpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            f.seek(SeekFrom::Start(2 * one + 6)).unwrap(); // high bytes of record 2's len
            f.write_all(&[0x10, 0x00]).unwrap(); // +1 MiB: past EOF, under the cap
        }
        let err = BrokerLog::open(&path).err().expect("must refuse to open");
        assert!(
            err.to_string().contains("offset 2") && err.to_string().contains("length"),
            "{err}"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), clean_len);
        let (log, dropped) = BrokerLog::open_repair(&path, Durability::Strict).unwrap();
        assert_eq!((dropped, log.head()), (3 * one, Offset(2)));
        drop(log);
    }

    /// The stored payload of `rec(seq, 1, seq + 1)` stamped with record format
    /// `version`, framed — a record a build of that version would have written.
    fn record_in_format(seq: u64, version: u8) -> Vec<u8> {
        let mut payload = rec(seq, 1, seq + 1).encode().unwrap()[12..].to_vec();
        payload[0] = version;
        Frame::new(payload).encode().unwrap()
    }

    /// An OLDER record format mid-log (one this build no longer reads), an offset gap
    /// followed by more records, a complete-but-invalid LAST frame, a frame whose
    /// length runs past the end with an intact record behind it (a rotted length, not
    /// an interrupted write), and a newer version byte on a payload that does not
    /// carry its own offset are all corruption: refused, the file untouched, and only
    /// the explicit `open_repair` truncates.
    #[test]
    fn an_older_format_a_gap_and_a_bad_last_frame_are_corruption_not_torn() {
        let one = rec(0, 1, 1).encode().unwrap();
        let mut good = Vec::new();
        for i in 0..3 {
            good.extend_from_slice(&rec(i, 1, i + 1).encode().unwrap());
        }
        // (a) a record whose payload carries BREC_VERSION-1, followed by a good one.
        let mut a = good.clone();
        a.extend_from_slice(&record_in_format(3, BREC_VERSION - 1));
        a.extend_from_slice(&rec(4, 1, 5).encode().unwrap());
        // (b) an offset gap (seq 5 where 3 is expected) followed by more bytes.
        let mut b = good.clone();
        b.extend_from_slice(&rec(5, 1, 6).encode().unwrap());
        b.extend_from_slice(&rec(6, 1, 7).encode().unwrap());
        // (c) a complete last frame with a corrupt CRC (no bytes after it).
        let mut c = good.clone();
        let mut bad = rec(3, 1, 4).encode().unwrap();
        bad[20] ^= 0x55;
        c.extend_from_slice(&bad);
        // (d) a frame whose length field claims more bytes than the log holds, with an
        // intact current-format record after it.
        let mut d = good.clone();
        let mut rotted = rec(3, 1, 4).encode().unwrap();
        rotted[4..8].copy_from_slice(&100_000u32.to_le_bytes());
        d.extend_from_slice(&rotted);
        d.extend_from_slice(&rec(4, 1, 5).encode().unwrap());
        // (e) a newer version byte on a payload that does not carry its own offset —
        // the shape of a sealed record read without its key (a random first byte).
        let mut e = good.clone();
        let mut foreign = rec(3, 1, 4).encode().unwrap()[12..].to_vec();
        foreign[0] = BREC_VERSION + 1;
        foreign[1..9].copy_from_slice(&9u64.to_le_bytes());
        e.extend_from_slice(&Frame::new(foreign).encode().unwrap());
        let cases = [
            ("older", a),
            ("gap", b),
            ("badlast", c),
            ("rotted", d),
            ("unplaced", e),
        ];
        for (tag, bytes) in cases {
            let (_tmp, path) = tmp(&format!("corrupt_{tag}"));
            let _ = std::fs::remove_file(&path);
            std::fs::write(&path, &bytes).unwrap();
            let err = BrokerLog::open(&path).err().expect("must refuse to open");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{tag}");
            assert!(err.to_string().contains("offset 3"), "{tag}: {err}");
            assert_eq!(
                std::fs::metadata(&path).unwrap().len(),
                bytes.len() as u64,
                "{tag}: file untouched"
            );
            let (log, dropped) = BrokerLog::open_repair(&path, Durability::Strict).unwrap();
            assert_eq!(
                log.head(),
                Offset(3),
                "{tag}: repair keeps the intact prefix"
            );
            assert_eq!(dropped as usize, bytes.len() - 3 * one.len(), "{tag}");
            drop(log);
        }
    }

    /// A record in a NEWER format than this build reads — mid-log, as the last frame,
    /// or intact behind an earlier damaged frame — was written by a newer astream, not
    /// damaged: every open refuses it as `Unsupported`, naming its offset and without
    /// pointing at a repair, and `open_repair` refuses it the same way rather than
    /// truncating it. The file is never touched.
    #[test]
    fn a_newer_format_is_refused_by_name_and_never_repaired() {
        let mut good = Vec::new();
        for i in 0..3 {
            good.extend_from_slice(&rec(i, 1, i + 1).encode().unwrap());
        }
        let mut mid = good.clone();
        mid.extend_from_slice(&record_in_format(3, BREC_VERSION + 1));
        mid.extend_from_slice(&rec(4, 1, 5).encode().unwrap());
        let mut last = good.clone();
        last.extend_from_slice(&record_in_format(3, u8::MAX));
        let mut behind = good.clone();
        let mut rotted = rec(3, 1, 4).encode().unwrap();
        rotted[4..8].copy_from_slice(&100_000u32.to_le_bytes());
        behind.extend_from_slice(&rotted);
        behind.extend_from_slice(&record_in_format(4, BREC_VERSION + 1));
        for (tag, bytes, want_offset) in [
            ("mid", mid, "offset 3"),
            ("last", last, "offset 3"),
            ("behind", behind, "offset 4"),
        ] {
            let (_tmp, path) = tmp(&format!("newer_{tag}"));
            let _ = std::fs::remove_file(&path);
            std::fs::write(&path, &bytes).unwrap();
            for repair in [false, true] {
                let err = if repair {
                    BrokerLog::open_repair(&path, Durability::Strict).err()
                } else {
                    BrokerLog::open(&path).err()
                }
                .expect("a newer format must not open");
                assert_eq!(err.kind(), io::ErrorKind::Unsupported, "{tag} {repair}");
                let msg = err.to_string();
                assert!(msg.contains(want_offset), "{tag} {repair}: {msg}");
                assert!(!msg.contains("asb repair"), "{tag} {repair}: {msg}");
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    bytes,
                    "{tag} {repair}: the file is untouched"
                );
            }
        }
    }

    /// The log file is exclusively locked while open: a second open (the shape of two
    /// brokers serving one log) fails AlreadyExists until the first is closed.
    #[test]
    fn open_log_is_exclusively_locked() {
        let (_tmp, path) = tmp("lock");
        let _ = std::fs::remove_file(&path);
        let first = BrokerLog::open(&path).unwrap();
        let err = BrokerLog::open(&path).err().expect("must refuse to open");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists, "{err}");
        drop(first);
        let again = BrokerLog::open(&path).unwrap();
        // An explicit unlock (what the broker does at shutdown) releases it early too.
        again.unlock();
        let third = BrokerLog::open(&path).unwrap();
        drop(third);
        drop(again);
    }

    /// The batch a `commit_begin` hands out: a test that stages something and wants its
    /// write, never the empty answer.
    fn begun_write(log: &mut BrokerLog) -> BatchWrite {
        match log.commit_begin().unwrap() {
            Begun::Write(write) => write,
            Begun::Empty(_) => panic!("nothing staged to write"),
        }
    }

    /// A BATCH IS WRITTEN THROUGH THE LOG'S OWN OPEN FILE, AND NEVER A STALE ONE. What a
    /// batch is written and flushed through outside the lock is the log's own descriptor,
    /// shared (`Backing::batch_out`), not a second one: no batch opens a descriptor, so a
    /// process at its descriptor limit fails none for `EMFILE`. Asked of the `Arc` itself
    /// (`BatchOut::is_of`), which a `dup` — a new `File`, whatever number it came back
    /// under — cannot pass. It is lent from the batch's begin to its end, while the log
    /// refuses to stage. And after retention replaces the file, the next batch is written
    /// through the NEW one: it is in the file a reopen reads — written through the old
    /// file it would have landed on the unlinked inode, acked and gone.
    #[test]
    fn a_batch_is_written_through_the_logs_own_file_and_never_a_stale_one() {
        let (_tmp, path) = tmp("batch_out");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        for seq in 1..=2u64 {
            log.stage_publish(1, seq, format!("/a/s/{seq}"), b"x".to_vec())
                .unwrap();
            let mut write = begun_write(&mut log);
            assert!(
                write.out.is_of(&log.file),
                "batch {seq} is written through the log's own descriptor"
            );
            assert!(matches!(
                log.stage_publish(1, 100 + seq, "/a/s/more".into(), vec![]),
                Err(StageErr::Io(_))
            ));
            let res = write.run();
            log.commit_end(write, res).unwrap();
        }
        assert_eq!(log.head(), Offset(2));

        #[cfg(feature = "retention")]
        {
            assert_eq!(log.retain_before(Offset(1)).unwrap(), 1);
            log.stage_publish(1, 3, "/a/s/3".into(), b"x".to_vec())
                .unwrap();
            let mut write = begun_write(&mut log);
            assert!(
                write.out.is_of(&log.file),
                "the batch after a compaction is written through the new file"
            );
            let res = write.run();
            log.commit_end(write, res).unwrap();
            assert_eq!(log.head(), Offset(3));
            drop(log);
            let log = BrokerLog::open(&path).unwrap();
            assert_eq!(
                (log.base(), log.head()),
                (Offset(1), Offset(3)),
                "the batch after the compaction is in the file a reopen reads"
            );
        }
    }

    /// A FAULT ARMED IN THE WRITE WINDOW IS THE NEXT BATCH'S, ROLLBACK INCLUDED. The
    /// fault-injection seam can be re-armed between a batch's begin and its end — the
    /// broker's log lock is free there, and `Broker::inject_faults` takes it — and the
    /// batch in flight keeps the seam it BEGAN with: its flush (the sync fault) and its
    /// rollback (the truncate fault) both. A batch begun with only the sync fault fails
    /// its flush and rolls back cleanly although a truncate fault was armed meanwhile;
    /// the next batch, begun with both, fails its flush and its rollback, and poisons
    /// the log — the control that the armed truncate fault is live, and lands where it
    /// was meant to.
    #[test]
    fn a_fault_armed_in_the_write_window_is_the_next_batchs_rollback_included() {
        let (_tmp, path) = tmp("window_fault");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        log.publish(1, 1, "/a/s/x".into(), b"durable".to_vec())
            .unwrap();
        let durable_len = log.bytes().len() as u64;
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        log.stage_publish(1, 2, "/a/s/x".into(), b"lost".to_vec())
            .unwrap();
        let mut write = begun_write(&mut log);
        // The write window: armed now, for a later batch.
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: true,
        });
        let res = write.run();
        let err = log.commit_end(write, res).unwrap_err();
        assert!(err.to_string().contains("injected fsync fault"), "{err}");
        assert!(
            log.poisoned().is_none(),
            "the batch's rollback met the truncate fault armed after it began: {:?}",
            log.poisoned()
        );
        assert_eq!(log.head(), Offset(1), "nothing promoted");
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            durable_len,
            "rolled back to the durable prefix"
        );

        // The next batch began with both faults armed: they are its.
        log.stage_publish(1, 2, "/a/s/x".into(), b"orphan".to_vec())
            .unwrap();
        assert!(log.commit_batch().is_err());
        let reason = log.poisoned().expect("the next batch's rollback failed");
        assert!(reason.contains("injected truncate fault"), "{reason}");
    }

    /// A failed batch fsync rolls the staged suffix back: nothing promoted (head,
    /// dedup, records unchanged), the file truncated back to the durable prefix, and a
    /// reopen recovers exactly that prefix.
    #[test]
    fn failed_fsync_rolls_the_staged_batch_back() {
        let (_tmp, path) = tmp("fsyncfault");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        log.publish(1, 1, "/a/s/x".into(), b"durable".to_vec())
            .unwrap();
        let durable_len = log.bytes().len() as u64;
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        assert_eq!(
            log.stage_publish(1, 2, "/a/s/x".into(), b"lost".to_vec())
                .unwrap(),
            (Offset(1), false)
        );
        log.stage_commit("g".into(), 0).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            durable_len,
            "staging only buffers the batch: commit_batch writes it in one call"
        );
        let err = log.commit_batch().unwrap_err();
        assert!(err.to_string().contains("injected fsync fault"), "{err}");
        assert_eq!(log.head(), Offset(1), "nothing promoted");
        assert_eq!(
            log.group_start("g"),
            Offset(0),
            "the commit was not applied"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            durable_len,
            "the staged suffix was truncated off the file"
        );
        assert_eq!(log.bytes().len() as u64, durable_len);
        assert!(log.poisoned().is_none());
        // The rolled-back key is NOT in dedup: re-staging it appends anew at offset 1.
        log.inject_faults(InjectedFaults::default());
        assert_eq!(
            log.stage_publish(1, 2, "/a/s/x".into(), b"retry".to_vec())
                .unwrap(),
            (Offset(1), false),
            "the failed batch left no phantom dedup entry"
        );
        log.commit_batch().unwrap();
        drop(log);
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.head(), Offset(2));
        assert_eq!(log.read_from(Offset(1))[0].body, b"retry");
        drop(log);
    }

    /// If the rollback truncate itself fails the log POISONS: every later mutation is
    /// refused (nothing can be appended after an orphaned on-disk suffix), and a reopen
    /// re-establishes the prefix from the bytes actually on disk.
    #[test]
    fn failed_rollback_poisons_the_log_until_reopen() {
        let (_tmp, path) = tmp("poison");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        log.publish(1, 1, "/a/s/x".into(), b"durable".to_vec())
            .unwrap();
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: true,
        });
        log.stage_publish(1, 2, "/a/s/x".into(), b"orphan".to_vec())
            .unwrap();
        assert!(log.commit_batch().is_err());
        let reason = log.poisoned().expect("poisoned").to_string();
        assert!(reason.contains("injected truncate fault"), "{reason}");
        log.inject_faults(InjectedFaults::default());
        // Poisoned: batched and un-batched mutations are all refused, nothing appended.
        assert!(matches!(
            log.stage_publish(1, 3, "/a/s/x".into(), b"x".to_vec()),
            Err(StageErr::Io(_))
        ));
        assert!(matches!(
            log.stage_commit("g".into(), 0),
            Err(StageErr::Io(_))
        ));
        assert!(log.publish(1, 3, "/a/s/x".into(), b"x".to_vec()).is_err());
        assert!(log.commit("g".into(), 0).is_err());
        assert!(log.commit_batch().is_err());
        assert_eq!(log.head(), Offset(1));
        drop(log);
        // Reopen: the orphaned (complete, in-order) frame is on disk, so it is
        // recovered as record 1 — an un-acked write that landed, which its producer's
        // idempotent retry dedups against; never a seq collision.
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.head(), Offset(2));
        assert_eq!(log.read_from(Offset(1))[0].body, b"orphan");
        assert!(log.poisoned().is_none());
        drop(log);
    }

    /// A batch whose anti-rollback watermark write fails after its fsync is durable
    /// but not covered, so nothing may be acknowledged on the strength of it: the log
    /// poisons, and a retried key is refused rather than acked as a dedup hit.
    /// A crash between the first commit's fsync and its watermark update leaves the
    /// log ahead of the watermark the commit bound to it BEFORE writing a byte
    /// (identity of its first record, the old head). That is the ordinary lagging
    /// case: a verified open accepts it and advances the watermark. The same state
    /// with an unbound (zero-identity) watermark stays refused.
    #[cfg(feature = "anti-rollback")]
    #[test]
    fn a_log_ahead_of_its_own_bound_watermark_opens_verified() {
        const KEY: [u8; 32] = [0x24; 32];
        let (_tmp, path) = tmp("hwlag");
        let hw = watermark_path(&path);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&hw);
        let mut log = BrokerLog::open_encrypted_verified_init(&path, KEY).unwrap();
        log.stage_publish(1, 1, "/a/s/x".into(), b"one".to_vec())
            .unwrap();
        log.commit_batch().unwrap();
        let id = log_identity(&log.bytes());
        drop(log);
        // Roll the watermark back to what the first commit bound before its write.
        write_watermark(&hw, &KEY, &id, 0, 0).unwrap();
        let log = BrokerLog::open_encrypted_verified(&path, KEY)
            .expect("a log ahead of its own bound watermark was refused");
        assert_eq!(log.head(), Offset(1));
        drop(log);
        assert_eq!(read_watermark(&hw, &KEY).unwrap(), Some((id, 0, 1)));
        // An unbound watermark (no identity) under the same log is still refused.
        write_watermark(&hw, &KEY, &[0u8; 16], 0, 0).unwrap();
        assert!(BrokerLog::open_encrypted_verified(&path, KEY).is_err());
    }

    /// An EMPTY log under a bound watermark is accepted only as a pre-bind whose
    /// records never landed (at exactly the watermark's base and head) — never as a
    /// log truncated to nothing with its base sidecar moved up to the watermark's
    /// head, which is what a key-less rollback of a whole log would look like.
    #[cfg(all(feature = "anti-rollback", feature = "retention"))]
    #[test]
    fn an_empty_log_is_accepted_only_under_its_own_pre_bind() {
        const KEY: [u8; 32] = [0x25; 32];
        let (_tmp, path) = retained_path("hwempty");
        let hw = watermark_path(&path);
        let _ = std::fs::remove_file(&hw);
        let mut log = BrokerLog::open_encrypted_verified_init(&path, KEY).unwrap();
        for i in 0..3u64 {
            log.publish(1, i, "/a/s/x".into(), b"x".to_vec()).unwrap();
        }
        drop(log);
        // The attack: every record gone, the base moved up to the head.
        FsOpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(0)
            .unwrap();
        write_base(&path, 3).unwrap();
        assert!(
            BrokerLog::open_encrypted_verified(&path, KEY).is_err(),
            "a log truncated to nothing was accepted"
        );
        // The crash: retention emptied the log, and the next first commit bound the
        // watermark but its records never landed.
        write_watermark(&hw, &KEY, &[7u8; 16], 3, 3).unwrap();
        let log = BrokerLog::open_encrypted_verified(&path, KEY)
            .expect("an empty log under its own pre-bind was refused");
        assert_eq!((log.base(), log.head()), (Offset(3), Offset(3)));
        drop(log);
    }

    #[cfg(feature = "anti-rollback")]
    #[test]
    fn a_failed_watermark_write_poisons_the_batched_path() {
        let (_tmp, path) = tmp("hwbatch");
        let hw_tmp = {
            let mut s = watermark_path(&path).into_os_string();
            s.push(".tmp");
            PathBuf::from(s)
        };
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(watermark_path(&path));
        let _ = std::fs::remove_dir(&hw_tmp);
        let mut log = BrokerLog::open_encrypted_verified_init(&path, [0x42; 32]).unwrap();
        log.stage_publish(1, 1, "/a/s/x".into(), b"one".to_vec())
            .unwrap();
        log.commit_batch().unwrap();
        std::fs::create_dir(&hw_tmp).unwrap();
        log.stage_publish(1, 2, "/a/s/x".into(), b"two".to_vec())
            .unwrap();
        assert!(log.commit_batch().is_err());
        std::fs::remove_dir(&hw_tmp).unwrap();
        assert!(log.poisoned().is_some());
        assert!(
            matches!(
                log.stage_publish(1, 2, "/a/s/x".into(), b"two".to_vec()),
                Err(StageErr::Io(_))
            ),
            "a retry was acknowledged past a watermark that does not cover it"
        );
        drop(log);
        let log = BrokerLog::open_encrypted_verified(&path, [0x42; 32]).unwrap();
        assert_eq!(log.head(), Offset(2));
        drop(log);
    }

    #[cfg(feature = "retention")]
    fn retained_path(tag: &str) -> (Cleanup, PathBuf) {
        let (guard, path) = tmp(tag);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(base_path(&path));
        (guard, path)
    }

    /// A producer binding lives only in its `/a/bind` record, so retention refuses
    /// a floor above it (the next open would rebuild the table without it, freeing
    /// the id for any principal) and the log is untouched; a floor at or below it
    /// compacts as usual, and the binding survives a reopen.
    #[cfg(feature = "retention")]
    #[test]
    fn retention_refuses_to_drop_a_producer_binding() {
        let (_tmp, path) = retained_path("retbind");
        let mut log = BrokerLog::open(&path).unwrap();
        log.publish(1, 1, "/a/s/x".into(), b"a".to_vec()).unwrap(); // 0
        log.publish(1, 2, "/a/s/x".into(), b"b".to_vec()).unwrap(); // 1
        assert_eq!(
            log.stage_bind(7, "alice".into()).unwrap(),
            (Offset(2), false)
        );
        log.commit_batch().unwrap();
        log.publish(1, 3, "/a/s/x".into(), b"c".to_vec()).unwrap(); // 3
        let bytes = log.bytes().len();
        let err = log.retain_before(Offset(3)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        let msg = err.to_string();
        assert!(
            msg.contains("offset 2") && msg.contains("binding") && msg.contains("alice"),
            "{msg}"
        );
        assert_eq!((log.base(), log.bytes().len()), (Offset(0), bytes));
        assert!(log.poisoned().is_none());
        assert_eq!(log.retain_before(Offset(2)).unwrap(), 2);
        drop(log);
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.base(), Offset(2));
        assert_eq!(log.binding(7), Some(("alice", Offset(2))));
        drop(log);
    }

    /// A will that would still fire lives only in its `/a/will` record, so
    /// retention refuses a floor above it. A will that can no longer append anything
    /// — already fired (its key is on the log), fenced by a later record of its
    /// producer, or superseded by a later registration — does not hold the floor.
    #[cfg(feature = "retention")]
    #[test]
    fn retention_refuses_to_drop_a_pending_will_only() {
        let will = |producer_id, producer_seq, subject: &str| WillRecord {
            producer_id,
            producer_seq,
            subject: subject.to_string(),
            body: b"gone".to_vec(),
        };
        let (_tmp, path) = retained_path("retwill");
        let mut log = BrokerLog::open(&path).unwrap();
        log.stage_will_register(&will(9, 4, "/f/x/gone")).unwrap(); // 0: pending
        log.stage_will_register(&will(8, 4, "/f/y/gone")).unwrap(); // 1: will be fired
        log.stage_will_register(&will(6, 4, "/f/z/gone")).unwrap(); // 2: will be fenced
        log.stage_will_register(&will(5, 4, "/f/w/old")).unwrap(); // 3: superseded
        log.stage_will_register(&will(5, 4, "/f/w/new")).unwrap(); // 4: pending
        log.commit_batch().unwrap();
        log.stage_will_fire(will(8, 4, "/f/y/gone")).unwrap(); // 5
        log.stage_publish(6, 5, "/f/z/live".into(), b"live".to_vec())
            .unwrap(); // 6
        log.commit_batch().unwrap();

        // Producer 9's will at offset 0 is pending: nothing above offset 0 may go.
        let err = log.retain_before(Offset(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{err}");
        assert!(
            err.to_string().contains("offset 0") && err.to_string().contains("will"),
            "{err}"
        );
        assert_eq!(log.base(), Offset(0));
        assert!(log.poisoned().is_none());
        // Fire it; then the fired, fenced and superseded ones go, and the pending
        // will at offset 4 holds the floor there.
        log.stage_will_fire(will(9, 4, "/f/x/gone")).unwrap(); // 7
        log.commit_batch().unwrap();
        let err = log.retain_before(Offset(6)).unwrap_err();
        assert!(err.to_string().contains("offset 4"), "{err}");
        assert_eq!(log.retain_before(Offset(4)).unwrap(), 4);
        drop(log);
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.pending_wills(), vec![will(5, 4, "/f/w/new")]);
        drop(log);
    }

    /// A bind the log refuses (too large to store) leaves no binding behind, not
    /// even for the rest of its batch: the same producer can still bind for real.
    #[test]
    fn a_refused_bind_leaves_no_phantom_binding() {
        let (_tmp, path) = tmp("bindphantom");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        let huge = "p".repeat(crate::MAX_RECORD_PAYLOAD);
        assert!(matches!(log.stage_bind(7, huge), Err(StageErr::TooLarge)));
        assert!(log.binding(7).is_none(), "a refused bind left a binding");
        assert_eq!(
            log.stage_bind(7, "alice".into()).unwrap(),
            (Offset(0), false)
        );
        log.commit_batch().unwrap();
        assert_eq!(log.binding(7), Some(("alice", Offset(0))));
        drop(log);
    }

    /// A record refused for size spends none of its producer's distinct-subject
    /// budget, so the next new subject in the same batch still fits under the cap.
    #[test]
    fn a_record_refused_for_size_spends_no_subject_budget() {
        let (_tmp, path) = tmp("budgetphantom");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        for i in 0..MAX_SUBJECTS_PER_PRODUCER as u64 - 1 {
            log.stage_publish(1, i, format!("/a/s/{i}"), Vec::new())
                .unwrap();
        }
        log.commit_batch().unwrap();
        assert_eq!(log.subjects_per_producer(1), MAX_SUBJECTS_PER_PRODUCER - 1);
        let too_big = vec![0u8; crate::MAX_RECORD_PAYLOAD];
        assert!(matches!(
            log.stage_publish(1, 1 << 40, "/a/big".into(), too_big),
            Err(StageErr::TooLarge)
        ));
        assert!(
            log.stage_publish(1, (1 << 40) + 1, "/a/last".into(), b"x".to_vec())
                .is_ok(),
            "the refused record spent the producer's last subject"
        );
        log.commit_batch().unwrap();
        assert_eq!(log.subjects_per_producer(1), MAX_SUBJECTS_PER_PRODUCER);
        drop(log);
    }

    /// Replica staging keeps the follower's log an identical prefix of the leader's:
    /// in-order records extend it, a re-shipped record already held is idempotent, a
    /// gap or a different record at a held offset is refused by offset.
    #[test]
    fn stage_replica_extends_dedups_or_refuses_by_offset() {
        let (_tmp, path) = tmp("replica");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.stage_replica(rec(0, 7, 1)).unwrap(), (Offset(0), false));
        assert_eq!(log.stage_replica(rec(1, 7, 2)).unwrap(), (Offset(1), false));
        // Already held (still staged): idempotent, nothing appended.
        assert_eq!(log.stage_replica(rec(0, 7, 1)).unwrap(), (Offset(0), true));
        log.commit_batch().unwrap();
        // Already held (durable): idempotent.
        assert_eq!(log.stage_replica(rec(1, 7, 2)).unwrap(), (Offset(1), true));
        // A gap is refused, naming the offsets; nothing staged.
        match log.stage_replica(rec(5, 7, 6)) {
            Err(StageErr::Diverged(m)) => assert!(m.contains("gap") && m.contains('5'), "{m}"),
            other => panic!("expected a gap error, got {other:?}"),
        }
        // A DIFFERENT record at a held offset is refused, never overwritten.
        match log.stage_replica(rec(1, 9, 9)) {
            Err(StageErr::Diverged(m)) => assert!(m.contains("offset 1"), "{m}"),
            other => panic!("expected a divergence error, got {other:?}"),
        }
        assert_eq!(log.head(), Offset(2));
        // A replicated commit record applies its group commit on the follower.
        let mut c = rec(2, 0, 0);
        c.subject = COMMIT_SUBJECT.to_string();
        c.body.clear();
        c.commit = Some(GroupCommit {
            group: "g".into(),
            upto: 1,
        });
        assert_eq!(log.stage_replica(c).unwrap(), (Offset(2), false));
        log.commit_batch().unwrap();
        assert_eq!(log.group_start("g"), Offset(2));
        // The replicated dedup map matches the leader's: (7,1) dedups to 0.
        assert_eq!(
            log.stage_publish(7, 1, "/a/s/x".into(), b"dup".to_vec())
                .unwrap(),
            (Offset(0), true)
        );
        drop(log);
    }

    /// The reserved commit subject cannot be published to (batched or not, as a
    /// publish or a transaction output): it would bypass dedup and never be delivered.
    #[test]
    fn reserved_commit_subject_is_refused_for_publishes() {
        let (_tmp, path) = tmp("reserved");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        assert!(matches!(
            log.stage_publish(5, 1, COMMIT_SUBJECT.into(), b"x".to_vec()),
            Err(StageErr::Reserved(_))
        ));
        assert!(matches!(
            log.stage_process_and_produce(5, 1, COMMIT_SUBJECT.into(), vec![], "g".into(), 0),
            Err(StageErr::Reserved(_))
        ));
        let e = log
            .publish(5, 1, COMMIT_SUBJECT.into(), b"x".to_vec())
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(log
            .process_and_produce(5, 1, COMMIT_SUBJECT.into(), vec![], "g".into(), 0)
            .is_err());
        assert_eq!(log.head(), Offset(0), "nothing appended");
        assert!(log.staged.is_empty());
        // A real commit still lands on the reserved subject (and stays out of dedup).
        assert_eq!(log.commit("g".into(), 3).unwrap(), Offset(0));
        assert!(log.dedup.is_empty());
        drop(log);
    }

    /// `fork_shared` swaps exactly one record and SHARES every other one (no payload
    /// copy under the lock).
    #[test]
    fn fork_shared_swaps_one_and_shares_the_rest() {
        let (_tmp, path) = tmp("fork");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        for i in 1..=3 {
            log.publish(1, i, "/a/s/x".into(), format!("m{i}").into_bytes())
                .unwrap();
        }
        let live = log.read_from(Offset(0));
        let alt = log.fork_shared(Offset(1), rec(99, 0, 0));
        assert_eq!(alt.len(), 3);
        assert!(Arc::ptr_eq(&alt[0], &live[0]) && Arc::ptr_eq(&alt[2], &live[2]));
        assert_eq!(
            alt[1].seq,
            Offset(1),
            "the replacement takes the fork offset"
        );
        assert_eq!(alt[1].body, b"b99");
        assert_eq!(live[1].body, b"m2", "the live log is untouched");
        // Out of range: the records as-is.
        assert_eq!(log.fork_shared(Offset(9), rec(0, 0, 0)).len(), 3);
        drop(log);
    }

    // -----------------------------------------------------------------------
    // Replica admission, the will fence and the reserved ack half.
    // -----------------------------------------------------------------------

    /// The `/a/will` body a forged registration would carry.
    fn will_body(subject: &str, body: &[u8]) -> Vec<u8> {
        WillRecord {
            producer_id: 0,
            producer_seq: 0,
            subject: subject.to_string(),
            body: body.to_vec(),
        }
        .encode_body()
    }

    fn replicated(seq: u64, subject: &str, body: Vec<u8>) -> BrokerRecord {
        BrokerRecord {
            seq: Offset(seq),
            producer_id: 4242,
            producer_seq: 7,
            subject: subject.to_string(),
            body,
            commit: None,
        }
    }

    /// A hidden replicated record is refused once THIS BATCH has staged a record,
    /// even though the PROMOTED head — the only thing the connection thread can read
    /// before it queues the op — is still 0.
    ///
    /// This is the whole of the injection: a `/a/will` admitted on the strength of
    /// "the log is still empty" and staged onto a log the broker OWNS, which then
    /// fires it at the next open as an arbitrary publish outside the capability
    /// matrix. The admission test lives where the append does.
    #[test]
    fn a_hidden_replicate_is_refused_once_this_batch_has_staged_a_record() {
        let (_tmp, path) = tmp("injectstaged");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(replica_marker_path(&path));
        let mut log = BrokerLog::open(&path).unwrap();
        assert_eq!(
            log.stage_publish(1, 1, "/f/x/one".into(), b"seed".to_vec())
                .unwrap(),
            (Offset(0), false)
        );
        assert_eq!(
            log.head(),
            Offset(0),
            "the promoted head is still 0: this is exactly what the guard on the \
             connection thread reads while the batch is in flight"
        );
        let rec = replicated(1, WILL_SUBJECT, will_body("/f/pwned/gone", b"owned"));
        match log.stage_replica(rec) {
            Err(StageErr::ReservedReplica(msg)) => {
                assert!(msg.contains("reserved subject /a/will"), "{msg}")
            }
            other => panic!("the /a/will was not refused: {other:?}"),
        }
        // The same for /a/bind, which locks a principal out of attaching for good.
        let rec = replicated(1, BIND_SUBJECT, b"victim".to_vec());
        assert!(matches!(
            log.stage_replica(rec),
            Err(StageErr::ReservedReplica(_))
        ));
        log.commit_batch().unwrap();
        assert_eq!(log.head(), Offset(1), "only the publish landed");
        assert!(log.pending_wills().is_empty(), "a will was injected");
        assert!(!log.is_replica());
        assert!(!replica_marker_path(&path).exists());
        drop(log);
    }

    /// Writing the replica marker never follows a symlink planted at its name: the
    /// file the link points to stays untouched, and the marker is a file of its own.
    #[test]
    #[cfg(unix)]
    fn the_replica_marker_does_not_follow_a_planted_symlink() {
        let (_tmp, path) = tmp("markerlink");
        let marker = replica_marker_path(&path);
        let victim = path.with_extension("victim");
        let _victim = Cleanup::new(&[&victim]);
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&marker);
        std::fs::write(&victim, b"precious").unwrap();
        let mut log = BrokerLog::open(&path).unwrap();
        std::os::unix::fs::symlink(&victim, &marker).unwrap();
        log.declare_replica().unwrap();
        assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
        assert!(!std::fs::symlink_metadata(&marker).unwrap().is_symlink());
        drop(log);
    }

    /// A replicated record REFUSED FOR SIZE does not declare the log a replica. The
    /// marker is one-way, durable and fsynced, and it stops this broker firing any
    /// will ever again; a record the log does not take must not write it. A request
    /// frame is accepted at sizes whose record payload is not, so the attacker picks
    /// the body length — no capability, not one byte appended.
    #[test]
    fn a_replicate_refused_for_size_does_not_declare_the_log_a_replica() {
        let (_tmp, path) = tmp("injectsize");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(replica_marker_path(&path));
        let mut log = BrokerLog::open(&path).unwrap();
        let too_big = replicated(0, "/f/x/one", vec![7u8; crate::MAX_RECORD_PAYLOAD]);
        assert!(matches!(
            log.stage_replica(too_big),
            Err(StageErr::TooLarge)
        ));
        assert_eq!(log.head(), Offset(0), "nothing appended");
        assert!(
            !log.is_replica(),
            "a record the log refused declared it a replica"
        );
        assert!(!replica_marker_path(&path).exists());
        // ...and the log is still an ordinary one: a will registered on it fires.
        let will = WillRecord {
            producer_id: 9,
            producer_seq: 4,
            subject: "/f/x/gone".to_string(),
            body: b"gone".to_vec(),
        };
        log.stage_will_register(&will).unwrap();
        log.commit_batch().unwrap();
        assert_eq!(log.stage_will_fire(will).unwrap(), (Offset(1), false));
        log.commit_batch().unwrap();
        drop(log);
    }

    /// The follower bring-up the rule exists to allow is unchanged: an EMPTY log
    /// takes a leader's first shipped record, is declared a replica by it, and the
    /// rest of that same batch — hidden records included, which is what a guarded
    /// leader's own first records are — lands beside it.
    #[test]
    fn a_leaders_first_batch_still_declares_an_empty_log_and_carries_its_hidden_records() {
        let (_tmp, path) = tmp("bringup");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(replica_marker_path(&path));
        let mut log = BrokerLog::open(&path).unwrap();
        assert_eq!(
            log.stage_replica(replicated(0, "/f/x/one", b"first".to_vec()))
                .unwrap(),
            (Offset(0), false)
        );
        assert!(log.is_replica(), "the first shipped record declares it");
        assert!(replica_marker_path(&path).exists());
        assert_eq!(
            log.stage_replica(replicated(1, BIND_SUBJECT, b"leader-principal".to_vec()))
                .unwrap(),
            (Offset(1), false),
            "a hidden record later in the SAME batch still ships: the log is a replica"
        );
        assert_eq!(
            log.stage_replica(replicated(2, WILL_SUBJECT, will_body("/f/x/gone", b"gone")))
                .unwrap(),
            (Offset(2), false)
        );
        log.commit_batch().unwrap();
        assert_eq!(log.head(), Offset(3));
        drop(log);
    }

    /// A replica takes every record its leader committed: the distinct-subject bound
    /// is the LEADER's admission policy, and a follower's view of it can differ (its
    /// will reservations are rebuilt only when IT reopens). Here the follower still
    /// holds the reservations of every will the leader shipped, while the leader —
    /// reopened, holding only the latest — accepted a publish on a new subject.
    #[test]
    fn a_replica_takes_a_record_its_own_subject_budget_would_refuse() {
        let (_tmp, path) = tmp("replicabound");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(replica_marker_path(&path));
        let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        let n = MAX_SUBJECTS_PER_PRODUCER as u64;
        for i in 0..n {
            let will = will_body(&format!("/f/x/gone{i}"), b"gone");
            log.stage_replica(replicated(i, WILL_SUBJECT, will))
                .unwrap();
        }
        log.commit_batch().unwrap();
        assert_eq!(log.will_reserved_subjects(4242), MAX_SUBJECTS_PER_PRODUCER);
        assert_eq!(
            log.stage_replica(replicated(n, "/f/x/new", b"hello".to_vec()))
                .unwrap(),
            (Offset(n), false),
            "the follower refused a record its leader committed"
        );
        log.commit_batch().unwrap();
        drop(log);
    }

    /// AN ACK IS NOT AN INCARNATION. `ack` keys its record `ACK_SEQ_BASE | offset`,
    /// which is above every sequence a will can hold, so folding acks into the fence's
    /// high water would fence that producer's will permanently — the goodbye would
    /// never land, on the connection's end or on any later open, and the refusal would
    /// reach nobody. The reserved half is excluded on all three paths the fence reads:
    /// the durable fold, the in-flight batch, and the rebuild on open.
    #[test]
    fn an_ack_does_not_raise_the_will_fence() {
        let (_tmp, path) = tmp("ackfence");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        log.publish(7, 5, "/f/x/live".into(), b"state=live".to_vec())
            .unwrap();
        log.publish(7, ACK_SEQ_BASE | 3, "/f/x/answers".into(), b"a".to_vec())
            .unwrap();
        assert_eq!(
            log.producer_high_water(7),
            Some(5),
            "the durable fold took the ack for an incarnation"
        );
        log.stage_publish(7, ACK_SEQ_BASE | 9, "/f/x/answers".into(), b"b".to_vec())
            .unwrap();
        assert_eq!(
            log.producer_high_water(7),
            Some(5),
            "the in-flight batch took the ack for an incarnation"
        );
        log.commit_batch().unwrap();
        drop(log);
        let log = BrokerLog::open(&path).unwrap();
        assert_eq!(
            log.producer_high_water(7),
            Some(5),
            "the rebuild on open took the ack for an incarnation"
        );
        drop(log);
    }

    /// A will may not be registered in the reserved half. The fence does not reach
    /// there, so such a will could never be fenced by a later record — a goodbye its
    /// producer had already outlived would fire over a live node's `state=live`.
    /// Refused at REGISTRATION, the only point where the client is listening.
    #[test]
    fn a_will_in_the_reserved_ack_half_is_refused_at_registration() {
        let (_tmp, path) = tmp("willreserved");
        let _ = std::fs::remove_file(&path);
        let mut log = BrokerLog::open(&path).unwrap();
        let will = WillRecord {
            producer_id: 7,
            producer_seq: ACK_SEQ_BASE | 4,
            subject: "/f/x/gone".to_string(),
            body: b"gone".to_vec(),
        };
        match log.stage_will_register(&will) {
            Err(StageErr::SeqReserved(msg)) => assert!(msg.contains("reserved for acks"), "{msg}"),
            other => panic!("the will was accepted in the ack half: {other:?}"),
        }
        assert_eq!(log.head(), Offset(0), "nothing was appended");
        // One sequence below the reservation is an ordinary will.
        let ok = WillRecord {
            producer_seq: ACK_SEQ_BASE - 1,
            ..will
        };
        log.stage_will_register(&ok).unwrap();
        log.commit_batch().unwrap();
        assert_eq!(log.pending_wills(), vec![ok]);
        drop(log);
    }

    /// `retain_before_carrying_state` compacts past `/a/bind`, pending `/a/will`, and
    /// `/a/commit` records by persisting them in `<log>.state`, and a reopen recovers
    /// the bindings, pending wills, producer sequence fences, and group commits.
    #[cfg(feature = "retention")]
    #[test]
    fn retain_before_carrying_state_preserves_bindings_wills_and_commits_across_restart() {
        let (_tmp, path) = retained_path("retcarry");
        let will = WillRecord {
            producer_id: 9,
            producer_seq: 4,
            subject: "/f/x/gone".to_string(),
            body: b"bye".to_vec(),
        };
        {
            let mut log = BrokerLog::open(&path).unwrap();
            // offset 0: bind producer 7 -> "alice"
            assert_eq!(
                log.stage_bind(7, "alice".into()).unwrap(),
                (Offset(0), false)
            );
            // offset 1: pending will for producer 9
            log.stage_will_register(&will).unwrap();
            // offset 2: group commit for "g1" up to 10
            log.stage_commit("g1".into(), 10).unwrap();
            // offset 3, 4: regular publishes
            log.stage_publish(7, 5, "/a/s/1".into(), b"m1".to_vec())
                .unwrap();
            log.stage_publish(7, 6, "/a/s/2".into(), b"m2".to_vec())
                .unwrap();
            log.commit_batch().unwrap();

            // Standard `retain_before` refuses to drop offset 0 (/a/bind) or 1 (/a/will).
            assert!(log.retain_before(Offset(4)).is_err());

            // State-carrying compaction drops offsets 0..4 and persists state in `<log>.state`.
            assert_eq!(log.retain_before_carrying_state(Offset(4)).unwrap(), 4);
            assert_eq!(log.base(), Offset(4));
            assert_eq!(log.binding(7), Some(("alice", Offset(0))));
            assert_eq!(log.pending_wills(), vec![will.clone()]);
            assert_eq!(log.group_start("g1"), Offset(11));
            // `Last` of the quiet subject `/a/s/1` (written at offset 3 < base 4) still answers!
            let all = Filter::new("/a/s/*").unwrap();
            let (page, _) = log.last_matching(&all, "", 10, 100, log.head().0);
            let got: Vec<(&str, u64, &[u8])> = page
                .iter()
                .map(|r| (r.subject.as_str(), r.seq.0, &r.body[..]))
                .collect();
            assert_eq!(
                got,
                vec![("/a/s/1", 3, &b"m1"[..]), ("/a/s/2", 4, &b"m2"[..])]
            );
        }

        // Reopen from disk: `<log>.state` restores the binding, pending will, high-water
        // fence, group commit, AND quiet-subject `Last` state even though records 0..4
        // were compacted away.
        let mut log = BrokerLog::open(&path).unwrap();
        assert_eq!(log.base(), Offset(4));
        assert_eq!(log.head(), Offset(5));
        assert_eq!(log.binding(7), Some(("alice", Offset(0))));
        assert_eq!(log.pending_wills(), vec![will.clone()]);
        assert_eq!(log.group_start("g1"), Offset(11));
        let all = Filter::new("/a/s/*").unwrap();
        let (page, _) = log.last_matching(&all, "", 10, 100, log.head().0);
        let got: Vec<(&str, u64, &[u8])> = page
            .iter()
            .map(|r| (r.subject.as_str(), r.seq.0, &r.body[..]))
            .collect();
        assert_eq!(
            got,
            vec![("/a/s/1", 3, &b"m1"[..]), ("/a/s/2", 4, &b"m2"[..])]
        );
        // A colliding bind for producer 7 is still refused after restart.
        assert!(matches!(
            log.stage_bind(7, "mallory".into()),
            Err(StageErr::Collision(_))
        ));
        drop(log);
    }

    /// `REQUIREMENTS` R7 acceptance: publish `seq = 10`, restart the log, publish
    /// `seq = 10` again -> `deduped = true` at the original offset; publish 5,000
    /// newer sequences (`11..=5010`), reopen, and publish `seq = 10` again -> still
    /// deduped at the original offset.
    #[test]
    fn r7_dedup_window_survives_restart_and_5000_newer_sequences() {
        let (_tmp, path) = tmp("r7_dedup_window");
        let _ = std::fs::remove_file(&path);
        let orig_off = {
            let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
            let (off, dup) = log
                .publish(42, 10, "/f/fleet/in/a".into(), b"hello".to_vec())
                .unwrap();
            assert!(!dup);
            off
        };
        {
            let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
            let (off, dup) = log
                .publish(42, 10, "/f/fleet/in/a".into(), b"hello".to_vec())
                .unwrap();
            assert!(dup);
            assert_eq!(off, orig_off);
            for seq in 11..=5010u64 {
                log.stage_publish(42, seq, "/f/fleet/in/a".into(), b"x".to_vec())
                    .unwrap();
            }
            log.commit_batch().unwrap();
            let (off2, dup2) = log
                .publish(42, 10, "/f/fleet/in/a".into(), b"hello".to_vec())
                .unwrap();
            assert!(dup2);
            assert_eq!(off2, orig_off);
        }
        {
            let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
            let (off3, dup3) = log
                .publish(42, 10, "/f/fleet/in/a".into(), b"hello".to_vec())
                .unwrap();
            assert!(dup3);
            assert_eq!(off3, orig_off);
        }
    }
    /// Retirement observes all preceding staged records, just as commit will apply them.
    #[test]
    fn staged_retirement_uses_the_preceding_effective_last_owner() {
        let root = crate::test_scratch::Scratch::new("retire-order");
        let subject = "/t/session/presence";
        let mut ordinary =
            BrokerLog::open_with(root.path().join("ordinary"), Durability::Relaxed).unwrap();
        ordinary
            .publish(1, 1, subject.into(), b"one".to_vec())
            .unwrap();
        ordinary.stage_retire(1, subject.into()).unwrap();
        ordinary.commit_batch().unwrap();
        let filter = Filter::new("/t/session/>").unwrap();
        assert!(ordinary
            .last_matching(&filter, "", 8, 8, ordinary.head().0)
            .0
            .is_empty());
        let mut observations = Vec::new();
        for (n, target) in [subject, "/t/session/>"].into_iter().enumerate() {
            let mut fresh =
                BrokerLog::open_with(root.path().join(format!("fresh-{n}")), Durability::Relaxed)
                    .unwrap();
            fresh
                .stage_publish(1, 1, subject.into(), b"new".to_vec())
                .unwrap();
            let retired = fresh.stage_retire(1, target.into());
            let accepted = retired.is_ok_and(|(_, absent)| !absent);
            fresh.commit_batch().unwrap();
            observations.push(
                accepted
                    && fresh
                        .last_matching(&filter, "", 8, 8, fresh.head().0)
                        .0
                        .is_empty(),
            );

            let mut changed = BrokerLog::open_with(
                root.path().join(format!("changed-{n}")),
                Durability::Relaxed,
            )
            .unwrap();
            changed
                .publish(1, 1, subject.into(), b"old owner".to_vec())
                .unwrap();
            changed
                .stage_publish(2, 1, subject.into(), b"new owner".to_vec())
                .unwrap();
            let refused = matches!(
                changed.stage_retire(1, target.into()),
                Err(StageErr::Collision(_))
            );
            changed.commit_batch().unwrap();
            let page = changed.last_matching(&filter, "", 8, 8, changed.head().0).0;
            observations.push(
                refused
                    && page.len() == 1
                    && page[0].producer_id == 2
                    && page[0].body == b"new owner",
            );

            let mut replaced = BrokerLog::open_with(
                root.path().join(format!("replaced-{n}")),
                Durability::Relaxed,
            )
            .unwrap();
            replaced
                .publish(1, 1, subject.into(), b"old".to_vec())
                .unwrap();
            replaced.stage_retire(1, target.into()).unwrap();
            replaced
                .stage_publish(2, 1, subject.into(), b"after retire".to_vec())
                .unwrap();
            let refused = matches!(
                replaced.stage_retire(1, target.into()),
                Err(StageErr::Collision(_))
            );
            replaced.commit_batch().unwrap();
            let page = replaced
                .last_matching(&filter, "", 8, 8, replaced.head().0)
                .0;
            observations.push(
                refused
                    && page.len() == 1
                    && page[0].producer_id == 2
                    && page[0].body == b"after retire",
            );
        }
        assert_eq!(observations, vec![true; 6]);
    }

    /// Zero suffix bytes do not prove that a complete, corrupted record was never committed.
    #[test]
    fn a_complete_zero_damaged_record_is_refused_without_truncation() {
        let root = crate::test_scratch::Scratch::new("zero-corrupt");
        let path = root.path().join("log");
        {
            let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
            log.publish(1, 1, "/t/one".into(), b"first".to_vec())
                .unwrap();
            log.publish(1, 2, "/t/two".into(), b"ends in zero\0\0".to_vec())
                .unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        let intact = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        assert_eq!(
            intact.head(),
            Offset(2),
            "complete trailing-zero payload is valid"
        );
        drop(intact);
        let next = rec(2, 1, 3).encode().unwrap();
        let mut incomplete = original.clone();
        incomplete.extend_from_slice(&next[..20]);
        std::fs::write(&path, &incomplete).unwrap();
        let recovered = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        assert_eq!(
            recovered.head(),
            Offset(2),
            "actually incomplete frame is still a torn tail"
        );
        drop(recovered);
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let first_len = Frame::decode(&original).unwrap().unwrap().consumed;
        let mut damaged = original.clone();
        damaged[first_len + astream_wire::HEADER_SIZE..].fill(0);
        std::fs::write(&path, &damaged).unwrap();
        let opened = BrokerLog::open_with(&path, Durability::Relaxed);
        let refused = opened
            .as_ref()
            .is_err_and(|e| e.kind() == std::io::ErrorKind::InvalidData);
        drop(opened);
        assert_eq!(
            (refused, std::fs::read(&path).unwrap() == damaged),
            (true, true)
        );
    }

    /// A torn header that kept its first four bytes: magic, version, flags, then the
    /// length and CRC fields as zeros. It decodes as a CRC-valid EMPTY frame (the CRC-32
    /// of nothing is 0) — which is exactly what the codec writes for an empty payload,
    /// so these are not hand-rolled bytes that merely resemble it.
    fn torn_header() -> Vec<u8> {
        use astream_wire::frame::{MAGIC, VERSION};
        let bytes = vec![MAGIC[0], MAGIC[1], VERSION, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(Frame::new(Vec::new()).encode().unwrap(), bytes);
        bytes
    }

    /// The empty frame at the scan position takes the classification of a frame that
    /// does not decode, by name — never "record payload does not parse", which reads as
    /// an older record format. Under that classification (trailing zeros prove no
    /// interrupted write: `a_complete_zero_damaged_record_is_refused_without_truncation`)
    /// it is refused whether a zeroed extent, an intact record, or nothing follows it:
    /// its magic is never zero. An all-zero tail is still torn, through the same
    /// classification.
    #[test]
    fn an_empty_frame_at_the_scan_position_is_classified_as_an_undecodable_frame() {
        let mut good = Vec::new();
        for i in 0..2 {
            good.extend_from_slice(&rec(i, 1, i + 1).encode().unwrap());
        }
        let mut alone = good.clone();
        alone.extend_from_slice(&torn_header());
        let mut extent = alone.clone();
        extent.extend_from_slice(&[0u8; 4096]);
        let mut followed = alone.clone();
        followed.extend_from_slice(&rec(2, 1, 3).encode().unwrap());
        for (name, bytes) in [
            ("alone", &alone),
            ("zeroed extent", &extent),
            ("intact record behind it", &followed),
        ] {
            let s = scan(bytes, None, 0);
            assert_eq!((s.records.len(), s.valid_len), (2, good.len()), "{name}");
            match &s.tail {
                Tail::Corrupt { offset, reason, .. } => assert!(
                    *offset == 2 && reason.contains("an empty frame: a torn header, or damage"),
                    "{name}: {reason}"
                ),
                other => panic!("{name}: an empty frame must be refused, got {other:?}"),
            }
        }
        let mut zeros = good.clone();
        zeros.extend_from_slice(&[0u8; 64]);
        assert_eq!(scan(&zeros, None, 0).tail, Tail::Torn);
    }

    /// On a real file whose durable floor is UNKNOWN (no identity sidecar: the rules that
    /// predate the floor): the open refuses the empty frame naming it and the offset, the
    /// file untouched, and `open_repair` cuts exactly from it — the 12 bytes, or the 12
    /// bytes and the record behind them. With the floor KNOWN (the sidecar of the open
    /// that wrote the log), the same bytes are a torn batch: the header alone is cut and
    /// nothing moves (a write that never finished), while with an intact record of the
    /// torn batch behind it the cut removes that record and moves the generation.
    #[test]
    fn an_empty_frame_is_refused_by_name_and_repair_cuts_from_it() {
        let root = crate::test_scratch::Scratch::new("empty-frame");
        let path = root.path().join("log");
        let (clean, seen) = {
            let mut log = BrokerLog::open_with(&path, Durability::Strict).unwrap();
            for i in 1..=2 {
                log.publish(1, i, "/a/s/x".into(), format!("b{i}").into_bytes())
                    .unwrap();
            }
            (log.bytes(), log.identity())
        };
        let sidecar = std::fs::read(logid::sidecar_path(&path)).unwrap();
        let next = rec(2, 1, 3).encode().unwrap();
        let header = torn_header();
        for (behind, moves) in [(&[][..], false), (&next[..], true)] {
            let mut damaged = clean.clone();
            damaged.extend_from_slice(&header);
            damaged.extend_from_slice(behind);
            std::fs::write(&path, &damaged).unwrap();
            std::fs::write(logid::sidecar_path(&path), &sidecar).unwrap();
            let log = BrokerLog::open_with(&path, Durability::Strict)
                .expect("with the floor known, a torn batch opens");
            assert_eq!(log.head(), Offset(2));
            assert_eq!(log.identity().id, seen.id);
            assert_eq!(
                log.identity().generation,
                seen.generation + u64::from(moves)
            );
            drop(log);
            assert_eq!(
                std::fs::read(&path).unwrap(),
                clean,
                "the torn batch was cut"
            );

            std::fs::write(&path, &damaged).unwrap();
            std::fs::remove_file(logid::sidecar_path(&path)).unwrap();
            let err = BrokerLog::open_with(&path, Durability::Strict)
                .err()
                .expect("an empty frame must be refused");
            let msg = err.to_string();
            assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{msg}");
            assert!(
                msg.contains("offset 2") && msg.contains("an empty frame: a torn header"),
                "{msg}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                damaged,
                "the file is untouched"
            );
            let (log, dropped) = BrokerLog::open_repair(&path, Durability::Strict).unwrap();
            assert_eq!(
                (dropped, log.head()),
                ((header.len() + behind.len()) as u64, Offset(2))
            );
            drop(log);
            assert_eq!(std::fs::read(&path).unwrap(), clean);
            std::fs::remove_file(logid::sidecar_path(&path)).unwrap();
        }
    }

    /// A zero-length frame is never a "later record": its CRC holds for any magic and
    /// version followed by eight zero bytes. Plaintext never counted one (it does not
    /// parse); under a key every CRC-valid frame counted, so such bytes in a torn tail
    /// turned the torn write into a "rotted length field". The control: a non-empty
    /// CRC-valid frame still counts under a key, so a real length rot is still caught.
    #[test]
    fn an_empty_frame_is_never_a_later_record() {
        const KEY: [u8; 32] = [0x31; 32];
        // A frame header whose length (200) runs past the end of these bytes.
        let mut partial = torn_header();
        partial[4] = 200;
        partial[8..12].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        partial.extend_from_slice(b"partial");
        let mut with_empty = partial.clone();
        with_empty.extend_from_slice(&torn_header());
        let mut with_frame = partial.clone();
        with_frame.extend_from_slice(&Frame::new(vec![7; 40]).encode().unwrap());
        assert_eq!(
            [
                later_record_exists(&with_empty, None),
                later_record_exists(&with_empty, Some(&KEY)),
                later_record_exists(&with_frame, Some(&KEY)),
            ],
            [false, false, true]
        );
    }

    /// The same on a real encrypted file. A torn tail with an empty frame inside it
    /// opens with the tail cut, where it used to be refused as a rotted length; the
    /// control — a sealed record's frame behind the same partial frame — is still
    /// refused, naming the length field, while the log's durable floor is unknown (no
    /// identity sidecar). And an empty frame at the scan position of an encrypted log is
    /// refused by name, not as "wrong at-rest key": it is checked before the payload is
    /// decrypted. With the floor known, both are a torn batch — the record behind the
    /// partial frame authenticates at the offset the torn batch starts at, and is that
    /// batch's own seal — and open with it cut.
    #[cfg(feature = "at-rest")]
    #[test]
    fn an_empty_frame_in_an_encrypted_torn_tail_is_not_a_later_record() {
        const KEY: [u8; 32] = [0x32; 32];
        let root = crate::test_scratch::Scratch::new("empty-frame-keyed");
        let path = root.path().join("log");
        let (clean, sealed_record) = {
            let mut log = BrokerLog::open_encrypted(&path, KEY).unwrap();
            for i in 1..=3 {
                log.publish(1, i, "/a/s/x".into(), format!("b{i}").into_bytes())
                    .unwrap();
            }
            let bytes = log.bytes();
            let first = Frame::decode(&bytes).unwrap().unwrap().consumed;
            let second = first + Frame::decode(&bytes[first..]).unwrap().unwrap().consumed;
            // Keep two records; the third's frame is a real sealed record to plant.
            (bytes[..second].to_vec(), bytes[second..].to_vec())
        };
        let sidecar = std::fs::read(logid::sidecar_path(&path)).unwrap();
        // The floor known (the sidecar the writing open left), or not (none at all).
        let floor_known = |known: bool| {
            if known {
                std::fs::write(logid::sidecar_path(&path), &sidecar).unwrap();
            } else {
                let _ = std::fs::remove_file(logid::sidecar_path(&path));
            }
        };
        let mut partial = torn_header();
        partial[4] = 200; // runs past the end of everything appended below
        partial[8..12].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        partial.extend_from_slice(b"partial");

        let mut torn = clean.clone();
        torn.extend_from_slice(&partial);
        torn.extend_from_slice(&torn_header());
        std::fs::write(&path, &torn).unwrap();
        let log = BrokerLog::open_encrypted(&path, KEY).expect("a torn tail must open");
        assert_eq!(log.head(), Offset(2));
        drop(log);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            clean,
            "the torn tail was cut"
        );

        let mut rotted = clean.clone();
        rotted.extend_from_slice(&partial);
        rotted.extend_from_slice(&sealed_record);
        std::fs::write(&path, &rotted).unwrap();
        floor_known(true);
        let log = BrokerLog::open_encrypted(&path, KEY).expect("a torn batch opens");
        assert_eq!(log.head(), Offset(2));
        drop(log);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            clean,
            "the torn batch was cut"
        );
        std::fs::write(&path, &rotted).unwrap();
        floor_known(false);
        let err = BrokerLog::open_encrypted(&path, KEY)
            .err()
            .expect("a rotted length with a sealed record behind it must be refused");
        assert!(err.to_string().contains("length"), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            rotted,
            "the file is untouched"
        );

        let mut at_scan = clean.clone();
        at_scan.extend_from_slice(&torn_header());
        std::fs::write(&path, &at_scan).unwrap();
        floor_known(true);
        let log = BrokerLog::open_encrypted(&path, KEY).expect("a torn header opens");
        assert_eq!(log.head(), Offset(2));
        drop(log);
        std::fs::write(&path, &at_scan).unwrap();
        floor_known(false);
        let err = BrokerLog::open_encrypted(&path, KEY)
            .err()
            .expect("an empty frame must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("an empty frame") && !msg.contains("authenticate"),
            "{msg}"
        );
    }

    /// A verified (anti-rollback) open at any tier but Strict is refused
    /// `InvalidInput`, naming both, before the file is created or anything is
    /// prepared: a weaker tier's power loss leaves the log below a watermark that did
    /// persist. The control: the same opens at Strict succeed.
    #[cfg(feature = "anti-rollback")]
    #[test]
    fn a_watermark_open_refuses_every_tier_but_strict() {
        const KEY: [u8; 32] = [0x33; 32];
        let root = crate::test_scratch::Scratch::new("anti-rollback-tier");
        let path = root.path().join("log");
        for (mode, tier, named) in [
            (AntiRollbackMode::Init, Durability::Relaxed, "Relaxed"),
            (AntiRollbackMode::Verify, Durability::Relaxed, "Relaxed"),
            (AntiRollbackMode::Init, Durability::ordered(), "Ordered"),
            (AntiRollbackMode::Verify, Durability::ordered(), "Ordered"),
        ] {
            let mut prepared = false;
            let err = BrokerLog::open_core(
                Storage::Os(&path),
                &OpenOptions::new(tier),
                false,
                Some(KEY),
                mode,
                || {
                    prepared = true;
                    Ok(Prepared::Nothing)
                },
            )
            .err()
            .expect("a weak-tier verified open must be refused");
            let msg = err.to_string();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{msg}");
            assert!(
                msg.contains("anti-rollback") && msg.contains("Strict") && msg.contains(named),
                "{msg}"
            );
            assert!(!prepared, "refused before anything was prepared");
            assert!(!path.exists(), "refused before the file was created");
            assert!(!watermark_path(&path).exists());
        }
        let (log, _) = BrokerLog::open_inner(
            &path,
            Durability::Strict,
            false,
            Some(KEY),
            AntiRollbackMode::Init,
        )
        .expect("Strict establishes the watermark");
        drop(log);
        let (log, _) = BrokerLog::open_inner(
            &path,
            Durability::Strict,
            false,
            Some(KEY),
            AntiRollbackMode::Verify,
        )
        .expect("Strict verifies it");
        drop(log);
    }

    #[cfg(feature = "retention")]
    #[test]
    fn state_sidecar_is_one_bounded_regular_frame() {
        let root = crate::test_scratch::Scratch::new("state-frame");
        let log = root.path().join("log");
        assert!(read_state(&log, None, 0).unwrap().bind.is_empty());
        let mut state = CompactedState::default();
        state.bind.insert(7, ("owned".into(), Offset(3)));
        state.high_water.insert(7, 19);
        write_state(&log, None, 4, &state).unwrap();
        let got = read_state(&log, None, 4).unwrap();
        assert_eq!(got.bind.get(&7), Some(&("owned".into(), Offset(3))));
        assert_eq!(got.high_water.get(&7), Some(&19));
        let sp = state_path(&log);
        let valid = std::fs::read(&sp).unwrap();
        let mut extra = valid.clone();
        extra.push(0);
        std::fs::write(&sp, extra).unwrap();
        assert!(
            read_state(&log, None, 4).is_err(),
            "one frame, not a prefix"
        );
        std::fs::write(&sp, &valid[..valid.len() - 1]).unwrap();
        assert!(read_state(&log, None, 4).is_err(), "truncated frame");
        let file = File::create(&sp).unwrap();
        file.set_len((astream_wire::HEADER_SIZE + astream_wire::MAX_PAYLOAD_LEN + 1) as u64)
            .unwrap();
        drop(file);
        let oversized = std::fs::metadata(&sp).unwrap().len();
        assert!(read_state(&log, None, 4).is_err(), "oversized file");
        assert_eq!(std::fs::metadata(&sp).unwrap().len(), oversized);
        std::fs::remove_file(&sp).unwrap();
        std::fs::create_dir(&sp).unwrap();
        assert!(
            read_state(&log, None, 4).is_err(),
            "directory is not absence"
        );
    }

    #[cfg(all(unix, feature = "retention"))]
    #[test]
    fn state_sidecar_never_adopts_a_symlink() {
        let root = crate::test_scratch::Scratch::new("state-link");
        let log = root.path().join("log");
        assert!(read_state(&log, None, 0).unwrap().bind.is_empty());
        let mut state = CompactedState::default();
        state.bind.insert(7, ("private".into(), Offset(0)));
        write_state(&log, None, 1, &state).unwrap();
        assert_eq!(
            read_state(&log, None, 1).unwrap().bind.get(&7).unwrap().0,
            "private"
        );
        let sp = state_path(&log);
        let target = root.path().join("private-state");
        std::fs::rename(&sp, &target).unwrap();
        let original = std::fs::read(&target).unwrap();
        std::os::unix::fs::symlink(&target, &sp).unwrap();
        let valid_link_refused = read_state(&log, None, 1).is_err();
        std::fs::remove_file(&sp).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), &sp).unwrap();
        let dangling_refused = read_state(&log, None, 1).is_err();
        assert_eq!(
            (
                valid_link_refused,
                dangling_refused,
                std::fs::read(&target).unwrap() == original
            ),
            (true, true, true)
        );
    }
}

/// Model-based verification: run random sequences of staged operations through BOTH the
/// real file-backed `BrokerLog` (the group-commit `stage_*` + `commit_batch` path) AND
/// an in-memory reference model of the log's observable semantics (the SPEC), and
/// assert they agree — at every flush, and after recovery from a simulated crash.
///
/// The model is a deliberately-trivial spec (a `Vec` + two `HashMap`s); the real log
/// adds encoding, fsync, the recovery scan, and rollback. Agreement over many seeded
/// random sequences cross-checks offset assignment, exactly-once dedup (the durable AND
/// the in-batch dedup maps are compared directly, so the commit-subject-skips-dedup
/// rule is load-bearing: producer 0 publishes are in the mix and a commit record's
/// (0,0) key must NOT shadow them), the group-commit `.max()` monotonicity, the
/// reserved-subject refusal, and CRASH RECOVERY: a random step drops the log with an
/// unsynced staged batch and cuts the file mid-frame (or zero-fills the tail), and the
/// reopened log must equal the model's rule "the staged records wholly on disk landed;
/// the torn one and everything after it did not". The model shares the impl's staging
/// vocabulary (it is a spec of the same interface, not a foreign oracle); what it
/// independently pins down is the observable state after every flush and crash.
/// Std-only (a seeded xorshift PRNG): reproducible, and no third-party dev-dependency.
#[cfg(test)]
mod model_tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Seek, SeekFrom};

    /// Removes a seed's log when dropped, so a failing seed leaves nothing behind.
    struct Cleanup(PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(replica_marker_path(&self.0));
            let _ = std::fs::remove_file(state_path(&self.0));
        }
    }

    /// xorshift64* — a tiny deterministic PRNG (std-only, reproducible per seed).
    struct Rng(u64);
    impl Rng {
        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }
    }

    /// The reference SPEC of the log's observable semantics. `durable` mirrors
    /// `read_from(0)` (offset, subject, body) for EVERY record (commit records too, on
    /// the unified axis); `pending` is the not-yet-fsync'd batch.
    #[derive(Default)]
    struct Model {
        durable: Vec<(u64, String, Vec<u8>)>,
        dedup: HashMap<(u64, u64), u64>,
        gc: HashMap<String, u64>,
        next: u64,
        pending: Vec<Rec>,
        pdedup: HashMap<(u64, u64), u64>,
    }
    struct Rec {
        off: u64,
        dedup_key: Option<(u64, u64)>, // Some iff subject != COMMIT_SUBJECT
        pid: u64,
        pseq: u64,
        subject: String,
        body: Vec<u8>,
        commit: Option<(String, u64)>,
    }
    impl Model {
        fn dup_offset(&self, key: (u64, u64)) -> Option<u64> {
            self.dedup
                .get(&key)
                .or_else(|| self.pdedup.get(&key))
                .copied()
        }
        fn stage_publish(
            &mut self,
            pid: u64,
            pseq: u64,
            subject: String,
            body: Vec<u8>,
        ) -> (u64, bool) {
            if let Some(o) = self.dup_offset((pid, pseq)) {
                return (o, true);
            }
            let off = self.next + self.pending.len() as u64;
            self.pdedup.insert((pid, pseq), off);
            self.pending.push(Rec {
                off,
                dedup_key: Some((pid, pseq)),
                pid,
                pseq,
                subject,
                body,
                commit: None,
            });
            (off, false)
        }
        fn stage_commit(&mut self, group: String, upto: u64) -> u64 {
            let off = self.next + self.pending.len() as u64;
            self.pending.push(Rec {
                off,
                dedup_key: None,
                pid: 0,
                pseq: 0,
                subject: COMMIT_SUBJECT.to_string(),
                body: Vec::new(),
                commit: Some((group, upto)),
            });
            off
        }
        #[allow(clippy::too_many_arguments)]
        fn stage_rpw(
            &mut self,
            pid: u64,
            pseq: u64,
            subject: String,
            body: Vec<u8>,
            group: String,
            upto: u64,
        ) -> (u64, bool) {
            if let Some(o) = self.dup_offset((pid, pseq)) {
                return (o, true);
            }
            let off = self.next + self.pending.len() as u64;
            self.pdedup.insert((pid, pseq), off);
            self.pending.push(Rec {
                off,
                dedup_key: Some((pid, pseq)),
                pid,
                pseq,
                subject,
                body,
                commit: Some((group, upto)),
            });
            (off, false)
        }
        fn promote(&mut self, r: Rec) {
            if let Some(k) = r.dedup_key {
                self.dedup.insert(k, r.off);
            }
            if let Some((g, u)) = &r.commit {
                let e = self.gc.entry(g.clone()).or_insert(0);
                *e = (*e).max(*u);
            }
            self.next += 1;
            self.durable.push((r.off, r.subject, r.body));
        }
        fn commit_batch(&mut self) {
            for r in std::mem::take(&mut self.pending) {
                self.promote(r);
            }
            self.pdedup.clear();
        }
        /// The crash rule: the first `landed` staged records were wholly on disk and
        /// are recovered as durable (an un-acked write that landed); the rest are gone.
        fn crash(&mut self, landed: usize) {
            let pending = std::mem::take(&mut self.pending);
            for (i, r) in pending.into_iter().enumerate() {
                if i < landed {
                    self.promote(r);
                }
            }
            self.pdedup.clear();
        }
        fn group_start(&self, g: &str) -> u64 {
            self.gc.get(g).map(|u| u.saturating_add(1)).unwrap_or(0)
        }
        /// The on-disk frame length of each pending record, from the SAME encoder the
        /// log uses (the cut points must fall on real frame boundaries).
        fn pending_frame_lens(&self) -> Vec<usize> {
            self.pending
                .iter()
                .map(|r| {
                    BrokerRecord {
                        seq: Offset(r.off),
                        producer_id: r.pid,
                        producer_seq: r.pseq,
                        subject: r.subject.clone(),
                        body: r.body.clone(),
                        commit: r.commit.as_ref().map(|(g, u)| GroupCommit {
                            group: g.clone(),
                            upto: *u,
                        }),
                    }
                    .encode()
                    .unwrap()
                    .len()
                })
                .collect::<Vec<usize>>()
                .into_iter()
                .enumerate()
                // The batch's last record carries its seal, every other the one-byte
                // continues mark.
                .map(|(i, len)| {
                    len + if i + 1 == self.pending.len() {
                        crate::brecord::SEAL_LEN
                    } else {
                        1
                    }
                })
                .collect()
        }
    }

    fn stage_pub(log: &mut BrokerLog, pid: u64, pseq: u64, s: String, b: Vec<u8>) -> (u64, bool) {
        let (o, d) = log
            .stage_publish(pid, pseq, s, b)
            .expect("tiny op never errors");
        (o.0, d)
    }

    fn sorted_map(m: &HashMap<(u64, u64), Offset>) -> Vec<((u64, u64), u64)> {
        let mut v: Vec<_> = m.iter().map(|(k, o)| (*k, o.0)).collect();
        v.sort_unstable();
        v
    }
    fn sorted_model_map(m: &HashMap<(u64, u64), u64>) -> Vec<((u64, u64), u64)> {
        let mut v: Vec<_> = m.iter().map(|(k, o)| (*k, *o)).collect();
        v.sort_unstable();
        v
    }

    /// The in-flight (staged) view must match after every stage op.
    fn compare_staged(log: &BrokerLog, model: &Model, ctx: &str) {
        assert_eq!(
            sorted_map(&log.staged_dedup),
            sorted_model_map(&model.pdedup),
            "{ctx}: in-batch dedup map"
        );
        assert_eq!(log.staged.len(), model.pending.len(), "{ctx}: staged count");
    }

    fn compare(log: &BrokerLog, model: &Model, ctx: &str) {
        assert_eq!(log.head().0, model.next, "{ctx}: head");
        let recs: Vec<(u64, String, Vec<u8>)> = log
            .read_from(Offset(0))
            .into_iter()
            .map(|r| (r.seq.0, r.subject.clone(), r.body.clone()))
            .collect();
        assert_eq!(recs, model.durable, "{ctx}: durable records");
        // The dedup map itself, not just its effect on a few probes: commit records
        // must be absent from it and every publish/rpw key present at its offset.
        assert_eq!(
            sorted_map(&log.dedup),
            sorted_model_map(&model.dedup),
            "{ctx}: durable dedup map"
        );
        for g in ["g0", "g1", "g2"] {
            assert_eq!(
                log.group_start(g).0,
                model.group_start(g),
                "{ctx}: group_start {g}"
            );
        }
        compare_staged(log, model, ctx);
    }

    #[test]
    fn staged_log_matches_reference_model_over_random_sequences() {
        // Every step is a real file write and every commit a full flush, so each seed
        // costs device time. Counted on these forty seeds: each of the six steps runs
        // 120 times or more, and the 146 crashes leave a zero-filled tail 12 times, a
        // torn prefix 40, a whole batch landed 35 and nothing staged 59. More seeds add
        // repetitions of these, not another kind of step or crash.
        const SEEDS: u64 = 40;
        let pid_proc = std::process::id();
        for seed in 0..SEEDS {
            let path = std::env::temp_dir().join(format!("astream_model_{pid_proc}_{seed}.log"));
            let _ = std::fs::remove_file(&path);
            let _tmp = Cleanup(path.clone());

            let mut model = Model::default();
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            {
                let mut log = BrokerLog::open(&path).unwrap();
                let steps = 6 + rng.below(26);
                for _ in 0..steps {
                    match rng.below(6) {
                        0 => {
                            // producer 0 is in the mix: its keys must never be shadowed
                            // by a commit record's (0,0) identity.
                            let pid = rng.below(5);
                            let pseq = rng.below(7);
                            let subj = format!("/a/s/{}", rng.below(3));
                            let body = vec![rng.below(256) as u8];
                            let r = stage_pub(&mut log, pid, pseq, subj.clone(), body.clone());
                            let m = model.stage_publish(pid, pseq, subj, body);
                            assert_eq!(r, m, "seed {seed}: stage_publish");
                            compare_staged(&log, &model, &format!("seed {seed} after publish"));
                        }
                        1 => {
                            let group = format!("g{}", rng.below(3));
                            let upto = rng.below(50);
                            let r = log.stage_commit(group.clone(), upto).unwrap().0;
                            let m = model.stage_commit(group, upto);
                            assert_eq!(r, m, "seed {seed}: stage_commit");
                            compare_staged(&log, &model, &format!("seed {seed} after commit"));
                        }
                        2 => {
                            let pid = rng.below(5);
                            let pseq = rng.below(7);
                            let subj = format!("/a/out/{}", rng.below(3));
                            let body = vec![rng.below(256) as u8];
                            let group = format!("g{}", rng.below(3));
                            let upto = rng.below(50);
                            let (o, d) = log
                                .stage_process_and_produce(
                                    pid,
                                    pseq,
                                    subj.clone(),
                                    body.clone(),
                                    group.clone(),
                                    upto,
                                )
                                .expect("tiny op never errors");
                            let m = model.stage_rpw(pid, pseq, subj, body, group, upto);
                            assert_eq!((o.0, d), m, "seed {seed}: stage_rpw");
                            compare_staged(&log, &model, &format!("seed {seed} after rpw"));
                        }
                        3 => {
                            // The reserved subject is refused and changes nothing.
                            let pid = rng.below(5);
                            let pseq = rng.below(7);
                            assert!(matches!(
                                log.stage_publish(pid, pseq, COMMIT_SUBJECT.into(), vec![1]),
                                Err(StageErr::Reserved(_))
                            ));
                            compare_staged(&log, &model, &format!("seed {seed} after reserved"));
                        }
                        4 => {
                            // CRASH with an unsynced staged batch on disk: the process
                            // dies and the tail is cut mid-frame (a torn write) or
                            // zero-filled. Recovery must equal the model's crash rule.
                            let lens = model.pending_frame_lens();
                            let durable_len = log.bytes().len();
                            let landed = rng.below(lens.len() as u64 + 1) as usize;
                            let mut keep = durable_len + lens[..landed].iter().sum::<usize>();
                            // Zero-fill (the file extended, the data blocks never
                            // landed) starts at a frame boundary; a prefix cut (the
                            // write stopped mid-frame) can fall anywhere in the frame.
                            let zero_fill = landed < lens.len() && rng.below(3) == 0;
                            if landed < lens.len() && !zero_fill {
                                keep += rng.below(lens[landed] as u64) as usize;
                            }
                            // The process dies inside `commit_batch`, between the
                            // batch's write and its fsync: the batch's bytes, as that
                            // write puts them (its last record sealed), reached the
                            // file, and the crash keeps only a prefix of them.
                            let staged = log.staged_bytes_as_written();
                            assert_eq!(staged.len(), lens.iter().sum::<usize>());
                            drop(log);
                            FsOpenOptions::new()
                                .append(true)
                                .open(&path)
                                .unwrap()
                                .write_all(&staged)
                                .unwrap();
                            {
                                let f = FsOpenOptions::new().write(true).open(&path).unwrap();
                                if zero_fill {
                                    let total = f.metadata().unwrap().len() as usize;
                                    let mut f = f;
                                    f.seek(SeekFrom::Start(keep as u64)).unwrap();
                                    f.write_all(&vec![0u8; total - keep]).unwrap();
                                } else {
                                    f.set_len(keep as u64).unwrap();
                                }
                            }
                            log = BrokerLog::open(&path).unwrap();
                            // A batch is kept whole or not at all: its seal is on its
                            // last record, so a prefix of it is a torn batch, cut at its
                            // start, and only one that landed entirely survives.
                            let kept = if landed == lens.len() { landed } else { 0 };
                            model.crash(kept);
                            assert_eq!(
                                log.bytes().len(),
                                durable_len + lens[..kept].iter().sum::<usize>(),
                                "seed {seed}: the torn batch was cut at its start"
                            );
                            compare(&log, &model, &format!("seed {seed} after crash"));
                        }
                        _ => {
                            log.commit_batch().unwrap();
                            model.commit_batch();
                            compare(&log, &model, &format!("seed {seed} mid-flush"));
                        }
                    }
                }
                log.commit_batch().unwrap();
                model.commit_batch();
                compare(&log, &model, &format!("seed {seed} final-flush"));
            }

            // RECOVERY: reopen from disk; the recovered state must equal the model.
            let mut log2 = BrokerLog::open(&path).unwrap();
            compare(&log2, &model, &format!("seed {seed} after-reopen"));
            // dedup survived the restart: every known key re-stages as a deduped hit to
            // its original offset (and appends nothing).
            for (&(pid, pseq), &off) in model.dedup.iter() {
                let (o, d) = stage_pub(&mut log2, pid, pseq, "/a/s/0".into(), vec![0]);
                assert_eq!(
                    (o, d),
                    (off, true),
                    "seed {seed}: dedup recovered for ({pid},{pseq})"
                );
            }
            log2.abort_batch(); // discard the throwaway dedup probes (none staged)
        }
    }

    #[test]
    fn retire_subject_reclaims_producer_budget_live_and_across_reopen() {
        let path = std::env::temp_dir().join(format!("astream_retire_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _tmp = Cleanup(path.clone());
        let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        let pid = 42u64;
        log.publish(
            pid,
            1,
            "/f/local/pub/n-1/s1/presence".into(),
            b"live".to_vec(),
        )
        .unwrap();
        log.publish(pid, 2, "/f/local/pub/n-1/s1/ev".into(), b"started".to_vec())
            .unwrap();
        assert_eq!(log.subjects_per_producer(pid), 2);

        // Another producer cannot retire pid's subject.
        assert!(log
            .retire_subject(99, "/f/local/pub/n-1/s1/presence")
            .is_err());
        assert_eq!(log.subjects_per_producer(pid), 2);

        // Retiring the exited session's two subjects removes them from `Last` and
        // decrements `subjects_per_producer` back to 0.
        assert!(log
            .retire_subject(pid, "/f/local/pub/n-1/s1/presence")
            .unwrap()
            .is_some());
        assert_eq!(log.subjects_per_producer(pid), 1);
        // Idempotent second retire on the same subject appends nothing.
        assert!(log
            .retire_subject(pid, "/f/local/pub/n-1/s1/presence")
            .unwrap()
            .is_none());
        assert!(log
            .retire_subject(pid, "/f/local/pub/n-1/s1/ev")
            .unwrap()
            .is_some());
        assert_eq!(log.subjects_per_producer(pid), 0);

        let filter = Filter::new("/f/local/pub/n-1/>").unwrap();
        let (rows, _) = log.last_matching(&filter, "", 16, 16, log.head().0);
        assert!(rows.is_empty(), "retired subjects are removed from Last");
        drop(log);

        // Reopen from disk: the `/a/retire` records replay and keep the budget at 0.
        let log2 = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        assert_eq!(log2.subjects_per_producer(pid), 0);
        let (rows2, _) = log2.last_matching(&filter, "", 16, 16, log2.head().0);
        assert!(rows2.is_empty());
    }

    #[test]
    fn epoch_fenced_replication_and_watermark_visibility() {
        let path =
            std::env::temp_dir().join(format!("astream_epoch_wm_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let _tmp = Cleanup(path.clone());
        let rec = |seq: u64, pid: u64, pseq: u64| BrokerRecord {
            seq: Offset(seq),
            producer_id: pid,
            producer_seq: pseq,
            subject: "/a/s/x".to_string(),
            body: format!("b{seq}").into_bytes(),
            commit: None,
        };
        let mut log = BrokerLog::open_with(&path, Durability::Relaxed).unwrap();
        assert_eq!(log.replication_epoch(), 0);
        assert_eq!(log.committed_watermark_offset(), Offset(0));

        // Append two records under leader epoch 3 via `replicate_fenced`.
        assert_eq!(
            log.replicate_fenced(3, 0, rec(0, 1, 1)).unwrap(),
            (Offset(0), false)
        );
        assert_eq!(log.replication_epoch(), 3);
        assert_eq!(
            log.replicate_fenced(3, 1, rec(1, 1, 2)).unwrap(),
            (Offset(1), false)
        );
        assert_eq!(log.head(), Offset(2));

        // Stale leader epoch 2 is fenced (`epoch_is_fenced(3, 2) == true`).
        assert!(log.replicate_fenced(2, 2, rec(2, 1, 3)).is_err());
        assert!(log.advance_replication_epoch(2).is_err());
        assert_eq!(log.head(), Offset(2));

        // Higher leader epoch 5 advances the epoch and appends offset 2.
        assert_eq!(
            log.replicate_fenced(5, 2, rec(2, 1, 3)).unwrap(),
            (Offset(2), false)
        );
        assert_eq!(log.replication_epoch(), 5);
        assert_eq!(log.head(), Offset(3));

        // Before replica acks arrive, committed_watermark_offset is 0, so
        // fetch_replicated_committed surfaces 0 records even though head == 3.
        let filter = Filter::new("/a/s/>").unwrap();
        let (visible0, _) = log.fetch_replicated_committed(0, &filter, 16, 16);
        assert!(visible0.is_empty());
        assert!(!log.is_offset_replicated_committed(Offset(0)));

        // Replica ack up to 2 commits offsets 0 and 1, leaving offset 2 uncommitted.
        assert_eq!(log.record_replica_ack(Offset(2)), Offset(2));
        assert!(log.is_offset_replicated_committed(Offset(0)));
        assert!(log.is_offset_replicated_committed(Offset(1)));
        assert!(!log.is_offset_replicated_committed(Offset(2)));
        let (visible2, _) = log.fetch_replicated_committed(0, &filter, 16, 16);
        assert_eq!(visible2.len(), 2);

        // Stale replica ack (1 < 2) never regresses the watermark (`replica_ack_monotone`).
        assert_eq!(log.record_replica_ack(Offset(1)), Offset(2));

        // Overshooting replica ack (10 > head 3) is clamped by `committed_watermark(3, 10) == 3`.
        assert_eq!(log.record_replica_ack(Offset(10)), Offset(3));
        assert!(log.is_offset_replicated_committed(Offset(2)));
        assert!(!log.is_offset_replicated_committed(Offset(3)));
        let (visible3, _) = log.fetch_replicated_committed(0, &filter, 16, 16);
        assert_eq!(visible3.len(), 3);
    }
}

/// The identity rules of an unclean stop, the clean close and the durable floor, on
/// REAL files, a reboot stood in by [`Boot::Known`] — the crash simulation's claims,
/// met by the real file system too.
#[cfg(test)]
mod unclean_tests {
    use super::*;
    use crate::test_scratch::Scratch;

    const BOOT_A: [u8; 16] = [0x0A; 16];
    const BOOT_B: [u8; 16] = [0x0B; 16];

    fn opts(durability: Durability, boot: [u8; 16]) -> OpenOptions {
        let mut o = OpenOptions::new(durability);
        o.boot = Boot::Known(boot);
        o.volume = Volume::Boot;
        o
    }

    fn open(path: &Path, durability: Durability, boot: [u8; 16]) -> BrokerLog {
        BrokerLog::open_opts(path, opts(durability, boot), || Ok(Prepared::Nothing)).unwrap()
    }

    fn publish(log: &mut BrokerLog, n: u64) {
        let from = log.head().0;
        for i in 0..n {
            log.publish(7, from + i + 1, "/a/x".into(), vec![i as u8; 9])
                .unwrap();
        }
    }

    /// A Relaxed log that did not stop cleanly: reopened in the SAME boot (a process
    /// crash: the page cache kept everything) nothing moves; reopened in ANOTHER boot,
    /// the generation moves once, under the same id, with lineage at the recovered head,
    /// so a reader that kept offsets resumes instead of forgetting — and the next open
    /// in that boot moves nothing more.
    #[test]
    fn an_unclean_relaxed_stop_moves_the_generation_only_after_a_reboot() {
        let root = Scratch::new("unclean-relaxed");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Relaxed, BOOT_A);
        publish(&mut log, 3);
        let before = log.identity();
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_A);
        assert_eq!(log.identity(), before, "a process crash moves nothing");
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        let after = log.identity();
        assert_eq!(
            (after.id, after.generation),
            (before.id, before.generation + 1)
        );
        assert_eq!(log.lineage().steps(), &[(after.generation, 3)]);
        assert_eq!(after.continues(before, log.lineage()), Some(3));
        assert_eq!(log.served().served, crate::proto::Tier::Relaxed);
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_eq!(log.identity(), after, "the same boot again moves nothing");
    }

    /// A clean close (a full flush, `Closed` and the floor at the head in the sidecar)
    /// keeps the identity across a reboot at any tier — through `close_clean` itself,
    /// through an unserved `Broker` that drops, and through the last listener's
    /// shutdown. The control: the same log dropped without it moves.
    #[cfg(unix)]
    #[test]
    fn a_clean_close_keeps_the_identity_across_a_reboot() {
        let root = Scratch::new("clean-close");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Relaxed, BOOT_A);
        publish(&mut log, 2);
        let id = log.identity();
        log.close_clean().unwrap();
        assert!(
            log.publish(7, 99, "/a/x".into(), Vec::new()).is_err(),
            "closed: no more appends"
        );
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_eq!(log.identity(), id, "a clean close survives the reboot");
        drop(log);

        // An unserved broker that drops closes its log cleanly.
        let broker = crate::Broker::open_opts(&path, opts(Durability::Relaxed, BOOT_A), || {
            Ok(Prepared::Nothing)
        })
        .unwrap();
        let id = broker.log_identity();
        drop(broker);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_eq!(log.identity(), id, "an unserved broker's drop closed it");
        drop(log);

        // So does the last listener's shutdown.
        let broker = crate::Broker::open_opts(&path, opts(Durability::Relaxed, BOOT_A), || {
            Ok(Prepared::Nothing)
        })
        .unwrap();
        let id = broker.log_identity();
        let mut handle = broker.serve(root.path().join("sock")).unwrap();
        handle.shutdown();
        drop(broker);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_eq!(log.identity(), id, "the last listener's shutdown closed it");
        drop(log);

        // The control: no clean close, and the reboot moves the generation.
        let mut log = open(&path, Durability::Relaxed, BOOT_A);
        publish(&mut log, 1);
        let id = log.identity();
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_eq!(log.identity().generation, id.generation + 1);
    }

    /// Strict never moves the generation for stopping uncleanly (its acks were on stable
    /// media), while a sidecar marked `Lossy` (a full flush failed while serving) moves
    /// it whatever the tier and the boot.
    #[test]
    fn lossy_moves_the_generation_and_an_unclean_strict_stop_does_not() {
        let root = Scratch::new("lossy");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Strict, BOOT_A);
        publish(&mut log, 2);
        let id = log.identity();
        drop(log);
        let log = open(&path, Durability::Strict, BOOT_B);
        assert_eq!(log.identity(), id, "Strict: an unclean stop moves nothing");
        logid::write(&log.dir, &log.identity.lossy()).unwrap();
        drop(log);
        let log = open(&path, Durability::Strict, BOOT_B);
        let moved = log.identity();
        assert_eq!(
            (moved.id, moved.generation),
            (id.id, id.generation + 1),
            "Lossy moves it"
        );
        assert_eq!(moved.continues(id, log.lineage()), Some(2));
    }

    /// A replica that stopped uncleanly takes a fresh id at generation 0 — never its
    /// leader's id at the next generation, a name only the leader may give.
    #[test]
    fn a_replica_mints_a_fresh_id_instead_of_moving_the_generation() {
        let root = Scratch::new("replica-unclean");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Relaxed, BOOT_A);
        log.declare_replica().unwrap();
        publish(&mut log, 2);
        let id = log.identity();
        drop(log);
        let log = open(&path, Durability::Relaxed, BOOT_B);
        assert_ne!(log.identity().id, id.id);
        assert_eq!(log.identity().generation, 0);
        assert!(log.lineage().steps().is_empty());
    }

    /// An embedder's own unclean verdict and astream's agree on one move: the
    /// generation moves by one, however many verdicts there are.
    #[test]
    fn an_embedder_verdict_and_astreams_move_the_generation_once() {
        let root = Scratch::new("prepared-unclean");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Relaxed, BOOT_A);
        publish(&mut log, 1);
        let id = log.identity();
        drop(log);
        let log = BrokerLog::open_opts(&path, opts(Durability::Relaxed, BOOT_B), || {
            Ok(Prepared::Unclean(
                "a legacy mark from another boot".to_string(),
            ))
        })
        .unwrap();
        assert_eq!(log.identity().generation, id.generation + 1);
        drop(log);
        // The embedder's verdict alone, in the same boot, after a clean open: one move.
        let log = BrokerLog::open_opts(&path, opts(Durability::Relaxed, BOOT_B), || {
            Ok(Prepared::Unclean("the embedder says so".to_string()))
        })
        .unwrap();
        assert_eq!(log.identity().generation, id.generation + 2);
    }

    /// A Strict power loss that tears a multi-record batch whose first records landed:
    /// the batch is cut at its start, and since the cut removes intact records the
    /// generation moves, with lineage at the batch's start (the move announced in the
    /// sidecar before the cut).
    #[test]
    fn a_torn_batch_is_cut_at_its_start_with_lineage() {
        let root = Scratch::new("torn-batch");
        let path = root.path().join("log");
        let mut log = open(&path, Durability::Strict, BOOT_A);
        publish(&mut log, 2);
        let id = log.identity();
        let durable = log.bytes();
        // Stage a three-record batch and write it, as `commit_batch` would, then lose its
        // last record's bytes (its seal) to the power loss.
        for i in 0..3 {
            log.stage_publish(8, i + 1, "/a/y".into(), vec![0x55; 20])
                .unwrap();
        }
        let batch = log.staged_bytes_as_written();
        drop(log);
        let mut torn = durable.clone();
        torn.extend_from_slice(&batch);
        let cut_at = torn.len() - 10;
        torn[cut_at..].fill(0);
        std::fs::write(&path, &torn).unwrap();
        let log = open(&path, Durability::Strict, BOOT_B);
        assert_eq!(log.head(), Offset(2), "cut at the batch's start");
        assert_eq!(log.identity().generation, id.generation + 1);
        assert_eq!(log.identity().continues(id, log.lineage()), Some(2));
        drop(log);
        assert_eq!(std::fs::read(&path).unwrap(), durable);
    }

    /// A `kill -9` during a multi-page batch write: Linux stops a write between pages
    /// for a fatal signal, so the file holds whole records of the batch and one cut
    /// short. Reopened in the SAME boot, at either tier, the batch the dead process never
    /// finished (never acked) is cut at its start and NOTHING moves; reopened after a
    /// reboot, the same bytes are a power loss's, and the cut is announced.
    #[test]
    fn a_short_write_left_by_a_kill_moves_nothing_in_the_same_boot() {
        for tier in [Durability::Strict, Durability::Relaxed] {
            let root = Scratch::new("short-write");
            let path = root.path().join("log");
            let mut log = open(&path, tier, BOOT_A);
            publish(&mut log, 2);
            let id = log.identity();
            let durable = log.bytes();
            for i in 0..3 {
                log.stage_publish(8, i + 1, "/a/y".into(), vec![0x55; 2000])
                    .unwrap();
            }
            let batch = log.staged_bytes_as_written();
            drop(log);
            // The first page of the batch's write landed: record 2 whole, record 3 cut.
            let page = 4096 - durable.len() % 4096;
            assert!(batch.len() > page && page > 2100, "the batch spans a page");
            let short = [&durable[..], &batch[..page]].concat();
            std::fs::write(&path, &short).unwrap();
            let log = open(&path, tier, BOOT_A);
            assert_eq!(log.head(), Offset(2), "{tier:?}: cut at the batch's start");
            assert_eq!(log.identity(), id, "{tier:?}: a kill moves nothing");
            drop(log);
            assert_eq!(std::fs::read(&path).unwrap(), durable);
            // After a reboot the same shape is a power loss: announced, with lineage —
            // Strict cut at the batch's start, Relaxed (whose unclean stop is announced
            // anyway) keeping its longest intact prefix.
            std::fs::write(&path, &short).unwrap();
            let log = open(&path, tier, BOOT_B);
            let kept = if tier == Durability::Strict { 2 } else { 3 };
            assert_eq!(log.head(), Offset(kept), "{tier:?}");
            assert_eq!(log.identity().generation, id.generation + 1, "{tier:?}");
            assert_eq!(log.identity().continues(id, log.lineage()), Some(kept));
        }
    }
}

#[cfg(test)]
mod ordered_tests {
    //! The Ordered tier and the escalation table at the log: the flush each commit owes,
    //! the held head, the durable head, the counts, the latch and the failed flush. On
    //! the crash simulation's device wherever the behaviour must not depend on the
    //! platform (its barrier exists everywhere); on a real file for what the platform
    //! decides (the tier served).
    use super::*;
    use crate::crash_sim::Sim;
    use crate::proto::{ServedReason, Tier};
    use crate::test_scratch::Scratch;
    use crate::{Escalate, Escalation};

    const BOOT_A: [u8; 16] = [0x0A; 16];
    const BOOT_B: [u8; 16] = [0x0B; 16];
    const ORDERED: Durability = Durability::Ordered { horizon: None };

    fn opts(durability: Durability, boot: [u8; 16]) -> OpenOptions {
        let mut o = OpenOptions::new(durability);
        o.boot = Boot::Known(boot);
        o.volume = Volume::Boot;
        o.escalation = Escalation::new([
            Escalate::Subject("/e/>".into()),
            Escalate::PrincipalPrefix("h-".into()),
            Escalate::Group("/cur/p/>".into()),
        ]);
        o
    }

    fn open_sim(sim: &Sim, durability: Durability, boot: [u8; 16]) -> BrokerLog {
        BrokerLog::open_core(
            Storage::Sim(sim.clone()),
            &opts(durability, boot),
            false,
            None,
            AntiRollbackMode::Off,
            || Ok(Prepared::Nothing),
        )
        .map(|(log, _)| log)
        .expect("a simulated log opens")
    }

    fn stage(log: &mut BrokerLog, pid: u64, seq: u64, subject: &str) -> (u64, bool) {
        let (off, dup) = log
            .stage_publish(pid, seq, subject.to_string(), vec![seq as u8; 5])
            .expect("staged");
        (off.0, dup)
    }

    /// An Ordered batch is BARRIERED, never flushed in full inside the commit; one
    /// holding an escalated record holds the visible head at that record and names the
    /// full flush it owes, and only that flush — taken outside the lock through the
    /// handle, as the broker's writer does — makes it, and what follows it, visible.
    #[test]
    fn an_escalated_record_holds_the_visible_head_until_its_full_flush() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        assert_eq!(log.served().served, Tier::Ordered);
        assert_eq!(stage(&mut log, 1, 1, "/o/a"), (0, false));
        assert_eq!(stage(&mut log, 1, 2, "/e/a"), (1, false));
        assert_eq!(stage(&mut log, 1, 3, "/o/b"), (2, false));
        let c = log.commit_batch().unwrap();
        assert_eq!(
            c,
            Committed {
                visible_now: Offset(1),
                owe_full: Some(Offset(3)),
                held: Vec::new(),
            }
        );
        assert!(c.acks_now(0, false), "the record before the escalated one");
        assert!(!c.acks_now(1, false) && !c.acks_now(2, false));
        assert_eq!(log.visible_head(), Offset(1));
        assert_eq!(
            log.durable_head(),
            Offset(0),
            "a barrier moves no durable head"
        );
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (1, 0));
        log.flush_handle().full().unwrap();
        log.full_flush_done(3);
        assert_eq!(
            (log.durable_head(), log.visible_head()),
            (Offset(3), Offset(3))
        );
        assert_eq!(log.flush_counts().full, 1);
        // An ordinary batch after it owes nothing and holds nothing.
        stage(&mut log, 1, 4, "/o/c");
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(4), None));
        assert_eq!(log.durable_head(), Offset(3));
    }

    /// Each kind of rule escalates its own record: a person's producer, bound by a
    /// `/a/bind` staged earlier in the SAME batch, and a pure commit of a person's group
    /// (hidden, producer 0 — only the Group rule sees it).
    #[test]
    fn a_same_batch_binding_and_a_group_commit_escalate() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        stage(&mut log, 1, 1, "/o/a");
        log.stage_bind(9, "h-alice".into()).unwrap();
        stage(&mut log, 9, 1, "/o/b");
        let c = log.commit_batch().unwrap();
        assert_eq!(
            c.visible_now,
            Offset(2),
            "the bound person's publish is held"
        );
        log.full_flush_done(log.head().0);
        log.stage_commit("/cur/p/alice/inbox".into(), 2).unwrap();
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(3), Some(Offset(4))));
        log.full_flush_done(4);
        // A node's commit and an unbound producer's publish are ordinary.
        log.stage_commit("/cur/n/node/inbox".into(), 3).unwrap();
        stage(&mut log, 1, 2, "/o/c");
        assert_eq!(log.commit_batch().unwrap().owe_full, None);
    }

    /// A RESENT op that the table names, whose original no full flush covers yet, owes
    /// one even though the batch stages nothing — and its ack waits for it although the
    /// original is already visible. Once covered, the same resend owes nothing.
    #[test]
    fn a_resent_escalated_op_on_an_uncovered_original_owes_a_full_flush() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        stage(&mut log, 2, 1, "/o/y");
        assert_eq!(log.commit_batch().unwrap().owe_full, None);
        assert_eq!(stage(&mut log, 2, 1, "/e/y"), (0, true));
        let c = log.commit_batch().unwrap();
        assert_eq!(
            c,
            Committed {
                visible_now: Offset(1),
                owe_full: Some(Offset(1)),
                held: vec![0],
            }
        );
        assert!(!c.acks_now(0, true), "the resend waits for the flush");
        assert!(c.acks_now(0, false), "the original was acked long ago");
        log.full_flush_done(1);
        assert_eq!(stage(&mut log, 2, 1, "/e/y"), (0, true));
        assert_eq!(log.commit_batch().unwrap().owe_full, None, "now covered");
    }

    /// Strict flushes every batch in full inside the commit, escalated or not: nothing is
    /// ever owed or held, and the durable head is the head after every batch. Relaxed
    /// flushes nothing, but owes the escalated record its full flush, like Ordered.
    #[test]
    fn strict_owes_nothing_and_relaxed_owes_the_escalated_record() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, Durability::Strict, BOOT_A);
        stage(&mut log, 1, 1, "/e/a");
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(1), None));
        assert_eq!(log.durable_head(), Offset(1));
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (0, 1));
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, Durability::Relaxed, BOOT_A);
        stage(&mut log, 1, 1, "/o/a");
        assert_eq!(log.commit_batch().unwrap().owe_full, None);
        stage(&mut log, 1, 2, "/e/a");
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(1), Some(Offset(2))));
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (0, 0));
    }

    /// A barrier the device refuses WHILE serving latches the log to Strict, saying why,
    /// and that batch is flushed in full before it is acked — never acked on an order
    /// nobody promised.
    #[test]
    fn a_barrier_refused_while_serving_latches_strict_and_flushes_that_batch() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        log.refuse_barrier = true;
        stage(&mut log, 1, 1, "/o/a");
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(1), None));
        assert_eq!(log.durable_head(), Offset(1));
        let served = log.served();
        assert_eq!(
            (served.requested, served.served, served.reason),
            (
                Tier::Ordered,
                Tier::Strict,
                ServedReason::LatchedAfterBarrierError
            )
        );
        assert_eq!(log.horizon(), None, "a latched log keeps no horizon");
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (0, 1));
    }

    /// AN ORDERED BATCH IS FLUSHED IN ITS UNLOCKED HALF AND SETTLED AT ITS END. The
    /// barrier — and the full flush a refused one falls back to — is issued by
    /// `BatchWrite::run`, which the broker's writer runs with the log lock released; the
    /// refusing-device seam travels with the batch from its begin, as the fault seam does.
    /// Armed only in the write window, the refusal is the next batch's: this one is
    /// barriered and latches nothing. Begun refused and un-armed in the window, the next
    /// batch is still refused and flushed in full — and none of that shows until its
    /// locked end: in the window the log still serves Ordered and has counted no full
    /// flush; at the end it has latched to Strict, saying why, counted the full flush, and
    /// the durable head is the head.
    #[test]
    fn an_ordered_batch_is_flushed_in_its_unlocked_half_and_settled_at_its_end() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        let begin = |log: &mut BrokerLog| match log.commit_begin().unwrap() {
            Begun::Write(write) => write,
            Begun::Empty(_) => panic!("nothing staged to write"),
        };

        stage(&mut log, 1, 1, "/o/a");
        let mut write = begin(&mut log);
        log.refuse_barrier = true; // the write window: for the next batch
        let res = write.run();
        log.commit_end(write, res).unwrap();
        assert_eq!(log.served().served, Tier::Ordered, "barriered, not latched");
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (1, 0));
        assert_eq!(
            log.durable_head(),
            Offset(0),
            "a barrier makes nothing durable"
        );

        stage(&mut log, 1, 2, "/o/b");
        let mut write = begin(&mut log);
        log.refuse_barrier = false; // the write window: too late for this batch
        let res = write.run();
        assert_eq!(
            log.served().served,
            Tier::Ordered,
            "nothing settled in the window"
        );
        assert_eq!(log.flush_counts().full, 0, "nothing counted in the window");
        log.commit_end(write, res).unwrap();
        let served = log.served();
        assert_eq!(
            (served.served, served.reason),
            (Tier::Strict, ServedReason::LatchedAfterBarrierError)
        );
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full), (1, 1));
        assert_eq!(
            log.durable_head(),
            Offset(2),
            "the fallback flushed it in full"
        );
    }

    /// A FAILED full flush POISONS the open. The run is recorded `Lossy` (so the next
    /// open moves the generation whatever the boot — the same boot here), the log latches
    /// to Strict for `Hello`, and the held head and the durable head stay where they were
    /// for the rest of the open: nothing a later flush reports can release what the
    /// failed one was to cover.
    ///
    /// The fsyncgate shape, step by step: the escalated record's barrier succeeded (its
    /// pages went to the drive and are clean in the page cache), the full flush failed
    /// (the drive dropped its cache), and the producer resends. Before the poison the
    /// resend deduplicated to the record, owed a full flush, and that flush — with nothing
    /// dirty left to rewrite, so it returns 0 on a real device — released the held head:
    /// the record was shown, every `Mark` said it durable, and the resend was acked, for a
    /// record on no media. Now the resend is refused, and so is every batch, `sync_full`
    /// and the clean close, until a reopen; a success reported anyway moves nothing.
    #[test]
    fn a_failed_full_flush_poisons_the_open_and_no_later_flush_releases_what_it_held() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        stage(&mut log, 1, 1, "/o/a");
        log.commit_batch().unwrap();
        let before = log.identity();
        stage(&mut log, 1, 2, "/e/a");
        let c = log.commit_batch().unwrap();
        assert_eq!(c.owe_full, Some(Offset(2)));
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        let err = log.flush_handle().full().unwrap_err();
        log.full_flush_failed(&err);
        log.inject_faults(InjectedFaults::default());
        assert_eq!(log.flush_counts().failed, 1);
        assert_eq!(
            (log.visible_head(), log.durable_head()),
            (Offset(1), Offset(0)),
            "the escalated record stays unseen, and nothing is durable past the open's"
        );
        assert_eq!(log.served().reason, ServedReason::LatchedAfterFlushFailure);
        assert!(log.poisoned().is_some());
        let Ok(logid::Prior::Recorded(rec)) = logid::read(&Dir::Sim(sim.clone())) else {
            panic!("the sidecar is readable")
        };
        assert_eq!(rec.state, logid::State::Lossy);
        // The resend that would dedup to the held record: refused, not owed a flush.
        assert!(matches!(
            log.stage_publish(1, 2, "/e/a".into(), b"x".to_vec()),
            Err(StageErr::Io(_))
        ));
        assert!(log.commit_batch().is_err(), "no batch commits");
        assert!(log.sync_full().is_err(), "no flush is asked for");
        assert!(
            !log.full_flush_owed(log.head().0),
            "nor owed by the horizon"
        );
        // A success reported anyway (a flush issued before the failure was known):
        // nothing moves.
        log.full_flush_done(log.head().0);
        assert_eq!(
            (log.visible_head(), log.durable_head()),
            (Offset(1), Offset(0)),
            "a later flush that succeeds says nothing about what the failed one covered"
        );
        assert_eq!(log.flush_counts().full, 0);
        assert!(
            log.close_clean().is_err(),
            "a poisoned log does not close clean"
        );
        drop(log);
        let log = open_sim(&sim, ORDERED, BOOT_A);
        assert_eq!(
            log.identity().generation,
            before.generation + 1,
            "Lossy moves the generation even in the same boot"
        );
        assert!(log.poisoned().is_none(), "the reopen recovers");
    }

    /// A flush that fails INSIDE a commit — the batch's own barrier here — rolls the batch
    /// back as Strict always did; and when earlier acks rode barriers (records no full
    /// flush covers, which the rollback cannot reach), it is a failed full flush's case:
    /// `Lossy`, the latch and the poison. On a log whose every promoted record a full
    /// flush covers, the same failure rolls back and nothing more — Strict's old contract.
    #[test]
    fn a_failed_flush_inside_a_commit_poisons_only_over_uncovered_acks() {
        // Covered: Strict, every batch flushed in full.
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, Durability::Strict, BOOT_A);
        stage(&mut log, 1, 1, "/o/a");
        log.commit_batch().unwrap();
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        stage(&mut log, 1, 2, "/o/b");
        assert!(log.commit_batch().is_err());
        log.inject_faults(InjectedFaults::default());
        assert!(log.poisoned().is_none(), "nothing uncovered: a rollback");
        assert_eq!(log.flush_counts().failed, 1);
        stage(&mut log, 1, 3, "/o/c");
        assert_eq!(log.commit_batch().unwrap().visible_now, Offset(2));
        // Uncovered: an Ordered log whose barrier is refused (latched to Strict, the batch
        // flushed in full) after an ordinary batch rode a barrier, and the full flush that
        // stands in for the barrier fails.
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        stage(&mut log, 1, 1, "/o/a");
        log.commit_batch().unwrap();
        assert_eq!(log.durable_head(), Offset(0), "acked on a barrier");
        log.refuse_barrier = true;
        log.inject_faults(InjectedFaults {
            sync: true,
            truncate: false,
        });
        stage(&mut log, 1, 2, "/o/b");
        assert!(log.commit_batch().is_err());
        assert!(
            log.poisoned().is_some(),
            "the acked barrier batch may be gone"
        );
        assert_eq!(log.head(), Offset(1), "the failed batch rolled back");
        let Ok(logid::Prior::Recorded(rec)) = logid::read(&Dir::Sim(sim.clone())) else {
            panic!("the sidecar is readable")
        };
        assert_eq!(rec.state, logid::State::Lossy);
    }

    /// The group commit a HELD batch carries does not steer `group_start` until the full
    /// flush that releases it succeeds: a second reader of a person's inbox resumes below
    /// a read mark that is neither visible nor durable yet. Released, it takes effect; a
    /// commit the table does not name, in an unheld batch, takes effect at once.
    #[test]
    fn a_held_group_commit_does_not_move_group_start_until_released() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, Durability::Relaxed, BOOT_A);
        for seq in 1..=3 {
            stage(&mut log, 1, seq, "/o/m");
        }
        log.commit_batch().unwrap();
        log.stage_commit("/cur/p/alice/inbox".into(), 2).unwrap();
        let c = log.commit_batch().unwrap();
        assert_eq!((c.visible_now, c.owe_full), (Offset(3), Some(Offset(4))));
        assert_eq!(
            log.group_start("/cur/p/alice/inbox"),
            Offset::ZERO,
            "held: the read mark steers nobody yet"
        );
        log.full_flush_done(4);
        assert_eq!(log.group_start("/cur/p/alice/inbox"), Offset(3));
        // An ordinary group's commit, in a batch nothing holds: at once.
        log.stage_commit("/cur/n/node/inbox".into(), 1).unwrap();
        assert_eq!(log.commit_batch().unwrap().owe_full, None);
        assert_eq!(log.group_start("/cur/n/node/inbox"), Offset(2));
    }

    /// `sync_full` flushes in full when something is uncovered and counts a skip when
    /// nothing is; a direct (un-batched) escalated publish is durable when it returns, an
    /// ordinary one only ordered.
    #[test]
    fn sync_full_skips_when_covered_and_a_direct_escalated_publish_is_durable() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, ORDERED, BOOT_A);
        log.publish(1, 1, "/o/a".into(), vec![1]).unwrap();
        assert_eq!(log.durable_head(), Offset(0));
        assert_eq!(log.sync_full().unwrap(), Offset(1));
        assert_eq!(log.sync_full().unwrap(), Offset(1));
        let counts = log.flush_counts();
        assert_eq!((counts.barrier, counts.full, counts.skipped), (1, 1, 1));
        log.publish(1, 2, "/e/a".into(), vec![2]).unwrap();
        assert_eq!(
            log.durable_head(),
            Offset(2),
            "escalated: durable at return"
        );
        assert_eq!(log.flush_counts().full, 2);
    }

    /// The tier SERVED on this platform, from a real file: Ordered on Apple with the log
    /// on the boot volume, Strict elsewhere (never weaker), and Strict off the boot
    /// volume — recorded in `<log>.id`, so an unclean stop of a run that SERVED Ordered
    /// moves the generation in the next boot, and one served Strict does not.
    #[test]
    fn the_tier_served_is_decided_at_open_and_recorded() {
        let root = Scratch::new("ordered-served");
        let path = root.path().join("log");
        let mut o = opts(Durability::ordered(), BOOT_A);
        let mut log = BrokerLog::open_opts(&path, o.clone(), || Ok(Prepared::Nothing)).unwrap();
        let served = log.served();
        assert_eq!(
            (served.requested, served.horizon_ms, served.rules),
            (Tier::Ordered, Some(1000), 3)
        );
        let apple = cfg!(target_vendor = "apple");
        if apple {
            assert_eq!(
                (served.served, served.reason),
                (Tier::Ordered, ServedReason::None)
            );
        } else {
            assert_eq!(
                (served.served, served.reason),
                (Tier::Strict, ServedReason::NotApple)
            );
        }
        log.publish(1, 1, "/o/a".into(), vec![1]).unwrap();
        let before = log.identity();
        drop(log); // no clean close: an unclean stop
        o.boot = Boot::Known(BOOT_B);
        let log = BrokerLog::open_opts(&path, o.clone(), || Ok(Prepared::Nothing)).unwrap();
        assert_eq!(
            log.identity().generation,
            before.generation + u64::from(apple),
            "a served-Ordered run's unclean stop moves the generation; a Strict one's does not"
        );
        drop(log);
        o.volume = Volume::Other;
        let mut log = BrokerLog::open_opts(&path, o.clone(), || Ok(Prepared::Nothing)).unwrap();
        let served = log.served();
        let reason = if apple {
            ServedReason::NotBootVolume
        } else {
            ServedReason::NotApple
        };
        assert_eq!((served.served, served.reason), (Tier::Strict, reason));
        // The other half, on EVERY platform: an Ordered request served Strict (off the
        // boot volume here) records Strict, so its unclean stop moves nothing in another
        // boot. Were the ASKED tier recorded instead, this reopen would read a weak run
        // off the boot volume and move the generation.
        log.publish(1, 2, "/o/b".into(), vec![2]).unwrap();
        let before = log.identity();
        drop(log); // no clean close: an unclean stop
        o.boot = Boot::Known(BOOT_A);
        let log = BrokerLog::open_opts(&path, o, || Ok(Prepared::Nothing)).unwrap();
        assert_eq!(
            log.identity(),
            before,
            "a run served Strict moves nothing at an unclean stop, off the boot volume too"
        );
    }

    /// The boot volume is judged by the OPENED FILE's device, never its directory's: a
    /// log reached through a path whose directory is on another device is still on the
    /// device it is written to. `/dev/fd/N` is that path without a second volume to
    /// mount: its directory is devfs, and opening it reaches the file behind descriptor
    /// `N` — here a scratch file on the boot volume. Judged by the directory (the rule
    /// before), it was off the boot volume; judged by `fstat`, it is on it — and devfs's
    /// own `/dev/null` is not.
    #[cfg(unix)]
    #[test]
    fn the_boot_volume_is_the_opened_files_device_not_its_directorys() {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::MetadataExt;
        let root = Scratch::new("ordered-volume");
        let target = FsOpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.path().join("log"))
            .unwrap();
        assert!(
            on_boot_volume(&target),
            "the scratch dir is on the boot volume"
        );
        let through = format!("/dev/fd/{}", target.as_raw_fd());
        let via = FsOpenOptions::new()
            .read(true)
            .write(true)
            .open(&through)
            .unwrap();
        assert_ne!(
            std::fs::metadata("/dev/fd").unwrap().dev(),
            target.metadata().unwrap().dev(),
            "the path's directory is on another device"
        );
        assert!(on_boot_volume(&via), "judged by the file it reaches");
        assert!(!on_boot_volume(&File::open("/dev/null").unwrap()));
    }

    /// A RESENT op on the un-batched path that the table names, whose original no full
    /// flush covers yet, is flushed in full before the call returns — what the broker's
    /// batched path does for the same resend (its ack waits for the flush its batch owes).
    /// Relaxed, so no barrier and no platform decides it: the original rides the page
    /// cache, the escalated resend makes it durable.
    #[test]
    fn a_direct_escalated_resend_of_an_uncovered_original_is_flushed_before_it_returns() {
        let sim = Sim::fresh();
        let mut log = open_sim(&sim, Durability::Relaxed, BOOT_A);
        assert_eq!(
            log.publish(2, 1, "/o/y".into(), vec![1]).unwrap(),
            (Offset(0), false)
        );
        assert_eq!(log.durable_head(), Offset(0), "page cache only");
        assert_eq!(
            log.publish(2, 1, "/e/y".into(), vec![1]).unwrap(),
            (Offset(0), true)
        );
        assert_eq!(
            log.durable_head(),
            Offset(1),
            "the resend's flush covers it"
        );
        assert_eq!(log.flush_counts().full, 1);
        // Covered now: the same resend owes nothing.
        log.publish(2, 1, "/e/y".into(), vec![1]).unwrap();
        assert_eq!(log.flush_counts().full, 1);
        // process_and_produce's resend, escalated by its output subject.
        log.process_and_produce(3, 1, "/o/z".into(), vec![2], "g".into(), 0)
            .unwrap();
        assert_eq!(log.durable_head(), Offset(1));
        log.process_and_produce(3, 1, "/e/z".into(), vec![2], "g".into(), 0)
            .unwrap();
        assert_eq!(log.durable_head(), Offset(2));
    }

    /// A table rule that does not parse refuses the open by name, before the file exists.
    #[test]
    fn a_bad_escalation_rule_refuses_the_open_before_the_file_exists() {
        let root = Scratch::new("ordered-badrule");
        let path = root.path().join("log");
        let mut o = OpenOptions::new(ORDERED);
        o.escalation = Escalation::new([Escalate::Group("no-slash".into())]);
        let err = BrokerLog::open_opts(&path, o, || Ok(Prepared::Nothing))
            .err()
            .expect("refused");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("Group"), "{err}");
        assert!(!path.exists());
    }
}
