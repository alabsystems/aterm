// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The narrow filesystem primitives the updater needs, in their portable/
//! artifact-agnostic form: an advisory exclusive file lock (`flock(LOCK_EX)`, via
//! std's `File::lock`) for the apply/stage critical sections, and a same-volume
//! (`st_dev`) check. All safe Rust. The macOS `renamex_np(RENAME_SWAP)` directory
//! exchange stays in the `.app`-specific crate. Windows (where the updater is
//! inert) carries honest std-only approximations of both primitives so the crate
//! compiles everywhere.

use std::fs::File;
use std::io;
use std::path::Path;

/// An advisory exclusive lock held for the lifetime of the value. Dropping it (or
/// the process exiting / `exec`ing) releases the lock — `flock` is associated with
/// the open file description, so the kernel always cleans up.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Acquire `LOCK_EX` on `path` (created `0600` if absent), blocking until the
    /// lock is available. Blocking is fine: this runs before the window exists and
    /// the holder either re-execs (releasing immediately) or returns in millis.
    #[cfg(unix)]
    // Skip: OpenOptions::open for the advisory lock file — hardened raw_path
    // class; the lock path is private-dir confined and the lock is advisory
    // by design (a raced path costs a retry, never corruption). Audited
    // (update-atpkg).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            // A lock file is a rendezvous, not data — never clobber its contents.
            .truncate(false)
            .mode(0o600)
            // Apply intentionally holds this guard through execve: the kernel must
            // release the transaction lock atomically with successful image
            // replacement, while an exec error leaves it held for rollback.
            .custom_flags(libc::O_CLOEXEC)
            .open(path)?;
        // std's `File::lock` IS `flock(fd, LOCK_EX)` on unix — the same syscall with
        // the same blocking semantics, released on close/drop exactly as a direct
        // `libc::flock` call, and failures map to the same `io::Error`. Using the
        // safe std wrapper keeps this crate free of direct FFI here.
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Windows approximation of the advisory lock: open the file with
    /// `share_mode(0)` (exclusive — a second open by anyone fails with a sharing
    /// violation) in a bounded retry loop. The updater is inert on Windows
    /// (`enabled()` is already `false`), so an approximate advisory lock is an
    /// honest stand-in; the OS releases it when the handle closes, matching the
    /// flock open-file-description cleanup guarantee.
    #[cfg(windows)]
    // Skip: OpenOptions::open for the advisory lock file — hardened raw_path
    // class; the lock path is private-dir confined and the lock is advisory
    // by design (a raced path costs a retry, never corruption). Audited
    // (update-atpkg).
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        // Only a sharing/lock violation means "someone else holds it" — retry
        // those. ERROR_SHARING_VIOLATION (32) is what a competing `share_mode(0)`
        // open returns; ERROR_LOCK_VIOLATION (33) is the byte-range-lock cousin.
        // Any other error (missing parent dir = 3, access-denied ACL = 5, invalid
        // path, …) is PERMANENT: retrying just burns the full ~5 s before the real
        // error surfaces, so return it immediately (matching the unix branch, where
        // flock only blocks after a successful open).
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        let mut last_err = None;
        // Bounded wait (~5 s) rather than flock's indefinite block: holders
        // release in millis, and a wedged holder should surface as an error.
        for _ in 0..100 {
            match std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                // A lock file is a rendezvous, not data — never clobber its contents.
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
                    last_err = Some(e);
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(io::ErrorKind::TimedOut, "could not acquire update lock")
        }))
    }
}

/// Whether `a` and `b` live on the same filesystem volume (`st_dev`). Required
/// before attempting an in-volume atomic directory exchange (`RENAME_SWAP` is
/// in-volume only), which the consuming crate performs.
#[cfg(unix)]
// Skip: fs::metadata dev-id comparison — hardened raw_path class; a raced
// direntry only flips the answer to the CONSERVATIVE cross-volume copy
// path (fail-safe, never fail-open). Audited (update-atpkg).
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

/// Windows approximation of the `st_dev` check: compare the leading path
/// component (drive letter or UNC share prefix). There is no `st_dev` here;
/// two paths under the same drive/share prefix are treated as same-volume
/// (mount points nested inside a drive are not detected — an acceptable
/// approximation while the updater is inert on Windows). A missing path is
/// never same-volume (fail closed), matching the unix behavior.
#[cfg(windows)]
// Skip: fs::metadata dev-id comparison — hardened raw_path class; a raced
// direntry only flips the answer to the CONSERVATIVE cross-volume copy
// path (fail-safe, never fail-open). Audited (update-atpkg).
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_lock_releases_on_drop() {
        let p = std::env::temp_dir().join(format!("aterm-lock-{}", std::process::id()));
        {
            let _l = FileLock::acquire(&p).expect("first acquire");
        } // dropped here → released
        // Re-acquiring after the previous guard dropped must succeed (no deadlock).
        let l2 = FileLock::acquire(&p).expect("re-acquire after release");
        drop(l2);
        let _ = std::fs::remove_file(&p);
    }

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

    // A permanent open failure (here: parent directory does not exist → os error 3)
    // must surface IMMEDIATELY, not after the full ~5 s retry budget that is reserved
    // for genuine sharing-violation contention.
    #[cfg(windows)]
    #[test]
    fn acquire_fails_fast_on_permanent_error() {
        let missing = std::env::temp_dir()
            .join(format!("aterm-no-such-dir-{}", std::process::id()))
            .join("lock");
        let start = std::time::Instant::now();
        let err = match FileLock::acquire(&missing) {
            Ok(_) => panic!("acquire must fail on a missing parent"),
            Err(e) => e,
        };
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "permanent error must not burn the retry budget (took {:?})",
            start.elapsed()
        );
        // Not a sharing/lock violation, so the ORIGINAL os error is preserved (not a
        // TimedOut placeholder).
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
