// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE LOCAL LOG: `<state>/telemetry/rows.log`, appended to; once an append would take it past
//! [`Limits::file_bytes`] it becomes `rows.1.log` (each older file moving up one, the oldest
//! past [`Limits::files`] removed) and a new `rows.log` opens with the host's facts. So the log
//! never holds more than about `file_bytes * files` bytes.
//!
//! Every append and rotation from any process of this account goes under one exclusive lock
//! (`.lock`, [`aterm_uds::file_lock::lock_exclusive_until`]), one `write` a batch. A crash
//! mid-write leaves a line with no line feed: the next append ends it first, so the fragment
//! is a line of its own whose checksum fails, and every reader skips it as torn. Nothing is
//! flushed to disk: a power loss may take the newest rows, which the reader shows as missing,
//! never as wrong. The folder is the account's own (`0700`) and each file in it (`0600`): rows
//! say what this account ran, and no other account reads them.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::row::{Line, Row, parse_line};

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
        }
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
    /// they would take `rows.log` past its bound.
    ///
    /// # Errors
    /// The lock was not had within its wait ([`LOCK_WAIT`] unless [`Self::waiting`] set
    /// another), or a file could not be opened, renamed or written.
    pub fn append(&self, lines: &[String]) -> io::Result<Appended> {
        if lines.is_empty() {
            return Ok(Appended::default());
        }
        let _held = self.lock()?;
        let mut done = Appended {
            lines: lines.len(),
            ..Appended::default()
        };
        let batch: usize = lines.iter().map(String::len).sum();
        let (mut file, mut len) = self.open_current()?;
        if len > 0 && !ends_in_line_feed(&mut file, len)? {
            file.write_all(b"\n")?;
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
            file.write_all(lines[0].as_bytes())?;
            return Ok(done);
        }
        let mut out = String::with_capacity(batch + 256);
        if len == 0 {
            out.push_str(&self.facts.line(crate::now_ms()));
        }
        for line in lines {
            out.push_str(line);
        }
        file.write_all(out.as_bytes())?;
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
            let Some(line) = std::str::from_utf8(raw).ok().and_then(parse_line) else {
                out.torn += 1;
                continue;
            };
            match line {
                Line::Row(row) => {
                    out.oldest_ms = Some(out.oldest_ms.map_or(row.at_ms, |o| o.min(row.at_ms)));
                    if row.at_ms >= since {
                        out.rows.push(row);
                    }
                }
                Line::Host(host) => {
                    out.oldest_ms = Some(out.oldest_ms.map_or(host.at_ms, |o| o.min(host.at_ms)));
                    out.hosts.push(host);
                }
                Line::Gap { at_ms, dropped } => {
                    if at_ms >= since {
                        out.dropped += dropped;
                    }
                }
            }
        }
    }
    Ok(out)
}
