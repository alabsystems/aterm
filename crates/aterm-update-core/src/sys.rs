// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Portable filesystem primitives for the updater: an advisory exclusive file lock and a same-volume (`st_dev`) check.

use std::fs::File;
use std::io;
use std::path::Path;

/// An advisory exclusive lock held for the lifetime of the value; on unix, drop releases it by
/// `LOCK_UN`.
pub struct FileLock {
    _file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = self._file.unlock();
    }
}

/// Poll cadence for [`FileLock::acquire_within`] while a lock is held.
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(50);

#[cfg(any(windows, all(test, unix)))]
const WINDOWS_LOCK_HANG: std::time::Duration = std::time::Duration::from_secs(60);

impl FileLock {
    /// Open the lock file at `path` (created `0600` if absent).
    #[cfg(unix)]
    // Skip: OpenOptions::open for the lock file.
    #[cfg_attr(trust_verify, trust::skip)]
    fn open_lock_file(path: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = aterm_uds::kept_out(|| {
            std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(path)
        })?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        Ok(file)
    }

    /// Take `LOCK_EX` on `path` (created `0600` if absent), blocking until free.
    #[cfg(unix)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        let file: std::fs::File = Self::open_lock_file(path)?;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Take blocking `LOCK_EX` on an already-opened file without creating its rendezvous.
    pub fn lock_open(file: File) -> io::Result<Self> {
        let file: std::fs::File = file;
        #[expect(
            clippy::disallowed_methods,
            reason = "the guard's own take: its Drop releases by LOCK_UN"
        )]
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Acquire the lock within `limit`, returning `TimedOut` if still held.
    #[cfg(unix)]
    pub fn acquire_within(path: &Path, limit: std::time::Duration) -> io::Result<Self> {
        let file: std::fs::File = Self::open_lock_file(path)?;
        let deadline = std::time::Instant::now().checked_add(limit);
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "another process has held the update lock for more than {}s",
                                limit.as_secs()
                            ),
                        ));
                    }
                    std::thread::sleep(LOCK_POLL);
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }

    /// Windows approximation of the advisory lock via an exclusive `share_mode(0)` open retried until `limit`.
    #[cfg(windows)]
    // Skip: as the unix `open_lock_file`.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn acquire_within(path: &Path, limit: std::time::Duration) -> io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        let deadline = std::time::Instant::now().checked_add(limit);
        loop {
            match std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => return Ok(Self { _file: file }),
                Err(e) => {
                    if !matches!(
                        e.raw_os_error(),
                        Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
                    ) {
                        return Err(e);
                    }
                    if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "another process has held the update lock for more than {}s",
                                limit.as_secs()
                            ),
                        ));
                    }
                }
            }
            std::thread::sleep(LOCK_POLL);
        }
    }

    /// Windows: [`acquire_within`](Self::acquire_within) with a minute hang detector.
    #[cfg(windows)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        Self::acquire_required(path, std::time::Instant::now(), Self::acquire_within)
    }

    #[cfg(any(windows, all(test, unix)))]
    fn acquire_required(
        path: &Path,
        started: std::time::Instant,
        take: impl FnOnce(&Path, std::time::Duration) -> io::Result<Self>,
    ) -> io::Result<Self> {
        let left = WINDOWS_LOCK_HANG.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "required update lock wait expired",
            ));
        }
        take(path, left)
    }
}

#[cfg(all(test, any(unix, windows)))]
#[path = "sys_required_lock_tests.rs"]
mod required_lock_tests;

/// Whether `a` and `b` live on the same filesystem volume (`st_dev`).
#[cfg(unix)]
// Skip: fs::metadata over caller paths.
#[cfg_attr(trust_verify, trust::skip)]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(ma), Ok(mb)) => {
            use std::os::unix::fs::MetadataExt;
            ma.dev() == mb.dev()
        }
        _ => false,
    }
}

/// Windows approximation of the `st_dev` check via drive letter or UNC share prefix.
#[cfg(windows)]
// Skip: as the unix `same_volume`.
#[cfg_attr(trust_verify, trust::skip)]
pub fn same_volume(a: &Path, b: &Path) -> bool {
    use std::path::Component;
    if std::fs::metadata(a).is_err() || std::fs::metadata(b).is_err() {
        return false;
    }
    match (a.components().next(), b.components().next()) {
        (Some(Component::Prefix(pa)), Some(Component::Prefix(pb))) => pa == pb,
        _ => false,
    }
}

#[cfg(all(test, unix))]
#[path = "sys_bounded_lock_tests.rs"]
mod bounded_lock_tests;

#[cfg(test)]
#[path = "sys_tests.rs"]
mod tests;
