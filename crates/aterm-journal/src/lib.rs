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
#![forbid(unsafe_code)]
#![cfg(unix)]
#![cfg_attr(
    all(unix, anchor_rules, any(test, debug_assertions)),
    feature(macro_attr)
)]

use astream_broker::{BrokerLog, BrokerRecord, Durability, OpenOptions as LogOptions, Prepared};
use astream_wire::Offset;
use aterm_durable::Flush;
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

/// Fixed resource bounds for one immutable generation. Retention is deliberately
/// unavailable: dropping an old record could erase authorization or liabilities.
#[derive(Clone, Copy)]
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
            anchor: format!("{name}.{ANCHOR}"),
            astream: format!("{name}.{ASTREAM}"),
            checkpoint: format!("{name}.{CHECKPOINT}"),
            lock: format!("{name}.lock"),
            archive_prefix: format!("{name}-"),
            limits,
        })
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
    writer_identity: (u64, u64),
    _writer: WriterLock,
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
        let identity = recover_uncommitted(dir, &layout, fresh)
            .map_err(|error| stage_error(&layout, "recovery", error))?;
        let mut checkpoint = None;
        let log = BrokerLog::open_opts(dir.join(&layout.astream), log_options(), || {
            let (checked, present, metadata) = Checkpoint::load(dir, &layout, fresh)?;
            if metadata.len() != checked.length || (metadata.dev(), metadata.ino()) != identity {
                return Err(io::Error::other("journal changed between its writer locks"));
            }
            checkpoint = Some((checked, present));
            Ok(Prepared::Nothing)
        })
        .map_err(|error| stage_error(&layout, "astream open", error))?;
        let (checkpoint, present) =
            checkpoint.ok_or_else(|| io::Error::other("journal was not prepared"))?;
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
        let (_, metadata) = private_file::open_private(dir, &layout.astream)
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        // An existing checkpoint may be visible after its previous writer failed the
        // rename's directory barrier, or its commit. Finish that publication even without
        // rewriting it: the open's full flush, which covers this open's own checkpoint too.
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
            writer_identity,
            _writer: lock,
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
            write_private_durable(&self.dir, &self.layout.anchor, anchor)?;
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
            .open(&self.layout.astream)
            .map_err(|_| io::Error::other("journal lost its private inode"))?;
        let (_, lock) = dir
            .open(&self.layout.lock)
            .map_err(|_| io::Error::other("journal lost its writer lease"))?;
        let anchor_ok = match dir.read(&self.layout.anchor, 4096) {
            Ok(bytes) => bytes == self.anchor,
            Err(PrivateFileError::Missing) => self.pending_anchor.is_some(),
            Err(_) => false,
        };
        if (metadata.dev(), metadata.ino()) != self.identity
            || metadata.len() != self.log.stats().retained_bytes as u64
            || (lock.dev(), lock.ino()) != self.writer_identity
            || !anchor_ok
            || dir.read(&self.layout.checkpoint, 40).ok().as_deref()
                != Some(&self.checkpoint.bytes())
        {
            return Err(io::Error::other("journal or writer lease was replaced"));
        }
        Ok(())
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
    /// An uncertain append withholds the handle's observations until reopen.
    pub fn read_range(&self, offset: Offset, count: usize) -> io::Result<Vec<Arc<BrokerRecord>>> {
        if self.is_poisoned() {
            return Err(io::Error::other("uncertain journal: reopen"));
        }
        Ok(self.log.read_range(offset, count.min(256)))
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
                    "uncertain journal append: reopen and retry exact bytes",
                ));
            }
        };
        if self.intact().is_err() {
            self.poisoned = true;
            return Err(io::Error::other("journal changed during append; reopen"));
        }
        #[cfg(test)]
        tests::append_step(tests::Step::Published, self.log.flush_counts());
        // Read after the publish, which latches the log to Strict when the device refuses
        // its barrier. A Strict record is durable already, so the commit's full flush
        // below finds nothing to cover and skips: there the checkpoint is flushed in full
        // itself. On an Ordered log it takes barriers, ordered behind the record's.
        let ordered = self.served_ordered();
        if self
            .checkpoint
            .extend(
                &self.dir,
                &self.layout,
                self.log.stats().retained_bytes as u64,
            )
            .and_then(|()| {
                self.checkpoint
                    .save(&self.dir, &self.layout, self.checkpoint_flush(ordered))
            })
            .is_err()
        {
            self.poisoned = true;
            return Err(io::Error::other(
                "uncertain journal checkpoint: reopen and retry exact bytes",
            ));
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
    let mut options = LogOptions::new(Durability::Ordered { horizon: None });
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
/// Up to eight MiB of private physical bytes are retained for fresh comparisons.
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

/// What a read of a journal ([`read_committed_opened`]) opened, each file by the `(dev, ino)` of
/// its open descriptor. Each is a private open, which refuses a file with a second link, so a file
/// another folder holds for the read is another inode, or one moved out of the journal's folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opened {
    /// The anchor's.
    pub anchor: (u64, u64),
    /// The checkpoint's whose acknowledgement the read took.
    pub checkpoint: (u64, u64),
    /// The astream file's.
    pub astream: (u64, u64),
}

/// Read an established journal's acknowledged prefix without taking any lock,
/// so it works beside a live writer. The checkpoint is read first; then exactly
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
/// astream file's bytes is passed on as it is.
pub fn read_committed(
    dir: &Path,
    name: &str,
    anchor: &[u8],
    limits: Limits,
    known: Option<&Prefix>,
) -> io::Result<Committed> {
    read_committed_opened(dir, name, anchor, limits, known).map(|(committed, _)| committed)
}

/// [`read_committed`], with the files it opened ([`Opened`]): for a reader that keeps what it
/// read while a witness of the paths stands, which must be a witness of those very files.
/// # Errors
/// As [`read_committed`].
pub fn read_committed_opened(
    dir: &Path,
    name: &str,
    anchor: &[u8],
    limits: Limits,
    known: Option<&Prefix>,
) -> io::Result<(Committed, Opened)> {
    let layout = Layout::new(name, anchor, limits)?;
    // One directory verdict for the three files, as in `Log::intact`.
    let judged = private_file::JudgedDir::judge(dir)
        .map_err(|_| io::Error::other("journal anchor is not private"))?;
    let anchor_opened = match judged.read_with_metadata(&layout.anchor, 4096) {
        Ok((bytes, metadata)) if bytes == anchor => (metadata.dev(), metadata.ino()),
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
        let metadata = file.metadata().map_err(|_| malformed())?;
        match metadata.nlink() {
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
        break (bytes, (metadata.dev(), metadata.ino()));
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
        identity: (metadata.dev(), metadata.ino()),
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
                    bytes: (bytes.len() <= PREFIX_BYTES).then(|| Arc::from(bytes)),
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
                    bytes: (bytes.len() <= PREFIX_BYTES).then(|| Arc::from(bytes)),
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
fn lock(dir: &Path, layout: &Layout, deadline: Instant) -> io::Result<(WriterLock, (u64, u64))> {
    // The parent names the journal's directory. On the journal's device a barrier orders
    // that name ahead of everything the open writes after it, and the open's last full
    // flush makes it durable; on another device it is flushed in full here.
    private_file::create_private_dir(dir)?;
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        Flush::ordered_if_same_device(parent, dir).dir(parent)?;
    }
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
    let writer_identity = (metadata.dev(), metadata.ino());
    let (_, current) = private_file::open_private(dir, &layout.lock)
        .map_err(|_| io::Error::other("journal lock is not private"))?;
    if (current.dev(), current.ino()) != writer_identity {
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
    fn load(
        dir: &Path,
        layout: &Layout,
        fresh: bool,
    ) -> io::Result<(Self, bool, std::fs::Metadata)> {
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
                let metadata = checkpoint.extend_in(&judged, layout, length)?;
                if checkpoint.hash.clone().finalize() != bytes[8..] {
                    return Err(io::Error::other(
                        "acknowledged journal history changed; preserve it for authenticated repair",
                    ));
                }
                Ok((checkpoint, true, metadata))
            }
            Err(PrivateFileError::Missing) if fresh => {
                let (_, metadata) = judged
                    .open(&layout.astream)
                    .map_err(|_| io::Error::other("journal is not private"))?;
                if metadata.len() != 0 {
                    return Err(io::Error::other(
                        "nonempty journal has no acknowledgement checkpoint",
                    ));
                }
                Ok((checkpoint, false, metadata))
            }
            _ => Err(io::Error::other(
                "journal checkpoint is malformed or not private",
            )),
        }
    }
    fn extend(&mut self, dir: &Path, layout: &Layout, length: u64) -> io::Result<()> {
        let judged = private_file::JudgedDir::judge(dir)
            .map_err(|_| io::Error::other("journal is not private"))?;
        self.extend_in(&judged, layout, length).map(|_| ())
    }
    fn extend_in(
        &mut self,
        judged: &private_file::JudgedDir<'_>,
        layout: &Layout,
        length: u64,
    ) -> io::Result<std::fs::Metadata> {
        let (mut file, metadata) = judged
            .open(&layout.astream)
            .map_err(|_| io::Error::other("journal is not private"))?;
        if length < self.length
            || length > layout.limits.bytes as u64
            || metadata.len() < length
            || metadata.len()
                > layout
                    .limits
                    .bytes
                    .saturating_add(layout.limits.record_bytes) as u64
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
        Ok(metadata)
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
/// prepare callback rechecks the prefix after taking over the physical lock.
fn recover_uncommitted(dir: &Path, layout: &Layout, fresh: bool) -> io::Result<(u64, u64)> {
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
    #[expect(
        clippy::disallowed_methods,
        reason = "WriterLock is CLOEXEC/CLOFORK and unlocks the physical file before transferring ownership to astream"
    )]
    file.try_lock()
        .map_err(lock_error)
        .map_err(|error| stage_error(layout, "physical recovery lock", error))?;
    let mut held = WriterLock(file);
    let (checkpoint, _, checked) = Checkpoint::load(dir, layout, fresh)?;
    if checked.dev() != opened.dev() || checked.ino() != opened.ino() {
        return Err(io::Error::other(
            "journal changed while its prefix was checked",
        ));
    }
    let max_journal_bytes = layout
        .limits
        .bytes
        .saturating_add(layout.limits.record_bytes);
    if checked.len() > max_journal_bytes as u64 {
        return Err(io::Error::other(
            "journal lost acknowledged bytes or exceeded its capacity",
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
    Ok((opened.dev(), opened.ino()))
}

#[cfg(test)]
mod append_tests;
#[cfg(test)]
mod open_tests;
#[cfg(test)]
mod tests;
