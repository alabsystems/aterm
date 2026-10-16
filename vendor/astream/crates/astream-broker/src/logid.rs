//! A log's IDENTITY: WHICH log a broker serves, and WHICH incarnation of that log's
//! offsets — what a client that keeps an offset OUTSIDE the log has to check before it
//! trusts that offset again.
//!
//! An offset names a record in ONE log. A consumer that persists one — a
//! delivered-up-to watermark, a topic cursor, "I already acked offset N" — and later
//! reads a DIFFERENT log at the same address applies it to records it never saw: it
//! skips mail below a stale watermark, replays a foreign log's history as new, or
//! answers for the wrong record. The address stays the same when the log file is
//! deleted and recreated, when a repair truncates it and later appends reuse the cut
//! offsets, when an older copy is restored over it, or when the client is pointed at
//! another host's broker. [`LogIdentity`] is how the consumer tells.
//!
//! # What a consumer does when the identity changes
//!
//! Compare the identity a connection reports — `Client::log_identity`, or
//! `Client::seen_log_identity` after an attach, which already sent the `Hello` that
//! carries it — with the identity its persisted offsets were read under. ANY
//! difference, in [`id`](LogIdentity::id) or in
//! [`generation`](LogIdentity::generation), means EVERY offset persisted under the old
//! identity is meaningless against this log: not too high or too low, meaningless.
//! Drop them (or keep them filed under the OLD identity, to resume only if exactly that
//! identity comes back — the same log after an off/on), and start the way a first run
//! does, from a cursor resolved against THIS log: its head, a `Last` snapshot, or a
//! consumer group (group cursors live inside the log and move with it). `None` — a
//! broker that predates log identity — says nothing either way: the client is exactly
//! where it was before this type existed.
//!
//! # Where it lives, and when it changes
//!
//! In a sidecar beside the log, `<log>.id`: a CRC frame holding the identity and what
//! the open that wrote it saw (the retention base, the head, and a fingerprint of the
//! newest record's stored frame). It is written atomically (tmp file, fsync, rename,
//! directory fsync) during [`BrokerLog`](crate::BrokerLog)'s open, BEFORE the open
//! returns, so no client can be told an identity that a crash then loses; an open
//! that cannot write it FAILS, naming it. It is not IN the log because a record there
//! would take an offset (every new log's first publish would move off offset 0) and
//! would be one more hidden record retention must carry; a replica's log is the
//! leader's spine and may not hold a record of its own either.
//!
//! - A log that has never held a record — every log an open creates — gets a fresh
//!   random `id`, generation 0, at each open: nobody can hold an offset into it, and
//!   a sidecar beside it can only be a deleted predecessor's (or its own, from before
//!   it held anything).
//! - A log that holds records and has NO sidecar — one written before log identity
//!   existed — gets a DERIVED `id`, deterministic in its stored bytes (base, head,
//!   first and newest frame), generation 0. A leader and a follower holding the same
//!   records derive the same one. (One whose every record retention pruned stores no
//!   frame to derive from, and is minted a fresh `id` instead.)
//! - A log that holds records beside a sidecar this build cannot read (damaged, or
//!   another sidecar format) gets a fresh random `id`, generation 0 — never a derived
//!   one, which could be an identity the log already had: the sidecar existed, so the
//!   log has had an identity, and only a new one is certainly not it.
//! - Otherwise the sidecar's `id` stands, and the `generation` goes up by one when
//!   this log's offsets may now name different records than a client was told:
//!   [`open_repair`](crate::BrokerLog::open_repair) truncated records, the log is
//!   SHORTER than the last open saw, its retention base went backwards, or the record
//!   at the newest offset the last open saw is not the record it saw there (another
//!   log was put under this name). Any of these is written to the sidecar before the
//!   open goes on, so a crash right after a repair's truncation cannot lose the bump.
//! - A REPLICA reports its leader's identity: the leader sends it with every
//!   replicated record, and a log that is a replication target adopts it (rewriting
//!   its sidecar) once a batch carrying it commits AND reaches the replica's newest
//!   record. The leader ships in order from what the replica confirmed, so by then
//!   every record the replica holds was appended or compared byte for byte; an echo
//!   of a shared PREFIX vouches for nothing above it (a replica that kept records its
//!   leader has since cut matches below the cut only), and an open that refused a
//!   replicated record for holding a different one adopts nothing more. A leader that
//!   predates log identity sends none, and its followers report their own. A replica never
//!   advances its leader's generation itself — only the leader numbers its own
//!   rewrites, and a follower's `(id, generation + 1)` could be the very name the
//!   leader later gives a rewrite of its own. A replica found rewritten (any of the
//!   cases above) takes a fresh random `id` instead, until its next replicated batch
//!   hands it the leader's again.
//!
//! # Honest boundary
//!
//! - An identity names a LINEAGE of bytes, not a file. A log COPIED elsewhere, with or
//!   without its sidecar, is two logs under one identity as soon as both take writes;
//!   nothing here can tell those copies apart.
//! - Rewriting is detected only against what the LAST OPEN saw. Records lost after it
//!   — a Relaxed-tier log's page-cache records after a power loss, or a rollback that
//!   ends at or above that open's head and keeps the record it saw last — reuse their
//!   offsets under an unchanged identity. A Strict-tier log loses no acked record
//!   that way: a torn tail it truncates was never acknowledged and never delivered.
//! - A log restored from a backup TOGETHER WITH its sidecar carries the backup's
//!   identity, which is also the identity it carried while it grew past the backup:
//!   offsets a client kept from that growth are trusted against the records that will
//!   reuse them. Restore the log file alone — the sidecar already beside it then finds
//!   it shorter, or different, and moves the generation.
//! - Deleting `<log>.id` makes the log read as one that predates identity: it
//!   re-derives its id from its bytes at generation 0. That is a new identity unless
//!   the bytes are exactly those of an EARLIER derivation — a pre-identity log that
//!   grew and was then restored to the copy it was derived from derives the identity
//!   it had before the growth, and offsets kept under it past those bytes are
//!   trusted. To give a log a new identity by hand, make its sidecar unreadable
//!   (overwrite it) rather than delete it: an unreadable sidecar always yields a
//!   fresh one.
//! - The `id` is minted from std's per-thread OS-seeded `RandomState` (the same source
//!   as the broker's handshake nonce), and derived with FNV-1a-128: distinct and
//!   collision-resistant against accident, not claimed cryptographic. Anyone who can
//!   write the log's directory can write any identity there.
//! - The identity is told to every peer that sends `Hello`, before it authenticates:
//!   it is a random name for the log, not a secret.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use astream_wire::{Frame, HEADER_SIZE};

use crate::store::{create_fresh, sync_parent_dir};

/// WHICH log, and WHICH incarnation of its offsets — see the [module docs](self) for
/// what a consumer must do when either changes.
///
/// Displayed (and [parsed](Self::parse)) as `<id in 32 lowercase hex>.<generation>`,
/// the form to persist beside the offsets it vouches for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LogIdentity {
    /// Names the log: random when the log was created (or the identity it had cannot
    /// be read back), derived from its bytes when it predates log identity, and on a
    /// replica its leader's. Two different logs differ here.
    pub id: [u8; 16],
    /// Counts the rewrites of THIS log that may have given one of its offsets to a
    /// different record (a repair's truncation, a log found shorter than, or different
    /// from, what its last open saw). Same `id`, different generation: the same log
    /// file, but its old offsets are as meaningless as another log's.
    pub generation: u64,
}

impl LogIdentity {
    /// Bytes the identity takes on the wire: the `id`, then the generation as a
    /// little-endian `u64`.
    pub const WIRE_LEN: usize = 24;

    /// Append the wire form to `out`.
    pub(crate) fn put(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.id);
        out.extend_from_slice(&self.generation.to_le_bytes());
    }

    /// Read the wire form from the first [`WIRE_LEN`](Self::WIRE_LEN) bytes of `b`.
    pub(crate) fn from_wire(b: &[u8]) -> Option<LogIdentity> {
        let b = b.get(..Self::WIRE_LEN)?;
        Some(LogIdentity {
            id: b[..16].try_into().ok()?,
            generation: u64::from_le_bytes(b[16..].try_into().ok()?),
        })
    }

    /// Parse the [`Display`](fmt::Display) form, `<32 lowercase hex>.<generation>`.
    /// Exactly that form: anything else (upper case, a sign, a missing part) is `None`.
    pub fn parse(s: &str) -> Option<LogIdentity> {
        let (hex, generation) = s.split_once('.')?;
        let digits = |s: &str, ok: fn(&u8) -> bool| !s.is_empty() && s.bytes().all(|b| ok(&b));
        if hex.len() != 32
            || !digits(hex, |b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            || !digits(generation, u8::is_ascii_digit)
        {
            return None;
        }
        let mut id = [0u8; 16];
        for (i, byte) in id.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()?;
        }
        Some(LogIdentity {
            id,
            generation: generation.parse().ok()?,
        })
    }
}

impl fmt::Display for LogIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.id {
            write!(f, "{b:02x}")?;
        }
        write!(f, ".{}", self.generation)
    }
}

/// What `<log>.id` holds: the identity, and what the open (or adoption) that wrote it
/// saw — the evidence the NEXT open checks the log against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Sidecar {
    pub(crate) identity: LogIdentity,
    /// The retention base then.
    base: u64,
    /// The head then.
    head: u64,
    /// Fingerprint of the stored frame at offset `head - 1`, the newest record then
    /// (all zero when there was none).
    newest: [u8; 16],
}

const MAGIC: &[u8; 4] = b"ASID";
/// The sidecar's own format. A sidecar this build cannot read (another format, a torn
/// or tampered write) is treated as absent: the log takes a new identity, which is the
/// direction that costs a consumer a re-read and never a skipped record.
const FORMAT: u8 = 1;
const BODY_LEN: usize = 4 + 1 + LogIdentity::WIRE_LEN + 8 + 8 + 16;

/// The identity sidecar's path for a log at `path`: `<log>.id`.
pub(crate) fn sidecar_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".id");
    PathBuf::from(s)
}

impl Sidecar {
    fn encode(&self) -> Vec<u8> {
        let mut p = Vec::with_capacity(BODY_LEN);
        p.extend_from_slice(MAGIC);
        p.push(FORMAT);
        self.identity.put(&mut p);
        p.extend_from_slice(&self.base.to_le_bytes());
        p.extend_from_slice(&self.head.to_le_bytes());
        p.extend_from_slice(&self.newest);
        p
    }

    fn decode(bytes: &[u8]) -> Option<Sidecar> {
        let d = Frame::decode(bytes).ok()??;
        let p = d.frame.payload;
        if d.consumed != bytes.len() || p.len() != BODY_LEN || &p[..4] != MAGIC || p[4] != FORMAT {
            return None;
        }
        let at = 5 + LogIdentity::WIRE_LEN;
        let u64_at = |i: usize| p[i..i + 8].try_into().ok().map(u64::from_le_bytes);
        Some(Sidecar {
            identity: LogIdentity::from_wire(&p[5..at])?,
            base: u64_at(at)?,
            head: u64_at(at + 8)?,
            newest: p[at + 16..].try_into().ok()?,
        })
    }

    /// Whether `durable` (the log's stored frames from `base`) still holds, at the
    /// newest offset this sidecar saw, the record it saw there. True when there is
    /// nothing to compare: it saw no record, or retention has since pruned it (a log
    /// SHORTER than it saw is caught by the head check, not here).
    fn still_names(&self, durable: &[u8], base: u64) -> bool {
        let Some(index) = self.head.checked_sub(1).and_then(|o| o.checked_sub(base)) else {
            return true;
        };
        let Ok(index) = usize::try_from(index) else {
            return true;
        };
        frames(durable)
            .nth(index)
            .is_none_or(|frame| fingerprint(frame) == self.newest)
    }
}

/// What an open finds at `<log>.id`.
enum Prior {
    /// No sidecar: a log created before log identity, or by this open.
    Absent,
    /// A file this build cannot read as a sidecar (damaged, or another format): the log
    /// HAS had an identity, and this cannot say which.
    Unreadable,
    /// The identity the last open (or adoption) recorded, with what it saw.
    Recorded(Sidecar),
}

/// Read `<log>.id` ([`Prior`]; a file that cannot be decoded is said on stderr, since
/// the log then takes a new identity); an I/O error otherwise.
fn read(log: &Path) -> io::Result<Prior> {
    let path = sidecar_path(log);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Prior::Absent),
        Err(e) => {
            return Err(io::Error::new(
                e.kind(),
                format!(
                    "cannot read the identity of {} from {}: {e}",
                    log.display(),
                    path.display()
                ),
            ))
        }
    };
    match Sidecar::decode(&bytes) {
        Some(sidecar) => Ok(Prior::Recorded(sidecar)),
        None => {
            eprintln!(
                "astream-broker: {} is not a log identity this build reads; {} takes a new \
                 identity, so every client re-reads it from scratch",
                path.display(),
                log.display()
            );
            Ok(Prior::Unreadable)
        }
    }
}

/// Durably replace `<log>.id` with `sidecar` (tmp file, fsync, rename, directory
/// fsync), never following a link planted at either name.
fn write(log: &Path, sidecar: &Sidecar) -> io::Result<()> {
    let path = sidecar_path(log);
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let framed = Frame::new(sidecar.encode())
        .encode()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    {
        let mut f = create_fresh(&tmp, &mut OpenOptions::new())?;
        f.write_all(&framed)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &path)?;
    sync_parent_dir(&path)
}

/// Settle the identity of the log at `log` as one open of it finds it — `created` by
/// this open, `replica` when it is a replication target, `cut` when this open's repair
/// truncated records, `durable` its intact stored frames (those [`frames`] walks) from
/// offset `base` up to `head` — persisting any change before returning. See the module
/// docs for the rules.
pub(crate) fn open(
    log: &Path,
    created: bool,
    replica: bool,
    cut: bool,
    durable: &[u8],
    base: u64,
    head: u64,
) -> io::Result<Sidecar> {
    let prior = if created { Prior::Absent } else { read(log)? };
    let minted = || LogIdentity {
        id: minted_id(log),
        generation: 0,
    };
    let identity = match &prior {
        // Never held a record (a created log always): no client can hold an offset
        // into it, so a fresh id is always right — and a sidecar beside it can only be
        // a deleted predecessor's, or this log's own from before it held anything.
        _ if head == 0 => minted(),
        // Every record pruned: nothing stored to derive from but two numbers, which
        // another such log can share — a fresh id instead.
        Prior::Absent if frames(durable).next().is_none() => minted(),
        Prior::Absent => LogIdentity {
            id: derived_id(durable, base, head),
            generation: 0,
        },
        // It had an identity this cannot read: a derived id could be that very one
        // (or an earlier one), so only a fresh id is certainly a different name.
        Prior::Unreadable => minted(),
        Prior::Recorded(p) => {
            let rewritten = cut || base < p.base || head < p.head || !p.still_names(durable, base);
            if rewritten && replica {
                // A replica's identity is its leader's lineage, numbered by the leader
                // alone: `generation + 1` here could be the name of a rewrite the
                // leader makes later. A fresh id is no name the leader can give; the
                // next replicated batch hands the leader's back.
                minted()
            } else {
                LogIdentity {
                    id: p.identity.id,
                    generation: p.identity.generation.saturating_add(u64::from(rewritten)),
                }
            }
        }
    };
    let now = Sidecar {
        identity,
        base,
        head,
        newest: frames(durable).last().map_or([0u8; 16], fingerprint),
    };
    let recorded = match &prior {
        Prior::Recorded(p) => Some(p),
        Prior::Absent | Prior::Unreadable => None,
    };
    if recorded != Some(&now) {
        // FAIL CLOSED: an identity told to clients but never recorded is one the next
        // open can contradict — after a repair, by reporting the generation from
        // before the cut, whose evidence the truncation removes. The log is refused
        // instead (and a repair refused before a byte is cut), naming the sidecar.
        write(log, &now).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "cannot record the identity of {} in {}: {e}",
                    log.display(),
                    sidecar_path(log).display()
                ),
            )
        })?;
    }
    Ok(now)
}

/// Take `leader` as this log's identity (a replica reporting its leader's), keeping
/// what the open saw. A no-op when it already is.
pub(crate) fn adopt(log: &Path, sidecar: &Sidecar, leader: LogIdentity) -> io::Result<Sidecar> {
    if sidecar.identity == leader {
        return Ok(sidecar.clone());
    }
    let adopted = Sidecar {
        identity: leader,
        ..sidecar.clone()
    };
    write(log, &adopted)?;
    Ok(adopted)
}

/// The stored frames in `bytes`, in order, without decoding a payload. `bytes` is a
/// prefix the log's recovery scan already accepted, so every header is well formed;
/// one that is short, or announces more bytes than follow, ends the walk. The length
/// is the header's `u32` at bytes 4..8 (the layout `astream_wire::frame` documents).
fn frames(bytes: &[u8]) -> impl Iterator<Item = &[u8]> + '_ {
    let mut pos = 0usize;
    std::iter::from_fn(move || {
        let header = bytes.get(pos..pos.checked_add(HEADER_SIZE)?)?;
        let len = u32::from_le_bytes(header[4..8].try_into().ok()?) as usize;
        let end = pos.checked_add(HEADER_SIZE)?.checked_add(len)?;
        let frame = bytes.get(pos..end)?;
        pos = end;
        Some(frame)
    })
}

/// FNV-1a-128 over `parts`, each prefixed with its length so no two different lists
/// hash the same bytes.
fn fnv1a_128(parts: &[&[u8]]) -> [u8; 16] {
    const OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
    const PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;
    let mut h = OFFSET;
    for part in parts {
        for &b in (part.len() as u64).to_le_bytes().iter().chain(part.iter()) {
            h ^= u128::from(b);
            h = h.wrapping_mul(PRIME);
        }
    }
    h.to_le_bytes()
}

/// A stored frame's fingerprint — over the bytes as stored, so an encrypted log's is
/// over ciphertext and says nothing about the plaintext.
fn fingerprint(frame: &[u8]) -> [u8; 16] {
    fnv1a_128(&[b"astream-log-id/frame/1", frame])
}

/// The id of a log that holds records and has no identity of its own yet:
/// deterministic in what it stores, so it is the same whichever build derives it and
/// however often — and different once the log holds anything else.
fn derived_id(durable: &[u8], base: u64, head: u64) -> [u8; 16] {
    fnv1a_128(&[
        b"astream-log-id/derived/1",
        &base.to_le_bytes(),
        &head.to_le_bytes(),
        frames(durable).next().unwrap_or(&[]),
        frames(durable).last().unwrap_or(&[]),
    ])
}

/// A fresh id for a log that has just been created (or never held a record): two
/// independent std `RandomState`s — keyed from the OS once per thread, then stepped per
/// construction — over a process-wide counter, the wall clock, the process id and the
/// path. Distinct per creation; not claimed cryptographically random.
fn minted_id(log: &Path) -> [u8; 16] {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    static MINTED: AtomicU64 = AtomicU64::new(0);
    let seq = MINTED.fetch_add(1, Ordering::Relaxed);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut id = [0u8; 16];
    for (i, half) in id.chunks_mut(8).enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u64(seq);
        h.write_u64(i as u64);
        h.write_u128(now);
        h.write_u32(std::process::id());
        log.hash(&mut h);
        half.copy_from_slice(&h.finish().to_le_bytes());
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(byte: u8, generation: u64) -> LogIdentity {
        LogIdentity {
            id: [byte; 16],
            generation,
        }
    }

    #[test]
    fn the_display_form_round_trips_and_nothing_else_parses() {
        let i = LogIdentity {
            id: *b"\x00\x01\x02\x03\x04\x05\x06\x07\x08\x09\x0a\x0b\x0c\xfd\xfe\xff",
            generation: 42,
        };
        let s = i.to_string();
        assert_eq!(s, "000102030405060708090a0b0cfdfeff.42");
        assert_eq!(LogIdentity::parse(&s), Some(i));
        for bad in [
            "",
            "000102030405060708090a0b0cfdfeff",
            "000102030405060708090a0b0cfdfeff.",
            "000102030405060708090A0B0CFDFEFF.42",
            "000102030405060708090a0b0cfdfeff.+42",
            "000102030405060708090a0b0cfdfe.42",
            "+00102030405060708090a0b0cfdfeff.42",
            "000102030405060708090a0b0cfdfeff.18446744073709551616",
        ] {
            assert_eq!(LogIdentity::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_wire_form_round_trips() {
        let i = ident(7, u64::MAX);
        let mut b = Vec::new();
        i.put(&mut b);
        assert_eq!(b.len(), LogIdentity::WIRE_LEN);
        assert_eq!(LogIdentity::from_wire(&b), Some(i));
        assert_eq!(LogIdentity::from_wire(&b[..23]), None);
    }

    #[test]
    fn a_sidecar_round_trips_and_a_damaged_one_reads_as_none() {
        let s = Sidecar {
            identity: ident(3, 9),
            base: 4,
            head: 11,
            newest: [5u8; 16],
        };
        let framed = Frame::new(s.encode()).encode().unwrap();
        assert_eq!(Sidecar::decode(&framed), Some(s.clone()));
        let mut flipped = framed.clone();
        *flipped.last_mut().unwrap() ^= 1;
        assert_eq!(Sidecar::decode(&flipped), None, "a CRC-failing sidecar");
        assert_eq!(Sidecar::decode(&framed[..framed.len() - 1]), None, "torn");
        let mut other_format = s.encode();
        other_format[4] = FORMAT + 1;
        let framed = Frame::new(other_format).encode().unwrap();
        assert_eq!(Sidecar::decode(&framed), None, "another sidecar format");
    }

    #[test]
    fn frames_walks_headers_and_stops_at_a_short_tail() {
        let mut bytes = Vec::new();
        for body in [&b"a"[..], b"bcd", b""] {
            bytes.extend_from_slice(&Frame::new(body.to_vec()).encode().unwrap());
        }
        let walked: Vec<&[u8]> = frames(&bytes).collect();
        assert_eq!(walked.len(), 3);
        assert_eq!(walked.concat(), bytes);
        assert_eq!(
            frames(&bytes[..bytes.len() - 1]).count(),
            2,
            "a short last frame"
        );
        assert_eq!(frames(&[]).count(), 0);
    }

    #[test]
    fn minted_ids_differ_and_derived_ones_repeat_only_for_the_same_bytes() {
        let p = Path::new("/nonexistent/log");
        assert_ne!(minted_id(p), minted_id(p));
        let one = Frame::new(b"one".to_vec()).encode().unwrap();
        let two = [one.clone(), Frame::new(b"two".to_vec()).encode().unwrap()].concat();
        assert_eq!(derived_id(&two, 0, 2), derived_id(&two, 0, 2));
        assert_ne!(derived_id(&two, 0, 2), derived_id(&one, 0, 1));
        assert_ne!(
            derived_id(&two, 0, 2),
            derived_id(&two, 5, 7),
            "base and head count"
        );
    }
}
