// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The stable sibling lock shared by cooperating config and document writers.
//!
//! Retain the lock file when releasing it: unlinking it would let later writers lock
//! another inode. A lock coordinates participating writers; it is not a filesystem
//! compare-and-swap against an editor that ignores this protocol.

use std::ffi::OsStr;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

/// The sibling name also used by amacs's atomic document commits.
#[must_use]
pub fn name(target: &OsStr) -> String {
    format!(".{}.aterm-write.lock", target.to_string_lossy())
}

/// An exclusive advisory lock, explicitly unlocked before its descriptor closes.
#[derive(Debug)]
pub struct Held {
    file: File,
    path: PathBuf,
}

/// Whether an observed descriptor still names the target's current file.
/// Follows aliases, as a normal read of the target does.
///
/// # Errors
/// The native identity reader's metadata/open error.
pub fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    astream_sys::file_matches_path(file, path)
}

impl Held {
    /// Take the target's sibling lock without waiting for another writer.
    ///
    /// The caller resolves aliases before taking this lock, then reads and classifies
    /// its target under it and revalidates the observed target before publication.
    ///
    /// # Errors
    /// Returns `WouldBlock` for a participating writer, `NotFound` for a missing
    /// parent, or the open, private-file admission, identity or locking error.
    pub fn try_take(target: &Path) -> io::Result<Self> {
        let file_name = target.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "write target has no file name")
        })?;
        let path = target.with_file_name(name(file_name));
        let mut create = OpenOptions::new();
        create.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            create.mode(0o600);
        }
        let file = match create.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_existing(&path)?,
            Err(error) => return Err(error),
        };
        let metadata = file.metadata()?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & 0x0000_0400 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "write lock is a reparse point",
                ));
            }
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.nlink() != 1 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "write lock has multiple links",
                ));
            }
        }
        match astream_sys::owner_private(&file)? {
            Ok(_) => {}
            #[cfg(unix)]
            Err(astream_sys::NotPrivate::Mode(_)) => {
                use std::os::unix::fs::PermissionsExt as _;
                file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
            }
            Err(why) => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    format!("write lock {} {why}", path.display()),
                ));
            }
        }
        if !metadata.is_file() || !named_file(&file, &path)? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write lock is not a stable regular file",
            ));
        }
        #[expect(
            clippy::disallowed_methods,
            reason = "Held::drop explicitly unlocks before closing, including inherited descriptors"
        )]
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    format!("another writer holds {}", path.display()),
                ));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        let held = Self { file, path };
        if !held.still_named()? {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "write lock changed while it was acquired",
            ));
        }
        Ok(held)
    }

    /// Whether the retained sibling still names this lock's regular file.
    ///
    /// # Errors
    /// An error reading either file identity, apart from a missing sibling.
    pub fn still_named(&self) -> io::Result<bool> {
        named_file(&self.file, &self.path)
    }
}

fn open_existing(path: &Path) -> io::Result<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        // Open the reparse point itself, just as amacs's save-lock admission does.
        OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(0x0020_0000)
            .open(path)
    }
    #[cfg(not(windows))]
    astream_sys::open_nonblocking_rw(path, astream_sys::Links::Refuse)
}

fn named_file(file: &File, path: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            same_file(file, path)
        }
        Ok(_) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
#[path = "write_lock_tests.rs"]
mod tests;
