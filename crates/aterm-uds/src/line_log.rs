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
    let _lock = lock(path)?;
    let mut file = open_append(path)?;
    let len = file.metadata()?.len();
    if len > 0 {
        file.seek(SeekFrom::Start(len - 1))?;
        let mut last = [0];
        file.read_exact(&mut last)?;
        if &last != b"\n" {
            file.write_all(b"\n")?;
        }
    }
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.flush()
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
mod tests {
    use super::*;

    #[test]
    fn a_multiline_append_is_refused_without_mutation_or_creation() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let path = tmp.path().join("ledger");
        assert_eq!(
            append(&path, "a\nb").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert!(!path.exists());
        assert!(!path.with_file_name("ledger.lock").exists());
        std::fs::write(&path, b"keep").unwrap();
        assert!(append(&path, "\n").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"keep");
        append(&path, "next").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"keep\nnext\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let created = tmp.path().join("new");
            ensure(&created).unwrap();
            assert_eq!(
                std::fs::metadata(created).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}
