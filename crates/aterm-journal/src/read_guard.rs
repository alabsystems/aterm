// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Writer exclusion for a bounded read, without opening or repairing the log.

use super::*;

/// Retain an existing journal's writer lock without opening its mutable log.
/// This excludes cooperating writers, but authenticates no record or anchor.
/// Read the acknowledged prefix separately and retain this guard through the
/// local action that depends on it. Drop it before network I/O.
pub struct ReadGuard {
    dir: PathBuf,
    name: String,
    identity: (u64, u64),
    _lock: WriterLock,
}

impl ReadGuard {
    /// Take the existing writer lock by `deadline`, without creating a file,
    /// changing permissions, publishing a checkpoint or recovering a tail.
    /// The lock path is checked again after any wait.
    ///
    /// # Errors
    /// An invalid name, absent or non-private directory/lock, changed lock
    /// identity, or a writer still holding the lock at the deadline.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::spec_unmodeled(
            machine = "JournalReadGuard",
            action = "Case",
            reason = "The environment chooses existing custody, contention and path replacement."
        )
    )]
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
            machine = "JournalReadGuard",
            action = "Take",
            project = "aterm_journal::read_guard::tests::transition"
        )
    )]
    pub fn take(dir: &Path, name: &str, deadline: Instant) -> io::Result<Self> {
        validate_name(name)?;
        let name = format!("{name}.lock");
        let judged = private_file::JudgedDir::private(dir)?;
        let (file, metadata) = aterm_uds::kept_out(|| judged.open(&name)).map_err(read_error)?;
        #[cfg(test)]
        let take_lock = tests::MUTANT.get() != 1;
        #[cfg(not(test))]
        let take_lock = true;
        if take_lock {
            aterm_uds::file_lock::lock_exclusive_until(
                &file,
                deadline,
                "another local journal writer is active",
            )?;
        }
        let guard = Self {
            dir: dir.into(),
            name,
            identity: (metadata.dev(), metadata.ino()),
            _lock: WriterLock(file),
        };
        #[cfg(test)]
        tests::after_lock(&guard);
        #[cfg(test)]
        let check_path = tests::MUTANT.get() != 2;
        #[cfg(not(test))]
        let check_path = true;
        if check_path {
            guard.check()?;
        }
        Ok(guard)
    }

    /// Check that the retained lock still guards its current private path.
    /// This opens no writer and authenticates no journal data. A replacement
    /// by an uncoordinated actor invalidates this guard rather than reacquiring.
    ///
    /// # Errors
    /// The directory or lock is no longer private, is missing, or names a
    /// different file from the one this guard holds.
    pub fn check(&self) -> io::Result<()> {
        let (_, current) = private_file::open_private(&self.dir, &self.name).map_err(read_error)?;
        if current.nlink() != 1 || (current.dev(), current.ino()) != self.identity {
            return Err(io::Error::other("journal read lock was replaced"));
        }
        Ok(())
    }
}

fn read_error(error: PrivateFileError) -> io::Error {
    match error {
        PrivateFileError::Io(error) => error,
        missing @ PrivateFileError::Missing => io::Error::new(io::ErrorKind::NotFound, missing),
        denied @ (PrivateFileError::NotPrivate(_) | PrivateFileError::Symlink) => {
            io::Error::new(io::ErrorKind::PermissionDenied, denied)
        }
        malformed @ PrivateFileError::Malformed(_) => {
            io::Error::new(io::ErrorKind::InvalidData, malformed)
        }
    }
}

#[cfg(test)]
mod tests;
