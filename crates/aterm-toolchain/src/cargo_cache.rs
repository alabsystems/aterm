// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict owned in-use protection for Cargo's source cache, using its existing lock.
//!
//! Shared `.package-cache-mutate` custody allows fetch/download locking while
//! excluding cooperating Cargo source-cache GC. It does not freeze source bytes,
//! contain descendants, protect an arbitrary Cargo-home deletion, or supply SDK custody.

use std::{io, path::Path};

/// An actual owned shared Cargo source-cache lock, released by `LOCK_UN` when dropped, so a
/// child forked meanwhile, which holds a copy of the descriptor until its exec, keeps nothing.
/// No descriptor, unlock, cloning or path-only recovery authority escapes this guard.
#[derive(Debug)]
#[must_use = "keep the actual guard until all source-cache users have settled"]
pub struct SourceCacheGuard(imp::Held);

impl SourceCacheGuard {
    /// Acquire the exact existing Cargo protocol's shared mutate lock without waiting.
    /// The explicit absolute Cargo home must already exist. A missing lock file is
    /// created without truncation; no existing bytes, permissions or inode are changed.
    ///
    /// # Errors
    /// Contention, unsupported platform/filesystem, any actual open/lock error, an
    /// unsafe lock file, or original directory/lock identity changing during acquisition.
    /// No failed acquisition is converted to an unlocked successful guard.
    pub fn try_acquire(cargo_home: &Path) -> io::Result<Self> {
        imp::acquire(cargo_home).map(Self)
    }

    /// Check that the original Cargo-home spelling and lock still name the held objects.
    /// This is an observation, not protection against arbitrary rename/unlink or manual edits.
    /// An error leaves this guard and its actual shared lock owned by the caller.
    ///
    /// # Errors
    /// Metadata/canonicalization failed, or the original directory/lock identity or
    /// required lock-file properties changed. Unsupported platforms refuse acquisition.
    pub fn check_current(&self) -> io::Result<()> {
        self.0.check_current()
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod imp {
    use std::{
        fs::{File, Metadata, OpenOptions, TryLockError},
        io,
        os::{
            fd::{AsRawFd as _, FromRawFd as _},
            unix::fs::{MetadataExt as _, OpenOptionsExt as _},
        },
        path::{Path, PathBuf},
    };

    const LOCK: &str = ".package-cache-mutate";

    #[derive(Debug)]
    pub(super) struct Held {
        file: File,
        directory: File,
        original: PathBuf,
        resolved: PathBuf,
    }

    fn identity(metadata: &Metadata) -> (u64, u64) {
        (metadata.dev(), metadata.ino())
    }

    fn valid_lock(metadata: &Metadata) -> io::Result<()> {
        // SAFETY: geteuid reads this process's effective uid and has no preconditions.
        let uid = unsafe { libc::geteuid() };
        if metadata.is_file()
            && metadata.nlink() == 1
            && metadata.uid() == uid
            && metadata.mode() & 0o022 == 0
        {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Cargo mutate lock is not an owned regular single-link file without group/other write mode bits",
            ))
        }
    }

    impl Held {
        pub(super) fn check_current(&self) -> io::Result<()> {
            let named = std::fs::canonicalize(&self.original)?;
            let held_home = self.directory.metadata()?;
            let named_home = std::fs::symlink_metadata(&named)?;
            let held_lock = self.file.metadata()?;
            let named_lock = std::fs::symlink_metadata(self.resolved.join(LOCK))?;
            valid_lock(&held_lock)?;
            valid_lock(&named_lock)?;
            if named != self.resolved
                || !held_home.is_dir()
                || !named_home.is_dir()
                || identity(&held_home) != identity(&named_home)
                || identity(&held_lock) != identity(&named_lock)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "original Cargo home or mutate lock identity changed",
                ));
            }
            Ok(())
        }
    }

    impl Drop for Held {
        /// Let go by `LOCK_UN` before the close (clippy.toml's flock release rule): the lock lives
        /// on the open file description, which a child between fork and exec still shares.
        fn drop(&mut self) {
            let _ = self.file.unlock();
        }
    }

    fn supported_filesystem(file: &File) -> io::Result<()> {
        let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the file descriptor is live; fstatfs initializes the writable
        // statfs on success and does not retain the pointer.
        if unsafe { libc::fstatfs(file.as_raw_fd(), info.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatfs initialized the complete statfs.
        let info = unsafe { info.assume_init() };
        #[cfg(target_os = "linux")]
        let nfs = info.f_type as u64 == libc::NFS_SUPER_MAGIC as u64;
        #[cfg(target_os = "macos")]
        let nfs = info.f_fstypename[..4] == [b'n' as _, b'f' as _, b's' as _, 0];
        if nfs {
            // Cargo skips NFS locks because even LOCK_NB can hang there. This
            // authority-bearing guard refuses instead of pretending to hold one.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "strict Cargo source-cache locking is unavailable on NFS",
            ));
        }
        Ok(())
    }

    pub(super) fn acquire(cargo_home: &Path) -> io::Result<Held> {
        acquire_with(cargo_home, supported_filesystem, |file| {
            #[expect(
                clippy::disallowed_methods,
                reason = "the strict owned Cargo shared source-cache guard's actual kernel acquisition, released by `Held`'s Drop (`LOCK_UN`); no best-effort fallback"
            )]
            let result = file.try_lock_shared();
            result.map_err(|error| match error {
                TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
                TryLockError::Error(error) => error,
            })
        })
    }

    pub(super) fn acquire_with(
        cargo_home: &Path,
        mut filesystem: impl FnMut(&File) -> io::Result<()>,
        lock: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<Held> {
        if !cargo_home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cargo home must be explicit and absolute",
            ));
        }
        let resolved = std::fs::canonicalize(cargo_home)?;
        let directory = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&resolved)?;
        filesystem(&directory)?;
        // Open relative to the actual retained directory, never a raced ancestor.
        // SAFETY: the directory descriptor and static NUL-terminated lock name are live;
        // O_CREAT consumes the mode argument, and a successful fd is owned exactly once.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c".package-cache-mutate".as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned this uniquely owned descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        // A lock file can be a separate mount even when its parent is local.
        // Reject that file's unsupported filesystem before trying any lock.
        filesystem(&file)?;
        let held = Held {
            file,
            directory,
            original: cargo_home.into(),
            resolved,
        };
        held.check_current()?;
        lock(&held.file)?;
        held.check_current()?;
        Ok(held)
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod imp {
    use std::{convert::Infallible, io, path::Path};

    #[derive(Debug)]
    pub(super) struct Held(Infallible);

    pub(super) fn acquire(_: &Path) -> io::Result<Held> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict Cargo source-cache guard is unavailable on this platform",
        ))
    }

    impl Held {
        pub(super) fn check_current(&self) -> io::Result<()> {
            match self.0 {}
        }
    }
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
#[path = "cargo_cache_tests.rs"]
mod tests;
