// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Strict owned in-use protection for Cargo's source cache, using its existing lock.
//!
//! Shared `.package-cache-mutate` custody allows fetch/download locking while
//! excluding cooperating Cargo source-cache GC. It does not freeze source bytes,
//! contain descendants, protect an arbitrary Cargo-home deletion, or supply SDK custody.

use std::{io, path::Path};

/// An actual owned shared Cargo source-cache lock. Its acquiring process releases by
/// `LOCK_UN` when dropped, so a forked child's descriptor cannot prolong that original
/// release. A foreign process copy can neither validate custody nor unlock the parent.
/// No descriptor, unlock, cloning or path-only recovery authority escapes this guard.
#[derive(Debug)]
#[must_use = "keep the actual guard until all source-cache users have settled"]
pub struct SourceCacheGuard(imp::Held);

/// Exclusive custody of both existing Cargo cache protocol locks, in Cargo's
/// mutation-then-download order. Retain it throughout source-cache reclamation.
/// This excludes cooperating readers and downloads; it does not authorize a
/// pathname deletion or protect against arbitrary manual namespace changes.
#[derive(Debug)]
#[must_use = "retain both Cargo locks until source-cache reclamation has settled"]
pub struct SourceCacheReclaimGuard(imp::ReclaimHeld);

impl SourceCacheReclaimGuard {
    /// Acquire both existing protocol locks without waiting or creating files.
    /// Missing, unsafe, unsupported or contended locks refuse reclamation.
    ///
    /// # Errors
    /// As [`SourceCacheGuard::try_acquire`], or either existing lock is absent
    /// or held incompatibly. A partial acquisition releases its mutate lock.
    pub fn try_acquire(cargo_home: &Path) -> io::Result<Self> {
        imp::acquire_reclaim(cargo_home).map(Self)
    }

    /// Observe that the original home and both names still identify our locks.
    ///
    /// # Errors
    /// A foreign process, changed identity, unsafe lock or failed observation.
    pub fn check_current(&self) -> io::Result<()> {
        self.0.check_current()
    }
}

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

    /// Add an exact original-directory restriction before creating the lock.
    /// The actual lock is opened relative to a retained copy of `original`, not
    /// a later pathname lookup. This descriptor only narrows ordinary cache
    /// acquisition; it grants no freshness, source, SDK or producer authority.
    /// The final component must name the same directory without a symlink.
    ///
    /// # Errors
    /// As [`Self::try_acquire`], or the readonly original directory and selected
    /// Cargo home differ. Refusal occurs before creating a lock in another home.
    #[cfg(unix)]
    pub fn try_acquire_original_directory(
        cargo_home: &Path,
        original: std::os::fd::BorrowedFd<'_>,
    ) -> io::Result<Self> {
        imp::acquire_original(cargo_home, original).map(Self)
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
            fd::{AsRawFd as _, BorrowedFd, FromRawFd as _},
            unix::fs::{MetadataExt as _, OpenOptionsExt as _},
        },
        path::{Path, PathBuf},
    };

    const LOCK: &str = ".package-cache-mutate";

    #[derive(Debug)]
    pub(super) struct Held {
        pub(super) creator: u32,
        file: File,
        directory: File,
        original: PathBuf,
        resolved: PathBuf,
    }

    #[derive(Debug)]
    pub(super) struct ReclaimHeld {
        mutate: Held,
        download: File,
    }

    impl ReclaimHeld {
        pub(super) fn check_current(&self) -> io::Result<()> {
            check_download(&self.mutate, &self.download)
        }
    }

    fn check_download(mutate: &Held, download: &File) -> io::Result<()> {
        mutate.check_current()?;
        let held = download.metadata()?;
        let named = std::fs::symlink_metadata(mutate.resolved.join(".package-cache"))?;
        valid_lock(&held)?;
        valid_lock(&named)?;
        if identity(&held) != identity(&named) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "original Cargo download lock identity changed",
            ));
        }
        Ok(())
    }

    impl Drop for ReclaimHeld {
        fn drop(&mut self) {
            // Release in reverse protocol order. A foreign fork copy may only
            // close its descriptors, never unlock the original parent's OFDs.
            if self.mutate.creator == std::process::id() {
                let _ = self.download.unlock();
            }
        }
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
            if self.creator != std::process::id() {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Cargo source-cache guard belongs to another process",
                ));
            }
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
        /// The creator releases by `LOCK_UN` before closing: a child between
        /// fork and exec still shares that description. A foreign copy closes
        /// only its own descriptor and cannot unlock the original holder.
        fn drop(&mut self) {
            if self.creator == std::process::id() {
                let _ = self.file.unlock();
            }
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
        acquire_with(cargo_home, supported_filesystem, shared_lock)
    }

    pub(super) fn acquire_original(
        cargo_home: &Path,
        original: BorrowedFd<'_>,
    ) -> io::Result<Held> {
        acquire_with_original(
            cargo_home,
            Some(original),
            supported_filesystem,
            shared_lock,
        )
    }

    fn shared_lock(file: &File) -> io::Result<()> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the strict owned Cargo shared source-cache guard's actual kernel acquisition, released by `Held`'s Drop (`LOCK_UN`); no best-effort fallback"
        )]
        let result = file.try_lock_shared();
        result.map_err(|error| match error {
            TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
            TryLockError::Error(error) => error,
        })
    }

    fn exclusive_lock(file: &File) -> io::Result<()> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the strict Cargo reclamation guard's nonblocking original protocol acquisition; its creator releases by LOCK_UN"
        )]
        let result = file.try_lock();
        result.map_err(|error| match error {
            TryLockError::WouldBlock => io::ErrorKind::WouldBlock.into(),
            TryLockError::Error(error) => error,
        })
    }

    pub(super) fn acquire_reclaim(cargo_home: &Path) -> io::Result<ReclaimHeld> {
        let mutate = acquire_mode(
            cargo_home,
            None,
            supported_filesystem,
            exclusive_lock,
            false,
        )?;
        // SAFETY: live retained directory and static NUL-terminated name;
        // O_NOFOLLOW refuses a replacement link and no file is created.
        let fd = unsafe {
            libc::openat(
                mutate.directory.as_raw_fd(),
                c".package-cache".as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned this uniquely owned descriptor.
        let download = unsafe { File::from_raw_fd(fd) };
        supported_filesystem(&download)?;
        check_download(&mutate, &download)?;
        exclusive_lock(&download)?;
        let held = ReclaimHeld { mutate, download };
        held.check_current()?;
        Ok(held)
    }

    pub(super) fn acquire_with(
        cargo_home: &Path,
        filesystem: impl FnMut(&File) -> io::Result<()>,
        lock: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<Held> {
        acquire_with_original(cargo_home, None, filesystem, lock)
    }

    pub(super) fn acquire_with_original(
        cargo_home: &Path,
        original: Option<BorrowedFd<'_>>,
        filesystem: impl FnMut(&File) -> io::Result<()>,
        lock: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<Held> {
        acquire_mode(cargo_home, original, filesystem, lock, true)
    }

    fn acquire_mode(
        cargo_home: &Path,
        original: Option<BorrowedFd<'_>>,
        mut filesystem: impl FnMut(&File) -> io::Result<()>,
        lock: impl FnOnce(&File) -> io::Result<()>,
        create: bool,
    ) -> io::Result<Held> {
        if !cargo_home.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Cargo home must be explicit and absolute",
            ));
        }
        let resolved = std::fs::canonicalize(cargo_home)?;
        let directory = if let Some(original) = original {
            // Retain the actual supplied description BEFORE any lock creation.
            // SAFETY: flags observation of the live borrowed descriptor only.
            let flags = unsafe { libc::fcntl(original.as_raw_fd(), libc::F_GETFL) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            if flags & libc::O_ACCMODE != libc::O_RDONLY {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "original Cargo directory is not readonly",
                ));
            }
            let directory = File::from(original.try_clone_to_owned()?);
            let held = directory.metadata()?;
            let named = std::fs::symlink_metadata(cargo_home)?;
            if !held.is_dir() || !named.is_dir() || identity(&held) != identity(&named) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Cargo home is not the original directory",
                ));
            }
            directory
        } else {
            OpenOptions::new()
                .read(true)
                .custom_flags(
                    libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
                )
                .open(&resolved)?
        };
        filesystem(&directory)?;
        // Open relative to the actual retained directory, never a raced ancestor.
        // SAFETY: the directory descriptor and static NUL-terminated lock name are live;
        // O_CREAT consumes the mode argument, and a successful fd is owned exactly once.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                c".package-cache-mutate".as_ptr(),
                libc::O_RDWR
                    | (if create { libc::O_CREAT } else { 0 })
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
            creator: std::process::id(),
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

#[cfg(windows)]
#[path = "cargo_cache_win.rs"]
mod imp;

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
mod imp {
    use std::{convert::Infallible, io, path::Path};

    #[derive(Debug)]
    pub(super) struct Held(Infallible);

    pub(super) type ReclaimHeld = Held;

    pub(super) fn acquire_reclaim(_: &Path) -> io::Result<ReclaimHeld> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict Cargo source-cache reclamation is unavailable on this platform",
        ))
    }

    pub(super) fn acquire(_: &Path) -> io::Result<Held> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "strict Cargo source-cache guard is unavailable on this platform",
        ))
    }

    #[cfg(unix)]
    pub(super) fn acquire_original(_: &Path, _: std::os::fd::BorrowedFd<'_>) -> io::Result<Held> {
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

#[cfg(all(test, windows))]
#[path = "cargo_cache_win_tests.rs"]
mod tests;
