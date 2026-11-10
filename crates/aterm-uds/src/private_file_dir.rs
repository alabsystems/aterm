// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Private transactions whose directory cannot change underneath their lock.

use super::{Opening, PrivateFileError, PrivateLock};
use aterm_dirfd::{AtFlags, Mode, OFlags};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::{ffi::OsStringExt, fs::OpenOptionsExt};
use std::path::Path;
use std::time::{Duration, Instant};

const FILE_FLAGS: OFlags = OFlags::from_bits(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);

/// One opened owner-private directory. Renaming its path never redirects its operations.
/// A fresh [`Self::view`] checks its privacy again after each wait.
pub struct PrivateDirectory {
    file: File,
}

impl PrivateDirectory {
    /// Open `path`, refusing a linked final component or a non-private directory.
    /// Parent aliases are permitted, as with [`super::open_private`].
    /// # Errors
    /// Opening or judging the directory fails.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = crate::kept_out(|| {
            File::options()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
                .open(path)
        })?;
        let directory = Self { file };
        directory.view()?;
        Ok(directory)
    }

    /// Judge this open directory now, for a batch of operations made without waiting.
    /// # Errors
    /// Its owner, kind, mode or access list no longer admits private operations.
    pub fn view(&self) -> io::Result<DirectoryView<'_>> {
        match astream_sys::owner_private_dir(&self.file)? {
            Ok(_) => Ok(DirectoryView { directory: self }),
            Err(why) => Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("not a private directory: {why:?}"),
            )),
        }
    }

    /// Whether `path` still names this directory and admits private operations now.
    /// This is a retargeting observation; operations remain descriptor-relative either way.
    /// # Errors
    /// An identity or privacy check fails. A missing path answers false.
    pub fn is_at(&self, path: &Path) -> io::Result<bool> {
        let current = match Self::open(path) {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        Ok(crate::directory_identity::of_file(&self.file)?
            == crate::directory_identity::of_file(&current.file)?)
    }

    /// Take the named private lock within `within`, on this same directory.
    /// Obtain a fresh view after waiting, before reading or writing rows.
    /// # Errors
    /// Admission, opening or locking fails, including contention past the deadline.
    pub fn lock(&self, name: &str, within: Duration, busy: &str) -> io::Result<PrivateLock> {
        let file = self.view()?.lock_file(name)?;
        crate::file_lock::lock_exclusive_with_deadline(
            &file,
            Instant::now().checked_add(within),
            busy,
            || {},
        )?;
        Ok(PrivateLock(file))
    }

    /// Take the named lock with kernel blocking handoff and no polling timer.
    /// `held` runs once if the initial attempt finds contention.
    /// # Errors
    /// Admission, opening or locking fails.
    pub fn wait_lock(&self, name: &str, held: impl FnOnce()) -> io::Result<PrivateLock> {
        let file = self.view()?.lock_file(name)?;
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

/// A fresh privacy verdict over an opened directory, for immediate descriptor-relative I/O.
/// Do not carry a view across a lock wait; obtain another afterward.
pub struct DirectoryView<'a> {
    directory: &'a PrivateDirectory,
}

impl DirectoryView<'_> {
    fn open(&self, name: &str, flags: OFlags, mode: Mode) -> io::Result<File> {
        super::one_component_io(name)?;
        crate::kept_out(|| {
            aterm_dirfd::openat(&self.directory.file, name, flags | FILE_FLAGS, mode)
                .map(File::from)
                .map_err(io::Error::from)
        })
    }

    fn lock_file(&self, name: &str) -> io::Result<File> {
        let file = match self.open(
            name,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL,
            Mode::from_bits(0o600),
        ) {
            Ok(file) => {
                aterm_dirfd::fchmod(&file, Mode::from_bits(0o600))?;
                file
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                self.open(name, OFlags::RDWR, Mode::empty())?
            }
            Err(error) => return Err(error),
        };
        super::judge_opened(file, Opening::ReadWrite)
            .map(|(file, _, _)| file)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))
    }

    /// Read a private regular file, capped at `max` bytes, from this directory.
    /// # Errors
    /// As [`super::read_private`], with lookup confined to this open directory.
    pub fn read(&self, name: &str, max: usize) -> Result<Vec<u8>, PrivateFileError> {
        let file = self
            .open(name, OFlags::RDONLY, Mode::empty())
            .map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => PrivateFileError::Missing,
                _ if error.raw_os_error() == Some(libc::ELOOP) => PrivateFileError::Symlink,
                _ => PrivateFileError::Io(error),
            })?;
        let (file, metadata, _) = super::judge_opened(file, Opening::Read)?;
        super::read_capped_with_len(&file, metadata.len(), max)
    }

    /// Stream this directory's entry names from an independent file description.
    /// # Errors
    /// Opening or reading the directory stream fails.
    pub fn names(&self) -> io::Result<impl Iterator<Item = io::Result<std::ffi::OsString>>> {
        Ok(self.entries()?.map(|entry| {
            entry
                .map(|entry| std::ffi::OsString::from_vec(entry.file_name().to_bytes().to_vec()))
                .map_err(io::Error::from)
        }))
    }

    fn entries(&self) -> io::Result<aterm_dirfd::Dir> {
        // `.` is not a user-supplied child name; open it explicitly for a fresh stream offset.
        crate::kept_out(|| {
            let fd = aterm_dirfd::openat(
                &self.directory.file,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | FILE_FLAGS,
                Mode::empty(),
            )
            .map_err(io::Error::from)?;
            // Darwin fdopendir clears FD_CLOFORK. Convert under the spawn exclusion,
            // then let kept_out mark the completed stream, not the pre-conversion fd.
            aterm_dirfd::Dir::new(fd).map_err(io::Error::from)
        })
    }

    /// Atomically replace `name` with private bytes; temporary, rename and directory flush
    /// all use this opened directory, even if its pathname is replaced meanwhile.
    /// # Errors
    /// As [`super::write_private`]. A directory-flush error can follow publication.
    pub fn write(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        self.write_checked(name, bytes, || Ok(()))
    }

    fn write_checked(
        &self,
        name: &str,
        bytes: &[u8],
        check: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<()> {
        super::one_component_io(name)?;
        for _ in 0..8 {
            let temporary = crate::atomic_file::fresh_temp_component(name)?;
            let mut file = match self.open(
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
                Mode::from_bits(0o600),
            ) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let mut cleanup = Temporary {
                directory: self.directory,
                name: temporary,
                armed: true,
            };
            aterm_dirfd::fchmod(&file, Mode::from_bits(0o600))?;
            file.write_all(bytes)?;
            aterm_durable::Flush::Durable.file(&file)?;
            check()?;
            aterm_dirfd::renameat(
                &self.directory.file,
                cleanup.name.as_str(),
                &self.directory.file,
                name,
            )?;
            cleanup.armed = false;
            return aterm_durable::DirFlushed::classify(
                aterm_durable::Flush::Durable.file(&self.directory.file),
            )
            .map(|_| ());
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "private temporary names collided",
        ))
    }

    /// Remove this entry without following it.
    /// # Errors
    /// The name is invalid or unlinking fails.
    pub fn remove(&self, name: &str) -> io::Result<()> {
        super::one_component_io(name)?;
        aterm_dirfd::unlinkat(&self.directory.file, name, AtFlags::empty()).map_err(io::Error::from)
    }
}

struct Temporary<'a> {
    directory: &'a PrivateDirectory,
    name: String,
    armed: bool,
}

impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ =
                aterm_dirfd::unlinkat(&self.directory.file, self.name.as_str(), AtFlags::empty());
        }
    }
}

#[cfg(test)]
#[path = "private_file_dir_tests.rs"]
mod tests;
