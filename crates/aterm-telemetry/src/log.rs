// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LOCAL LOG: `<state>/telemetry/rows.log`, appended to; once an append would take it past
//! [`Limits::file_bytes`] it becomes `rows.1.log` (each older file moving up one, the oldest
//! past [`Limits::files`] removed) and a new `rows.log` opens with the host's facts. So the log
//! never holds more than about `file_bytes * files` bytes.
//!
//! Every append and rotation from any process of this account goes under one exclusive lock
//! (`.lock`, [`aterm_uds::file_lock::lock_exclusive_until`]), one `write` a batch and one its
//! gap line. A write that fails (a disk that filled) is cut back to the length the file had,
//! so it leaves none of its bytes. A crash mid-write leaves a line with no line feed: the next
//! append ends it first, so the fragment is a line of its own whose checksum fails, and every
//! reader skips it as torn; a line cut only of its line feed is whole, and reads as written.
//! Nothing is flushed to disk: a power loss may take the newest rows, which the reader shows
//! as missing, never as wrong.
//!
//! A writer that could not append a row leaves a loss record in the folder
//! (`lost-<writer>-<rows>`, [`mark_lost`]), with no lock; the next append of any writer, under
//! the lock, takes each one it can remove and says their rows in a `kind=gap` line before its
//! batch. So a row lost by a process that writes no other (a build's holder) is still said.
//! The window's writer says its own losses in the gap line before its next batch.
//!
//! The folder is the account's own (`0700`) and each file in it (`0600`): rows say what this
//! account ran, and no other account reads them.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::row::{LineSince, Row, parse_line_since};

/// The log's bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The size past which `rows.log` rotates.
    pub file_bytes: u64,
    /// How many files the log keeps, `rows.log` included (at least 1).
    pub files: u32,
}

impl Limits {
    /// 4 MiB a file, four files: 16 MiB, about 55,000 rows at the 300 bytes a row the first
    /// live run measured (2452 bytes for eight rows and the host line). How long that reaches
    /// back is the Mac's own rate, not measured yet: the reader's `oldest=` says it.
    pub const DEFAULT: Self = Self {
        file_bytes: 4 << 20,
        files: 4,
    };
}

/// How long an append waits for another writer's lock before it gives up on its batch, unless
/// the log was made [`Log::waiting`] some other time.
pub const LOCK_WAIT: Duration = Duration::from_secs(2);

/// The log's folder under the aterm state directory `state`.
#[must_use]
pub fn dir_in(state: &Path) -> PathBuf {
    state.join("telemetry")
}

/// The log's folder for this process's state directory; `None` where it has none.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    aterm_types::dirs::state_dir().map(|s| dir_in(&s))
}

/// File `n` of the log: `rows.log` for 0, `rows.<n>.log` after.
#[must_use]
pub fn file_path(dir: &Path, n: u32) -> PathBuf {
    if n == 0 {
        dir.join("rows.log")
    } else {
        dir.join(format!("rows.{n}.log"))
    }
}

/// What an append did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appended {
    /// Lines written.
    pub lines: usize,
    /// It rotated first.
    pub rotated: bool,
    /// It ended a torn line first.
    pub mended: bool,
}

/// The log in one folder.
#[derive(Clone, Debug)]
pub struct Log {
    dir: PathBuf,
    limits: Limits,
    /// The host's facts, the line each new file opens with.
    facts: crate::facts::HostFacts,
    /// How long an append waits for the lock.
    lock_wait: Duration,
    /// The bytes its writes may still put before one is cut short, as a disk that fills does
    /// ([`Self::cut_after`]); `None` for a log no test cuts.
    #[cfg(test)]
    cut: Option<std::sync::Arc<std::sync::atomic::AtomicU64>>,
}

#[cfg(test)]
thread_local! {
    /// How many times this thread's appends waited for the lock ([`locks_waited`]).
    static LOCKS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How many times this thread's appends have waited for the lock.
#[cfg(test)]
pub(crate) fn locks_waited() -> u64 {
    LOCKS.with(std::cell::Cell::get)
}

/// The lock file held for one append, released by `LOCK_UN` before it closes.
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

impl Log {
    /// The log in `dir`, each new file opening with a line of `facts`.
    #[must_use]
    pub fn new(dir: PathBuf, limits: Limits, facts: crate::facts::HostFacts) -> Self {
        Self {
            dir,
            limits: Limits {
                files: limits.files.max(1),
                ..limits
            },
            facts,
            lock_wait: LOCK_WAIT,
            #[cfg(test)]
            cut: None,
        }
    }

    /// This log, its writes cut short once they would put more than `left` holds (each write
    /// takes what it put): the write that would pass it puts what fits and fails, as one does on
    /// a disk that fills.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn cut_after(mut self, left: std::sync::Arc<std::sync::atomic::AtomicU64>) -> Self {
        self.cut = Some(left);
        self
    }

    /// Write all of `bytes` to `file`, `at` bytes long, with the lock held: a write that fails
    /// is cut back to `at`, so it leaves none of its bytes for a reader to take as a line (a
    /// gap line cut only of its line feed is whole) or as a row the caller counts lost. Where the
    /// cut itself fails, what the write put stays, as after a crash mid-write.
    fn put(&self, file: &mut File, at: u64, bytes: &[u8]) -> io::Result<()> {
        let wrote = self.put_all(file, bytes);
        if wrote.is_err() {
            let _ = file.set_len(at);
        }
        wrote
    }

    fn put_all(&self, file: &mut File, bytes: &[u8]) -> io::Result<()> {
        #[cfg(test)]
        if let Some(left) = &self.cut {
            use std::sync::atomic::Ordering::SeqCst;
            let room = left.load(SeqCst);
            let fits = usize::try_from(room).unwrap_or(usize::MAX).min(bytes.len());
            left.store(room - fits as u64, SeqCst);
            file.write_all(&bytes[..fits])?;
            if fits < bytes.len() {
                return Err(io::ErrorKind::StorageFull.into());
            }
            return Ok(());
        }
        file.write_all(bytes)
    }

    /// This log, its appends waiting at most `wait` for another writer's lock.
    #[must_use]
    pub const fn waiting(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    /// Its folder.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Its bounds.
    #[must_use]
    pub const fn limits(&self) -> Limits {
        self.limits
    }

    fn lock(&self) -> io::Result<Held> {
        #[cfg(test)]
        LOCKS.with(|n| n.set(n.get() + 1));
        own_dir(&self.dir)?;
        let file = own_file(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false),
        )
        .open(self.dir.join(".lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry lock is not a regular file",
            ));
        }
        aterm_uds::file_lock::lock_exclusive_until(
            &file,
            Instant::now() + self.lock_wait,
            "another writer holds the telemetry log",
        )?;
        Ok(Held(file))
    }

    /// Append `lines` (each sealed, ending in a line feed) as one write, rotating first when
    /// they would take `rows.log` past its bound, after a gap line for the folder's loss records
    /// ([`mark_lost`]).
    ///
    /// # Errors
    /// The lock was not had within its wait ([`LOCK_WAIT`] unless [`Self::waiting`] set
    /// another), or a file could not be opened, renamed or written.
    pub fn append(&self, lines: &[String]) -> io::Result<Appended> {
        self.append_saying(0, lines).map_err(|(e, _)| e)
    }

    /// [`Self::append`] under one wait for the lock, its gap line also saying `said` rows the
    /// caller lost and kept no record of (a direct writer's own count, or the window's queue's).
    /// The gap line is a write of its own, so a batch that fails after it never leaves its rows
    /// to be said again. `Err`
    /// says whether the gap line was written: where it was not, the folder's records it took are
    /// left again as one, and the caller's `said` rows are still its own to say.
    ///
    /// # Errors
    /// As [`Self::append`].
    pub fn append_saying(
        &self,
        said: u64,
        lines: &[String],
    ) -> Result<Appended, (io::Error, bool)> {
        if lines.is_empty() && said == 0 {
            return Ok(Appended::default());
        }
        let _held = self.lock().map_err(|e| (e, false))?;
        let taken = take_lost(&self.dir);
        let mut gap = Appended::default();
        if let Some(rows) = taken.checked_add(said).filter(|&rows| rows > 0) {
            let line = [crate::row::gap_line(crate::now_ms(), rows)];
            gap = match self.append_held(&line) {
                Ok(done) => done,
                Err(e) => {
                    if taken > 0 {
                        // Taken off the folder and not said: left again as one record.
                        let _ = mark_lost(&self.dir, &fresh_writer("refold"), None, taken);
                    }
                    return Err((e, false));
                }
            };
        }
        let done = self.append_held(lines).map_err(|e| (e, true))?;
        Ok(Appended {
            lines: done.lines,
            rotated: gap.rotated || done.rotated,
            mended: gap.mended || done.mended,
        })
    }

    /// [`Self::append`] with the lock held.
    fn append_held(&self, lines: &[String]) -> io::Result<Appended> {
        let mut done = Appended {
            lines: lines.len(),
            ..Appended::default()
        };
        let batch: usize = lines.iter().map(String::len).sum();
        let (mut file, mut len) = self.open_current()?;
        if len > 0 && !ends_in_line_feed(&mut file, len)? {
            self.put(&mut file, len, b"\n")?;
            len += 1;
            done.mended = true;
        }
        if len > 0 && len.saturating_add(batch as u64) > self.limits.file_bytes {
            drop(file);
            self.rotate()?;
            done.rotated = true;
            (file, len) = self.open_current()?;
        }
        if len > 0 && lines.len() == 1 {
            self.put(&mut file, len, lines[0].as_bytes())?;
            return Ok(done);
        }
        let mut out = String::with_capacity(if len == 0 { batch + 256 } else { batch });
        if len == 0 {
            self.facts.write_line_into(crate::now_ms(), &mut out);
        }
        for line in lines {
            out.push_str(line);
        }
        self.put(&mut file, len, out.as_bytes())?;
        Ok(done)
    }

    fn open_current(&self) -> io::Result<(File, u64)> {
        let file = own_file(OpenOptions::new().read(true).append(true).create(true))
            .open(file_path(&self.dir, 0))?;
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "telemetry log is not a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
            if meta.mode() & 0o777 != 0o600 {
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok((file, meta.len()))
    }

    /// Move each file up one, the oldest past the bound removed.
    fn rotate(&self) -> io::Result<()> {
        let last = self.limits.files - 1;
        if last == 0 {
            return gone_ok(std::fs::remove_file(file_path(&self.dir, 0)));
        }
        gone_ok(std::fs::remove_file(file_path(&self.dir, last)))?;
        for n in (0..last).rev() {
            gone_ok(std::fs::rename(
                file_path(&self.dir, n),
                file_path(&self.dir, n + 1),
            ))?;
        }
        Ok(())
    }
}

/// The prefix of a loss record's name: `lost-<writer>-<rows>`.
const LOST_PREFIX: &str = "lost-";

/// A writer name for [`mark_lost`] no other writer of this account has: `<what>.<pid>.<ns>.<n>`,
/// this process's id, the time it was made and a count of the names this process made.
#[must_use]
pub fn fresh_writer(what: &str) -> String {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let what: String = what.chars().filter(char::is_ascii_alphanumeric).collect();
    format!("{what}.{}.{ns}.{made}", std::process::id())
}

/// Record that `writer` (a [`fresh_writer`] name) lost `rows` rows of the log in `dir`, with no
/// lock, for the next append to say. `was` is the count the writer's record holds where it left
/// one: that record is renamed to the sum, and where it is gone (an append took it) a new one is
/// made, so a record is taken whole or not at all. The new count, or the error that left none.
///
/// # Errors
/// The folder could not be made, or no record could be renamed or made.
pub fn mark_lost(dir: &Path, writer: &str, was: Option<u64>, rows: u64) -> io::Result<u64> {
    let name = |n: u64| dir.join(format!("{LOST_PREFIX}{writer}-{n}"));
    if let Some(was) = was {
        let now = was.saturating_add(rows);
        match std::fs::rename(name(was), name(now)) {
            Ok(()) => return Ok(now),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    own_dir(dir)?;
    own_file(OpenOptions::new().write(true).create_new(true)).open(name(rows))?;
    Ok(rows)
}

/// With the log's lock held: remove every loss record in `dir` ([`mark_lost`]) and return the
/// rows they hold. A record renamed meanwhile is not removed here and not counted: the next
/// append takes it under its new name.
fn take_lost(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut rows = 0u64;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(count) = name.to_str().and_then(lost_rows) else {
            continue;
        };
        if std::fs::remove_file(entry.path()).is_ok() {
            rows = rows.saturating_add(count);
        }
    }
    rows
}

/// The rows a loss record's file name holds: `lost-<writer>-<rows>`, at most `u32::MAX` (a name
/// that says more is no record of this log's: left alone and not counted).
fn lost_rows(name: &str) -> Option<u64> {
    let (writer, rows) = name.strip_prefix(LOST_PREFIX)?.rsplit_once('-')?;
    (!writer.is_empty() && !rows.starts_with('+'))
        .then(|| rows.parse::<u32>().ok())
        .flatten()
        .map(u64::from)
}

/// Make `dir` (and any folder above it that is missing) the account's own: `0700` on Unix.
fn own_dir(dir: &Path) -> io::Result<()> {
    aterm_types::fs_restricted::ensure_private_dir(dir)
}

/// `opts`, creating a file the account's own: `0600` on Unix.
fn own_file(opts: &mut OpenOptions) -> &mut OpenOptions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(aterm_uds::atomic_file::LOCK_OPEN_FLAGS);
    }
    opts
}

fn gone_ok(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn ends_in_line_feed(file: &mut File, len: u64) -> io::Result<bool> {
    let mut last = [0u8; 1];
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        file.read_exact_at(&mut last, len - 1)?;
    }
    #[cfg(not(unix))]
    {
        use std::io::{Read as _, Seek as _, SeekFrom};
        file.seek(SeekFrom::Start(len - 1))?;
        file.read_exact(&mut last)?;
    }
    Ok(last[0] == b'\n')
}

/// What a read of the log found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// The rows at or after `since`, oldest first.
    pub rows: Vec<Row>,
    /// The host lines read (every file's, whatever its time).
    pub hosts: Vec<crate::facts::HostLine>,
    /// Rows the writers lost at or after `since` (the gap lines' sum).
    pub dropped: u64,
    /// Lines skipped as torn (a crash mid-write, a checksum that fails, another schema).
    pub torn: u64,
    /// Files read.
    pub files: u32,
    /// Bytes read.
    pub bytes: u64,
    /// The oldest line's time, unix ms: how far back the log reaches.
    pub oldest_ms: Option<u64>,
}

/// Read every file of the log in `dir` (at most `files` of them), oldest first, keeping the rows
/// that ended at or after `since_ms`. A file that is gone (rotated away meanwhile) is skipped.
///
/// # Errors
/// A file that exists could not be read.
pub fn read(dir: &Path, files: u32, since_ms: Option<u64>) -> io::Result<Reading> {
    let mut out = Reading::default();
    let since = since_ms.unwrap_or(0);
    for n in (0..files.max(1)).rev() {
        let text = match aterm_uds::atomic_file::read_regular(&file_path(dir, n), 64 << 20) {
            Ok((bytes, _)) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        };
        out.files += 1;
        out.bytes += text.len() as u64;
        for raw in text.split(|b| *b == b'\n') {
            if raw.is_empty() {
                continue;
            }
            let Some(line) = std::str::from_utf8(raw)
                .ok()
                .and_then(|s| parse_line_since(s, since))
            else {
                out.torn += 1;
                continue;
            };
            match line {
                LineSince::Row(row) => {
                    out.oldest_ms = Some(out.oldest_ms.map_or(row.at_ms, |o| o.min(row.at_ms)));
                    out.rows.push(row);
                }
                LineSince::OldRow(at_ms) => {
                    out.oldest_ms = Some(out.oldest_ms.map_or(at_ms, |o| o.min(at_ms)));
                }
                LineSince::Host(host) => {
                    out.oldest_ms = Some(out.oldest_ms.map_or(host.at_ms, |o| o.min(host.at_ms)));
                    out.hosts.push(host);
                }
                LineSince::Gap { at_ms, dropped } => {
                    if at_ms >= since {
                        out.dropped = out.dropped.saturating_add(dropped);
                    }
                }
            }
        }
    }
    Ok(out)
}
