// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A private astream log whose acknowledgement includes a durable hash of its
//! physical prefix: an append is acknowledged only once the record and the
//! checkpoint that attests it are on stable media ([`Log::append`]). That costs one
//! full flush where astream serves the log Ordered (an Apple boot volume), and three
//! where it serves it Strict (off Apple, off the boot volume, or once a barrier was
//! refused), each write then flushed in full. Lost acknowledged bytes refuse open;
//! unacknowledged suffixes are archived before rollback. This protects accidental partial loss,
//! not rollback of the entire directory by a privileged adversary. The caller
//! authenticates every replayed record and supplies an independently pinned anchor.
//! One writer at a time holds the exclusive lock; readers take none, and see only
//! the prefix the checkpoint attests ([`read_committed`]).
//!
//! On Windows the private files are `aterm_uds::private_file`'s there (owner the token user,
//! a protected access list of the user and SYSTEM alone, no reparse point, one link) and a
//! file's identity is its volume and 128-bit file id ([`FileIdentity`]). astream locks its
//! log whole there with a lock Windows makes mandatory (`LockFileEx` through the log's own
//! handle), which refuses a read through any other handle while it is held: the writer reads
//! its own log through the log's descriptor ([`BrokerLog::copy_bytes_to`]), and recovery
//! through the handle that holds its lock, but a reader beside a live writer is refused
//! (`ERROR_LOCK_VIOLATION`, passed on as it is) until the writer drops (ATERM-KI-1116-48213).
#![forbid(unsafe_code)]
#![cfg(any(unix, windows))]
#![cfg_attr(all(anchor_rules, any(test, debug_assertions)), feature(macro_attr))]

use astream_broker::{BrokerLog, BrokerRecord, Durability, OpenOptions as LogOptions, Prepared};
use astream_wire::Offset;
use aterm_durable::Flush;
use aterm_uds::private_file::{self, PrivateFileError};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

/// A file's identity as the journal compares it: `(dev, ino)` on Unix; on Windows the volume's
/// serial number and the file's 128-bit id (`FILE_ID_INFO`), which only a query of an open
/// handle gives: a Windows `Metadata` carries neither.
#[cfg(unix)]
pub type FileIdentity = (u64, u64);
/// A file's identity as the journal compares it: `(dev, ino)` on Unix; on Windows the volume's
/// serial number and the file's 128-bit id (`FILE_ID_INFO`), which only a query of an open
/// handle gives ([`aterm_dirfd::file_id`]): a Windows `Metadata` carries neither.
#[cfg(windows)]
pub type FileIdentity = (u64, u128);

/// What the journal reads of a file it opened: its length (from `metadata`, read of that open
/// file), which file it is, and how many names it has.
struct Seen {
    len: u64,
    identity: FileIdentity,
    links: u64,
}

/// [`Seen`] of `file`, whose metadata is `metadata`.
#[cfg(unix)]
fn seen(_file: &File, metadata: &std::fs::Metadata) -> io::Result<Seen> {
    Ok(Seen {
        len: metadata.len(),
        identity: (metadata.dev(), metadata.ino()),
        links: metadata.nlink(),
    })
}

/// [`Seen`] of `file`, whose metadata is `metadata`: identity and link count from one query of
/// the handle each (`FileIdInfo`, `FileStandardInfo`), read through `aterm_dirfd`.
#[cfg(windows)]
fn seen(file: &File, metadata: &std::fs::Metadata) -> io::Result<Seen> {
    let stat = aterm_dirfd::fstat(file)?;
    Ok(Seen {
        len: metadata.len(),
        identity: (stat.st_dev, stat.st_ino),
        links: stat.st_nlink,
    })
}

/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE`: recovery's open of the log shares
/// everything, as every private open does, so a rename over a file it holds still lands.
#[cfg(windows)]
const SHARE_ALL: u32 = 0x0000_0007;

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
/// barrier to succeed: the file and then the directory each take one full flush,
/// and a directory whose entries cannot be flushed here is refused. A failure
/// after rename is an uncertain outcome, so the caller must fence publication and
/// reopen rather than assuming no write.
/// # Errors
/// Propagates private-path, file-write, rename and directory-barrier failures.
pub fn write_private_durable(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    private_file::write_private_flushed(dir, name, bytes, Flush::Durable)
}

/// Conservative framing allowance per append, in addition to body and subject.
pub const APPEND_OVERHEAD: usize = 256;

/// Maximum records covered by one application checkpoint and dependent write.
pub const MAX_APPEND_RECORDS: usize = 256;

/// Decoded-record cache budget for each local history log. The newest record stays
/// resident even if larger; retained indexes and records held by readers are outside this budget.
pub const DECODED_WINDOW_BYTES: usize = 16 * 1024 * 1024;

/// Local-history log options with a bounded decoded window. Older records remain on disk
/// and are decoded on demand; this sets no retention limit and preserves the requested durability.
#[must_use]
pub fn resident_options(durability: Durability) -> LogOptions {
    LogOptions::new(durability).with_resident_budget(DECODED_WINDOW_BYTES)
}

/// Fixed resource bounds for one immutable generation. Retention is deliberately
/// unavailable: dropping an old record could erase authorization or liabilities.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub records: usize,
    pub bytes: usize,
    /// Maximum application body size including its local receipt metadata.
    pub record_bytes: usize,
}
/// The suffixes of a journal's anchor, astream and checkpoint names (`<name>.<suffix>`).
const ANCHOR: &str = "anchor";
const ASTREAM: &str = "astream";
const CHECKPOINT: &str = "checkpoint";

mod read_guard;
pub use read_guard::ReadGuard;

fn validate_name(name: &str) -> io::Result<()> {
    if name.is_empty()
        || name.len() > 32
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err(io::Error::other("invalid journal name"));
    }
    Ok(())
}

struct Layout {
    name: String,
    anchor: String,
    astream: String,
    checkpoint: String,
    lock: String,
    archive_prefix: String,
    limits: Limits,
}
impl Layout {
    fn new(name: &str, anchor: &[u8], limits: Limits) -> io::Result<Self> {
        validate_name(name)?;
        if anchor.is_empty()
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
            anchor: format!("{name}.{ANCHOR}"),
            astream: format!("{name}.{ASTREAM}"),
            checkpoint: format!("{name}.{CHECKPOINT}"),
            lock: format!("{name}.lock"),
            archive_prefix: format!("{name}-"),
            limits,
        })
    }
}

/// One exclusive private writer. Drop or park ([`Log::park`]) before performing network I/O. Only
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
    identity: FileIdentity,
    writer_identity: FileIdentity,
    /// The astream file's stamp as this writer last left it, while every change it showed was
    /// this writer's own: none once a change no write of this writer made reached the file in
    /// its hold, which then parks nothing ([`Self::park`]).
    witness: Option<Stamp>,
    /// Resumes in a row since this journal's prefix was last hashed whole ([`RESUMES`]).
    resumes: u32,
    /// The lowest offset this handle serves: a resumed handle hashed none of the prefix, so it
    /// serves no record below the last one it parked ([`Self::resume`]).
    floor: u64,
    _writer: WriterLock,
}

/// Resumes of a parked journal in a row ([`Log::resume`]); the next hashes its whole prefix again.
pub const RESUMES: u32 = 64;

/// How [`Log::resume`] admitted a journal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reopened {
    /// From the parked state: nothing of the prefix was hashed again.
    Resumed,
    /// As [`Log::open`] admits it: the whole prefix was hashed.
    Opened,
}

/// What an open of the astream file says of the last change made to it through the file system:
/// which file it is and its length, and on Unix its mode, owner, and modification and
/// status-change times to the nanosecond. On Windows, which has no status-change time and
/// whose modification time a writer may set back, it stands for nothing a resume skips: a
/// writer there parks nothing ([`Log::park`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    identity: FileIdentity,
    len: u64,
    #[cfg(unix)]
    changed: (u32, u32, (i64, i64), (i64, i64)),
}
impl Stamp {
    /// The stamp of `file`, whose metadata is `metadata`.
    fn of(file: &File, metadata: &std::fs::Metadata) -> io::Result<Self> {
        let seen = seen(file, metadata)?;
        Ok(Self {
            identity: seen.identity,
            len: seen.len,
            #[cfg(unix)]
            changed: (
                metadata.mode(),
                metadata.uid(),
                (metadata.mtime(), metadata.mtime_nsec()),
                (metadata.ctime(), metadata.ctime_nsec()),
            ),
        })
    }
}

/// A writer let go between its holds ([`Log::park`]): no descriptor and no lock, only what a
/// [`Log::resume`] needs to stand for an open's whole-prefix hash where the journal stands as
/// this writer left it.
///
/// [`Log::resume`] admits an established journal as [`Log::open`] does, with every check an open
/// makes, except that both of an open's whole-prefix hashes (its recovery's, under the physical
/// lock, and astream's prepare's, under astream's) start from the parked SHA-256 state, and each
/// does so only where, under the lock it runs under, the checkpoint holds exactly the 40 bytes
/// this writer last saved, the astream file's stamp ([`Stamp`]) is the one this writer left it
/// with, the writer lock is the parked one's, and fewer than [`RESUMES`] resumes followed the
/// last whole hash. Any other journal is hashed from offset zero in the same admission, and that
/// verdict is the answer.
///
/// What a resume does not prove again is that the bytes below the parked length still hash to the
/// checkpoint. In place of that read stands the stamp: no change made through the file system has
/// reached the astream file since those bytes were last hashed, compared at each append of the
/// hold that parked it, at the park, and under each lock of the resume. A change the stamp cannot
/// show (one within the instant of this writer's own write, within one timestamp tick, or made
/// below the file system: the medium, a raw device) is refused where it breaks a frame's CRC,
/// which astream's open checks on every resume; is never served, since a resumed handle serves
/// no record below the last one it parked, which its caller judges against its own copy; is never
/// attested, since later checkpoints extend the parked state, so the next whole hash refuses the
/// journal; and is found within [`RESUMES`] resumes, at another process's or writer's open, and at
/// any lock-free read. Like an open, this protects against accidental partial loss, not an actor
/// who rewrites the checkpoint and the prefix together.
pub struct Parked {
    dir: PathBuf,
    name: String,
    limits: Limits,
    anchor: Vec<u8>,
    checkpoint: Checkpoint,
    astream: Stamp,
    writer_identity: FileIdentity,
    /// The records the journal held at the park.
    records: u64,
    resumes: u32,
}

impl Parked {
    /// The checkpoint a whole-prefix load of this journal starts from: the parked one, where the
    /// checkpoint holds exactly its `bytes` and the astream file shows the parked stamp.
    fn seed(&self, bytes: &[u8], astream: Stamp) -> Option<Checkpoint> {
        (bytes == self.checkpoint.bytes() && astream == self.astream)
            .then(|| self.checkpoint.clone())
    }
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
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalOpen",
            action = "Missing",
            reason = "The environment removes the checkpoint before opening"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalOpen",
            action = "Corrupt",
            reason = "The environment changes the checkpoint digest before opening"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalOpen",
            action = "Unenroll",
            reason = "The caller permits fresh enrollment and the anchor is absent"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalOpen",
            action = "FailBarrier",
            reason = "A required directory durability barrier fails in the environment"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalOpen",
            action = "FailEarly",
            reason = "The admission directory barrier fails before checkpoint publication"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalOpen",
            action = "Open",
            project = "aterm_journal::open_tests::transition"
        )
    )]
    pub fn open_until(
        dir: &Path,
        name: &str,
        anchor: &[u8],
        established: bool,
        limits: Limits,
        deadline: Instant,
    ) -> io::Result<Self> {
        let layout = Layout::new(name, anchor, limits)?;
        let (lock, writer_identity) = lock(dir, &layout, deadline)
            .map_err(|error| stage_error(&layout, "writer admission", error))?;
        Self::admit(
            dir,
            layout,
            anchor,
            established,
            lock,
            writer_identity,
            None,
        )
        .map(|(log, _)| log)
    }

    /// Let this writer go between holds, keeping what a [`Self::resume`] needs: both locks are
    /// released when it returns. `None` where nothing may be resumed: the handle is fenced, its
    /// open unfinished, its files not intact, a change no write of this writer made reached the
    /// astream file in its hold, or the platform is Windows ([`Stamp`]).
    #[must_use]
    pub fn park(self) -> Option<Parked> {
        if cfg!(windows) || self.poisoned || self.pending_anchor.is_some() {
            return None;
        }
        // Stamped while both locks are held: astream's drop only unlocks.
        let astream = self.intact().ok()?;
        if self.witness != Some(astream) || astream.len != self.checkpoint.length {
            return None;
        }
        Some(Parked {
            dir: self.dir.clone(),
            name: self.layout.name.clone(),
            limits: self.layout.limits,
            anchor: self.anchor.clone(),
            checkpoint: self.checkpoint.clone(),
            astream,
            writer_identity: self.writer_identity,
            records: self.log.stats().head.0,
            resumes: self.resumes,
        })
    }

    /// [`Self::open`] of an established journal, resuming `parked` ([`Parked`] says what that
    /// stands for); with none, exactly [`Self::open`].
    /// # Errors
    /// As [`Self::open`].
    pub fn resume(
        parked: Option<Parked>,
        dir: &Path,
        name: &str,
        anchor: &[u8],
        limits: Limits,
    ) -> io::Result<(Self, Reopened)> {
        let layout = Layout::new(name, anchor, limits)?;
        let (lock, writer_identity) = lock(dir, &layout, Instant::now())
            .map_err(|error| stage_error(&layout, "writer admission", error))?;
        let parked = parked.filter(|parked| {
            parked.dir == dir
                && parked.name == layout.name
                && parked.limits == limits
                && parked.anchor == anchor
                && parked.writer_identity == writer_identity
                && parked.resumes < RESUMES
        });
        Self::admit(
            dir,
            layout,
            anchor,
            true,
            lock,
            writer_identity,
            parked.as_ref(),
        )
    }

    /// Admit the journal under `lock`, the writer lock just taken: an open, whose whole-prefix
    /// loads start from `parked` where each finds the journal as it was parked ([`Parked`]).
    fn admit(
        dir: &Path,
        layout: Layout,
        anchor: &[u8],
        established: bool,
        lock: WriterLock,
        writer_identity: FileIdentity,
        parked: Option<&Parked>,
    ) -> io::Result<(Self, Reopened)> {
        let limits = layout.limits;
        let judged = private_file::JudgedDir::judge(dir)
            .map_err(|_| io::Error::other("journal anchor is not private"))?;
        let fresh = match judged.read(&layout.anchor, 4096) {
            Ok(bytes) if bytes == anchor => false,
            Ok(_) => return Err(io::Error::other("journal anchor changed")),
            Err(PrivateFileError::Missing) if !established => true,
            Err(PrivateFileError::Missing) => {
                return Err(io::Error::other("established journal anchor is missing"));
            }
            Err(_) => return Err(io::Error::other("journal anchor is not private")),
        };
        match judged.open(&layout.astream) {
            Ok(_) => {}
            Err(PrivateFileError::Missing) if fresh => {
                write_private_durable(dir, &layout.astream, &[])?
            }
            Err(_) => return Err(io::Error::other("journal is not a private regular file")),
        }
        let (identity, recovered_seeded) = recover_uncommitted(dir, &layout, fresh, parked)
            .map_err(|error| stage_error(&layout, "recovery", error))?;
        #[cfg(unix)]
        let mut checkpoint = None;
        let log = BrokerLog::open_opts(dir.join(&layout.astream), log_options(), || {
            #[cfg(unix)]
            {
                let (checked, present, seen, seeded) =
                    Checkpoint::load(dir, &layout, fresh, parked)?;
                if seen.len != checked.length || seen.identity != identity {
                    return Err(io::Error::other("journal changed between its writer locks"));
                }
                checkpoint = Some((checked, present, seeded));
            }
            // Under astream's lock, which refuses every read but its own handle's here: only
            // the length and the file are checked before astream's recovery may look at it.
            // The bytes are hashed through the log's own descriptor once it is open, below,
            // before anything is read from it or appended.
            #[cfg(windows)]
            {
                let (length, seen) = Checkpoint::peek(dir, &layout, fresh)?;
                if seen.len != length || seen.identity != identity {
                    return Err(io::Error::other("journal changed between its writer locks"));
                }
            }
            Ok(Prepared::Nothing)
        })
        .map_err(|error| stage_error(&layout, "astream open", error))?;
        #[cfg(windows)]
        let checkpoint = {
            let (checked, present, seen, seeded) =
                Checkpoint::load_through(dir, &layout, fresh, parked, Through::Log(&log))
                    .map_err(|error| stage_error(&layout, "astream open", error))?;
            if seen.len != checked.length || seen.identity != identity {
                return Err(io::Error::other("journal changed between its writer locks"));
            }
            Some((checked, present, seeded))
        };
        let (checkpoint, present, prepared_seeded) =
            checkpoint.ok_or_else(|| io::Error::other("journal was not prepared"))?;
        let resumed = parked.filter(|_| recovered_seeded && prepared_seeded);
        let stats = log.stats();
        if stats.retained_bytes > limits.bytes
            || stats.retained_records > limits.records
            || stats.base != Offset::ZERO
            || stats.head.0 != stats.retained_records as u64
            || stats.retained_bytes as u64 != checkpoint.length
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
            for (idx, record) in page.iter().enumerate() {
                if record.seq.0 != scan.0 + idx as u64
                    || record.subject.is_empty()
                    || astream_broker::store::is_hidden_subject(&record.subject)
                {
                    return Err(io::Error::other(
                        "acknowledged journal prefix does not decode",
                    ));
                }
                if record.body.len() > limits.record_bytes {
                    return Err(io::Error::other("journal record size exceeds its capacity"));
                }
            }
            scan = Offset(scan.0 + page.len() as u64);
        }
        // The open flushed the log in full, so a checkpoint written here is behind its bytes
        // already. On a log astream serves Ordered its barriers order its own bytes ahead of
        // its name, and the directory's full flush below makes both durable; where it
        // serves Strict the device's barriers are not trusted to, so it is flushed in full.
        if !present {
            let flush = if log.served().served == astream_broker::Tier::Ordered {
                Flush::Ordered
            } else {
                Flush::Durable
            };
            checkpoint.save(dir, &layout, flush)?;
        }
        let astream = private_file::open_private(dir, &layout.astream)
            .map_err(|_| io::Error::other("journal lost its private inode"))
            .and_then(|(file, metadata)| seen(&file, &metadata))?;
        // An existing checkpoint may be visible after its previous writer failed the
        // rename's directory barrier, or its commit. Finish that publication even without
        // rewriting it: the open's full flush, which covers this open's own checkpoint too.
        aterm_durable::durable_dir(dir)?;
        if astream.identity != identity {
            return Err(io::Error::other("journal inode changed during open"));
        }
        let mut opened = Self {
            log,
            checkpoint,
            dir: dir.into(),
            layout,
            poisoned: false,
            pending_anchor: fresh.then(|| anchor.to_vec()),
            anchor: anchor.to_vec(),
            identity,
            writer_identity,
            witness: None,
            resumes: resumed.map_or(0, |parked| parked.resumes + 1),
            floor: resumed.map_or(0, |parked| parked.records.saturating_sub(1)),
            _writer: lock,
        };
        opened.witness = Some(opened.intact()?);
        let reopened = if resumed.is_some() {
            Reopened::Resumed
        } else {
            Reopened::Opened
        };
        Ok((opened, reopened))
    }
    /// Finish opening only after the caller has authenticated all replayed
    /// records against its independent anchor. A failed replay must not pin a
    /// different anchor over an otherwise recoverable unanchored history.
    /// # Errors
    /// Propagates anchor publication or directory durability failures.
    pub fn finish_open(&mut self) -> io::Result<()> {
        self.intact()?;
        if let Some(anchor) = &self.pending_anchor {
            write_private_durable(&self.dir, &self.layout.anchor, anchor)?;
            self.pending_anchor = None;
        }
        Ok(())
    }
    /// A failed append or checkpoint fences this handle until reopen.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned || self.intact().is_err()
    }
    /// The files are this writer's, as it left them: the astream file's stamp ([`Stamp`]).
    fn intact(&self) -> io::Result<Stamp> {
        // One directory verdict for the four files, each still opened and judged on its
        // descriptor: every access calls this, and open(2) is the cost.
        let dir = private_file::JudgedDir::judge(&self.dir)
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        let astream = dir
            .open(&self.layout.astream)
            .map_err(|_| io::Error::other("journal lost its private inode"))
            .and_then(|(file, metadata)| Stamp::of(&file, &metadata))?;
        let lock = dir
            .open(&self.layout.lock)
            .map_err(|_| io::Error::other("journal lost its writer lease"))
            .and_then(|(file, metadata)| seen(&file, &metadata))?;
        let anchor_ok = match dir.read(&self.layout.anchor, 4096) {
            Ok(bytes) => bytes == self.anchor,
            Err(PrivateFileError::Missing) => self.pending_anchor.is_some(),
            Err(_) => false,
        };
        if astream.identity != self.identity
            || astream.len != self.log.stats().retained_bytes as u64
            || lock.identity != self.writer_identity
            || !anchor_ok
            || dir.read(&self.layout.checkpoint, 40).ok().as_deref()
                != Some(&self.checkpoint.bytes())
        {
            return Err(io::Error::other("journal or writer lease was replaced"));
        }
        Ok(astream)
    }
    pub fn bytes(&self) -> usize {
        self.log.stats().retained_bytes
    }
    /// The tier astream serves this open's log at, and why it differs from the Ordered one
    /// asked for: Strict off Apple, off the boot volume, or once a barrier was refused.
    pub fn served(&self) -> astream_broker::ServedInfo {
        self.log.served()
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
    /// Uncertain writes, unavailable or changed private journal files or writer leases, and
    /// unreadable retained records refuse the page. A decode failure never appears as the end
    /// of the journal.
    pub fn read_range(&self, offset: Offset, count: usize) -> io::Result<Vec<Arc<BrokerRecord>>> {
        if self.is_poisoned() {
            return Err(io::Error::other("uncertain journal: reopen"));
        }
        if offset.0 < self.floor {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a resumed journal serves no record below the last one it parked",
            ));
        }
        let stats = self.log.stats();
        let available = stats.head.0.saturating_sub(offset.0.max(stats.base.0));
        let wanted = count
            .min(256)
            .min(usize::try_from(available).unwrap_or(usize::MAX));
        let records = self.log.read_range(offset, wanted);
        if records.len() != wanted {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "journal retained records could not be decoded",
            ));
        }
        Ok(records)
    }
    /// Acknowledge only once the record and the prefix checkpoint that attests it
    /// are on stable media. On a log astream serves Ordered the record and the
    /// checkpoint each take barriers, in that order, and one full flush of the log
    /// commits both (`BrokerLog::sync_full`); on a log it serves Strict the record
    /// was flushed in full already, so the checkpoint is flushed in full too.
    /// # Errors
    /// Capacity and invalid input refuse before mutation. Any uncertain write
    /// fences this handle; reopen and retry the exact application bytes.
    pub fn append(&mut self, subject: &str, body: Vec<u8>) -> io::Result<Offset> {
        self.append_inner(
            subject,
            body,
            None::<(&Path, fn(Offset, Flush) -> io::Result<()>)>,
        )
    }

    /// [`Self::append`] with one dependent write into the directory `dir`: a witness
    /// of the prefix kept elsewhere, which a reader that takes no lock may trust as
    /// the acknowledgement. It is written only once the commit has made the record
    /// and its checkpoint durable, so whoever can read it finds them on stable media.
    /// `write` gets the record's offset and the flush to finish every file and
    /// directory it writes with. On an Ordered log and `dir` on the log's device that
    /// is [`Flush::Ordered`]: its barriers order it ahead of the next record there,
    /// whose commit makes it durable, so on stable media the log is never more than
    /// this one append ahead of it, but a power loss before that commit can take it
    /// while its record stays (the caller's custody takes that one record back).
    /// Anywhere else it is [`Flush::Durable`], on stable media before the
    /// acknowledgement.
    /// # Errors
    /// As [`Self::append`]; and `write`'s error, which fences this handle as an
    /// uncertain append does.
    pub fn append_then(
        &mut self,
        subject: &str,
        body: Vec<u8>,
        dir: &Path,
        write: impl FnOnce(Offset, Flush) -> io::Result<()>,
    ) -> io::Result<Offset> {
        self.append_inner(subject, body, Some((dir, write)))
    }

    /// Append 1 through [`MAX_APPEND_RECORDS`] records under one checkpoint, commit,
    /// and dependent write. Input and aggregate capacity are checked before publication.
    /// The result and `write`'s offset identify the first record; subsequent offsets are
    /// consecutive, in input order. The dependent write follows the entire durable group.
    ///
    /// Like [`Self::append_then`], the dependent write may remain Ordered on the log's
    /// device. A crash can therefore leave this entire group beyond the caller's last
    /// independent witness. Callers must authorize recovery of that exact group before
    /// appending; a single-record recovery allowance is insufficient. This method does
    /// not supply application custody or authenticate its records.
    /// # Errors
    /// Empty or oversized groups, invalid subjects, and capacity exhaustion refuse before
    /// mutation. Uncertain publication, checkpoint, commit, or dependent writes fence the
    /// handle; reopen and recover before retrying.
    pub fn append_many_then(
        &mut self,
        mut records: Vec<(&str, Vec<u8>)>,
        dir: &Path,
        write: impl FnOnce(Offset, Flush) -> io::Result<()>,
    ) -> io::Result<Offset> {
        self.append_group_inner(&mut records, Some((dir, write)))
    }

    /// Check a group's input, aggregate capacity, and first offset without publishing
    /// or reserving anything. A caller can refuse before writing an independent recovery
    /// permit. [`Self::append_many_then`] checks the then-current state again.
    /// # Errors
    /// As the pre-publication checks of [`Self::append_many_then`].
    pub fn check_append_many(&self, records: &[(&str, Vec<u8>)]) -> io::Result<Offset> {
        self.check_group(records).map(|(head, _)| Offset(head))
    }

    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "Latch",
            reason = "astream serves the log Strict: off Apple, off the boot volume, or a refused barrier"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "Apart",
            reason = "The caller's dependent directory is on another device than the log"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "Crash",
            reason = "The machine loses power during or after the append"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "DropRecord",
            reason = "The device loses the record's bytes that no full flush covered"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "DropCheckpoint",
            reason = "The device loses a checkpoint that no full flush covered"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "DropWitness",
            reason = "The device loses a dependent write that no full flush covered"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalAppend",
            action = "DropPrior",
            reason = "The device loses the previous append's dependent write before a full flush"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalAppend",
            action = "Publish",
            project = "aterm_journal::append_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalAppend",
            action = "Checkpoint",
            project = "aterm_journal::append_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalAppend",
            action = "Witness",
            project = "aterm_journal::append_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalAppend",
            action = "Commit",
            project = "aterm_journal::append_tests::transition"
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalAppend",
            action = "Ack",
            project = "aterm_journal::append_tests::transition"
        )
    )]
    fn append_inner(
        &mut self,
        subject: &str,
        body: Vec<u8>,
        then: Option<(&Path, impl FnOnce(Offset, Flush) -> io::Result<()>)>,
    ) -> io::Result<Offset> {
        self.append_group_inner(&mut [(subject, body)], then)
    }

    fn check_group(&self, records: &[(&str, Vec<u8>)]) -> io::Result<(u64, Stamp)> {
        let uncertain = || io::Error::other("uncertain journal: reopen and retry exact bytes");
        if self.poisoned {
            return Err(uncertain());
        }
        let astream = self.intact().map_err(|_| uncertain())?;
        if self.pending_anchor.is_some() {
            return Err(io::Error::other(
                "authenticate replay and finish opening before append",
            ));
        }
        if records.is_empty() || records.len() > MAX_APPEND_RECORDS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid journal group size",
            ));
        }
        let stats = self.log.stats();
        let mut bytes = stats.retained_bytes;
        for (subject, body) in records.iter() {
            if subject.is_empty() || astream_broker::store::is_hidden_subject(subject) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid journal subject",
                ));
            }
            let framed = body
                .len()
                .saturating_add(subject.len())
                .saturating_add(APPEND_OVERHEAD);
            if body.len() > self.layout.limits.record_bytes
                || framed > astream_broker::MAX_RECORD_PAYLOAD
            {
                return Err(io::Error::other("journal capacity exhausted"));
            }
            bytes = bytes.saturating_add(framed);
        }
        if stats.retained_records.saturating_add(records.len()) > self.layout.limits.records
            || bytes > self.layout.limits.bytes
        {
            return Err(io::Error::other("journal capacity exhausted"));
        }
        let head = stats.head.0;
        head.checked_add(records.len() as u64)
            .ok_or_else(|| io::Error::other("journal sequence exhausted"))?;
        Ok((head, astream))
    }

    fn append_group_inner(
        &mut self,
        records: &mut [(&str, Vec<u8>)],
        then: Option<(&Path, impl FnOnce(Offset, Flush) -> io::Result<()>)>,
    ) -> io::Result<Offset> {
        let (head, before) = self.check_group(records)?;
        // A change no write of this writer made reached the file in its hold: it parks nothing.
        if self.witness != Some(before) {
            self.witness = None;
        }
        let mut after = before;
        for (index, (subject, body)) in records.iter_mut().enumerate() {
            match self.log.publish_checked(
                1,
                head + index as u64 + 1,
                (*subject).into(),
                std::mem::take(body),
            ) {
                Ok((offset, false)) if offset.0 == head + index as u64 => {}
                Ok(_) => {
                    self.poisoned = true;
                    return Err(io::Error::other(
                        "journal append did not advance its physical prefix; reopen",
                    ));
                }
                Err(_) => {
                    self.poisoned = true;
                    return Err(io::Error::other(
                        "uncertain journal append: reopen and retry exact bytes",
                    ));
                }
            }
            match self.intact() {
                Ok(stamp) => after = stamp,
                Err(_) => {
                    self.poisoned = true;
                    return Err(io::Error::other("journal changed during append; reopen"));
                }
            }
            #[cfg(test)]
            tests::append_step(tests::Step::Published, self.log.flush_counts());
        }
        let offset = Offset(head);
        // Read after the publish, which latches the log to Strict when the device refuses
        // its barrier. A Strict record is durable already, so the commit's full flush
        // below finds nothing to cover and skips: there the checkpoint is flushed in full
        // itself. On an Ordered log it takes barriers, ordered behind the record's.
        let ordered = self.served_ordered();
        let length = self.log.stats().retained_bytes as u64;
        #[cfg(unix)]
        let extended = self.checkpoint.extend(&self.dir, &self.layout, length);
        #[cfg(windows)]
        let extended = self
            .checkpoint
            .extend_log(&self.dir, &self.layout, length, &self.log);
        if let Err(error) = extended.and_then(|()| {
            self.checkpoint
                .save(&self.dir, &self.layout, self.checkpoint_flush(ordered))
        }) {
            self.poisoned = true;
            return Err(io::Error::other(CheckpointFailure {
                path: self.dir.join(&self.layout.checkpoint),
                error,
            }));
        }
        if self.witness.is_some() {
            self.witness = Some(after);
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Checkpointed, self.log.flush_counts());
        #[cfg(any(test, debug_assertions))]
        let then = match then {
            Some((_, write)) if seam::mutant() == seam::Mutant::WitnessFirst => {
                self.dependent(offset, Flush::Ordered, write)?;
                None
            }
            then => then,
        };
        self.commit()?;
        if let Some((dir, write)) = then {
            let flush = self.dependent_flush(ordered, dir);
            self.dependent(offset, flush, write)?;
        }
        Ok(offset)
    }

    /// Whether astream serves this open's log Ordered: off Apple, off the boot volume,
    /// or once a barrier was refused, it serves Strict, every record flushed in full.
    fn served_ordered(&self) -> bool {
        self.log.served().served == astream_broker::Tier::Ordered
    }

    /// The flush that finishes an append's checkpoint: barriers on an Ordered log, the
    /// commit's full flush making them durable; full flushes on a Strict one, whose
    /// commit flushes nothing.
    fn checkpoint_flush(&self, ordered: bool) -> Flush {
        #[cfg(any(test, debug_assertions))]
        match seam::mutant() {
            seam::Mutant::Durable => return Flush::Durable,
            seam::Mutant::BarrierCheckpoint => return Flush::Ordered,
            seam::Mutant::None
            | seam::Mutant::UnorderedRecord
            | seam::Mutant::WitnessFirst
            | seam::Mutant::AcksAtPublish => {}
        }
        if ordered {
            Flush::Ordered
        } else {
            Flush::Durable
        }
    }

    /// The flush a dependent write into `dir` takes after the commit
    /// ([`Self::append_then`]): barriers only on an Ordered log and only on the log's
    /// own device, whose barriers order it ahead of the next record and whose next
    /// commit covers it; a full flush anywhere else.
    fn dependent_flush(&self, ordered: bool, dir: &Path) -> Flush {
        #[cfg(any(test, debug_assertions))]
        if seam::mutant() == seam::Mutant::Durable || seam::is_apart() {
            return Flush::Durable;
        }
        if ordered {
            Flush::ordered_if_same_device(dir, &self.dir)
        } else {
            Flush::Durable
        }
    }

    /// The caller's dependent write, fencing this handle when it fails.
    fn dependent(
        &mut self,
        offset: Offset,
        flush: Flush,
        write: impl FnOnce(Offset, Flush) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Err(error) = write(offset, flush) {
            self.poisoned = true;
            return Err(error);
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Witnessed, self.log.flush_counts());
        Ok(())
    }

    /// The one full flush that acknowledges an append on an Ordered log: everything the
    /// append wrote on the log's device before it, and the previous append's dependent
    /// write, is then on stable media. On a Strict log the record's own flush covers the
    /// head already, and astream skips it.
    fn commit(&mut self) -> io::Result<()> {
        #[cfg(any(test, debug_assertions))]
        seam::before_commit();
        if self.log.sync_full().is_err() {
            self.poisoned = true;
            return Err(io::Error::other(
                "uncertain journal commit: reopen and retry exact bytes",
            ));
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Committed, self.log.flush_counts());
        Ok(())
    }
}

/// What the journal's log is opened with: Ordered, with no horizon, so a record takes a
/// barrier and nothing flushes it in full but its append's commit
/// (`BrokerLog::sync_full`). astream serves it Strict where the device cannot promise
/// the order, which [`Log::append`] reads. An append is acknowledged only after that
/// commit (`Acks::FullFlush`), so a run that stopped without a clean close lost nothing it
/// acknowledged: the next open, in another boot, moves no generation and says nothing.
fn log_options() -> LogOptions {
    let mut options = resident_options(Durability::Ordered { horizon: None });
    options.acks = astream_broker::Acks::FullFlush;
    #[cfg(any(test, debug_assertions))]
    let options = seam::options(options);
    options
}

/// The journal's test seams, on this thread: a log judged off the boot volume (so astream
/// serves it Strict), a boot to open in, a dependent directory taken for another device's,
/// a hook just before each commit, and the negative controls of the append's ordering.
#[cfg(any(test, debug_assertions))]
pub mod seam {
    use std::cell::{Cell, RefCell};

    /// A mutation of the append, for a negative control.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub enum Mutant {
        #[default]
        None,
        /// The checkpoint and the dependent write flushed in full, whatever the log
        /// serves: the append as it was before one commit flushed its ordered writes.
        Durable,
        /// The checkpoint takes barriers only, whatever the log serves: on a Strict log
        /// nothing then flushes it in full before the acknowledgement.
        BarrierCheckpoint,
        /// The log opens Relaxed, so the record takes no flush: its checkpoint can reach
        /// the disk ahead of it.
        UnorderedRecord,
        /// The dependent write takes barriers before the commit: a reader that takes no
        /// lock can then find it while its record is not yet durable.
        WitnessFirst,
        /// The log opens acking at its publishes (`Acks::Publish`), so astream judges a
        /// run that stopped uncleanly as one that may have lost acked records.
        AcksAtPublish,
    }

    #[derive(Clone, Copy, Default)]
    struct Seams {
        strict: bool,
        apart: bool,
        boot: Option<[u8; 16]>,
        mutant: Mutant,
    }

    thread_local! {
        static SEAMS: Cell<Seams> = const {
            Cell::new(Seams { strict: false, apart: false, boot: None, mutant: Mutant::None })
        };
        static BEFORE_COMMIT: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
    }

    /// Restores the seams it changed when it drops.
    #[must_use = "the seam stops when it drops"]
    pub struct Guard {
        outer: Seams,
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            SEAMS.with(|seams| seams.set(self.outer));
        }
    }

    fn change(f: impl FnOnce(&mut Seams)) -> Guard {
        let outer = SEAMS.with(Cell::get);
        let mut seams = outer;
        f(&mut seams);
        SEAMS.with(|cell| cell.set(seams));
        Guard {
            outer,
            _thread: std::marker::PhantomData,
        }
    }

    /// A journal this thread opens judges its log off the boot volume, which astream
    /// serves Strict (`Volume::Other`).
    pub fn serve_strict() -> Guard {
        change(|seams| seams.strict = true)
    }

    /// A dependent write on this thread is taken for one on another device.
    pub fn apart() -> Guard {
        change(|seams| seams.apart = true)
    }

    /// A journal this thread opens is opened in the boot `boot` names (`Boot::Known`): two
    /// opens in different ones stand in a reboot between them.
    pub fn boot(boot: [u8; 16]) -> Guard {
        change(|seams| seams.boot = Some(boot))
    }

    /// Stops running its hook when it drops.
    #[must_use = "the hook stops when it drops"]
    pub struct Hook {
        _thread: std::marker::PhantomData<std::rc::Rc<()>>,
    }

    impl Drop for Hook {
        fn drop(&mut self) {
            BEFORE_COMMIT.with(|hook| hook.borrow_mut().take());
        }
    }

    /// `f` runs on this thread just before each append's commit, its one full flush: the
    /// record and its checkpoint are written then and not yet durable, which is what a
    /// reader that takes no lock may see of the append meanwhile.
    pub fn before_each_commit(f: impl FnMut() + 'static) -> Hook {
        BEFORE_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(f)));
        Hook {
            _thread: std::marker::PhantomData,
        }
    }

    pub(super) fn before_commit() {
        let taken = BEFORE_COMMIT.with(|hook| hook.borrow_mut().take());
        if let Some(mut f) = taken {
            f();
            BEFORE_COMMIT.with(|hook| {
                let mut hook = hook.borrow_mut();
                if hook.is_none() {
                    *hook = Some(f);
                }
            });
        }
    }

    /// The appends, and the opens, of this thread run `mutant`.
    pub fn mutate(mutant: Mutant) -> Guard {
        change(|seams| seams.mutant = mutant)
    }

    pub(super) fn mutant() -> Mutant {
        SEAMS.with(Cell::get).mutant
    }

    pub(super) fn is_apart() -> bool {
        SEAMS.with(Cell::get).apart
    }

    pub(super) fn options(mut options: super::LogOptions) -> super::LogOptions {
        let seams = SEAMS.with(Cell::get);
        if seams.strict {
            options.volume = astream_broker::Volume::Other;
        }
        if let Some(boot) = seams.boot {
            options.boot = astream_broker::Boot::Known(boot);
        }
        match seams.mutant {
            Mutant::UnorderedRecord => options.durability = super::Durability::Relaxed,
            Mutant::AcksAtPublish => options.acks = astream_broker::Acks::Publish,
            _ => {}
        }
        options
    }
}

/// One journal file's acknowledged prefix: the astream file's identity ([`FileIdentity`]) and
/// the exact bytes its checkpoint attests. Equal values name identical bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Acknowledged {
    /// The astream file's identity: `(dev, ino)` on Unix ([`FileIdentity`]).
    pub identity: FileIdentity,
    /// Acknowledged physical bytes, from offset zero.
    pub length: u64,
    /// SHA-256 of exactly those bytes.
    pub digest: [u8; 32],
}

/// An acknowledged prefix a reader has already decoded, and how many records
/// it holds. Hand it back to [`read_committed`] to be told only what changed.
/// Up to eight MiB of private physical bytes are retained for fresh comparisons,
/// where the read that gave it was asked to ([`Retain::Bytes`]).
#[derive(Clone, PartialEq, Eq)]
pub struct Prefix {
    acknowledged: Acknowledged,
    records: u64,
    max_record_bytes: usize,
    bytes: Option<Arc<[u8]>>,
}
const PREFIX_BYTES: usize = 8 * 1024 * 1024;
impl std::fmt::Debug for Prefix {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Prefix")
            .field("acknowledged", &self.acknowledged)
            .field("records", &self.records)
            .field("max_record_bytes", &self.max_record_bytes)
            .finish()
    }
}
impl Prefix {
    pub const fn acknowledged(&self) -> Acknowledged {
        self.acknowledged
    }
    pub const fn records(&self) -> u64 {
        self.records
    }
    /// Whether this prefix keeps the physical bytes it attests ([`Retain`]).
    #[cfg(any(test, debug_assertions))]
    #[doc(hidden)]
    #[must_use]
    pub const fn retains_bytes(&self) -> bool {
        self.bytes.is_some()
    }
}

/// Whether a read's [`Prefix`] keeps the physical bytes it attests (up to eight MiB), so that a
/// later read handed it tells unchanged bytes by comparing them, or keeps only their digest, so
/// that a later read hashes them again. Either answers every later read alike: only what the
/// reader holds, and what an unchanged read costs, differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retain {
    /// Keep the bytes, up to eight MiB: an unchanged read compares them and hashes nothing.
    Bytes,
    /// Keep only the digest: an unchanged read hashes the bytes it reads once.
    Digest,
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

/// The files [`read_committed`] reads in a journal's folder.
pub struct CommittedFiles {
    /// The anchor the journal was created under.
    pub anchor: PathBuf,
    /// The checkpoint naming the acknowledged prefix.
    pub checkpoint: PathBuf,
    /// The astream file holding the records.
    pub astream: PathBuf,
}

/// The files [`read_committed`] reads in `dir` for the journal `name`. A reader that keeps what a
/// read found can watch these and `dir` to tell when another read could find something else: a
/// writer appends to the astream file and renames each new checkpoint over the last. Nothing is
/// checked here; an invalid `name` names files [`read_committed`] refuses.
#[must_use]
pub fn committed_files(dir: &Path, name: &str) -> CommittedFiles {
    let file = |suffix: &str| dir.join(format!("{name}.{suffix}"));
    CommittedFiles {
        anchor: file(ANCHOR),
        checkpoint: file(CHECKPOINT),
        astream: file(ASTREAM),
    }
}

/// What a read of a journal ([`read_committed_opened`]) opened, each file by the identity
/// ([`FileIdentity`]) of its open descriptor. Each is a private open, which refuses a file with a
/// second link, so a file another folder holds for the read is another inode, or one moved out of
/// the journal's folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The anchor's.
    pub anchor: FileIdentity,
    /// The checkpoint's whose acknowledgement the read took.
    pub checkpoint: FileIdentity,
    /// The astream file's.
    pub astream: FileIdentity,
}

/// Read an established journal's acknowledged prefix without taking any lock,
/// so it works beside a live writer on Unix (on Windows astream's lock, which is mandatory
/// there, refuses the read of the astream file's bytes while a writer holds the journal:
/// ATERM-KI-1116-48213). The checkpoint is read first; then exactly
/// the bytes it counts are read from the astream file and must hash to it. A
/// writer replaces the checkpoint by atomic rename only after the bytes it
/// covers are written and flushed ahead of it, never rewrites bytes below it,
/// and leaves a torn or unacknowledged tail only past it, so an append is wholly
/// in or wholly out. A checkpoint read before its append commits names bytes
/// that are not yet durable: a reader can see an append its writer has yet to
/// acknowledge, never one whose bytes it cannot read.
/// A checkpoint renamed over while it was being opened or read is read again,
/// a bounded number of times; past that, the one read last stands, which the
/// newer one only extends.
/// With `known`, equal freshly read bytes and the same acknowledgement reuse its
/// digest when those bytes were retained. Other inputs are hashed again: when
/// the known bytes still head the new prefix of the same file, only the records
/// after them are decoded.
/// Nothing is repaired, archived or written.
/// # Errors
/// Refuses an invalid name, anchor or limits (as [`Log::open`] does); a
/// missing, changed or not private anchor; a missing, malformed or not private
/// checkpoint (one with a second link included); a
/// prefix over the limits; an astream file that is missing, not private or
/// shorter than the prefix; bytes that do not hash to the checkpoint; and an attested
/// prefix that does not decode into in-order records. A failed read of the
/// astream file's bytes is passed on as it is: on Windows, `ERROR_LOCK_VIOLATION` while a
/// writer holds the journal open.
pub fn read_committed(
    dir: &Path,
    name: &str,
    anchor: &[u8],
    limits: Limits,
    known: Option<&Prefix>,
) -> io::Result<Committed> {
    read_committed_opened(dir, name, anchor, limits, known, Retain::Bytes)
        .map(|(committed, _)| committed)
}

/// [`read_committed`], with the files it opened ([`Opened`]): for a reader that keeps what it
/// read while a witness of the paths stands, which must be a witness of those very files. The
/// new [`Prefix`] keeps its bytes or only their digest as `retain` asks.
/// # Errors
/// As [`read_committed`].
pub fn read_committed_opened(
    dir: &Path,
    name: &str,
    anchor: &[u8],
    limits: Limits,
    known: Option<&Prefix>,
    retain: Retain,
) -> io::Result<(Committed, Opened)> {
    let layout = Layout::new(name, anchor, limits)?;
    // One directory verdict for the three files, as in `Log::intact`.
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| io::Error::other("journal anchor is not private"))?;
    let anchor_opened = match judged.read_with_file(&layout.anchor, 4096) {
        Ok((bytes, file, metadata)) if bytes == anchor => seen(&file, &metadata)?.identity,
        Ok(_) => return Err(io::Error::other("journal anchor changed")),
        Err(PrivateFileError::Missing) => {
            return Err(io::Error::other("established journal anchor is missing"));
        }
        Err(_) => return Err(io::Error::other("journal anchor is not private")),
    };
    // A writer's save renames a new checkpoint over the name, so an open that
    // got the old inode just before reads the checkpoint it replaced, whose
    // descriptor is left no link. The name then holds the newer checkpoint:
    // read it again. A second link is refused, at the open or after it.
    let malformed = || io::Error::other("journal checkpoint is malformed or not private");
    let mut relinked = 0;
    let (checkpoint, checkpoint_opened) = loop {
        let (file, _) = judged.open(&layout.checkpoint).map_err(|_| malformed())?;
        #[cfg(test)]
        tests::read_step(tests::Reading::Opened);
        let mut bytes = Vec::with_capacity(41);
        (&file)
            .take(41)
            .read_to_end(&mut bytes)
            .map_err(|_| malformed())?;
        let opened = file
            .metadata()
            .and_then(|metadata| seen(&file, &metadata))
            .map_err(|_| malformed())?;
        match opened.links {
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
        break (bytes, opened.identity);
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
        .open(&layout.astream)
        .map_err(|_| io::Error::other("journal is not a private regular file"))?;
    if metadata.len() < length {
        return Err(io::Error::other("journal lost acknowledged bytes"));
    }
    let acknowledged = Acknowledged {
        identity: seen(&file, &metadata)?.identity,
        length,
        digest,
    };
    let opened = Opened {
        anchor: anchor_opened,
        checkpoint: checkpoint_opened,
        astream: acknowledged.identity,
    };
    let mut bytes = vec![
        0;
        usize::try_from(length)
            .map_err(|_| io::Error::other("journal checkpoint length overflow"))?
    ];
    file.read_exact(&mut bytes)?;
    if let Some(known) = known
        && known.acknowledged == acknowledged
        && known.records <= limits.records as u64
        && known.max_record_bytes <= limits.record_bytes
        && known
            .bytes
            .as_ref()
            .is_some_and(|saved| saved.as_ref() == bytes.as_slice())
    {
        return Ok((Committed::Unchanged, opened));
    }
    #[cfg(test)]
    tests::hashing();
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
    let committed = match extends {
        Some(known) if known.acknowledged.length == length => Committed::Unchanged,
        Some(known) => {
            let records = decode(&bytes[hashed..], known.records, &layout)?;
            let max_record_bytes = records
                .iter()
                .map(|r| r.body.len())
                .max()
                .unwrap_or(0)
                .max(known.max_record_bytes);
            Committed::Extended {
                prefix: Prefix {
                    acknowledged,
                    records: known.records + records.len() as u64,
                    max_record_bytes,
                    bytes: (retain == Retain::Bytes && bytes.len() <= PREFIX_BYTES)
                        .then(|| Arc::from(bytes)),
                },
                records,
            }
        }
        None => {
            let records = decode(&bytes, 0, &layout)?;
            let max_record_bytes = records.iter().map(|r| r.body.len()).max().unwrap_or(0);
            Committed::Rebuilt {
                prefix: Prefix {
                    acknowledged,
                    records: records.len() as u64,
                    max_record_bytes,
                    bytes: (retain == Retain::Bytes && bytes.len() <= PREFIX_BYTES)
                        .then(|| Arc::from(bytes)),
                },
                records,
            }
        }
    };
    Ok((committed, opened))
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
        if record.seq.0 != offset
            || record.subject.is_empty()
            || astream_broker::store::is_hidden_subject(&record.subject)
        {
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
fn lock(dir: &Path, layout: &Layout, deadline: Instant) -> io::Result<(WriterLock, FileIdentity)> {
    // The parent names the journal's directory. On the journal's device a barrier orders
    // that name ahead of everything the open writes after it, and the open's last full
    // flush makes it durable; on another device it is flushed in full here.
    private_file::create_private_dir(dir)?;
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        Flush::ordered_if_same_device(parent, dir).dir(parent)?;
    }
    #[cfg(unix)]
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS)
        .open(dir.join(&layout.lock))
    {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    // Made, where missing, under the user's protected access list from its creation, never
    // given one after; an elevated process makes none (`private_file::ELEVATED`), so it opens
    // no writer. One that is there is judged below, as on Unix.
    #[cfg(windows)]
    match private_file::open_private_rw(dir, &layout.lock) {
        Ok(_) => {}
        Err(PrivateFileError::Io(error)) => return Err(error),
        Err(_) => return Err(io::Error::other("journal lock is not private")),
    }
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| io::Error::other("journal lock is not private"))?;
    let (file, metadata) = aterm_uds::kept_out(|| judged.open(&layout.lock))
        .map_err(|_| io::Error::other("journal lock is not private"))?;
    aterm_uds::file_lock::lock_exclusive_until_with(
        &file,
        deadline,
        "another local journal writer is active",
        || {
            #[cfg(test)]
            tests::lock_held();
        },
    )?;
    let lock = WriterLock(file);
    let writer_identity = seen(&lock.0, &metadata)?.identity;
    let current = private_file::open_private(dir, &layout.lock)
        .map_err(|_| io::Error::other("journal lock is not private"))
        .and_then(|(file, metadata)| seen(&file, &metadata))?;
    if current.identity != writer_identity {
        return Err(io::Error::other("journal lock changed while acquiring it"));
    }
    Ok((lock, writer_identity))
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
    context_error(format!("journal {} {stage}", layout.name), error)
}

#[derive(Debug)]
struct ContextError {
    context: String,
    error: io::Error,
}

fn context_error(context: String, error: io::Error) -> io::Error {
    io::Error::new(error.kind(), ContextError { context, error })
}

fn private_error(error: PrivateFileError) -> io::Error {
    match error {
        PrivateFileError::Io(error) => error,
        error => io::Error::other(error),
    }
}

impl std::fmt::Display for ContextError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.context, self.error)
    }
}

impl std::error::Error for ContextError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[derive(Debug)]
struct CheckpointFailure {
    path: PathBuf,
    error: io::Error,
}

impl std::fmt::Display for CheckpointFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "uncertain journal checkpoint {}: {}; reopen and retry exact bytes",
            self.path.display(),
            self.error
        )
    }
}

impl std::error::Error for CheckpointFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Digest of the exact acknowledged physical prefix. Kept incrementally in
/// memory, but reconstructed and checked before astream can repair any tail.
#[derive(Clone)]
struct Checkpoint {
    length: u64,
    hash: aterm_digest::Sha256,
}
/// Where a Windows checkpoint reads the astream file's bytes. astream locks its log whole with
/// `File::try_lock` (`LockFileEx` through its own handle), and Windows makes that lock
/// mandatory: while it is held, a read through any other handle, this process's included, is
/// refused (`ERROR_LOCK_VIOLATION`). So the bytes are read through the handle that holds the
/// lock, and the name is still opened, and judged private, for the length and the identity.
/// (On Unix the lock is advisory and the bytes are read through that judged open of the name.)
#[cfg(windows)]
#[derive(Clone, Copy)]
enum Through<'a> {
    /// Recovery's own handle, which holds the physical lock while it checks the prefix.
    Held(&'a File),
    /// The open log's own descriptor ([`BrokerLog::copy_bytes_to`]): it copies from offset
    /// zero, so an extension reads the whole log and hashes only its new bytes.
    Log(&'a BrokerLog),
}

/// The bytes `[from, to)` of what [`BrokerLog::copy_bytes_to`] writes from offset zero, into a
/// hash; how far it wrote is `at`.
#[cfg(windows)]
struct Window<'a> {
    at: u64,
    from: u64,
    to: u64,
    hash: &'a mut aterm_digest::Sha256,
}

#[cfg(windows)]
impl io::Write for Window<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let start = self.at;
        let end = start.saturating_add(bytes.len() as u64);
        let first = self.from.clamp(start, end);
        let last = self.to.clamp(first, end);
        let window = usize::try_from(first - start)
            .ok()
            .zip(usize::try_from(last - start).ok())
            .and_then(|(first, last)| bytes.get(first..last))
            .ok_or_else(|| io::Error::other("journal checkpoint length overflow"))?;
        self.hash.update(window);
        self.at = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Checkpoint {
    /// The checkpoint `dir` holds, its prefix read again and hashed through a judged open of
    /// the astream file's name; whether it was there (a fresh journal's may not be); and what
    /// that open saw.
    #[cfg(unix)]
    fn load(
        dir: &Path,
        layout: &Layout,
        fresh: bool,
        parked: Option<&Parked>,
    ) -> io::Result<(Self, bool, Seen, bool)> {
        Self::load_with(dir, layout, fresh, parked, |checkpoint, judged, length| {
            checkpoint.extend_in(dir, judged, layout, length)
        })
    }

    /// [`Self::load`]'s Windows twin: the prefix's bytes are read `through` the handle that
    /// holds the log's lock ([`Through`]).
    #[cfg(windows)]
    fn load_through(
        dir: &Path,
        layout: &Layout,
        fresh: bool,
        parked: Option<&Parked>,
        through: Through<'_>,
    ) -> io::Result<(Self, bool, Seen, bool)> {
        Self::load_with(dir, layout, fresh, parked, |checkpoint, judged, length| {
            checkpoint.extend_through(dir, judged, layout, length, through)
        })
    }

    /// The length the checkpoint `dir` holds attests (0 where a fresh journal has none, whose
    /// astream file must then be empty) and what a judged open of the astream file's name sees,
    /// reading none of its bytes: what astream's open lets the journal check under its lock on
    /// Windows ([`Through`]).
    #[cfg(windows)]
    fn peek(dir: &Path, layout: &Layout, fresh: bool) -> io::Result<(u64, Seen)> {
        let malformed = || io::Error::other("journal checkpoint is malformed or not private");
        let judged = private_file::JudgedDir::judge(dir).map_err(|_| malformed())?;
        let length = match judged.read(&layout.checkpoint, 40) {
            Ok(bytes) if bytes.len() == 40 => Some(u64::from_le_bytes(
                bytes[..8].try_into().map_err(|_| malformed())?,
            )),
            Err(PrivateFileError::Missing) if fresh => None,
            _ => return Err(malformed()),
        };
        let seen = judged
            .open(&layout.astream)
            .map_err(|_| io::Error::other("journal is not private"))
            .and_then(|(file, metadata)| seen(&file, &metadata))?;
        if length.is_none() && seen.len != 0 {
            return Err(io::Error::other(
                "nonempty journal has no acknowledgement checkpoint",
            ));
        }
        Ok((length.unwrap_or(0), seen))
    }

    /// The checkpoint `dir` holds, its prefix hashed by `extend`; whether it was there; what
    /// `extend`'s open of the astream file saw (a fresh journal's own open, where there is none);
    /// and whether the hash started from `parked`'s state, which it does where the journal
    /// stands as it was parked ([`Parked::seed`]), and from offset zero otherwise.
    fn load_with(
        dir: &Path,
        layout: &Layout,
        fresh: bool,
        parked: Option<&Parked>,
        extend: impl FnOnce(&mut Self, &private_file::JudgedDir<'_>, u64) -> io::Result<Seen>,
    ) -> io::Result<(Self, bool, Seen, bool)> {
        let mut checkpoint = Self {
            length: 0,
            hash: aterm_digest::Sha256::new(),
        };
        let judged = private_file::JudgedDir::judge(dir)
            .map_err(|_| io::Error::other("journal checkpoint is malformed or not private"))?;
        match judged.read(&layout.checkpoint, 40) {
            Ok(bytes) if bytes.len() == 40 => {
                let length = u64::from_le_bytes(
                    bytes[..8]
                        .try_into()
                        .map_err(|_| io::Error::other("invalid journal checkpoint"))?,
                );
                let seed = match parked {
                    Some(parked) => {
                        let astream = judged
                            .open(&layout.astream)
                            .map_err(|_| io::Error::other("journal is not private"))
                            .and_then(|(file, metadata)| Stamp::of(&file, &metadata))?;
                        parked.seed(&bytes, astream)
                    }
                    None => None,
                };
                let seeded = seed.is_some();
                if let Some(seed) = seed {
                    checkpoint = seed;
                }
                let seen = extend(&mut checkpoint, &judged, length)?;
                if checkpoint.hash.clone().finalize() != bytes[8..] {
                    return Err(io::Error::other(
                        "acknowledged journal history changed; preserve it for authenticated repair",
                    ));
                }
                Ok((checkpoint, true, seen, seeded))
            }
            Err(PrivateFileError::Missing) if fresh => {
                let seen = judged
                    .open(&layout.astream)
                    .map_err(|_| io::Error::other("journal is not private"))
                    .and_then(|(file, metadata)| seen(&file, &metadata))?;
                if seen.len != 0 {
                    return Err(io::Error::other(
                        "nonempty journal has no acknowledgement checkpoint",
                    ));
                }
                Ok((checkpoint, false, seen, false))
            }
            _ => Err(io::Error::other(
                "journal checkpoint is malformed or not private",
            )),
        }
    }

    /// `dir`'s verdict for an append's extension.
    fn judge(dir: &Path) -> io::Result<private_file::JudgedDir<'_>> {
        private_file::JudgedDir::judge(dir).map_err(|error| {
            context_error(
                format!("judge directory {}", dir.display()),
                private_error(error),
            )
        })
    }

    #[cfg(unix)]
    fn extend(&mut self, dir: &Path, layout: &Layout, length: u64) -> io::Result<()> {
        let judged = Self::judge(dir)?;
        self.extend_in(dir, &judged, layout, length).map(|_| ())
    }

    /// [`Self::extend`]'s Windows twin, through the open log's own descriptor
    /// ([`Through::Log`]).
    #[cfg(windows)]
    fn extend_log(
        &mut self,
        dir: &Path,
        layout: &Layout,
        length: u64,
        log: &BrokerLog,
    ) -> io::Result<()> {
        let judged = Self::judge(dir)?;
        self.extend_through(dir, &judged, layout, length, Through::Log(log))
            .map(|_| ())
    }

    /// The astream file opened by its name under `judged`, and what the open saw, which must
    /// hold the bytes up to `length`, no fewer than this checkpoint has, within the limits.
    fn open_to(
        &self,
        dir: &Path,
        judged: &private_file::JudgedDir<'_>,
        layout: &Layout,
        length: u64,
    ) -> io::Result<(File, Seen)> {
        let (file, metadata) = judged.open(&layout.astream).map_err(|error| {
            context_error(
                format!("open private {}", dir.join(&layout.astream).display()),
                private_error(error),
            )
        })?;
        let seen = seen(&file, &metadata)?;
        if length < self.length
            || length > layout.limits.bytes as u64
            || seen.len < length
            || seen.len
                > layout
                    .limits
                    .bytes
                    .saturating_add(layout.limits.record_bytes) as u64
        {
            return Err(io::Error::other(
                "journal lost acknowledged bytes or exceeded its capacity",
            ));
        }
        Ok((file, seen))
    }

    #[cfg(unix)]
    fn extend_in(
        &mut self,
        dir: &Path,
        judged: &private_file::JudgedDir<'_>,
        layout: &Layout,
        length: u64,
    ) -> io::Result<Seen> {
        let (file, seen) = self.open_to(dir, judged, layout, length)?;
        self.read_to(&file, dir, layout, length)?;
        Ok(seen)
    }

    /// [`Self::extend_in`]'s Windows twin: the bytes are read `through` the handle that holds
    /// the log's lock, and the name is opened and judged for the length and the identity all
    /// the same. A recovery handle on another file than the name's is refused.
    #[cfg(windows)]
    fn extend_through(
        &mut self,
        dir: &Path,
        judged: &private_file::JudgedDir<'_>,
        layout: &Layout,
        length: u64,
        through: Through<'_>,
    ) -> io::Result<Seen> {
        let (file, named) = self.open_to(dir, judged, layout, length)?;
        drop(file);
        match through {
            Through::Held(held) => {
                if seen(held, &held.metadata()?)?.identity != named.identity {
                    return Err(io::Error::other(
                        "journal changed while its prefix was checked",
                    ));
                }
                self.read_to(held, dir, layout, length)?;
            }
            Through::Log(log) => {
                let mut window = Window {
                    at: 0,
                    from: self.length,
                    to: length,
                    hash: &mut self.hash,
                };
                let copied = log.copy_bytes_to(&mut window).map_err(|error| {
                    context_error(
                        format!("read {}", dir.join(&layout.astream).display()),
                        error,
                    )
                })?;
                if copied < length {
                    return Err(io::Error::other(
                        "journal lost acknowledged bytes or exceeded its capacity",
                    ));
                }
                self.length = length;
            }
        }
        Ok(named)
    }

    /// Hash the astream file's bytes from this checkpoint's length to `length`, read from
    /// `file` at those offsets, and take `length` as this checkpoint's.
    fn read_to(
        &mut self,
        mut file: &File,
        dir: &Path,
        layout: &Layout,
        length: u64,
    ) -> io::Result<()> {
        file.seek(SeekFrom::Start(self.length)).map_err(|error| {
            context_error(
                format!("seek {}", dir.join(&layout.astream).display()),
                error,
            )
        })?;
        let mut remaining = length - self.length;
        let mut buffer = [0; 8192];
        while remaining != 0 {
            let count = usize::try_from(remaining.min(buffer.len() as u64))
                .map_err(|_| io::Error::other("journal checkpoint length overflow"))?;
            file.read_exact(&mut buffer[..count]).map_err(|error| {
                context_error(
                    format!("read {}", dir.join(&layout.astream).display()),
                    error,
                )
            })?;
            self.hash.update(&buffer[..count]);
            #[cfg(test)]
            tests::hashed(count);
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
    /// Rename the checkpoint into place, finished by `flush`: its directory's flush must
    /// succeed ([`private_file::write_private_flushed`]).
    fn save(&self, dir: &Path, layout: &Layout, flush: Flush) -> io::Result<()> {
        private_file::write_private_flushed(dir, &layout.checkpoint, &self.bytes(), flush)
    }
}

/// The checkpoint is the application's commit point. Preserve any bytes after
/// it for diagnosis, then roll back that unacknowledged suffix. The application
/// writer and the physical file are both locked while this happens; astream's
/// prepare callback rechecks the prefix after taking over the physical lock (on Windows its
/// length and file, and the open log's own descriptor its bytes: [`Through`]).
fn recover_uncommitted(
    dir: &Path,
    layout: &Layout,
    fresh: bool,
    parked: Option<&Parked>,
) -> io::Result<(FileIdentity, bool)> {
    let (file, opened) = open_for_recovery(dir, layout)?;
    #[expect(
        clippy::disallowed_methods,
        reason = "WriterLock is CLOEXEC/CLOFORK and unlocks the physical file before transferring ownership to astream"
    )]
    file.try_lock()
        .map_err(lock_error)
        .map_err(|error| stage_error(layout, "physical recovery lock", error))?;
    let mut held = WriterLock(file);
    #[cfg(unix)]
    let (checkpoint, _, checked, seeded) = Checkpoint::load(dir, layout, fresh, parked)?;
    #[cfg(windows)]
    let (checkpoint, _, checked, seeded) =
        Checkpoint::load_through(dir, layout, fresh, parked, Through::Held(&held.0))?;
    if checked.identity != opened {
        return Err(io::Error::other(
            "journal changed while its prefix was checked",
        ));
    }
    let max_journal_bytes = layout
        .limits
        .bytes
        .saturating_add(layout.limits.record_bytes);
    if checked.len > max_journal_bytes as u64 {
        return Err(io::Error::other(
            "journal lost acknowledged bytes or exceeded its capacity",
        ));
    }
    let tail_len = checked
        .len
        .checked_sub(checkpoint.length)
        .ok_or_else(|| io::Error::other("journal lost acknowledged bytes"))?;
    if tail_len == 0 {
        return Ok((opened, seeded));
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
        layout.archive_prefix,
        aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(&tail))
    );
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| io::Error::other("journal suffix archive is corrupt or not private"))?;
    match judged.read(&name, max_journal_bytes) {
        Ok(old) if old == tail => {}
        Err(PrivateFileError::Missing) => {
            let mut bytes = tail.len() as u64;
            let mut archives = 0;
            let uncommitted_prefix = format!("{}uncommitted-", layout.archive_prefix);
            for (count, entry) in std::fs::read_dir(dir)?.enumerate() {
                if count >= layout.limits.records.saturating_add(32) {
                    return Err(io::Error::other("journal directory exceeds its entry cap"));
                }
                let entry = entry?;
                let file_name = entry.file_name();
                let file_name = file_name.to_string_lossy();
                if !file_name.starts_with(&uncommitted_prefix) {
                    continue;
                }
                archives += 1;
                if archives >= layout.limits.records {
                    return Err(io::Error::other("journal archive entry capacity exhausted"));
                }
                let (_, metadata) = judged
                    .open(&file_name)
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
    // A cut tail is a change the parked stamp did not show: the length moved.
    Ok((opened, false))
}

/// The astream file opened to read and write for recovery, never through a link, and checked
/// to be the file a private open of its name judged just before, with its identity.
#[cfg(unix)]
fn open_for_recovery(dir: &Path, layout: &Layout) -> io::Result<(File, FileIdentity)> {
    let (_, admitted) = private_file::open_private(dir, &layout.astream)
        .map_err(|_| io::Error::other("journal is not private"))?;
    let file = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(dir.join(&layout.astream))
    })?;
    let opened = file.metadata()?;
    if opened.dev() != admitted.dev()
        || opened.ino() != admitted.ino()
        || opened.mode() != admitted.mode()
        || opened.uid() != admitted.uid()
    {
        return Err(io::Error::other("journal changed during private open"));
    }
    Ok((file, (opened.dev(), opened.ino())))
}

/// The astream file opened to read and write for recovery, as itself (a reparse point put at
/// its name is opened, never followed, and is another file), sharing everything as every
/// private open does, and checked to be the file a private open of its name judged just
/// before, with its identity. Windows keeps no mode or owner on the handle to compare: an
/// access list changed in between is judged again on the private open of the name the
/// prefix's check makes ([`Checkpoint::load_through`]) once this one holds the lock.
#[cfg(windows)]
fn open_for_recovery(dir: &Path, layout: &Layout) -> io::Result<(File, FileIdentity)> {
    let admitted = private_file::open_private(dir, &layout.astream)
        .map_err(|_| io::Error::other("journal is not private"))
        .and_then(|(file, metadata)| seen(&file, &metadata))?;
    let file = aterm_uds::kept_out(|| {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(SHARE_ALL)
            .custom_flags(aterm_uds::acl::FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dir.join(&layout.astream))
    })?;
    let opened = seen(&file, &file.metadata()?)?;
    if opened.identity != admitted.identity {
        return Err(io::Error::other("journal changed during private open"));
    }
    Ok((file, opened.identity))
}

#[cfg(test)]
mod append_tests;
#[cfg(test)]
mod group_tests;
#[cfg(test)]
mod open_tests;
#[cfg(all(test, unix))]
mod resume_tests;
#[cfg(test)]
mod tests;
