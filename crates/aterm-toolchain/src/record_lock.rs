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
    pub fn into_close_only(mut self) -> CloseOnlyLock {
        CloseOnlyLock {
            file: self.file.take().expect("a live record guard owns its file"),
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
/// Its ordinary drop only closes its descriptor; duplicates keep the same kernel lock
/// until their final close. It provides no resource admission or Timing evidence.
#[cfg(unix)]
#[derive(Debug)]
pub struct CloseOnlyLock {
    file: File,
}

#[cfg(unix)]
impl CloseOnlyLock {
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
        self.file.try_clone().map(|file| Self { file })
    }

    /// Duplicate this actual description as an owned close-on-exec descriptor for explicit
    /// spawn or inheritance plumbing. Its recipient must preserve close-only handling.
    /// # Errors
    /// The OS could not duplicate the held descriptor.
    pub fn try_clone_fd(&self) -> std::io::Result<std::os::fd::OwnedFd> {
        std::os::fd::AsFd::as_fd(&self.file).try_clone_to_owned()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs::{Metadata, OpenOptions, TryLockError};
    use std::os::fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _, OwnedFd, RawFd};
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    use aterm_spec::wait::{MUST_HAPPEN, until_within};

    fn create(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .unwrap()
    }

    fn open(path: &Path) -> File {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .unwrap()
    }

    fn identity(metadata: Metadata) -> (u64, u64) {
        (metadata.dev(), metadata.ino())
    }

    fn assert_blocked(path: &Path) {
        assert!(
            matches!(
                RecordLock::try_lock(open(path), 0),
                Err(TryLockError::WouldBlock)
            ),
            "an independent open must observe the actual original lock at {}",
            path.display()
        );
    }

    fn reacquire_after_final_close(path: &Path) -> RecordLock {
        // A sibling test's concurrent fork can briefly inherit a CLOEXEC descriptor until
        // its exec. Require actual independent reacquisition, with only a hang detector.
        until_within(
            MUST_HAPPEN,
            "the original lock's final close",
            || match RecordLock::try_lock(open(path), 0) {
                Ok(guard) => Some(guard),
                Err(TryLockError::WouldBlock) => None,
                Err(TryLockError::Error(error)) => panic!("reacquire {}: {error}", path.display()),
            },
        )
    }

    fn flags(fd: RawFd) -> i32 {
        // SAFETY: F_GETFD only queries this number, returning EBADF when it is not open.
        unsafe { libc::fcntl(fd, libc::F_GETFD) }
    }

    fn inherited_offer(fd: OwnedFd) -> RawFd {
        let previous = flags(fd.as_raw_fd());
        assert!(previous >= 0);
        // SAFETY: this fixture alone owns fd. Clearing its descriptor flag models the actual
        // spawn inheritance offer, without changing stdio or any other descriptor.
        assert_eq!(
            unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, previous & !libc::FD_CLOEXEC) },
            0
        );
        fd.into_raw_fd()
    }

    fn reclaim_refused(fd: RawFd, expected: (u64, u64)) -> OwnedFd {
        assert_eq!(
            flags(fd) & libc::FD_CLOEXEC,
            0,
            "refusal must not close or mark the original"
        );
        // SAFETY: this fixture offered its sole owner and adoption refused without taking it.
        // Nothing else closes or replaces the still-open, exclusively offered number.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        let file = File::from(fd);
        assert_eq!(identity(file.metadata().unwrap()), expected);
        file.into()
    }

    #[test]
    fn inherited_actual_guard_adopts_cloexec_and_keeps_lock_after_sender_closes() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("offered.lock");
        let sender = RecordLock::try_lock(create(&path), 0)
            .unwrap()
            .into_close_only();
        let original = identity(sender.metadata().unwrap());
        let offered = inherited_offer(sender.try_clone_fd().unwrap());
        // SAFETY: this test exclusively offers the actual sender's shared description; the
        // sender remains held throughout adoption and the subsequent acknowledgement checks.
        let adopted = unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.unwrap();
        assert_eq!(identity(adopted.metadata().unwrap()), original);
        assert_ne!(adopted.file.as_raw_fd(), offered);
        assert_ne!(flags(adopted.file.as_raw_fd()) & libc::FD_CLOEXEC, 0);
        let exported = adopted.try_clone_fd().unwrap();
        assert_ne!(flags(exported.as_raw_fd()) & libc::FD_CLOEXEC, 0);
        assert_blocked(&path);
        drop(sender);
        assert_blocked(&path);
        drop(adopted);
        assert_blocked(&path);
        drop(exported);
        assert_eq!(
            identity(reacquire_after_final_close(&path).metadata().unwrap()),
            original
        );
    }

    #[test]
    fn inherited_adoption_refuses_wrong_inode_unheld_and_independent_descriptions() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("original.lock");
        let sender = RecordLock::try_lock(create(&path), 0)
            .unwrap()
            .into_close_only();
        let original = identity(sender.metadata().unwrap());
        let other_path = world.path().join("other.lock");
        let other = create(&other_path);
        let other_identity = identity(other.metadata().unwrap());
        let offered = inherited_offer(other.into());
        // SAFETY: this fixture exclusively offers a stable raw descriptor. The independently
        // held original prevents an unrelated OFD from being mistaken for shared custody.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
        drop(reclaim_refused(offered, other_identity));
        assert_blocked(&path);

        let independent = inherited_offer(open(&path).into());
        // SAFETY: the same stable, exclusive offer, but this description is deliberately not
        // the sender's. The genuine sender remains continuously held through the refusal.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(independent, &path) }.is_err());
        drop(reclaim_refused(independent, original));
        assert_blocked(&path);

        let unlocked = inherited_offer(open(&other_path).into());
        // SAFETY: the offered original file is unheld, intentionally failing the lock proof.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(unlocked, &other_path) }.is_err());
        drop(reclaim_refused(unlocked, other_identity));
        let unheld = RecordLock::try_lock(open(&other_path), 0).unwrap();
        drop(unheld);

        let moved = world.path().join("moved.lock");
        let offered = inherited_offer(sender.try_clone_fd().unwrap());
        std::fs::rename(&path, &moved).unwrap();
        drop(create(&path));
        // SAFETY: actual shared offer remains continuously held; the named inode was replaced.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
        drop(reclaim_refused(offered, original));
        assert_blocked(&moved);
    }

    #[test]
    fn inherited_adoption_leaves_stdio_cloexec_and_nonregular_numbers_untouched() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("original.lock");
        let sender = RecordLock::try_lock(create(&path), 0)
            .unwrap()
            .into_close_only();
        for fd in [0, 1, 2, -1] {
            let before = flags(fd);
            // SAFETY: ineligible standard/negative numbers are never owned or changed by this
            // API; this call makes no offer of the test process's standard streams.
            assert!(unsafe { CloseOnlyLock::adopt_inherited(fd, &path) }.is_err());
            assert_eq!(flags(fd), before);
        }
        let cloexec = sender.try_clone_fd().unwrap();
        let before = flags(cloexec.as_raw_fd());
        assert_ne!(before & libc::FD_CLOEXEC, 0);
        // SAFETY: this known ineligible CLOEXEC descriptor remains owned by this fixture.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(cloexec.as_raw_fd(), &path) }.is_err());
        assert_eq!(flags(cloexec.as_raw_fd()), before);
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let socket: OwnedFd = socket.into();
        let socket_identity = identity(File::from(socket.try_clone().unwrap()).metadata().unwrap());
        let offered = inherited_offer(socket);
        // SAFETY: exclusive stable offer of a socket, intentionally ineligible as a file guard.
        assert!(unsafe { CloseOnlyLock::adopt_inherited(offered, &path) }.is_err());
        drop(reclaim_refused(offered, socket_identity));
        assert_blocked(&path);
    }

    #[test]
    fn ordinary_guard_drop_still_unlocks_an_independent_file_duplicate() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("ordinary.lock");
        let ordinary = RecordLock::try_lock(create(&path), 0).unwrap();
        let duplicate = ordinary.try_clone().unwrap();
        let original = identity(ordinary.metadata().unwrap());
        assert_eq!(identity(duplicate.metadata().unwrap()), original);
        assert_blocked(&path);
        drop(ordinary);
        // A plain File copy does not change the ordinary guard's explicit-unlock contract.
        let reacquired = RecordLock::try_lock(open(&path), 0).unwrap();
        assert_eq!(identity(reacquired.metadata().unwrap()), original);
        drop(reacquired);
        drop(duplicate);
    }

    #[test]
    fn consumed_transfer_and_actual_duplicates_hold_until_the_final_close() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("transferred.lock");
        let ordinary = RecordLock::try_lock(create(&path), 0).unwrap();
        let original = identity(ordinary.metadata().unwrap());
        let held = ordinary.into_close_only();
        assert_eq!(identity(held.metadata().unwrap()), original);
        assert_blocked(&path);
        let duplicate = held.try_clone().unwrap();
        assert_eq!(identity(duplicate.metadata().unwrap()), original);
        let descriptor = held.try_clone_fd().unwrap();
        drop(held);
        assert_blocked(&path);
        drop(duplicate);
        assert_blocked(&path);
        drop(descriptor);
        let reacquired = reacquire_after_final_close(&path);
        assert_eq!(identity(reacquired.metadata().unwrap()), original);
    }

    #[test]
    fn close_only_copies_preserve_the_original_inode_when_its_name_is_replaced() {
        let world = aterm_tempfile::tempdir().unwrap();
        let path = world.path().join("named.lock");
        let original_path = world.path().join("original.lock");
        let held = RecordLock::try_lock(create(&path), 0)
            .unwrap()
            .into_close_only();
        let original = identity(held.metadata().unwrap());
        std::fs::rename(&path, &original_path).unwrap();
        let replacement = create(&path);
        assert_ne!(identity(replacement.metadata().unwrap()), original);
        assert_eq!(identity(held.metadata().unwrap()), original);
        let duplicate = held.try_clone().unwrap();
        assert_eq!(identity(duplicate.metadata().unwrap()), original);
        // The changed pathname is an independent inode: it supplies no custody of the old one.
        let unrelated = RecordLock::try_lock(open(&path), 0).unwrap();
        assert_blocked(&original_path);
        drop(held);
        assert_blocked(&original_path);
        drop(duplicate);
        let reacquired = reacquire_after_final_close(&original_path);
        assert_eq!(identity(reacquired.metadata().unwrap()), original);
        drop(unrelated);
    }
}

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
