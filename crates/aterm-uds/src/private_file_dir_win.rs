// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
// Author: Andrew Yates

//! Private transactions whose folder cannot change underneath their lock, on Windows: a folder
//! held open by a handle of its own, judged on that handle under the Windows arm's rule (the
//! token user's, a protected access list of the user and SYSTEM alone, no reparse point), and
//! every file of it opened, made, renamed, linked and removed relative to that handle, never by
//! a path (aterm-dirfd's Windows calls).

use super::win::{self, Folder, Opening, Verdict};
use super::{PrivateFileError, PrivateLock};
use std::fs::File;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

/// One opened owner-private folder. Renaming its path never redirects its operations.
/// A fresh [`Self::view`] checks its privacy again after each wait.
pub struct PrivateDirectory {
    folder: Folder,
}

impl PrivateDirectory {
    /// Open `path` as itself, refusing a reparse point at its last component (a junction, a
    /// symlink, a cloud placeholder) or a folder that is not private.
    /// Folders above it are reached as a path reaches them, as with [`super::open_private`].
    /// # Errors
    /// Opening or judging the folder fails.
    pub fn open(path: &Path) -> io::Result<Self> {
        Self::from_file(win::open_folder(path)?)
    }

    /// Judge and retain this exact already opened folder. No path is reopened. The handle
    /// needs `READ_CONTROL` and `FILE_READ_ATTRIBUTES`, which its judgement reads by.
    /// This is private filesystem admission, not ownership or execution authority.
    /// # Errors
    /// The handle is not on a private folder of the token user's.
    pub fn from_file(file: File) -> io::Result<Self> {
        let directory = Self {
            folder: Folder::held(file),
        };
        directory.view()?;
        Ok(directory)
    }

    /// Judge this open folder now, for a batch of operations made without waiting.
    /// # Errors
    /// Its kind, owner or access list no longer admits private operations.
    pub fn view(&self) -> io::Result<DirectoryView<'_>> {
        match win::verdict_of(self.folder.file())? {
            Verdict::Private => Ok(DirectoryView { directory: self }),
            why => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("not a private directory: {why:?}"),
            )),
        }
    }

    /// Whether `path` still names this folder and admits private operations now.
    /// This is a retargeting observation; operations remain handle-relative either way.
    /// # Errors
    /// An identity or privacy check fails. A missing path answers false.
    pub fn is_at(&self, path: &Path) -> io::Result<bool> {
        let current = match Self::open(path) {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(aterm_dirfd::file_id(self.folder.file())?
            == aterm_dirfd::file_id(current.folder.file())?)
    }

    /// Take the named private lock within `within`, on this same folder.
    /// Obtain a fresh view after waiting, before reading or writing rows.
    /// # Errors
    /// Admission, opening or locking fails, including contention past the deadline.
    pub fn lock(&self, name: &str, within: Duration, busy: &str) -> io::Result<PrivateLock> {
        let file = self.view()?.open_lock(name)?;
        crate::file_lock::lock_exclusive_with_deadline(
            &file,
            Instant::now().checked_add(within),
            busy,
            || {},
        )?;
        Ok(PrivateLock(file))
    }

    /// Take the named lock with the kernel's blocking handoff and no polling timer.
    /// `held` runs once if the initial attempt finds contention.
    /// # Errors
    /// Admission, opening or locking fails.
    pub fn wait_lock(&self, name: &str, held: impl FnOnce()) -> io::Result<PrivateLock> {
        let file = self.view()?.open_lock(name)?;
        match crate::file_lock::lock_exclusive_until(&file, Instant::now(), "held") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                held();
                crate::file_lock::lock_exclusive_blocking(&file)?;
            }
            Err(error) => return Err(error),
        }
        Ok(PrivateLock(file))
    }
}

/// A fresh privacy verdict over an opened folder, for immediate handle-relative I/O.
/// Do not carry a view across a lock wait; obtain another afterward.
pub struct DirectoryView<'a> {
    directory: &'a PrivateDirectory,
}

impl DirectoryView<'_> {
    fn folder(&self) -> &Folder {
        &self.directory.folder
    }

    /// Open or create a singly linked owner-private regular lock file in this folder: made with
    /// the token user's protected access list where missing, and judged on its handle.
    /// This takes no lock; the caller supplies its guard and obtains a fresh view after waiting.
    /// # Errors
    /// The name, opening, kind, ownership, access list or link count is refused.
    pub fn open_lock(&self, name: &str) -> io::Result<File> {
        super::one_component_io(name)?;
        match win::open_relative(self.folder(), name, Opening::ReadWrite) {
            Ok((file, _, _)) => Ok(file),
            Err(PrivateFileError::Io(error)) => Err(error),
            Err(error) => Err(io::Error::new(io::ErrorKind::PermissionDenied, error)),
        }
    }

    /// Read a private regular file, capped at `max` bytes, from this folder.
    /// # Errors
    /// As [`super::read_private`], with lookup confined to this open folder.
    pub fn read(&self, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
        let (file, metadata) = self.open_private(name)?;
        super::read_capped_with_len(&file, metadata.len(), max, name)
    }

    /// Open a private regular file and retain the same handle and metadata for later identity
    /// checks. Lookup is confined to this folder; no path is reopened.
    /// # Errors
    /// As [`super::open_private`], with lookup confined to this open folder.
    pub fn open_private(&self, name: &str) -> Result<(File, std::fs::Metadata), PrivateFileError> {
        if !super::one_component(name) {
            return Err(PrivateFileError::Malformed(super::ONE_COMPONENT));
        }
        let (file, metadata, _) = win::open_relative(self.folder(), name, Opening::Read)?;
        Ok((file, metadata))
    }

    /// Stream this folder's entry names from a listing handle of its own.
    /// # Errors
    /// Opening the listing fails; each entry's read may fail too.
    pub fn names(&self) -> io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>>> {
        let listing = aterm_dirfd::Dir::read_from(self.folder().file())?;
        Ok(listing.map(|entry| {
            entry
                .map(|entry| entry.file_name().to_os_string())
                .map_err(io::Error::from)
        }))
    }

    /// Atomically replace `name` with private bytes; temporary, rename and folder flush all use
    /// this opened folder, even if its pathname is replaced meanwhile.
    /// # Errors
    /// As [`super::write_private`]. A folder-flush error can follow publication.
    pub fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.write_checked(name, bytes, || Ok(()))
    }

    fn write_checked(
        &self,
        name: &str,
        bytes: &[u8],
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        let landed = self.write_entry_landed(name, bytes, check)?;
        aterm_durable::DirFlushed::classify(landed).map(|_| ())
    }

    /// Atomically replace a private entry through this held folder, preserving stages.
    /// `check` runs after the temporary's durable flush and before the handle-relative rename.
    /// The folder's own privacy is checked again at commit. An outer error leaves the entry
    /// unchanged; an inner error means the new bytes are published with unknown entry
    /// durability. The folder's flush never reopens its pathname (on Windows it is noted:
    /// NTFS commits a folder's entries through its log).
    /// # Errors
    /// Invalid names, temporary preparation, privacy or `check` failures, and rename failures
    /// are outer errors. A failed held-folder flush is an inner error.
    pub fn write_entry_landed(
        &self,
        name: &str,
        bytes: &[u8],
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<io::Result<()>> {
        super::one_component_io(name)?;
        win::write_in(
            self.folder(),
            Path::new(name),
            name,
            bytes,
            win::Flushing {
                flush: aterm_durable::Flush::Durable,
                diagnose: false,
            },
            || {
                check()?;
                self.directory.view().map(drop)
            },
        )
    }

    /// Remove this entry as itself, never following it.
    /// # Errors
    /// The name is invalid, missing, or its removal fails.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        super::one_component_io(name)?;
        win::remove_in(self.folder(), name)
    }
}

#[cfg(test)]
#[path = "private_file_dir_win_tests.rs"]
mod tests;
