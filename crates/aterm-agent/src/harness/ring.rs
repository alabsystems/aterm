// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bounded ledger: an append-only ring of segment files that never says
//! "full" (design §4.4, `LedgerRing` in §11 item 5).
//!
//! Every harness capability writes its rows here (`rm`, `usage`, `actuation`,
//! `alignment`, `recovery`). The operator WAL in [`crate::operator`] shares the
//! 64 MiB figure and nothing else: that log is fail-closed (`WalFull`) and
//! compacts into a checkpoint, this one is LOSSY by design — when it is out of
//! room it forgets its oldest segment and takes the new row.
//!
//! # On disk
//!
//! ```text
//! <dir>/                                   0700, a real directory, never a link
//!   <name>.lock                            0600, the single-writer lock
//!   <name>.00000000000000000001.jsonl      0600, oldest segment
//!   <name>.00000000000000000412.jsonl      0600, newest segment = the ACTIVE one
//! ```
//!
//! A segment is named by the id its first slot holds, zero-padded to 20 digits
//! so that name order is id order. Rotation therefore never renames anything:
//! it creates one file and unlinks one file, and each of those is a whole step
//! a crash can land on either side of (see "Crashes" below). The design's
//! single `ledger/rm.jsonl` path becomes this family of files; several rings
//! with different names share one directory.
//!
//! # Line framing (the decision this module owns)
//!
//! ```text
//! {"id":812,"n":27,"row":{"cmd":"rm -rf target/x"}}\n
//! ```
//!
//! `id` is the row's id, `n` is the byte length of what follows `"row":` up to
//! the closing brace, and the row is the caller's line, byte for byte. The
//! reader is a fixed-format parser (prefix, two canonical decimals, exactly `n`
//! bytes, one `}`); it never parses the row. So:
//!
//! * when every appended line is one JSON value — the design's case — each
//!   segment is real JSONL and `jq .row` reads it; the ring neither checks that
//!   nor relies on it;
//! * a row cut short by a torn write fails the `n` check, a region the
//!   filesystem zero-filled fails it too (a row may not contain NUL, so NUL in
//!   a line is never data), and a line that lost ONLY its newline is whole and
//!   is kept;
//! * ids are EXPLICIT, so a damaged stretch cannot shift the ids of the rows
//!   after it.
//!
//! Bit rot that keeps a row's length is not detected: there is no checksum.
//!
//! # Ids
//!
//! Ids start at 1 and only grow. Every line in a segment occupies one id SLOT:
//! a well-framed line names its slot, and any other line — sealed junk, an
//! unterminated tail — burns the slot after the one before it. A torn tail
//! found on reopen is therefore never handed out again, however many times the
//! ring is reopened before the next append, and without the open writing
//! anything: the next append writes the missing newline in front of its own
//! record, which turns the fragment into a junk line that keeps burning its
//! slot forever. [`Ring::floor_id`] is the first slot the ring still holds, so
//! a reader whose cursor is below it knows rows were dropped in between.
//!
//! # The bound, and what rotation drops
//!
//! A segment holds at most `max_bytes / segments` bytes (and a config that
//! leaves a segment under [`MIN_SEGMENT_BYTES`] is refused). A record that would
//! not fit the active segment starts a new one; BEFORE the record is written
//! the oldest segments are unlinked until at most `segments` files remain and
//! the record fits under `max_bytes`. A framed record larger than one segment
//! is refused as invalid input — which is what keeps one append from flushing
//! the whole ledger, and what makes the bound strict: after any append the ring
//! holds at most `max_bytes` bytes, with no "plus one line" slack. Segments are
//! only ever unlinked from the OLDEST end, never the active one, and a ring
//! that keeps one config for its whole life loses exactly one segment per
//! rotation. One exception is stated rather than hidden: a ring opened with a
//! SMALLER config than it was written with is trimmed to the new config at open
//! (oldest first, possibly several segments), and keeps its newest segment even
//! if that alone is over the new bound; the next append rotates it out. A row
//! longer than the new `max_bytes` is then read as junk: its slot stays burned
//! and its text is not returned.
//!
//! A full DISK is not the ring's business: the write fails, the error is the
//! filesystem's, and nothing is dropped to make room for it.
//!
//! Row size and row rate quotas for agent-fed rows (design §4.4: 4 KiB/row,
//! 64 rows/min) are the CALLER's job and are deliberately absent here.
//!
//! # Crashes
//!
//! * mid-write: the tail is an unterminated fragment; see "Ids".
//! * after a write that filled the segment, before the rotation: the next
//!   append after reopen rotates.
//! * after the new segment was created, before the oldest was unlinked (the
//!   order rotation uses): reopen finds one file too many and trims from the
//!   oldest end; the empty newest segment is adopted as the active one, and its
//!   NAME carries the next id, so nothing has to be read to continue the ids.
//!
//! An `Err` from [`Ring::append`] means the row may or may not be in the ring.
//! The id it would have carried is never reported for a different row.
//!
//! # Privacy and the single writer
//!
//! Following [`crate::operator`]: the directory is created component by
//! component at 0700, must be a real directory (the closest existing ancestor
//! too), and is forced to 0700; files are opened `O_NOFOLLOW | O_CLOEXEC |
//! O_NONBLOCK`, must be regular files, and are forced to 0600. Unlike the
//! operator this module holds no `unsafe`, so it cannot ask for the process
//! uid. What it checks instead: forcing the mode succeeds only for the owner
//! (or uid 0), and every file must have the directory's owner. A uid-0 caller
//! is thus NOT told apart from the directory's owner by [`Ring::open`]; a host
//! that knows its uid passes it to [`Ring::open_owned_by`], which is the
//! operator-grade check.
//!
//! [`Ring::open`] takes an exclusive advisory lock on `<name>.lock` and a
//! second ring on the same directory and name — in this process or another — is
//! refused with [`io::ErrorKind::ResourceBusy`]. The lock is released
//! explicitly on drop, not by closing the descriptor (the fork window
//! `operator.rs` documents).
//!
//! # Durability
//!
//! By default an append returns after `sync_data` on the segment, and a new
//! segment's directory entry is synchronized before it is used. That is what
//! makes "this id was handed out" and "this row is on disk" the same event.
//! [`Ring::set_durable`] turns it off for callers that prefer speed; then a
//! power loss may drop the unsynchronized tail and ids of dropped rows are
//! issued again after the restart.
//!
//! # The model, and what binds it to this code
//!
//! The design (§11 item 5) owes this ring a derived model, and it is
//! `harness_ledger_ring_model` in `aterm-spec`'s registry (Tier 0 there, in
//! `tests/derived_ring_ty.rs`) — ONE model, not a copy per crate. It states
//! the slot accounting — the writer's next id against the slots on disk, the
//! floor, the segment count, the torn flag, and the three crash shapes — with
//! one mutant per defect it exists to catch: a reopen that hands a torn slot
//! out again, a rotation that forgets two segments, a rotation that forgets
//! none. The real ring is then driven through every action of that model by
//! the Tier-1 bind in `aterm-agent/tests/conformance_harness.rs`, and each
//! observed transition checked against it, with forged successors as the
//! negative controls.
//!
//! # MEASURED versus assumed
//!
//! MEASURED by the tests, on the machine that ran them: every invariant above
//! against real files in a temp directory, with a torn tail at EVERY cut point
//! of a record, the crash shapes fabricated on disk, and one write failure that
//! cannot be undone (forced by swapping the active handle for a read-only one).
//! The model is checked by the in-process interpreter over its whole bounded
//! space, and additionally by `ty` where that binary is installed. ASSUMED, not
//! measured: anything about power loss (no test cuts power), a write failure
//! that CAN be undone (nothing here can make `write` fail and `ftruncate`
//! succeed), `O_NOFOLLOW` semantics beyond the final component, advisory locks
//! on network filesystems, and every non-Unix code path (compiled for, never
//! run here).
//!
//! Deliberately absent: a lock-free reader for a second process (every open
//! takes the writer lock, so an out-of-process reader asks the process that
//! holds the ring), checksums, compaction, and any notion of time.
//!
//! STATUS: unit-tested; not wired into any shipped verb.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write as _};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The design's per-ledger ceiling (§4.4): the operator WAL's number, not its
/// mechanism.
pub const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Default segment count: one rotation forgets an eighth of the history. The
/// design leaves the count open; this number is this module's choice.
pub const DEFAULT_SEGMENTS: usize = 8;
/// Upper bound on [`RingConfig::segments`], so a directory listing and the
/// in-memory segment table stay small.
pub const MAX_SEGMENTS: usize = 1024;
/// Upper bound on a ring name's length in bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// Lower bound on [`RingConfig::segment_bytes`]: the frame around an EMPTY row
/// is 41 bytes at the largest id, so a segment much smaller than this could
/// hold no useful row at all.
pub const MIN_SEGMENT_BYTES: u64 = 64;

/// The read buffer every segment scan uses.
///
/// MEASURED 2026-09-22, one 8 388 440-byte segment on APFS with a warm page
/// cache: 0.61 ms at the `BufReader` default of 8 KiB (1023 `read(2)`) against
/// 0.28 ms at 64 KiB. A segment is bounded by `max_bytes / segments`, which is
/// 8 MiB for the rm ring, so the default buffer was the wrong size for every
/// scan this module makes.
const SCAN_BUF_BYTES: usize = 64 * 1024;

const SEGMENT_EXT: &str = "jsonl";
const LOCK_EXT: &str = "lock";
const ID_DIGITS: usize = 20;
const FRAME_ID: &str = "{\"id\":";
const FRAME_LEN: &str = ",\"n\":";
const FRAME_ROW: &str = ",\"row\":";

/// The shape of a ring: its byte bound and how many segment files share it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingConfig {
    /// Total bytes the ring may hold across all its segments.
    pub max_bytes: u64,
    /// How many segment files share `max_bytes`; rotation forgets one of them.
    pub segments: usize,
}

impl Default for RingConfig {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            segments: DEFAULT_SEGMENTS,
        }
    }
}

impl RingConfig {
    /// Bytes one segment may hold, which is also the largest framed record the
    /// ring accepts.
    #[must_use]
    pub fn segment_bytes(self) -> u64 {
        match u64::try_from(self.segments) {
            Ok(count) if count > 0 => self.max_bytes / count,
            _ => 0,
        }
    }

    fn validate(self) -> io::Result<()> {
        if !(2..=MAX_SEGMENTS).contains(&self.segments) {
            return Err(invalid(format!(
                "a ring needs 2..={MAX_SEGMENTS} segments, not {}: with one, every rotation \
                 would forget the whole ledger",
                self.segments
            )));
        }
        if self.segment_bytes() < MIN_SEGMENT_BYTES {
            return Err(invalid(format!(
                "max_bytes {} leaves {} bytes in each of {} segments; a segment needs at least \
                 {MIN_SEGMENT_BYTES} to hold one framed row",
                self.max_bytes,
                self.segment_bytes(),
                self.segments
            )));
        }
        Ok(())
    }
}

/// One row read back from the ring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The id [`Ring::append`] returned for this row.
    pub id: u64,
    /// The appended line, byte for byte. Data, never instructions: a row can
    /// carry text an agent or a screen injected.
    pub line: String,
}

#[derive(Debug)]
struct Segment {
    first_id: u64,
    path: PathBuf,
    bytes: u64,
    /// Well-framed rows in the segment; unset until something had to count.
    /// Only the active segment's count ever changes, and only through `&mut`.
    rows: OnceLock<u64>,
}

/// The advisory lock on `<name>.lock`, released by unlocking rather than by
/// closing (see the module docs).
#[derive(Debug)]
struct ProcessLock(File);

impl Drop for ProcessLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A bounded, append-only, lossy ledger. See the module docs for the contract.
#[derive(Debug)]
pub struct Ring {
    dir: PathBuf,
    name: String,
    cfg: RingConfig,
    /// The directory's owner on Unix; every file must share it.
    owner: Option<u32>,
    /// Oldest first; the last one is the active segment.
    segments: Vec<Segment>,
    /// Append handle on the active segment; `None` exactly when there is none.
    active: Option<File>,
    next_id: u64,
    total_bytes: u64,
    /// The active segment does not end in a newline: the next record is
    /// written with one in front of it.
    tail_dirty: bool,
    /// A failed write could not be undone: what the active segment holds is
    /// unknown, so the next append starts a fresh one.
    force_rotate: bool,
    durable: bool,
    // Declared last so every other handle is closed before the lock goes.
    _lock: ProcessLock,
}

impl Ring {
    /// Open (creating what is missing) the ring `name` inside `dir`.
    ///
    /// Reads the newest segment once to find the next id; older segments are
    /// only listed. Enforces the bound on what it finds, oldest first. Refuses
    /// a relative `dir`, a name outside `[a-z0-9_-]{1,64}`, a degenerate
    /// config, anything link-like where a directory or a file should be, and a
    /// ring that is already open ([`io::ErrorKind::ResourceBusy`]).
    pub fn open(dir: &Path, name: &str, cfg: RingConfig) -> io::Result<Self> {
        Self::open_inner(dir, name, cfg, None)
    }

    /// [`Ring::open`], and additionally refuse unless the directory and every
    /// file in the ring belong to `uid`. The caller supplies the uid because
    /// this module holds no `unsafe` to ask for it. Ignored off Unix.
    pub fn open_owned_by(dir: &Path, name: &str, cfg: RingConfig, uid: u32) -> io::Result<Self> {
        Self::open_inner(dir, name, cfg, Some(uid))
    }

    fn open_inner(
        dir: &Path,
        name: &str,
        cfg: RingConfig,
        expected_uid: Option<u32>,
    ) -> io::Result<Self> {
        cfg.validate()?;
        validate_name(name)?;
        let owner = admit_dir(dir, expected_uid)?;
        let lock_path = dir.join(format!("{name}.{LOCK_EXT}"));
        let lock = open_private(&lock_path, Access::Lock, owner)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::ResourceBusy,
                    format!(
                        "ledger ring {name} in {} is already open elsewhere",
                        dir.display()
                    ),
                ));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error),
        }
        let lock = ProcessLock(lock);
        let segments = list_segments(dir, name, owner)?;
        let total_bytes = segments
            .iter()
            .fold(0u64, |sum, segment| sum.saturating_add(segment.bytes));
        let mut ring = Self {
            dir: dir.to_path_buf(),
            name: name.to_owned(),
            cfg,
            owner,
            segments,
            active: None,
            next_id: 1,
            total_bytes,
            tail_dirty: false,
            force_rotate: false,
            durable: true,
            _lock: lock,
        };
        ring.make_room(0)?;
        ring.adopt_active()?;
        Ok(ring)
    }

    /// Append one row and return its id.
    ///
    /// Refuses (`InvalidInput`, no id consumed, nothing written) a line holding
    /// a newline or a NUL, and a line whose framed record is larger than one
    /// segment. Never refuses for being full: the oldest segment makes room.
    /// Any other error is the filesystem's, and then the row may or may not be
    /// in the ring (module docs, "Crashes").
    pub fn append(&mut self, line: &str) -> io::Result<u64> {
        if line.bytes().any(|byte| byte == b'\n' || byte == 0) {
            return Err(invalid(
                "a ledger row is one line: it may hold neither a newline nor a NUL".to_owned(),
            ));
        }
        let id = self.next_id;
        if id == u64::MAX {
            return Err(io::Error::other("ledger ring id space is exhausted"));
        }
        let record = frame(id, line);
        let record_len = byte_len(record.len());
        let segment_bytes = self.cfg.segment_bytes();
        if record_len > segment_bytes {
            return Err(invalid(format!(
                "a framed row of {record_len} bytes does not fit one {segment_bytes}-byte segment"
            )));
        }

        let seal = u64::from(self.tail_dirty);
        let fits = !self.force_rotate
            && self.active.is_some()
            && self.segments.last().is_some_and(|active| {
                active.bytes.saturating_add(seal).saturating_add(record_len) <= segment_bytes
            });
        if !fits {
            self.start_segment(id)?;
        }
        let seal = u64::from(self.tail_dirty);
        let needed = seal.saturating_add(record_len);
        self.make_room(needed)?;

        let mut buffer = Vec::with_capacity(record.len().saturating_add(1));
        if self.tail_dirty {
            buffer.push(b'\n');
        }
        buffer.extend_from_slice(record.as_bytes());
        let (Some(file), Some(segment)) = (self.active.as_mut(), self.segments.last_mut()) else {
            return Err(io::Error::other("ledger ring has no active segment"));
        };
        let before = segment.bytes;
        if let Err(error) = file.write_all(&buffer) {
            if file.set_len(before).is_err() {
                // The fragment stays. On a rescan it burns this slot, so burn
                // it here too, and stop trusting what the segment ends with.
                let now = file
                    .metadata()
                    .map_or_else(|_| before.saturating_add(needed), |meta| meta.len());
                self.total_bytes = self.total_bytes.saturating_sub(before).saturating_add(now);
                segment.bytes = now;
                segment.rows = OnceLock::new();
                self.next_id = id.saturating_add(1);
                self.force_rotate = true;
            }
            return Err(error);
        }
        segment.bytes = before.saturating_add(needed);
        if let Some(rows) = segment.rows.get_mut() {
            *rows = rows.saturating_add(1);
        }
        self.total_bytes = self.total_bytes.saturating_add(needed);
        self.tail_dirty = false;
        self.next_id = id.saturating_add(1);
        if self.durable {
            file.sync_data()?;
        }
        Ok(id)
    }

    /// Rows with `id > after_id`, oldest first, at most `max` of them, across
    /// segments. `after_id = 0` reads from the oldest row the ring still has;
    /// compare a cursor with [`Ring::floor_id`] to learn whether rows were
    /// dropped in between. Junk lines and torn tails are skipped, never
    /// returned.
    ///
    /// COST, stated because a tail read looks cheaper than it is: a segment
    /// that cannot hold a row above `after_id` is skipped whole, but the first
    /// one that can is walked from its start, so the newest 20 rows of a 7 MiB
    /// segment cost the same walk as its newest 4,096 (MEASURED on
    /// [`find_byte`]). That is a DELIBERATE boundary, not an oversight: a row's
    /// slot is decided by the accounting that walk performs — every line burns
    /// one slot, and a well-framed row is emitted only where its id is at or
    /// above the running cursor — so a reader that seeked into the middle of a
    /// segment could return a row the forward walk would have rejected as
    /// out of order. This ledger is an audit record, and the tie breaks to the
    /// exact answer. What was taken instead is the constant: the walk itself
    /// is now word-at-a-time, three times cheaper for the identical result.
    pub fn read_since(&self, after_id: u64, max: usize) -> io::Result<Vec<Entry>> {
        let mut out = Vec::new();
        if max == 0 {
            return Ok(out);
        }
        let line_cap = self.line_cap();
        for (index, segment) in self.segments.iter().enumerate() {
            let below = self.id_ceiling(index);
            if below.saturating_sub(1) <= after_id {
                continue;
            }
            let file = open_private(&segment.path, Access::Read, self.owner)?;
            let end = scan(
                BufReader::with_capacity(SCAN_BUF_BYTES, &file),
                segment.first_id,
                below,
                line_cap,
                &mut |id, row| {
                    if id > after_id {
                        out.push(Entry {
                            id,
                            line: row.to_owned(),
                        });
                        if out.len() >= max {
                            return ControlFlow::Break(());
                        }
                    }
                    ControlFlow::Continue(())
                },
            )?;
            if end.stopped {
                break;
            }
        }
        Ok(out)
    }

    /// How many rows the ring holds: well-framed rows, never junk lines or a
    /// torn tail. The first call after an open reads the segments this process
    /// did not write and caches each count; later calls are arithmetic.
    pub fn len(&self) -> io::Result<u64> {
        let line_cap = self.line_cap();
        let mut total = 0u64;
        for (index, segment) in self.segments.iter().enumerate() {
            let rows = match segment.rows.get() {
                Some(rows) => *rows,
                None => {
                    let file = open_private(&segment.path, Access::Read, self.owner)?;
                    let end = scan(
                        BufReader::with_capacity(SCAN_BUF_BYTES, &file),
                        segment.first_id,
                        self.id_ceiling(index),
                        line_cap,
                        &mut |_, _| ControlFlow::Continue(()),
                    )?;
                    *segment.rows.get_or_init(|| end.rows)
                }
            };
            total = total.saturating_add(rows);
        }
        Ok(total)
    }

    /// Whether the ring holds no rows. Same cost as [`Ring::len`].
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Whether the ring has EVER had a row that is still in it — the same
    /// question as `len() > 0`, answered from the two ids already in memory
    /// after [`Ring::open`] instead of by scanning every segment.
    ///
    /// The difference is not academic: `len()` reads every byte of every
    /// segment this process did not write, and a caller that only wants the
    /// boolean (is there a hook row at all?) was paying 72 MiB of reads on a
    /// full ring to learn it.
    ///
    /// EXACT for the question asked, with one stated caveat: an id is issued
    /// by [`Ring::append`] before the row is framed on disk, so a ring whose
    /// only row is a torn tail answers `true` here and `0` from
    /// [`Ring::len`]. That tie breaks toward "something was written", which
    /// is the safe answer for a presence check.
    #[must_use]
    pub fn has_rows(&self) -> bool {
        self.next_id > self.floor_id()
    }

    /// Bytes on disk across all segments, junk included; the lock file is not
    /// counted.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The id the next successful append returns.
    #[must_use]
    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// The first id slot the ring still holds (the oldest segment's name), or
    /// [`Ring::next_id`] when it holds nothing. Rows below it were dropped.
    #[must_use]
    pub fn floor_id(&self) -> u64 {
        self.segments
            .first()
            .map_or(self.next_id, |segment| segment.first_id)
    }

    /// Segment files currently on disk.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The config this ring was opened with.
    #[must_use]
    pub fn config(&self) -> RingConfig {
        self.cfg
    }

    /// Turn per-append synchronization on or off (module docs, "Durability").
    pub fn set_durable(&mut self, durable: bool) {
        self.durable = durable;
    }

    /// Synchronize the active segment and the directory now — what a
    /// non-durable ring calls before it publishes an id outside the process.
    pub fn sync(&self) -> io::Result<()> {
        if let Some(file) = &self.active {
            file.sync_data()?;
        }
        sync_dir(&self.dir)
    }

    /// Ids in segment `index` are below this: the next segment's name, or the
    /// next id for the active one.
    fn id_ceiling(&self, index: usize) -> u64 {
        self.segments
            .get(index.saturating_add(1))
            .map_or(self.next_id, |next| next.first_id)
    }

    /// The longest line a scan keeps in memory: `max_bytes`, the whole ring's
    /// bound, and deliberately NOT this config's `segment_bytes()`.
    ///
    /// A segment file this config wrote holds no line longer than one of its
    /// own segments, so the buffer is already bounded by the file — but a
    /// ring written with FEWER segments had bigger ones, and a row between
    /// this config's `segment_bytes()` and `max_bytes` is a row that ring
    /// legitimately holds. Capping at `segment_bytes()` would read those rows
    /// as junk and silently drop them, where the module doc ("The bound, and
    /// what rotation drops") says the junk threshold is the new `max_bytes`.
    /// The cut that loses no legitimate row is the one taken here.
    fn line_cap(&self) -> usize {
        usize::try_from(self.cfg.max_bytes).unwrap_or(usize::MAX)
    }

    /// Unlink oldest segments until at most `cfg.segments` remain and `needed`
    /// more bytes fit under `cfg.max_bytes`. Never touches the newest segment.
    fn make_room(&mut self, needed: u64) -> io::Result<()> {
        while self.segments.len() > self.cfg.segments
            || (self.segments.len() > 1
                && self.total_bytes.saturating_add(needed) > self.cfg.max_bytes)
        {
            let oldest = self.segments.remove(0);
            match fs::remove_file(&oldest.path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    self.segments.insert(0, oldest);
                    return Err(error);
                }
            }
            self.total_bytes = self.total_bytes.saturating_sub(oldest.bytes);
            if self.durable {
                // Best effort: an unlink the disk forgets comes back as one
                // file too many, which the next open trims again.
                let _ = sync_dir(&self.dir);
            }
        }
        Ok(())
    }

    /// Adopt the newest segment on disk as the active one and read the next id
    /// out of it. Writes nothing.
    fn adopt_active(&mut self) -> io::Result<()> {
        let line_cap = self.line_cap();
        let Some(segment) = self.segments.last_mut() else {
            return Ok(());
        };
        let file = open_private(&segment.path, Access::Append, self.owner)?;
        let end = scan(
            BufReader::with_capacity(SCAN_BUF_BYTES, &file),
            segment.first_id,
            u64::MAX,
            line_cap,
            &mut |_, _| ControlFlow::Continue(()),
        )?;
        let bytes = file.metadata()?.len();
        self.total_bytes = self
            .total_bytes
            .saturating_sub(segment.bytes)
            .saturating_add(bytes);
        segment.bytes = bytes;
        segment.rows = OnceLock::from(end.rows);
        self.next_id = end.cursor;
        self.tail_dirty = !end.terminated;
        self.active = Some(file);
        Ok(())
    }

    /// Start the segment whose first slot is `first_id` and make it active.
    fn start_segment(&mut self, first_id: u64) -> io::Result<()> {
        let path = self.dir.join(format!(
            "{}.{first_id:0ID_DIGITS$}.{SEGMENT_EXT}",
            self.name
        ));
        let file = open_private(&path, Access::Append, self.owner)?;
        if file.metadata()?.len() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "{} already holds rows this ring did not list",
                    path.display()
                ),
            ));
        }
        // Bookkeeping BEFORE the sync: if the sync fails the file still exists,
        // and a ring that forgot it would collide with it on the next try.
        self.segments.push(Segment {
            first_id,
            path,
            bytes: 0,
            rows: OnceLock::from(0),
        });
        self.tail_dirty = false;
        self.force_rotate = false;
        let file = self.active.insert(file);
        if self.durable {
            file.sync_all()?;
            sync_dir(&self.dir)?;
        }
        Ok(())
    }
}

/// The framed record for one row, newline included.
fn frame(id: u64, line: &str) -> String {
    format!(
        "{FRAME_ID}{id}{FRAME_LEN}{}{FRAME_ROW}{line}}}\n",
        line.len()
    )
}

/// The inverse of [`frame`] for one line WITHOUT its newline; `None` for
/// anything [`frame`] could not have written.
fn parse_record(line: &[u8]) -> Option<(u64, &str)> {
    let text = std::str::from_utf8(line).ok()?;
    let rest = text.strip_prefix(FRAME_ID)?;
    let (id, rest) = take_decimal(rest)?;
    let rest = rest.strip_prefix(FRAME_LEN)?;
    let (declared, rest) = take_decimal(rest)?;
    let rest = rest.strip_prefix(FRAME_ROW)?;
    let row = rest.strip_suffix('}')?;
    if byte_len(row.len()) != declared || find_byte(row.as_bytes(), 0).is_some() {
        return None;
    }
    Some((id, row))
}

/// A canonical decimal `u64` prefix (no sign, no leading zero) and the rest.
fn take_decimal(text: &str) -> Option<(u64, &str)> {
    let digits = text
        .bytes()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(text.len());
    let (number, rest) = text.split_at_checked(digits)?;
    if number.is_empty() || (number.len() > 1 && number.starts_with('0')) {
        return None;
    }
    Some((number.parse().ok()?, rest))
}

fn byte_len(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

struct ScanEnd {
    /// The slot after the last line: the next id, for the active segment.
    cursor: u64,
    rows: u64,
    /// The bytes ended in a newline (or there were none).
    terminated: bool,
    /// The visitor asked to stop; `cursor`, `rows`, `terminated` are partial.
    stopped: bool,
}

/// Walk a segment's lines, giving each well-framed row whose id is in
/// `cursor..below` to `visit`. Every other line — junk, overlong, an id out of
/// order, an unterminated tail — burns one slot and is skipped.
///
/// A line that lies WHOLLY inside one buffer chunk — every line of a
/// well-formed ring, since a row is a few hundred bytes and the buffer is
/// [`SCAN_BUF_BYTES`] — is settled where it lies, with no copy into `line`.
/// That path exists because a tail read pays for the whole segment: MEASURED
/// 2026-09-22 on a 7,228,894-byte / 40,000-row ring, `read_since` asking for
/// the newest 20 rows cost 4.56 ms and asking for 4,096 cost 4.60 ms — all of
/// it the walk, none of it the rows returned. Copying each line first was
/// pure overhead on that walk. The accumulating path stays for the only case
/// that needs it: a line split across two chunks.
fn scan<R: BufRead>(
    mut reader: R,
    first_id: u64,
    below: u64,
    line_cap: usize,
    visit: &mut dyn FnMut(u64, &str) -> ControlFlow<()>,
) -> io::Result<ScanEnd> {
    let mut end = ScanEnd {
        cursor: first_id,
        rows: 0,
        terminated: true,
        stopped: false,
    };
    let mut line: Vec<u8> = Vec::new();
    let mut overlong = false;
    loop {
        let (newline, used, settled) = {
            let chunk = match reader.fill_buf() {
                Ok(chunk) => chunk,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if chunk.is_empty() {
                break;
            }
            let newline = find_byte(chunk, b'\n');
            let take = newline.unwrap_or(chunk.len());
            let used = take.saturating_add(usize::from(newline.is_some()));
            let whole = chunk.get(..take).unwrap_or(&[]);
            if newline.is_some() && line.is_empty() && !overlong {
                // The whole line is in front of us: settle it in place.
                let over = take > line_cap;
                settle(&mut end, whole, over, below, visit);
                (true, used, true)
            } else {
                if !overlong {
                    if line.len().saturating_add(take) > line_cap {
                        overlong = true;
                        line.clear();
                    } else {
                        line.extend_from_slice(whole);
                    }
                }
                (newline.is_some(), used, false)
            }
        };
        reader.consume(used);
        if newline {
            if !settled {
                settle(&mut end, &line, overlong, below, visit);
            }
            if end.stopped {
                return Ok(end);
            }
            line.clear();
            overlong = false;
            end.terminated = true;
        } else {
            end.terminated = false;
        }
    }
    if !end.terminated {
        settle(&mut end, &line, overlong, below, visit);
    }
    Ok(end)
}

/// The offset of the first `needle` byte in `bytes`, or `None` — a word at a
/// time.
///
/// The obvious `bytes.iter().position(|b| *b == b'\n')` walks ONE BYTE at a
/// time, and on a ledger segment that byte walk IS the scan's cost: MEASURED
/// 2026-09-22 on a 7,228,894-byte / 40,000-row segment, reading the bytes took
/// 0.37 ms and splitting them into lines that way took 3.1 ms. The framing
/// parser's "a row may never contain NUL" check is the same walk again over
/// the same bytes.
///
/// This reads a `usize` at a time and tests the whole word for a zero byte
/// (`x - 0x01…01 & !x & 0x80…80`, the standard trick), then falls back to the
/// byte walk inside the ONE word that holds the hit and over the trailing
/// bytes. Safe Rust, no dependency, and the answer is identical — which the
/// test beside it checks against the byte walk at every offset of every
/// length, for a needle that is present, absent, and repeated.
///
/// MEASURED on that segment, same binary shape, best of 7 in release:
/// `read_since` for the newest 20 rows 4.59 ms → 1.37 ms, for 4,096 rows
/// 4.59 ms → 1.40 ms, and `Ring::open` — which scans the active segment to
/// adopt it, so EVERY hook invocation pays it — 4.60 ms → 1.39 ms.
fn find_byte(bytes: &[u8], needle: u8) -> Option<usize> {
    const LANES: usize = size_of::<usize>();
    /// `0x0101…01`: one in every byte lane.
    const ONES: usize = usize::MAX / 255;
    /// `0x8080…80`: the high bit of every byte lane.
    const HIGH: usize = ONES << 7;
    let spread = ONES.wrapping_mul(needle as usize);
    let mut at = 0;
    while at + LANES <= bytes.len() {
        let Some(word) = bytes
            .get(at..at + LANES)
            .and_then(|lane| <[u8; LANES]>::try_from(lane).ok())
            .map(usize::from_ne_bytes)
        else {
            break;
        };
        // Zero in every lane that held the needle; the test below is true
        // exactly when some lane is zero.
        let zeroed = word ^ spread;
        if zeroed.wrapping_sub(ONES) & !zeroed & HIGH != 0 {
            return bytes
                .get(at..at + LANES)
                .and_then(|lane| lane.iter().position(|byte| *byte == needle))
                .map(|hit| at + hit);
        }
        at += LANES;
    }
    bytes
        .get(at..)
        .and_then(|rest| rest.iter().position(|byte| *byte == needle))
        .map(|hit| at + hit)
}

fn settle(
    end: &mut ScanEnd,
    line: &[u8],
    overlong: bool,
    below: u64,
    visit: &mut dyn FnMut(u64, &str) -> ControlFlow<()>,
) {
    let row = if overlong { None } else { parse_record(line) };
    match row {
        Some((id, text)) if id >= end.cursor && id < below => {
            end.cursor = id.saturating_add(1);
            end.rows = end.rows.saturating_add(1);
            if visit(id, text).is_break() {
                end.stopped = true;
            }
        }
        _ => end.cursor = end.cursor.saturating_add(1),
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    let well_formed = !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        });
    if well_formed {
        Ok(())
    } else {
        Err(invalid(format!(
            "ring name {name:?} must be 1..={MAX_NAME_BYTES} bytes of [a-z0-9_-]"
        )))
    }
}

/// `<name>.<20 digits>.jsonl` → the first id, for exactly that shape.
fn parse_segment_name(file_name: &str, ring: &str) -> Option<u64> {
    let digits = file_name
        .strip_prefix(ring)?
        .strip_prefix('.')?
        .strip_suffix(SEGMENT_EXT)?
        .strip_suffix('.')?;
    if digits.len() != ID_DIGITS || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|id| *id >= 1)
}

fn list_segments(dir: &Path, name: &str, owner: Option<u32>) -> io::Result<Vec<Segment>> {
    let mut segments = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(first_id) = file_name
            .to_str()
            .and_then(|text| parse_segment_name(text, name))
        else {
            continue;
        };
        let path = entry.path();
        // Admitted through a descriptor, like every other file: regular, not a
        // link, ours, 0600.
        let file = open_private(&path, Access::Read, owner)?;
        segments.push(Segment {
            first_id,
            path,
            bytes: file.metadata()?.len(),
            rows: OnceLock::new(),
        });
    }
    segments.sort_by_key(|segment| segment.first_id);
    Ok(segments)
}

fn invalid(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn denied(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

fn is_link_like(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        return metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0;
    }
    #[cfg(not(windows))]
    false
}

/// Create what is missing of `dir`, one 0700 component at a time, below the
/// closest existing ancestor — which must be a real directory. Links ABOVE that
/// ancestor are not policed (`/var` and `/tmp` are system aliases on macOS),
/// the same line `operator.rs` draws.
fn create_missing(dir: &Path) -> io::Result<()> {
    let mut anchor = dir.to_path_buf();
    let mut missing: Vec<OsString> = Vec::new();
    loop {
        match fs::symlink_metadata(&anchor) {
            Ok(metadata) => {
                if is_link_like(&metadata) || !metadata.file_type().is_dir() {
                    return Err(denied(format!(
                        "{} is not a real directory",
                        anchor.display()
                    )));
                }
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = anchor.file_name().map(OsString::from);
                let Some(name) = name else {
                    return Err(invalid(format!(
                        "{} has no existing ancestor to create it under",
                        dir.display()
                    )));
                };
                missing.push(name);
                if !anchor.pop() {
                    return Err(invalid(format!("{} has no parent", dir.display())));
                }
            }
            Err(error) => return Err(error),
        }
    }
    for name in missing.into_iter().rev() {
        let parent = anchor.clone();
        anchor.push(name);
        create_private_dir(&anchor)?;
        // Best effort: a directory a power cut forgets is created again.
        let _ = sync_dir(&parent);
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    fs::DirBuilder::new().mode(0o700).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// Admit `dir` as the ring's private directory and return its owner (Unix).
#[cfg(unix)]
fn admit_dir(dir: &Path, expected_uid: Option<u32>) -> io::Result<Option<u32>> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !dir.is_absolute() {
        return Err(invalid(format!(
            "ledger directory {} must be absolute",
            dir.display()
        )));
    }
    // `create_missing` has refused a link at `dir` itself before this follows
    // the path to force the mode; forcing it succeeds only for the owner (or
    // uid 0).
    create_missing(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let metadata = fs::symlink_metadata(dir)?;
    if is_link_like(&metadata) || !metadata.file_type().is_dir() || metadata.mode() & 0o777 != 0o700
    {
        return Err(denied(format!(
            "{} must be a real directory with mode 0700",
            dir.display()
        )));
    }
    if let Some(uid) = expected_uid
        && metadata.uid() != uid
    {
        return Err(denied(format!(
            "{} belongs to uid {}, not uid {uid}",
            dir.display(),
            metadata.uid()
        )));
    }
    Ok(Some(metadata.uid()))
}

#[cfg(not(unix))]
fn admit_dir(dir: &Path, _expected_uid: Option<u32>) -> io::Result<Option<u32>> {
    if !dir.is_absolute() {
        return Err(invalid(format!(
            "ledger directory {} must be absolute",
            dir.display()
        )));
    }
    create_missing(dir)?;
    let metadata = fs::symlink_metadata(dir)?;
    if is_link_like(&metadata) || !metadata.file_type().is_dir() {
        return Err(denied(format!("{} is not a real directory", dir.display())));
    }
    Ok(None)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    /// The lock file: created if missing, never read or written.
    Lock,
    /// A segment to append to (and scan once): created if missing.
    Append,
    /// A segment to read: must exist.
    Read,
}

/// Open one of the ring's files the way `operator.rs` opens its private state:
/// never through a link, a regular file, forced to 0600, owned like the
/// directory.
fn open_private(path: &Path, access: Access, owner: Option<u32>) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_link_like(&metadata) => {
            return Err(denied(format!(
                "{} is a symlink or reparse point",
                path.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    match access {
        Access::Lock => options.create(true).read(true).write(true).truncate(false),
        Access::Append => options.create(true).read(true).append(true),
        Access::Read => options.read(true),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(denied(format!("{} is not a regular file", path.display())));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        let metadata = file.metadata()?;
        if metadata.mode() & 0o777 != 0o600 || owner.is_some_and(|uid| metadata.uid() != uid) {
            return Err(denied(format!(
                "{} must be a 0600 regular file owned like its directory",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = owner;
    Ok(file)
}

#[cfg(unix)]
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_path: &Path) -> io::Result<()> {
    // `std` cannot open a Windows directory to flush it; directory-entry
    // durability there is the filesystem's, and is not claimed.
    Ok(())
}

#[cfg(test)]
#[path = "ring_tests.rs"]
mod tests;
