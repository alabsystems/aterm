// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Newline-framed append logs whose writers cooperate with atomic file replacement.
//! The sibling lock remains at its path; callers must not unlink it. Paths aliased through
//! hard links, and external writers that do not take this lock, are outside this protocol.

use std::fs::{File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::Path;
use std::time::{Duration, Instant};

/// Serialize appends and replacement by a stable sibling (`<filename>.lock`). A stalled
/// holder cannot block a supervisor indefinitely: actual contention waits at most 250 ms.
/// Hold the returned guard across the entire read/transform/atomic-replace operation.
///
/// # Errors
/// An invalid filename or [`crate::file_lock::LockFile::take`]'s path, admission or lock error.
pub fn lock(path: &Path) -> io::Result<crate::file_lock::LockFile> {
    let mut name = path
        .file_name()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?
        .to_os_string();
    name.push(".lock");
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_millis(250)).unwrap_or(now);
    crate::file_lock::LockFile::take(
        &path.with_file_name(name),
        deadline,
        "journal is being updated by another process",
    )
}

/// Create or validate the regular log, without retaining an open descriptor across replacement.
/// A new Unix log is created with mode 0600, subject to the caller's umask.
///
/// # Errors
/// The path, lock, open or metadata error; final links, reparse points and non-regular files
/// are refused. The parent directory must already exist.
pub fn ensure(path: &Path) -> io::Result<()> {
    let _lock = lock(path)?;
    open_append(path).map(drop)
}

/// Append one line and its newline, reopening the current named file under [`lock`]. Preserve
/// all prior bytes, adding a delimiter if the old final row was interrupted. This checks the
/// writes and flush, without promising power-loss durability or repairing malformed old rows.
///
/// # Errors
/// A newline in `line`, [`ensure`]'s admission errors, or any read, seek, write or flush error.
pub fn append(path: &Path, line: &str) -> io::Result<()> {
    if line.contains('\n') {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    }
    hold(path)?.append(&[line]).map(drop)
}

/// The log at a path, open under its [`lock`] until dropped: a writer that must read what other
/// writers appended before it decides what to append (a counter folded from the rows) reads and
/// appends under one hold.
pub struct Held {
    _lock: crate::file_lock::LockFile,
    file: File,
}

/// Which file a [`Held`] log is and how long: a reader that kept an earlier identity reads only
/// the bytes past its `len`, or the whole file again where `dev` or `ino` changed (another
/// process replaced it) or it shrank.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    /// The device the file is on (0 off Unix).
    pub dev: u64,
    /// Its inode (off Unix, its creation time in 100 ns units).
    pub ino: u64,
    /// Its length in bytes.
    pub len: u64,
}

/// [`Identity`] of a file's metadata.
#[must_use]
pub fn identity_of(meta: &std::fs::Metadata) -> Identity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Identity {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        Identity {
            dev: 0,
            ino: meta.creation_time(),
            len: meta.len(),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Identity {
            dev: 0,
            ino: 0,
            len: meta.len(),
        }
    }
}

/// Open the log at `path` under its [`lock`], creating it as [`ensure`] does.
///
/// # Errors
/// [`ensure`]'s.
pub fn hold(path: &Path) -> io::Result<Held> {
    let lock = lock(path)?;
    let file = open_append(path)?;
    Ok(Held { _lock: lock, file })
}

impl Held {
    /// The held file's [`Identity`] now.
    ///
    /// # Errors
    /// Its metadata's.
    pub fn identity(&self) -> io::Result<Identity> {
        Ok(identity_of(&self.file.metadata()?))
    }

    /// At most `max` bytes from `offset` on.
    ///
    /// # Errors
    /// A seek or read error.
    pub fn read_from(&mut self, offset: u64, max: u64) -> io::Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut out = Vec::new();
        (&self.file).take(max).read_to_end(&mut out)?;
        Ok(out)
    }

    /// Append `lines`, each with its newline, in one write after the delimiter an interrupted
    /// final row lacks ([`append`]); the file's length after them.
    ///
    /// # Errors
    /// A newline in any line (nothing is written), or any read, seek, write or flush error.
    pub fn append(&mut self, lines: &[&str]) -> io::Result<u64> {
        if lines.iter().any(|line| line.contains('\n')) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let len = self.file.metadata()?.len();
        let mut bytes =
            Vec::with_capacity(lines.iter().map(|line| line.len() + 1).sum::<usize>() + 1);
        if len > 0 {
            self.file.seek(SeekFrom::Start(len - 1))?;
            let mut last = [0];
            self.file.read_exact(&mut last)?;
            if &last != b"\n" {
                bytes.push(b'\n');
            }
        }
        for line in lines {
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(b'\n');
        }
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        Ok(len + bytes.len() as u64)
    }

    /// The held file, for a flush ([`aterm_durable::ordered`]).
    #[must_use]
    pub const fn file(&self) -> &File {
        &self.file
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    let regular = metadata.is_file();
    #[cfg(windows)]
    let regular = {
        use std::os::windows::fs::MetadataExt as _;
        regular && metadata.file_attributes() & 0x0400 == 0 // FILE_ATTRIBUTE_REPARSE_POINT
    };
    if !regular {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

#[cfg(test)]
#[path = "line_log_tests.rs"]
mod tests;
