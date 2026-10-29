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

impl FileLock {
    /// Open the lock file at `path` (created `0600` if absent).
    #[cfg(unix)]
    // Skip: OpenOptions::open for the lock file.
    #[cfg_attr(trust_verify, trust::skip)]
    fn open_lock_file(path: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC)
            .open(path)
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
        let deadline = std::time::Instant::now() + limit;
        loop {
            #[expect(
                clippy::disallowed_methods,
                reason = "the guard's own take: its Drop releases by LOCK_UN"
            )]
            let taken = file.try_lock();
            match taken {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
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
        let deadline = std::time::Instant::now() + limit;
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
                    if std::time::Instant::now() >= deadline {
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

    /// Windows: [`acquire_within`](Self::acquire_within) with a 5 s limit.
    #[cfg(windows)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        Self::acquire_within(path, std::time::Duration::from_secs(5))
    }
}

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
mod bounded_lock_tests {
    use super::FileLock;
    use std::time::Duration;

    /// A held lock times out `acquire_within` and is immediately free once dropped.
    #[test]
    fn acquire_within_times_out_while_another_holder_has_it() {
        let dir = std::env::temp_dir().join(format!("aterm-bounded-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("apply.lock");

        let held = FileLock::acquire(&path).expect("first holder takes the lock");
        let error = match FileLock::acquire_within(&path, Duration::from_millis(50)) {
            Ok(_) => panic!("a held lock must not be acquirable"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");

        drop(held);
        FileLock::acquire_within(&path, Duration::ZERO)
            .expect("the dropped lock must be free at the first try");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dropped lock is free immediately even while a cloned descriptor remains open.
    #[cfg(unix)]
    #[test]
    fn a_dropped_lock_is_free_while_a_copy_of_its_descriptor_lives() {
        let p = std::env::temp_dir().join(format!("aterm-lock-dup-{}", std::process::id()));
        let held = FileLock::acquire(&p).expect("acquire");
        let childs_copy = held._file.try_clone().expect("dup the lock's descriptor");
        drop(held);
        FileLock::acquire_within(&p, std::time::Duration::ZERO)
            .expect("free the moment its guard drops, whatever else holds the descriptor");
        drop(childs_copy);
        let open = || {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&p)
                .expect("open")
        };
        let held = FileLock::lock_open(open()).expect("lock an open file");
        let childs_copy = held._file.try_clone().expect("dup");
        drop(held);
        FileLock::acquire_within(&p, std::time::Duration::ZERO).expect("free");
        drop(childs_copy);
        let _ = std::fs::remove_file(&p);
    }

    /// `FileLock::acquire` sets `FD_CLOEXEC` on its descriptor.
    #[cfg(unix)]
    #[test]
    fn file_lock_is_close_on_exec() {
        use std::os::fd::AsRawFd;

        let p = std::env::temp_dir().join(format!("aterm-lock-cloexec-{}", std::process::id()));
        let lock = FileLock::acquire(&p).expect("acquire");
        // SAFETY: F_GETFD only reads descriptor flags from this live File.
        let flags = unsafe { libc::fcntl(lock._file.as_raw_fd(), libc::F_GETFD) };
        assert!(flags >= 0);
        assert_ne!(
            flags & libc::FD_CLOEXEC,
            0,
            "successful exec must release the apply transaction atomically"
        );
        drop(lock);
        let _ = std::fs::remove_file(p);
    }

    #[cfg(windows)]
    #[test]
    fn acquire_fails_fast_on_permanent_error() {
        let missing = std::env::temp_dir()
            .join(format!("aterm-no-such-dir-{}", std::process::id()))
            .join("lock");
        let err = match FileLock::acquire_within(&missing, std::time::Duration::from_secs(120)) {
            Ok(_) => panic!("acquire must fail on a missing parent"),
            Err(e) => e,
        };
        assert_ne!(err.kind(), std::io::ErrorKind::TimedOut);
        assert!(!matches!(err.raw_os_error(), Some(32 | 33)));
    }

    #[test]
    fn same_volume_true_within_a_dir_false_for_missing() {
        let d = std::env::temp_dir();
        let a = d.join(format!("aterm-sv-a-{}", std::process::id()));
        let b = d.join(format!("aterm-sv-b-{}", std::process::id()));
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"y").unwrap();
        assert!(
            same_volume(&a, &b),
            "two files in the same dir are same-volume"
        );
        assert!(
            !same_volume(&a, Path::new("/no/such/path-aterm-xyz")),
            "a missing path is not same-volume (fail closed)"
        );
        let _ = std::fs::remove_file(&a);
        let _ = std::fs::remove_file(&b);
    }
}
