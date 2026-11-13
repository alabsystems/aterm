// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Exclusive lock on a file carrying a prefix record readable by other processes.

use std::fs::File;

/// Exclusive lock on a file carrying a prefix record readable by peers ([`crate::lease`]).
#[derive(Debug)]
pub struct RecordLock {
    // None only while a consumed Unix guard transfers its original description. Its Drop
    // must then close nothing and, in particular, must not unlock the transferred copies.
    file: Option<File>,
    /// The record's length.
    #[cfg(windows)]
    record: u64,
}

impl RecordLock {
    /// Take `file`'s exclusive lock without blocking.
    ///
    /// # Errors
    /// `WouldBlock` while another handle holds it; the OS error otherwise.
    pub fn try_lock(file: File, record: u64) -> Result<Self, std::fs::TryLockError> {
        let file: std::fs::File = file;
        #[cfg(windows)]
        {
            record_range::try_lock(&file, record)?;
            Ok(Self {
                file: Some(file),
                record,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = record;
            #[expect(
                clippy::disallowed_methods,
                reason = "the record guard's own take: its Drop releases by LOCK_UN"
            )]
            file.try_lock()?;
            Ok(Self { file: Some(file) })
        }
    }

    /// Take `file`'s exclusive lock, blocking until the handle that holds it lets go (or its
    /// process ends, which lets go of it whatever ended it).
    ///
    /// # Errors
    /// The OS error.
    pub fn lock(file: File, record: u64) -> std::io::Result<Self> {
        let file: std::fs::File = file;
        #[cfg(windows)]
        {
            record_range::lock(&file, record)?;
            Ok(Self {
                file: Some(file),
                record,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = record;
            #[expect(
                clippy::disallowed_methods,
                reason = "the record guard's own take: its Drop releases by LOCK_UN"
            )]
            file.lock()?;
            Ok(Self { file: Some(file) })
        }
    }

    /// Let the lock go, keeping the file open.
    pub fn unlock(&self) {
        let Some(file) = &self.file else {
            return;
        };
        #[cfg(windows)]
        record_range::unlock(file, self.record);
        #[cfg(not(windows))]
        {
            let _ = file.unlock();
        }
    }

    /// Transfer this Unix lock's original open description into close-only custody.
    ///
    /// The consumed ordinary guard no longer calls `LOCK_UN`, so closing one transferred
    /// guard leaves the lock held by its actual remaining duplicates. The final close lets
    /// the kernel release it. No path is reopened and no new lock or admission is inferred.
    /// Existing independent `File` duplicates must likewise not explicitly unlock it.
    #[cfg(unix)]
    #[must_use]
    pub fn into_close_only(self) -> CloseOnlyLock {
        self.into_sole_close_only().offered_out()
    }

    /// [`Self::into_close_only`] for a description this process opened and locked itself, no
    /// copy of which exists outside this guard: its last guard's drop lets it go by `LOCK_UN`
    /// unless a copy was offered out since ([`CloseOnlyLock::try_clone_fd`]).
    #[cfg(unix)]
    #[must_use]
    pub fn into_sole_close_only(mut self) -> CloseOnlyLock {
        CloseOnlyLock {
            file: self.file.take().expect("a live record guard owns its file"),
            custody: std::sync::Arc::new(Custody::default()),
        }
    }
}

impl std::ops::Deref for RecordLock {
    type Target = File;
    fn deref(&self) -> &File {
        self.file
            .as_ref()
            .expect("a live record guard owns its file")
    }
}

impl std::ops::DerefMut for RecordLock {
    fn deref_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("a live record guard owns its file")
    }
}

impl Drop for RecordLock {
    fn drop(&mut self) {
        self.unlock();
    }
}

/// Unix close-only custody of an actual transferred record-lock description.
///
/// This type has no path-only constructor or unlock method, and does not dereference to `File`.
/// A description that may have copies outside these guards ([`RecordLock::into_close_only`],
/// [`Self::try_clone_fd`], or one adopted from a sender by [`Self::adopt_inherited`]) is only
/// closed at a drop: its duplicates keep the same kernel lock until their final close. One in
/// sole custody ([`RecordLock::into_sole_close_only`]) and never offered out is let go by
/// `LOCK_UN` at its last guard's drop, because a close alone leaves the lock held in every child another
/// thread forked meanwhile until its `exec` (clippy.toml: the flock release rule). It provides
/// no resource admission or Timing evidence.
#[cfg(unix)]
#[derive(Debug)]
pub struct CloseOnlyLock {
    file: File,
    /// Shared by every in-process guard of this description ([`Self::try_clone`]).
    custody: std::sync::Arc<Custody>,
}

/// What every in-process guard of one description knows: whether a copy left their custody.
#[cfg(unix)]
#[derive(Debug, Default)]
struct Custody {
    offered_out: std::sync::atomic::AtomicBool,
}

#[cfg(unix)]
impl Drop for CloseOnlyLock {
    fn drop(&mut self) {
        // Unique custody means no other guard of this description is alive in this process;
        // two guards dropping at once both see a shared one and leave the release to the close.
        let last = std::sync::Arc::get_mut(&mut self.custody).is_some();
        if last
            && !self
                .custody
                .offered_out
                .load(std::sync::atomic::Ordering::Acquire)
        {
            let _ = self.file.unlock();
        }
    }
}

#[cfg(unix)]
impl CloseOnlyLock {
    /// Verify a received copy against this continuously retained original description.
    /// Matching path metadata alone is insufficient: an independent open of the same
    /// inode is blocked by this guard, while its actual shared description remains held.
    /// The received copy is never explicitly unlocked, including on refusal.
    /// # Errors
    /// The descriptor is not the original regular file or shared held description,
    /// or its metadata, duplication or lock observation cannot be read.
    pub fn verify_copy(&self, received: std::os::fd::BorrowedFd<'_>) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt as _;
        let invalid = |why| std::io::Error::new(std::io::ErrorKind::InvalidData, why);
        let original = self.file.metadata()?;
        let file = File::from(received.try_clone_to_owned()?);
        let offered = file.metadata()?;
        if !original.is_file()
            || !offered.is_file()
            || (original.dev(), original.ino()) != (offered.dev(), offered.ino())
        {
            return Err(invalid(
                "received lock does not name the retained original file",
            ));
        }
        match RecordLock::try_lock(file, 0) {
            Ok(held) => {
                self.custody
                    .offered_out
                    .store(true, std::sync::atomic::Ordering::Release);
                drop(held.into_close_only());
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(invalid("received lock is an independent description"));
            }
            Err(std::fs::TryLockError::Error(why)) => return Err(why),
        }
        Ok(())
    }

    /// Admit an explicitly inherited Unix lock descriptor against its trusted original path.
    ///
    /// Rejects standard descriptors, close-on-exec descriptors, non-regular files, a different
    /// inode, an unlocked fresh open, and an independent description blocked by the original
    /// holder. Refusal leaves the original number open and unchanged. Success closes that
    /// number and returns a close-on-exec copy with close-only drop behavior.
    ///
    /// The path and lock observations are checks on an actual controlled transfer, never new
    /// admission or recovery authority. They alone cannot establish historical provenance:
    /// another holder could release a lock between the two lock observations.
    ///
    /// # Safety
    /// Call before threads or callbacks can close, replace or otherwise use `fd`. An eligible
    /// descriptor must have been exclusively offered to this call with no remaining Rust or
    /// foreign owner; success consumes it, while refusal returns custody of the open number to
    /// the caller. Known ineligible candidates may be supplied solely for checked refusal.
    /// A transfer that may succeed requires genuine original close-only guard custody through
    /// the controlled spawn, adoption and acknowledgement. `expected` must be the independently
    /// validated original path, not authority reconstructed from a descriptor number, PID,
    /// environment value or untrusted pathname.
    ///
    /// # Errors
    /// An eligibility, identity or lock check fails, or metadata, duplication or opening fails.
    /// No refusal explicitly unlocks an inherited description or closes its original number.
    pub unsafe fn adopt_inherited(
        fd: std::os::fd::RawFd,
        expected: &std::path::Path,
    ) -> std::io::Result<Self> {
        use std::io;
        use std::os::fd::{BorrowedFd, FromRawFd as _, OwnedFd};
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

        let invalid = |why| io::Error::new(io::ErrorKind::InvalidData, why);
        if fd < 3 || !expected.is_absolute() {
            return Err(invalid(
                "lock adoption needs a non-stdio descriptor and an absolute original path",
            ));
        }
        // SAFETY: F_GETFD reads flags without taking ownership or touching memory. The caller
        // keeps this number stable before threads; an absent inherited number returns EBADF.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if flags & libc::FD_CLOEXEC != 0 {
            return Err(invalid(
                "the lock descriptor is close-on-exec, not an inherited offer",
            ));
        }
        // SAFETY: fd just answered and the caller forbids concurrent close/replacement. This
        // borrow never owns or closes it, including when duplication or a later check fails.
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let file = File::from(borrowed.try_clone_to_owned()?);
        let original = file.metadata()?;
        let named = std::fs::symlink_metadata(expected)?;
        let matches = |metadata: &std::fs::Metadata| {
            metadata.is_file()
                && metadata.dev() == original.dev()
                && metadata.ino() == original.ino()
        };
        if !original.is_file() || !matches(&named) {
            return Err(invalid(
                "the inherited lock does not open the original regular file",
            ));
        }
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(expected)?;
        if !matches(&probe.metadata()?) {
            return Err(invalid(
                "the original lock path changed while opening its probe",
            ));
        }
        match RecordLock::try_lock(probe, 0) {
            Err(std::fs::TryLockError::WouldBlock) => {}
            Ok(unheld) => {
                drop(unheld);
                return Err(invalid("no description holds the original lock"));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        let held = match RecordLock::try_lock(file, 0) {
            Ok(held) => held.into_close_only(),
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(invalid("another description holds the original lock"));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        };
        // Once the shared-description lock check succeeds, every remaining local path must
        // use close-only drop. A failed final check must not unlock the sender's copies.
        if !matches(&held.metadata()?) || !matches(&std::fs::symlink_metadata(expected)?) {
            return Err(invalid(
                "the original lock identity changed during adoption",
            ));
        }
        // SAFETY: all eligibility and transfer checks passed under the caller's exclusive,
        // stable startup ownership. This is the original number's only owner, closed once;
        // held already owns its CLOEXEC duplicate and keeps the shared lock through closure.
        drop(unsafe { OwnedFd::from_raw_fd(fd) });
        Ok(held)
    }

    /// Metadata from the original held file, without reopening a pathname.
    /// # Errors
    /// The held descriptor's metadata could not be read.
    pub fn metadata(&self) -> std::io::Result<std::fs::Metadata> {
        self.file.metadata()
    }

    /// Duplicate this actual description into another close-only guard.
    /// # Errors
    /// The OS could not duplicate the held descriptor.
    pub fn try_clone(&self) -> std::io::Result<Self> {
        self.file.try_clone().map(|file| Self {
            file,
            custody: std::sync::Arc::clone(&self.custody),
        })
    }

    /// Mark the description as possibly held outside these guards.
    fn offered_out(self) -> Self {
        self.custody
            .offered_out
            .store(true, std::sync::atomic::Ordering::Release);
        self
    }

    /// Duplicate this actual description as an owned close-on-exec descriptor for explicit
    /// spawn or inheritance plumbing. Its recipient must preserve close-only handling.
    /// # Errors
    /// The OS could not duplicate the held descriptor.
    pub fn try_clone_fd(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        let fd = std::os::fd::AsFd::as_fd(&self.file).try_clone_to_owned()?;
        self.custody
            .offered_out
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(fd)
    }
}

#[cfg(all(test, unix))]
#[path = "record_lock_tests.rs"]
mod tests;

/// [`RecordLock`]'s Windows range.
#[cfg(windows)]
mod record_range {
    use std::ffi::c_void;
    use std::fs::File;
    use std::os::windows::io::AsRawHandle as _;

    /// `OVERLAPPED`: the offset names the range.
    #[repr(C)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset: u32,
        offset_high: u32,
        event: *mut c_void,
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    /// `LockFileEx`'s answer when another handle holds part of the range.
    const ERROR_LOCK_VIOLATION: i32 = 33;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn LockFileEx(
            file: *mut c_void,
            flags: u32,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            file: *mut c_void,
            reserved: u32,
            bytes_low: u32,
            bytes_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    /// The `OVERLAPPED` naming `record` as the start.
    fn range(record: u64) -> (Overlapped, u32, u32) {
        let len = u64::MAX - record;
        let overlapped = Overlapped {
            internal: 0,
            internal_high: 0,
            offset: record as u32,
            offset_high: (record >> 32) as u32,
            event: std::ptr::null_mut(),
        };
        (overlapped, len as u32, (len >> 32) as u32)
    }

    pub(super) fn try_lock(file: &File, record: u64) -> Result<(), std::fs::TryLockError> {
        let (mut overlapped, low, high) = range(record);
        // SAFETY: `file` is an open, synchronous handle and `overlapped` a live `OVERLAPPED` for
        // the call.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                low,
                high,
                &mut overlapped,
            )
        };
        if ok != 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(ERROR_LOCK_VIOLATION) {
            Err(std::fs::TryLockError::WouldBlock)
        } else {
            Err(std::fs::TryLockError::Error(err))
        }
    }

    /// [`try_lock`]'s range, waiting while another handle holds part of it.
    pub(super) fn lock(file: &File, record: u64) -> std::io::Result<()> {
        let (mut overlapped, low, high) = range(record);
        // SAFETY: as in `try_lock`; without `LOCKFILE_FAIL_IMMEDIATELY` the call returns once
        // the range is this handle's.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK,
                0,
                low,
                high,
                &mut overlapped,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    pub(super) fn unlock(file: &File, record: u64) {
        let (mut overlapped, low, high) = range(record);
        // SAFETY: as in `try_lock`.
        let _ = unsafe { UnlockFileEx(file.as_raw_handle(), 0, low, high, &mut overlapped) };
    }
}
