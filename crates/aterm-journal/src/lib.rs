// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private strict astream log whose acknowledgement includes a durable hash of
//! its physical prefix. Lost acknowledged bytes refuse open; unacknowledged
//! suffixes are archived before rollback. This protects accidental partial loss,
//! not rollback of the entire directory by a privileged adversary. The caller
//! authenticates every replayed record and supplies an independently pinned anchor.
//! One writer at a time holds the exclusive lock; readers take none, and see only
//! the prefix the checkpoint attests ([`read_committed`]).
#![forbid(unsafe_code)]
#![cfg(unix)]

use astream_broker::{BrokerLog, BrokerRecord, Durability, OpenOptions as LogOptions, Prepared};
use astream_wire::Offset;
use aterm_uds::private_file::{self, PrivateFileError};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

/// Create or validate a private directory and durably publish its name.
/// # Errors
/// Propagates private-directory or parent-directory barrier failures.
pub fn create_private_dir_durable(dir: &Path) -> io::Result<()> {
    private_file::create_private_dir(dir)?;
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        aterm_durable::durable_dir(parent)?;
    }
    Ok(())
}

/// Write a private file and require the containing directory's durability
/// barrier to succeed. A failure after rename is an uncertain outcome, so the
/// caller must fence publication and reopen rather than assuming no write.
/// # Errors
/// Propagates private-path, file-write, rename and directory-barrier failures.
pub fn write_private_durable(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    private_file::write_private(dir, name, bytes)?;
    aterm_durable::durable_dir(dir)
}

/// Conservative framing allowance per append, in addition to body and subject.
pub const APPEND_OVERHEAD: usize = 256;

/// Fixed resource bounds for one immutable generation. Retention is deliberately
/// unavailable: dropping an old record could erase authorization or liabilities.
#[derive(Clone, Copy)]
pub struct Limits {
    pub records: usize,
    pub bytes: usize,
    /// Maximum application body size including its local receipt metadata.
    pub record_bytes: usize,
}
struct Layout {
    name: String,
    limits: Limits,
}
impl Layout {
    fn new(name: &str, anchor: &[u8], limits: Limits) -> io::Result<Self> {
        if name.is_empty()
            || name.len() > 32
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            || anchor.is_empty()
            || anchor.len() > 4096
            || limits.records == 0
            || limits.record_bytes == 0
            || limits.bytes < limits.record_bytes
            || limits.bytes.checked_add(limits.record_bytes).is_none()
        {
            return Err(io::Error::other("invalid journal layout or limits"));
        }
        Ok(Self {
            name: name.into(),
            limits,
        })
    }
    fn file(&self, suffix: &str) -> String {
        format!("{}.{suffix}", self.name)
    }
    fn archive_prefix(&self) -> String {
        format!("{}-", self.name)
    }
}

/// One exclusive private writer. Drop before performing network I/O. Only
/// append, bounded replay and immutable observations are exposed; astream's
/// retention and sidecar mutation APIs cannot bypass the acknowledged prefix.
/// A caller that only reads takes no lock at all: [`read_committed`] reads the
/// acknowledged prefix beside a live writer.
pub struct Log {
    log: BrokerLog,
    checkpoint: Checkpoint,
    dir: PathBuf,
    layout: Layout,
    poisoned: bool,
    pending_anchor: Option<Vec<u8>>,
    anchor: Vec<u8>,
    identity: (u64, u64),
    writer: WriterLock,
}
impl Log {
    /// Local directory supplied when this exclusive journal was opened. This
    /// identifies its custody tree; it grants no authority over another tree.
    pub fn directory(&self) -> &Path {
        &self.dir
    }

    /// Open an independently identified journal. `established` must come from
    /// the caller's durable enrollment claim, never from existence of this log.
    /// A false value permits initialization only while the anchor is absent.
    /// Authenticate replay and call `finish_open` before appending.
    /// # Errors
    /// Refuses damaged/private state, changed anchors, resource exhaustion,
    /// competing writers, or a failed durable write.
    pub fn open(
        dir: &Path,
        name: &str,
        anchor: &[u8],
        established: bool,
        limits: Limits,
    ) -> io::Result<Self> {
        Self::open_until(dir, name, anchor, established, limits, Instant::now())
    }

    /// [`Self::open`], allowing a competing local writer to finish until
    /// `deadline`. Only the dedicated writer lock waits; replay, pin checks and
    /// physical-file lock checks still run freshly after acquiring it.
    /// # Errors
    /// Returns [`Self::open`]'s errors, including `WouldBlock` if the local writer
    /// remains active at the deadline.
    pub fn open_until(
        dir: &Path,
        name: &str,
        anchor: &[u8],
        established: bool,
        limits: Limits,
        deadline: Instant,
    ) -> io::Result<Self> {
        let layout = Layout::new(name, anchor, limits)?;
        let lock = lock(dir, &layout, deadline)
            .map_err(|error| stage_error(&layout, "writer admission", error))?;
        let fresh = match private_file::read_private(dir, &layout.file("anchor"), 4096) {
            Ok(bytes) if bytes == anchor => false,
            Ok(_) => return Err(io::Error::other("journal anchor changed")),
            Err(PrivateFileError::Missing) if !established => true,
            Err(PrivateFileError::Missing) => {
                return Err(io::Error::other("established journal anchor is missing"));
            }
            Err(_) => return Err(io::Error::other("journal anchor is not private")),
        };
        match private_file::open_private(dir, &layout.file("astream")) {
            Ok(_) => {}
            Err(PrivateFileError::Missing) if fresh => {
                write_private_durable(dir, &layout.file("astream"), &[])?
            }
            Err(_) => return Err(io::Error::other("journal is not a private regular file")),
        }
        let identity = recover_uncommitted(dir, &layout, fresh)
            .map_err(|error| stage_error(&layout, "recovery", error))?;
        let mut checkpoint = None;
        let log = BrokerLog::open_opts(
            dir.join(layout.file("astream")),
            LogOptions::new(Durability::Strict),
            || {
                let checked = Checkpoint::load(dir, &layout, fresh)?;
                let (_, metadata) = private_file::open_private(dir, &layout.file("astream"))
                    .map_err(|_| io::Error::other("journal is not private"))?;
                if metadata.len() != checked.length || (metadata.dev(), metadata.ino()) != identity
                {
                    return Err(io::Error::other("journal changed between its writer locks"));
                }
                checkpoint = Some(checked);
                Ok(Prepared::Nothing)
            },
        )
        .map_err(|error| stage_error(&layout, "astream open", error))?;
        let mut checkpoint =
            checkpoint.ok_or_else(|| io::Error::other("journal was not prepared"))?;
        let stats = log.stats();
        if stats.retained_bytes > limits.bytes
            || stats.retained_records > limits.records
            || stats.base != Offset::ZERO
            || stats.head.0 != stats.retained_records as u64
        {
            return Err(io::Error::other(
                "journal capacity or unretained prefix is invalid",
            ));
        }
        let mut scan = Offset::ZERO;
        while scan.0 < stats.head.0 {
            let page = log.read_range(scan, 256);
            if page.is_empty() {
                return Err(io::Error::other(
                    "journal capacity or unretained prefix is invalid",
                ));
            }
            for record in &page {
                if record.body.len() > limits.record_bytes {
                    return Err(io::Error::other("journal record size exceeds its capacity"));
                }
            }
            scan = Offset(scan.0 + page.len() as u64);
        }
        checkpoint.extend(dir, &layout, stats.retained_bytes as u64)?;
        checkpoint.save(dir, &layout)?;
        let (_, metadata) = private_file::open_private(dir, &layout.file("astream"))
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        aterm_durable::durable_dir(dir)?;
        if (metadata.dev(), metadata.ino()) != identity {
            return Err(io::Error::other("journal inode changed during open"));
        }
        let opened = Self {
            log,
            checkpoint,
            dir: dir.into(),
            layout,
            poisoned: false,
            pending_anchor: fresh.then(|| anchor.to_vec()),
            anchor: anchor.to_vec(),
            identity,
            writer: lock,
        };
        opened.intact()?;
        Ok(opened)
    }
    /// Finish opening only after the caller has authenticated all replayed
    /// records against its independent anchor. A failed replay must not pin a
    /// different anchor over an otherwise recoverable unanchored history.
    /// # Errors
    /// Propagates anchor publication or directory durability failures.
    pub fn finish_open(&mut self) -> io::Result<()> {
        self.intact()?;
        if let Some(anchor) = &self.pending_anchor {
            write_private_durable(&self.dir, &self.layout.file("anchor"), anchor)?;
            self.pending_anchor = None;
        }
        Ok(())
    }
    /// A failed append or checkpoint fences this handle until reopen.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned || self.intact().is_err()
    }
    fn intact(&self) -> io::Result<()> {
        // One directory verdict for the four files, each still opened and judged on its
        // descriptor: every access calls this, and open(2) is the cost.
        let dir = private_file::JudgedDir::judge(&self.dir)
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        let (_, metadata) = dir
            .open(&self.layout.file("astream"))
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        let (_, lock) = dir
            .open(&self.layout.file("lock"))
            .map_err(|_| io::Error::other("journal lost its writer lease"))?;
        let held = self.writer.0.metadata()?;
        let anchor_ok = match dir.read(&self.layout.file("anchor"), 4096) {
            Ok(bytes) => bytes == self.anchor,
            Err(PrivateFileError::Missing) => self.pending_anchor.is_some(),
            Err(_) => false,
        };
        if (metadata.dev(), metadata.ino()) != self.identity
            || metadata.len() != self.log.stats().retained_bytes as u64
            || (lock.dev(), lock.ino()) != (held.dev(), held.ino())
            || !anchor_ok
            || dir
                .read(&self.layout.file("checkpoint"), 40)
                .ok()
                .as_deref()
                != Some(&self.checkpoint.bytes())
        {
            return Err(io::Error::other("journal or writer lease was replaced"));
        }
        Ok(())
    }
    pub fn bytes(&self) -> usize {
        self.log.stats().retained_bytes
    }
    /// The prefix this writer's checkpoint attests, comparable with what
    /// [`read_committed`] reports for the same bytes.
    pub fn acknowledged(&self) -> Acknowledged {
        Acknowledged {
            identity: self.identity,
            length: self.checkpoint.length,
            digest: self.checkpoint.hash.clone().finalize(),
        }
    }
    /// Bounded local replay; callers must validate each record before use.
    /// # Errors
    /// An uncertain append withholds the handle's observations until reopen.
    pub fn read_range(&self, offset: Offset, count: usize) -> io::Result<Vec<Arc<BrokerRecord>>> {
        if self.is_poisoned() {
            return Err(io::Error::other("uncertain journal: reopen"));
        }
        Ok(self.log.read_range(offset, count.min(256)))
    }
    /// Acknowledge only after the strict astream append and prefix checkpoint.
    /// # Errors
    /// Capacity and invalid input refuse before mutation. Any uncertain write
    /// fences this handle; reopen and retry the exact application bytes.
    pub fn append(&mut self, subject: &str, body: Vec<u8>) -> io::Result<Offset> {
        if self.is_poisoned() {
            return Err(io::Error::other(
                "uncertain journal: reopen and retry exact bytes",
            ));
        }
        if self.pending_anchor.is_some() {
            return Err(io::Error::other(
                "authenticate replay and finish opening before append",
            ));
        }
        if subject.is_empty() || astream_broker::store::is_hidden_subject(subject) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid journal subject",
            ));
        }
        let stats = self.log.stats();
        let framed = body
            .len()
            .saturating_add(subject.len())
            .saturating_add(APPEND_OVERHEAD);
        if body.len() > self.layout.limits.record_bytes
            || framed > astream_broker::MAX_RECORD_PAYLOAD
            || stats.retained_records >= self.layout.limits.records
            || stats.retained_bytes.saturating_add(framed) > self.layout.limits.bytes
        {
            return Err(io::Error::other("journal capacity exhausted"));
        }
        let sequence = stats
            .head
            .0
            .checked_add(1)
            .ok_or_else(|| io::Error::other("journal sequence exhausted"))?;
        let offset = match self.log.publish_checked(1, sequence, subject.into(), body) {
            Ok((offset, _)) => offset,
            Err(_) => {
                self.poisoned = true;
                return Err(io::Error::other(
                    "uncertain strict journal append: reopen and retry exact bytes",
                ));
            }
        };
        if self.intact().is_err() {
            self.poisoned = true;
            return Err(io::Error::other("journal changed during append; reopen"));
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Published);
        if self
            .checkpoint
            .extend(
                &self.dir,
                &self.layout,
                self.log.stats().retained_bytes as u64,
            )
            .and_then(|()| self.checkpoint.save(&self.dir, &self.layout))
            .is_err()
        {
            self.poisoned = true;
            return Err(io::Error::other(
                "uncertain journal checkpoint: reopen and retry exact bytes",
            ));
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Checkpointed);
        Ok(offset)
    }
}

/// One journal file's acknowledged prefix: the astream file's `(dev, ino)` and
/// the exact bytes its checkpoint attests. Equal values name identical bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acknowledged {
    /// The astream file's `(dev, ino)`.
    pub identity: (u64, u64),
    /// Acknowledged physical bytes, from offset zero.
    pub length: u64,
    /// SHA-256 of exactly those bytes.
    pub digest: [u8; 32],
}

/// An acknowledged prefix a reader has already decoded, and how many records
/// it holds. Hand it back to [`read_committed`] to be told only what changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefix {
    acknowledged: Acknowledged,
    records: u64,
    max_record_bytes: usize,
}
impl Prefix {
    pub const fn acknowledged(&self) -> Acknowledged {
        self.acknowledged
    }
    pub const fn records(&self) -> u64 {
        self.records
    }
}

/// What [`read_committed`] found, measured against the prefix the caller holds.
/// The caller authenticates every record before it relies on them, and keeps
/// the new [`Prefix`] only once all of them passed.
pub enum Committed {
    /// The same file still attests exactly the caller's prefix.
    Unchanged,
    /// The caller's prefix is still this one's head; `records` follow it, the
    /// first at offset `known.records()`.
    Extended {
        records: Vec<BrokerRecord>,
        prefix: Prefix,
    },
    /// No prefix was given, or the file, the bytes or the length no longer
    /// extend it: replay `records` from offset zero.
    Rebuilt {
        records: Vec<BrokerRecord>,
        prefix: Prefix,
    },
}

/// Read an established journal's acknowledged prefix without taking any lock,
/// so it works beside a live writer. The checkpoint is read first; then exactly
/// the bytes it counts are read from the astream file and must hash to it. A
/// writer replaces the checkpoint by atomic rename only after the bytes it
/// covers are durable, never rewrites bytes below it, and leaves a torn or
/// unacknowledged tail only past it, so an append is wholly in or wholly out.
/// A checkpoint renamed over while it was being opened or read is read again,
/// a bounded number of times; past that, the one read last stands, which the
/// newer one only extends.
/// With `known`, its bytes are hashed again: when they still head the new
/// prefix of the same file, only the records after them are decoded.
/// Nothing is repaired, archived or written.
/// # Errors
/// Refuses an invalid name, anchor or limits (as [`Log::open`] does); a
/// missing, changed or not private anchor; a missing, malformed or not private
/// checkpoint (one with a second link included); a
/// prefix over the limits; an astream file that is missing, not private or
/// shorter than the prefix; bytes that do not hash to the checkpoint; and an attested
/// prefix that does not decode into in-order records. A failed read of the
/// astream file's bytes is passed on as it is.
pub fn read_committed(
    dir: &Path,
    name: &str,
    anchor: &[u8],
    limits: Limits,
    known: Option<&Prefix>,
) -> io::Result<Committed> {
    let layout = Layout::new(name, anchor, limits)?;
    // One directory verdict for the three files, as in `Log::intact`.
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| io::Error::other("journal anchor is not private"))?;
    match judged.read(&layout.file("anchor"), 4096) {
        Ok(bytes) if bytes == anchor => {}
        Ok(_) => return Err(io::Error::other("journal anchor changed")),
        Err(PrivateFileError::Missing) => {
            return Err(io::Error::other("established journal anchor is missing"));
        }
        Err(_) => return Err(io::Error::other("journal anchor is not private")),
    }
    // A writer's save renames a new checkpoint over the name, so an open that
    // got the old inode just before reads the checkpoint it replaced, whose
    // descriptor is left no link. The name then holds the newer checkpoint:
    // read it again. A second link is refused, at the open or after it.
    let malformed = || io::Error::other("journal checkpoint is malformed or not private");
    let mut relinked = 0;
    let checkpoint = loop {
        let (file, _) = judged
            .open(&layout.file("checkpoint"))
            .map_err(|_| malformed())?;
        #[cfg(test)]
        tests::read_step(tests::Reading::Opened);
        let mut bytes = Vec::with_capacity(41);
        (&file)
            .take(41)
            .read_to_end(&mut bytes)
            .map_err(|_| malformed())?;
        match file.metadata().map_err(|_| malformed())?.nlink() {
            0 if relinked < 3 => {
                relinked += 1;
                #[cfg(test)]
                tests::read_step(tests::Reading::Relinked);
                continue;
            }
            0 | 1 => {}
            // Linked again since the open, which a save never does.
            _ => return Err(malformed()),
        }
        if bytes.len() != 40 {
            return Err(malformed());
        }
        break bytes;
    };
    let mut length = [0; 8];
    length.copy_from_slice(&checkpoint[..8]);
    let length = u64::from_le_bytes(length);
    let mut digest = [0; 32];
    digest.copy_from_slice(&checkpoint[8..]);
    if length > limits.bytes as u64 {
        return Err(io::Error::other(
            "journal acknowledged prefix exceeds its capacity",
        ));
    }
    #[cfg(test)]
    tests::read_step(tests::Reading::Checkpointed);
    let (mut file, metadata) = judged
        .open(&layout.file("astream"))
        .map_err(|_| io::Error::other("journal is not a private regular file"))?;
    if metadata.len() < length {
        return Err(io::Error::other("journal lost acknowledged bytes"));
    }
    let acknowledged = Acknowledged {
        identity: (metadata.dev(), metadata.ino()),
        length,
        digest,
    };
    let mut bytes = vec![
        0;
        usize::try_from(length)
            .map_err(|_| io::Error::other("journal checkpoint length overflow"))?
    ];
    file.read_exact(&mut bytes)?;
    let mut hash = aterm_digest::Sha256::new();
    let mut hashed = 0;
    let mut extends = None;
    if let Some(known) = known
        && known.acknowledged.identity == acknowledged.identity
        && known.acknowledged.length <= length
        && known.records <= limits.records as u64
        && known.max_record_bytes <= limits.record_bytes
    {
        hashed = usize::try_from(known.acknowledged.length)
            .map_err(|_| io::Error::other("journal checkpoint length overflow"))?;
        hash.update(&bytes[..hashed]);
        if hash.clone().finalize() == known.acknowledged.digest {
            extends = Some(known);
        }
    }
    hash.update(&bytes[hashed..]);
    if hash.finalize() != digest {
        return Err(io::Error::other(
            "acknowledged journal history changed; preserve it for authenticated repair",
        ));
    }
    match extends {
        Some(known) if known.acknowledged.length == length => Ok(Committed::Unchanged),
        Some(known) => {
            let records = decode(&bytes[hashed..], known.records, &layout)?;
            let max_record_bytes = records
                .iter()
                .map(|r| r.body.len())
                .max()
                .unwrap_or(0)
                .max(known.max_record_bytes);
            Ok(Committed::Extended {
                prefix: Prefix {
                    acknowledged,
                    records: known.records + records.len() as u64,
                    max_record_bytes,
                },
                records,
            })
        }
        None => {
            let records = decode(&bytes, 0, &layout)?;
            let max_record_bytes = records.iter().map(|r| r.body.len()).max().unwrap_or(0);
            Ok(Committed::Rebuilt {
                prefix: Prefix {
                    acknowledged,
                    records: records.len() as u64,
                    max_record_bytes,
                },
                records,
            })
        }
    }
}

/// Decode whole, in-order records from attested bytes. Every frame must be
/// complete: an attested prefix ends on a record boundary, so anything else is
/// damage, never a tail to cut.
fn decode(mut bytes: &[u8], first: u64, layout: &Layout) -> io::Result<Vec<BrokerRecord>> {
    let undecodable = || io::Error::other("acknowledged journal prefix does not decode");
    let mut records = Vec::new();
    while !bytes.is_empty() {
        let offset = first + records.len() as u64;
        if offset >= layout.limits.records as u64 {
            return Err(io::Error::other("journal record capacity exceeded"));
        }
        let Ok(Some((payload, consumed))) = astream_wire::frame::decode_frame_slice(bytes) else {
            return Err(undecodable());
        };
        if payload.is_empty() || consumed == 0 {
            return Err(undecodable());
        }
        let record = BrokerRecord::from_payload(payload).ok_or_else(undecodable)?;
        if record.seq.0 != offset {
            return Err(undecodable());
        }
        if record.body.len() > layout.limits.record_bytes {
            return Err(io::Error::other("journal record size exceeds its capacity"));
        }
        records.push(record);
        bytes = bytes.get(consumed..).ok_or_else(undecodable)?;
    }
    Ok(records)
}

struct WriterLock(File);
impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}
fn lock(dir: &Path, layout: &Layout, deadline: Instant) -> io::Result<WriterLock> {
    create_private_dir_durable(dir)?;
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(dir.join(layout.file("lock")))
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let (file, metadata) = private_file::open_private(dir, &layout.file("lock"))
        .map_err(|_| io::Error::other("journal lock is not private"))?;
    aterm_uds::keep_out_of_children(&file)?;
    private_file::lock_exclusive_until_with(
        &file,
        deadline,
        "another local journal writer is active",
        || {
            #[cfg(test)]
            tests::lock_held();
        },
    )?;
    let lock = WriterLock(file);
    let (_, current) = private_file::open_private(dir, &layout.file("lock"))
        .map_err(|_| io::Error::other("journal lock is not private"))?;
    if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
        return Err(io::Error::other("journal lock changed while acquiring it"));
    }
    Ok(lock)
}

fn lock_error(error: std::fs::TryLockError) -> io::Error {
    match error {
        std::fs::TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "another local journal writer is active",
        ),
        std::fs::TryLockError::Error(error) => error,
    }
}

fn stage_error(layout: &Layout, stage: &str, error: io::Error) -> io::Error {
    // `name` passed the bounded ASCII journal-layout validation before this
    // diagnostic is formed. No record, anchor or application payload is read.
    io::Error::new(
        error.kind(),
        format!("journal {} {stage}: {error}", layout.name),
    )
}

/// Digest of the exact acknowledged physical prefix. Kept incrementally in
/// memory, but reconstructed and checked before astream can repair any tail.
struct Checkpoint {
    length: u64,
    hash: aterm_digest::Sha256,
}
impl Checkpoint {
    fn load(dir: &Path, layout: &Layout, fresh: bool) -> io::Result<Self> {
        let mut checkpoint = Self {
            length: 0,
            hash: aterm_digest::Sha256::new(),
        };
        match private_file::read_private(dir, &layout.file("checkpoint"), 40) {
            Ok(bytes) if bytes.len() == 40 => {
                let length = u64::from_le_bytes(
                    bytes[..8]
                        .try_into()
                        .map_err(|_| io::Error::other("invalid journal checkpoint"))?,
                );
                checkpoint.extend(dir, layout, length)?;
                if checkpoint.hash.clone().finalize() != bytes[8..] {
                    return Err(io::Error::other(
                        "acknowledged journal history changed; preserve it for authenticated repair",
                    ));
                }
            }
            Err(PrivateFileError::Missing) if fresh => {
                let (_, metadata) = private_file::open_private(dir, &layout.file("astream"))
                    .map_err(|_| io::Error::other("journal is not private"))?;
                if metadata.len() != 0 {
                    return Err(io::Error::other(
                        "nonempty journal has no acknowledgement checkpoint",
                    ));
                }
            }
            _ => {
                return Err(io::Error::other(
                    "journal checkpoint is malformed or not private",
                ));
            }
        }
        Ok(checkpoint)
    }
    fn extend(&mut self, dir: &Path, layout: &Layout, length: u64) -> io::Result<()> {
        let (mut file, metadata) = private_file::open_private(dir, &layout.file("astream"))
            .map_err(|_| io::Error::other("journal is not private"))?;
        if length < self.length
            || length > layout.limits.bytes as u64
            || metadata.len() < length
            || metadata.len() > (layout.limits.bytes + layout.limits.record_bytes) as u64
        {
            return Err(io::Error::other(
                "journal lost acknowledged bytes or exceeded its capacity",
            ));
        }
        file.seek(SeekFrom::Start(self.length))?;
        let mut remaining = length - self.length;
        let mut buffer = [0; 8192];
        while remaining != 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| io::Error::other("journal checkpoint length overflow"))?;
            file.read_exact(&mut buffer[..count])?;
            self.hash.update(&buffer[..count]);
            remaining -= count as u64;
        }
        self.length = length;
        Ok(())
    }
    fn bytes(&self) -> [u8; 40] {
        let mut bytes = [0; 40];
        bytes[..8].copy_from_slice(&self.length.to_le_bytes());
        bytes[8..].copy_from_slice(&self.hash.clone().finalize());
        bytes
    }
    fn save(&self, dir: &Path, layout: &Layout) -> io::Result<()> {
        write_private_durable(dir, &layout.file("checkpoint"), &self.bytes())
    }
}

/// The checkpoint is the application's commit point. Preserve any bytes after
/// it for diagnosis, then roll back that unacknowledged suffix. The application
/// writer and the physical file are both locked while this happens; astream's
/// prepare callback rechecks the prefix after taking over the physical lock.
fn recover_uncommitted(dir: &Path, layout: &Layout, fresh: bool) -> io::Result<(u64, u64)> {
    let (_, admitted) = private_file::open_private(dir, &layout.file("astream"))
        .map_err(|_| io::Error::other("journal is not private"))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(layout.file("astream")))?;
    let opened = file.metadata()?;
    if opened.dev() != admitted.dev()
        || opened.ino() != admitted.ino()
        || opened.mode() != admitted.mode()
        || opened.uid() != admitted.uid()
    {
        return Err(io::Error::other("journal changed during private open"));
    }
    aterm_uds::keep_out_of_children(&file)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "WriterLock is CLOEXEC/CLOFORK and unlocks the physical file before transferring ownership to astream"
    )]
    file.try_lock()
        .map_err(lock_error)
        .map_err(|error| stage_error(layout, "physical recovery lock", error))?;
    let mut held = WriterLock(file);
    let checkpoint = Checkpoint::load(dir, layout, fresh)?;
    let (_, checked) = private_file::open_private(dir, &layout.file("astream"))
        .map_err(|_| io::Error::other("journal is not private"))?;
    if checked.dev() != opened.dev() || checked.ino() != opened.ino() {
        return Err(io::Error::other(
            "journal changed while its prefix was checked",
        ));
    }
    let tail_len = checked
        .len()
        .checked_sub(checkpoint.length)
        .ok_or_else(|| io::Error::other("journal lost acknowledged bytes"))?;
    if tail_len == 0 {
        return Ok((opened.dev(), opened.ino()));
    }
    let mut tail = vec![
        0;
        usize::try_from(tail_len)
            .map_err(|_| io::Error::other("journal suffix too large"))?
    ];
    held.0.seek(SeekFrom::Start(checkpoint.length))?;
    held.0.read_exact(&mut tail)?;
    let name = format!(
        "{}uncommitted-{}",
        layout.archive_prefix(),
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&tail))
    );
    match private_file::read_private(dir, &name, layout.limits.bytes + layout.limits.record_bytes) {
        Ok(old) if old == tail => {}
        Err(PrivateFileError::Missing) => {
            let mut bytes = tail.len() as u64;
            let mut archives = 0;
            for (count, entry) in std::fs::read_dir(dir)?.enumerate() {
                if count >= layout.limits.records.saturating_add(32) {
                    return Err(io::Error::other("journal directory exceeds its entry cap"));
                }
                let entry = entry?;
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                if !file_name.starts_with(&format!("{}uncommitted-", layout.archive_prefix())) {
                    continue;
                }
                archives += 1;
                if archives >= layout.limits.records {
                    return Err(io::Error::other("journal archive entry capacity exhausted"));
                }
                let (_, metadata) = private_file::open_private(dir, &file_name)
                    .map_err(|_| io::Error::other("journal suffix archive is not private"))?;
                bytes = bytes.saturating_add(metadata.len());
            }
            if bytes > layout.limits.bytes as u64 {
                return Err(io::Error::other(
                    "journal suffix archive capacity exhausted; preserve history for repair",
                ));
            }
            write_private_durable(dir, &name, &tail)?;
        }
        _ => {
            return Err(io::Error::other(
                "journal suffix archive is corrupt or not private",
            ));
        }
    }
    // A previous attempt may have renamed this archive but failed its directory
    // barrier. Retry the barrier even when the exact archive already exists.
    aterm_durable::durable_dir(dir)?;
    held.0.set_len(checkpoint.length)?;
    aterm_durable::durable(&held.0)?;
    Ok((opened.dev(), opened.ino()))
}

#[cfg(test)]
mod tests;
