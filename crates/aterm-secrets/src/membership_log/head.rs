// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The vault's head (FL-146, `docs/DESIGN-vault-log-2026-11-14.md` §3 step 2): a sidecar beside
//! a Mac's records file that lets an open skip what a full one spends on records nothing reads.
//!
//! A full open reads every record of the file, verifies each one's signature, hashes each into its
//! id, and the resolve of this Mac's entries then opens every counted `Put` the log holds. On a
//! log of whole copies of one large entry nearly all of that is spent on replaced bodies
//! (ATERM-KI-1113-902: 1.15 s over 53.1 MB, of which about 0.95 MB was live). The head holds,
//! sealed with a MAC under a key derived from this Mac's vault identity:
//!
//! - which records file it describes, as that file stood ([`fsx::FileStamp`]), and the file's
//!   SHA-256: a stamp the file had settled under before it was read ([`fsx::stamp_settled`]), so
//!   no other bytes show it;
//! - every record of the file, in the log's order and at its place in the file: whole (the
//!   membership, epochs and wraps, each entry's winning `Put` with its siblings and the semantic
//!   op it carries, every tombstone, and every `Put` the resolve did not open), or shelved, its
//!   frame alone (header and body up to the ciphertext: [`Record::shelved`]) for each other
//!   opened `Put`;
//! - what the resolve read of each `Put` and `Delete` it opened, never a name or a value: which
//!   entry it writes, by a number the head gives the entry, and which write of that entry, by a
//!   number that is the same for two ops exactly where they write the same value, flags and
//!   anchor ([`Ops`]). An entry whose value carries the inner seal (a Claude Code token entry)
//!   is left out: which of its writes read the same depends on the epoch keys the reading log
//!   holds, which a log made from this one (a projection, a `Wrap` held since) changes, while
//!   every other entry's writes read the same under any keys that open them.
//!
//! The head holds no name and no value, but it does show structure to whoever reads the file
//! (mode 0600, beside the records file): the entry numbers follow the order of the entries'
//! audiences and names, the write numbers show which writes of an entry carry the same value, and
//! a `Delete` is tied to its entry. A reader of the records file alone, without this Mac's
//! identity, learns none of that.
//!
//! An open ([`open`]) that finds a head whose MAC verifies under this Mac's identity, whose
//! context is the reader's (this build's rules among it: [`context`]), and whose stamp is the
//! file's (or, where the stamp moved but not the length, whose digest is the file's, when the
//! open writes the head again under the new stamp) takes the records from the head without
//! verifying their
//! signatures again, reads nothing else of the file, and leaves each shelved `Put`'s ciphertext in
//! the file, read back and checked against its id when something asks for it: a peer, a write of
//! the file whole, an open whose register changed ([`payload::Shelved`]). The resolve of a log
//! read so takes a shelved `Put`'s entry and write from the head and opens none of them, but for
//! an entry another op of the log changes since, whose shelved `Put`s it opens as a full resolve
//! does. Any other head (missing, torn, altered, written under another identity or context, or
//! naming a file a writer has since appended to or replaced, or written by a build of other
//! rules) is not read: the open verifies the whole file, and the next resolve of the log it reads
//! writes the head again ([`Origin`]) from what that read found, without reading the file again.
//!
//! The head is a cache. Every record in it was verified in full by this Mac before the head was
//! written, and its bytes are the file's, so an open by it holds exactly what a full open of the
//! same file would. `current`, its layout and the wire are untouched: a build that knows no head
//! reads the file as before.

use super::payload::Source;
use super::{Kind, Log, Record, RecordId, Store, payload};
use crate::age::Identity;
use crate::fsx;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The head's first bytes, which name its format and version.
const MAGIC: &[u8] = b"aterm-vault-head/1\n";
/// The domain the head's MAC key is derived under from this Mac's identity.
const KEY_DOMAIN: &[u8] = b"aterm/vault-head/1 mac";
/// The most bytes a head holds: the records it holds whole stay under the stream bound, and a
/// shelved record's place is its frame and a few numbers.
const MAX_HEAD: usize = super::MAX_RECORDS_STREAM + 32 * 1024 * 1024;
/// A record held whole.
const WHOLE: u8 = 0;
/// A record shelved.
const SHELVED: u8 = 1;

/// The entry number of a Commons control `Put` ([`Op::register`]), which writes no entry.
pub(crate) const COMMONS: u32 = u32::MAX;
/// The write number of a `Delete` ([`Op::class`]).
pub(crate) const DELETE: u32 = u32::MAX;

/// SHA-256 of `bytes`: a record's id and a file's digest ([`super::digest`]).
pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    super::digest(bytes)
}

/// What the resolve read of one `Put` or `Delete` it opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Op {
    /// Which entry (audience and name) it writes, numbered by the head; [`COMMONS`] for a
    /// Commons control `Put`.
    pub(crate) register: u32,
    /// Which write of that entry: two ops of one entry share it exactly where they write the
    /// same thing (value, flags and anchor, or both delete); [`DELETE`] for a `Delete`.
    pub(crate) class: u32,
}

/// What a head says the resolve under one identity read of the ops it opened ([`Op`]), carried by
/// a log read through the head ([`Log::head_ops`]) and by every log made from it.
#[derive(Debug)]
pub(crate) struct Ops {
    recipient: [u8; 32],
    ops: BTreeMap<RecordId, Op>,
}

impl Ops {
    /// What the head says of `id`, where `recipient` is the identity whose resolve read it.
    pub(crate) fn op(&self, recipient: &[u8; 32], id: &RecordId) -> Option<Op> {
        (&self.recipient == recipient)
            .then(|| self.ops.get(id).copied())
            .flatten()
    }
}

impl Log {
    /// Whether the head this log was read through names `id`: an op that writes an entry whose
    /// value carries no inner seal, or a Commons control op ([`Ops`]). Which entry an op writes
    /// is in its outer plaintext, the same for every identity that opens it, so this holds for
    /// any reader, not only the identity whose resolve wrote the head.
    pub(crate) fn head_names(&self, id: &RecordId) -> bool {
        self.head
            .as_deref()
            .is_some_and(|ops| ops.ops.contains_key(id))
    }
}

/// What a resolve that writes the head ([`write`]) read: each op it opened, and the ones an open
/// must keep whole (each entry's winner, the op it carries and its siblings, and the Commons
/// control's winner).
#[derive(Default)]
pub(crate) struct Resolved {
    pub(crate) ops: BTreeMap<RecordId, Op>,
    pub(crate) keep: BTreeSet<RecordId>,
}

/// The records file a log is exactly ([`Log::origin`]): every record of it held, in the log, and
/// nothing else, as the file stood when it was read. A resolve of this log by this Mac's identity
/// writes the head for that file where none stands for it.
#[derive(Debug)]
pub(crate) struct Origin {
    head: PathBuf,
    stamp: fsx::FileStamp,
    /// The file's SHA-256, as the read that found its records hashed it.
    digest: [u8; 32],
    /// Where each record stands in the file and its length, as the read found them
    /// ([`Store::read_from`]): empty until a store takes this origin.
    places: Places,
    context: [u8; 32],
    /// The recipient of this Mac's identity, the one a head of this file is written for: a
    /// resolve under another (a test's, a tool's) writes none, so it never takes the place of
    /// the head this Mac's opens read.
    owner: [u8; 32],
    /// Whether the file had settled when the read began ([`fsx::stamp_settled`]), so its stamp
    /// shows these bytes alone: a head names no other, since a same-length rewrite in place
    /// within the file's own clock tick keeps its stamp.
    settled: bool,
    /// The recipients of the identities a head stands for, for this file, or whose resolve tried
    /// to write one: one read through it, or one a resolve wrote or failed to write (a full disk,
    /// a folder it may not write), which no later resolve of this log tries again.
    written: Mutex<BTreeSet<[u8; 32]>>,
}

impl Origin {
    /// The records file whose stamp is `stamp` and whose SHA-256 is `digest`, read whole under
    /// `context` ([`context`]) by a read that began at `began` ([`fsx::read_began`]), whose head
    /// is at `head` and is written for the identity whose recipient is `owner`.
    pub(crate) fn new(
        head: &Path,
        stamp: fsx::FileStamp,
        digest: [u8; 32],
        context: [u8; 32],
        owner: [u8; 32],
        began: std::time::SystemTime,
    ) -> Self {
        Self {
            head: head.to_path_buf(),
            stamp,
            digest,
            places: Places::new(),
            context,
            owner,
            settled: fsx::stamp_settled(&stamp, began),
            written: Mutex::new(BTreeSet::new()),
        }
    }

    fn stands_for(&self, recipient: &[u8; 32]) -> bool {
        &self.owner != recipient
            || self
                .written
                .lock()
                .is_ok_and(|written| written.contains(recipient))
    }

    fn stood_for(&self, recipient: [u8; 32]) {
        if let Ok(mut written) = self.written.lock() {
            written.insert(recipient);
        }
    }
}

/// The rules this build reads a head under: a digest of every source the crate's build reads
/// (its own and astream-aead's, every module only a test build declares left out, whatever its
/// file is named) and of the ring release `Cargo.lock` locks, whose Ed25519 verifier the record
/// check ends in (`build.rs`), so a build whose resolve, whose checks of a record or whose head
/// layout differ in any byte reads no head another build wrote, and a head is read only by the
/// build that wrote it or one built from the same sources and ring.
const RULES: &str = env!("ATERM_SECRETS_RULES");

/// What a store's records are read under, which a head must have been written under: this
/// build's rules ([`RULES`]), the vault, the keys a genesis is checked by, the genesis it pins
/// and the budgets its log holds records within.
#[must_use]
pub(crate) fn context(store: &Store, vault: &str) -> [u8; 32] {
    context_under(RULES, store, vault)
}

/// [`context`] under the rules `rules`.
fn context_under(rules: &str, store: &Store, vault: &str) -> [u8; 32] {
    let mut text = Vec::new();
    text.extend_from_slice(rules.as_bytes());
    text.push(0);
    text.extend_from_slice(vault.as_bytes());
    text.push(0);
    for key in store.keys().unwrap_or_default() {
        text.extend_from_slice(key);
    }
    text.push(0);
    text.extend_from_slice(&store.pinned_network().unwrap_or_default());
    text.extend_from_slice(format!("{:?}", store.budgets()).as_bytes());
    digest(&text)
}

/// The MAC key of the heads written under `identity`: HKDF-SHA-256 of its secret, in its own
/// domain, so no other use of the identity's key and no other Mac's identity makes it.
fn key(identity: &Identity) -> ring::hmac::Key {
    let prk =
        ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, KEY_DOMAIN).extract(identity.secret_bytes());
    ring::hmac::Key::from(
        prk.expand(&[KEY_DOMAIN], ring::hmac::HMAC_SHA256)
            .expect("an HMAC-SHA-256 key is within HKDF-SHA-256's bound"),
    )
}

fn put_stamp(out: &mut Vec<u8>, stamp: &fsx::FileStamp) {
    out.extend_from_slice(&stamp.dev.to_be_bytes());
    out.extend_from_slice(&stamp.ino.to_be_bytes());
    out.extend_from_slice(&stamp.len.to_be_bytes());
    out.extend_from_slice(&stamp.mtime_ns.to_be_bytes());
    out.extend_from_slice(&stamp.ctime_ns.to_be_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: usize) -> Option<()> {
    out.extend_from_slice(&u32::try_from(value).ok()?.to_be_bytes());
    Some(())
}

/// Each record's place in its records file and its length, by id.
type Places = BTreeMap<RecordId, (u64, usize)>;

/// Whether a resolve of `log` by the identity whose recipient is `recipient` writes its head:
/// `log` is exactly a records file ([`Log::origin`]) and no head stands for that file and
/// identity.
pub(crate) fn wants(log: &Log, recipient: &[u8; 32]) -> bool {
    #[cfg(test)]
    if super::entries::OUTER_ONLY.with(std::cell::Cell::get) {
        // A build before the inner seal, on a test's thread: its resolve reads other rules than
        // the build's own, and writes no head under them.
        return false;
    }
    log.origin
        .as_deref()
        .is_some_and(|origin| !origin.stands_for(recipient))
}

/// Write the head of `log` for `identity` from what its resolve read, where `log` is exactly the
/// records file its origin names and no head stands for that file and identity. It reads nothing
/// of the file: what the read that found its records saw ([`Origin`]) names it. Best effort, and
/// tried once for a log: a head not written leaves the next open a full one.
pub(crate) fn write(log: &Log, identity: &Identity, resolved: &Resolved) {
    let Some(origin) = log.origin.as_deref() else {
        return;
    };
    let recipient = identity.recipient().0;
    if !origin.settled || origin.stands_for(&recipient) || log.waiting() != 0 {
        return;
    }
    origin.stood_for(recipient);
    #[cfg(test)]
    TRIES.with(|tries| tries.set(tries.get() + 1));
    let Some(bytes) = encode(log, origin, identity, resolved) else {
        return;
    };
    if fsx::write_atomic(&origin.head, &bytes, 0o600).is_ok() {
        #[cfg(test)]
        WRITES.with(|writes| writes.set(writes.get() + 1));
    }
}

#[cfg(test)]
thread_local! {
    /// The heads written on this thread.
    pub(crate) static WRITES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The heads a resolve on this thread tried to write, written or not.
    pub(crate) static TRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The records files hashed whole on this thread by an open through a head
    /// ([`file_sha256`]).
    pub(crate) static HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The heads whose MAC an open on this thread checked ([`open`]).
    pub(crate) static MACS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn encode(log: &Log, origin: &Origin, identity: &Identity, resolved: &Resolved) -> Option<Vec<u8>> {
    if origin.places.len() != log.held_count() {
        return None;
    }
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&identity.recipient().0);
    out.extend_from_slice(&origin.context);
    put_stamp(&mut out, &origin.stamp);
    out.extend_from_slice(&origin.digest);
    put_u32(&mut out, log.held_count())?;
    for record in log.records() {
        let &(at, len) = origin.places.get(record.id())?;
        let frame = shelf_frame(record, resolved);
        out.push(if frame.is_some() { SHELVED } else { WHOLE });
        out.extend_from_slice(&at.to_be_bytes());
        put_u32(&mut out, len)?;
        match frame {
            Some(frame) => {
                out.extend_from_slice(record.id());
                put_u32(&mut out, frame.len())?;
                out.extend_from_slice(frame);
            }
            None => {
                let bytes = record.wire_bytes().ok()?;
                if bytes.len() != len {
                    return None;
                }
                out.extend_from_slice(&bytes);
            }
        }
    }
    put_u32(&mut out, resolved.ops.len())?;
    for (id, op) in &resolved.ops {
        log.get(id)?;
        out.extend_from_slice(id);
        out.extend_from_slice(&op.register.to_be_bytes());
        out.extend_from_slice(&op.class.to_be_bytes());
    }
    let mac = ring::hmac::sign(&key(identity), &out);
    out.extend_from_slice(mac.as_ref());
    Some(out)
}

/// The frame `record` is shelved by in the head: a `Put` the resolve opened that an open need not
/// keep whole ([`Resolved::keep`]).
fn shelf_frame<'a>(record: &'a Record, resolved: &Resolved) -> Option<&'a [u8]> {
    if record.kind() != Kind::Put
        || resolved.keep.contains(record.id())
        || !resolved.ops.contains_key(record.id())
    {
        return None;
    }
    record.frame()
}

/// A reader of a head's bytes.
struct Fields<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Fields<'a> {
    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(len)?;
        let taken = self.bytes.get(self.at..end)?;
        self.at = end;
        Some(taken)
    }
    fn array<const N: usize>(&mut self) -> Option<[u8; N]> {
        self.take(N)?.try_into().ok()
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.array()?))
    }
    fn len(&mut self) -> Option<usize> {
        usize::try_from(self.u32()?).ok()
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.array()?))
    }
    fn i128(&mut self) -> Option<i128> {
        Some(i128::from_be_bytes(self.array()?))
    }
}

/// The head at `head` beside the records file opened as `file`, which stood as `stamp` when it
/// was opened, read into `store` (empty, as the reader made it under `context` for this vault),
/// with the file's SHA-256 the head names: `None` where no head stands there that names this file
/// as it is, under this identity and context, with a MAC that verifies, and whose records `store`
/// takes, every one, in order.
pub(crate) fn open(
    head: &Path,
    file: &std::fs::File,
    stamp: fsx::FileStamp,
    context: [u8; 32],
    identity: &Identity,
    store: Store,
) -> Option<(Store, [u8; 32])> {
    let mut reader = fsx::open_private_reader(head).ok()??;
    if !names(&mut reader, stamp.len, context, identity) {
        return None;
    }
    let bytes = reader.read_private(MAX_HEAD).ok()?;
    let bytes = bytes.as_slice();
    let signed = bytes.len().checked_sub(32)?;
    #[cfg(test)]
    MACS.with(|macs| macs.set(macs.get() + 1));
    ring::hmac::verify(&key(identity), &bytes[..signed], &bytes[signed..]).ok()?;
    let mut fields = Fields {
        bytes: &bytes[..signed],
        at: 0,
    };
    if fields.take(MAGIC.len())? != MAGIC
        || fields.array::<32>()? != identity.recipient().0
        || fields.array::<32>()? != context
    {
        return None;
    }
    let named = fsx::FileStamp {
        dev: fields.u64()?,
        ino: fields.u64()?,
        len: fields.u64()?,
        mtime_ns: fields.i128()?,
        ctime_ns: fields.i128()?,
    };
    let file_digest = fields.array::<32>()?;
    let began = fsx::read_began();
    if named.len != stamp.len || (named != stamp && file_sha256(file, stamp.len)? != file_digest) {
        return None;
    }
    let source = Arc::new(Source::new(file.try_clone().ok()?, stamp.len));
    let count = fields.len()?;
    let mut records = Vec::with_capacity(count.min(super::MAX_HELD_RECORDS));
    let mut covered = 0u64;
    for _ in 0..count {
        let form = fields.take(1)?[0];
        let at = fields.u64()?;
        let len = fields.len()?;
        let end = at.checked_add(u64::try_from(len).ok()?)?;
        if end > stamp.len {
            return None;
        }
        covered = covered.checked_add(u64::try_from(len).ok()?)?;
        records.push(match form {
            WHOLE => Record::vouched(fields.take(len)?).ok()?,
            SHELVED => {
                let id = fields.array::<32>()?;
                let frame_len = fields.len()?;
                Record::shelved(id, len, fields.take(frame_len)?, at, Arc::clone(&source)).ok()?
            }
            _ => return None,
        });
    }
    if covered != stamp.len {
        return None;
    }
    let mut ops = BTreeMap::new();
    for _ in 0..fields.len()? {
        let id = fields.array::<32>()?;
        let op = Op {
            register: fields.u32()?,
            class: fields.u32()?,
        };
        ops.insert(id, op);
    }
    if fields.at != fields.bytes.len() {
        return None;
    }
    let recipient = identity.recipient().0;
    let ops = Ops { recipient, ops };
    let origin = Origin::new(head, stamp, file_digest, context, recipient, began);
    origin.stood_for(recipient);
    let read = store.read_through_head(records, ops, origin)?;
    if named != stamp
        && fsx::stamp_settled(&stamp, began)
        && file.metadata().ok().map(|meta| fsx::FileStamp::of(&meta)) == Some(stamp)
    {
        // The file's bytes under another stamp (a rename of the same bytes over it, a change of
        // its mode), settled before it was hashed and unchanged since: the head names the new
        // stamp from now on, so the next open does not hash the whole file again.
        restamped(head, &bytes[..signed], &stamp, identity);
    }
    Some((read, file_digest))
}

/// Whether the head `reader` holds says, in its fixed fields, that it was written for `identity`
/// under `context` for a records file of `len` bytes: read before the rest of the head and before
/// its MAC, unauthenticated, so an open that cannot take it (the first after an append, another
/// build's rules) reads none of the rest. A `false` only sends the open to a full one; a `true` is
/// checked again on the bytes the MAC authenticates.
fn names(reader: &mut fsx::OwnedReader, len: u64, context: [u8; 32], identity: &Identity) -> bool {
    use std::os::unix::fs::FileExt as _;
    let mut fixed = [0u8; FIXED_LEN];
    if reader.len() < FIXED_LEN as u64 || reader.file().read_exact_at(&mut fixed, 0).is_err() {
        return false;
    }
    let mut fields = Fields {
        bytes: &fixed,
        at: 0,
    };
    fields.take(MAGIC.len()) == Some(MAGIC)
        && fields.array::<32>() == Some(identity.recipient().0)
        && fields.array::<32>() == Some(context)
        && fields.u64().is_some()
        && fields.u64().is_some()
        && fields.u64() == Some(len)
}

/// The bytes before a head's records that [`names`] reads: its magic, recipient, context and the
/// stamp's device, inode and length.
const FIXED_LEN: usize = MAGIC.len() + 32 + 32 + 8 + 8 + 8;

/// Write `signed`, a head's bytes before its MAC that the open just checked, again naming
/// `stamp`, sealed under `identity`. Best effort: a head not written is hashed against the file
/// again at the next open.
fn restamped(head: &Path, signed: &[u8], stamp: &fsx::FileStamp, identity: &Identity) {
    let at = MAGIC.len() + 32 + 32;
    let mut stamped = Vec::with_capacity(STAMP_LEN);
    put_stamp(&mut stamped, stamp);
    let Some(field) = signed.get(at..at + STAMP_LEN) else {
        return;
    };
    if field.len() != stamped.len() {
        return;
    }
    let mut out = Vec::with_capacity(signed.len() + 32);
    out.extend_from_slice(&signed[..at]);
    out.extend_from_slice(&stamped);
    out.extend_from_slice(&signed[at + STAMP_LEN..]);
    let mac = ring::hmac::sign(&key(identity), &out);
    out.extend_from_slice(mac.as_ref());
    let _ = fsx::write_atomic(head, &out, 0o600);
}

/// The bytes a stamp takes in a head ([`put_stamp`]).
const STAMP_LEN: usize = 8 + 8 + 8 + 16 + 16;

/// SHA-256 of the first `len` bytes of `file`, which must hold that many.
fn file_sha256(file: &std::fs::File, len: u64) -> Option<[u8; 32]> {
    use std::os::unix::fs::FileExt as _;
    #[cfg(test)]
    HASHES.with(|hashes| hashes.set(hashes.get() + 1));
    let mut whole = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0; 256 * 1024];
    let mut at = 0u64;
    while at < len {
        let want = usize::try_from((len - at).min(buffer.len() as u64)).ok()?;
        let read = file.read_at(&mut buffer[..want], at).ok()?;
        if read == 0 {
            return None;
        }
        whole.update(&buffer[..read]);
        at += read as u64;
    }
    let mut out = [0; 32];
    out.copy_from_slice(whole.finish().as_ref());
    Some(out)
}

impl Record {
    /// Its bytes before its payload, for a `Put`, `Delete` or `Wrap`: what shelving it keeps.
    pub(crate) fn frame(&self) -> Option<&[u8]> {
        let payload = match self.body() {
            super::Body::Put { ciphertext, .. } | super::Body::Delete { ciphertext, .. } => {
                ciphertext.len()
            }
            super::Body::Wrap { capsule, .. } => capsule.len(),
            _ => return None,
        };
        let frame = self.wire_len().checked_sub(payload)?.checked_sub(64)?;
        match &self.wire {
            payload::Wire::Shelved(shelved) => {
                (shelved.frame.len() == frame).then_some(&shelved.frame[..])
            }
            payload::Wire::Whole(bytes) => bytes.get(..frame),
        }
    }
}

impl Store {
    /// This empty store with `records` held, each in its order, as a head read them
    /// ([`open`]), its log carrying the head's `ops` and `origin`: `None` where it does not take
    /// every one of them, held.
    fn read_through_head(mut self, records: Vec<Record>, ops: Ops, origin: Origin) -> Option<Self> {
        if !self.is_empty() {
            return None;
        }
        let count = records.len();
        for record in records {
            if !self.insert(record).ok()? {
                return None;
            }
        }
        let log = self.log.as_mut()?;
        if log.held_count() != count || log.waiting() != 0 || !self.unrooted.is_empty() {
            return None;
        }
        log.head = Some(Arc::new(ops));
        log.origin = Some(Arc::new(origin));
        Some(self)
    }

    /// Mark this store, recovered from the whole records file `origin` names as it stood
    /// ([`Store::recovering`]), as exactly that file: a resolve of its log writes the head of that
    /// file ([`write`]), each record at the place the read found it. Nothing is marked where the
    /// store is not exactly the file: a frame it refused or took twice, a record waiting on a
    /// parent, none held, or records held since the read.
    pub(crate) fn read_from(&mut self, mut origin: Origin) {
        let Some(order) = self.file_order.take() else {
            return;
        };
        let Some(log) = self.log.as_mut() else {
            return;
        };
        if log.waiting() != 0 || !self.unrooted.is_empty() || order.len() != log.held_count() {
            return;
        }
        let mut at = 0u64;
        for &(id, len) in order.iter() {
            if log.get(&id).is_none() || origin.places.insert(id, (at, len)).is_some() {
                return;
            }
            let Some(next) = u64::try_from(len).ok().and_then(|len| at.checked_add(len)) else {
                return;
            };
            at = next;
        }
        if at == origin.stamp.len {
            log.origin = Some(Arc::new(origin));
        }
    }

    /// The file a shelved record of this store is read back from, for a test that counts what an
    /// open read of it.
    #[cfg(test)]
    pub(crate) fn shelf_source(&self) -> Option<Arc<Source>> {
        self.log()?.records().find_map(|record| match &record.wire {
            payload::Wire::Shelved(shelved) => Some(Arc::clone(&shelved.source)),
            payload::Wire::Whole(_) => None,
        })
    }
}

#[cfg(test)]
#[path = "head_tests.rs"]
mod tests;
