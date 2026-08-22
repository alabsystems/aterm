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

/// How often [`FileLock::acquire_within`] re-tests a held lock. Matches the
/// Windows branch's cadence: short enough that the common "holder finishes in
/// millis" case costs one tick, long enough that a full wait is ~20 syscalls a
/// second rather than a spin.
#[cfg(unix)]
const LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(50);

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
    fn open_lock_file(path: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
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
            .open(path)
    }

    #[cfg(unix)]
    pub fn acquire(path: &Path) -> io::Result<Self> {
        // The ascription is LOAD-BEARING: it is the lock-order census's File
        // evidence (aterm-census `is_file_binding_rhs`), which keeps the flock
        // below categorized as the cross-process advisory lock it is rather
        // than graphed as an in-process mutex. The helper refactor removed the
        // constructor from this binding and silently did exactly that.
        let file: std::fs::File = Self::open_lock_file(path)?;
        // std's `File::lock` IS `flock(fd, LOCK_EX)` on unix — the same syscall with
        // the same blocking semantics, released on close/drop exactly as a direct
        // `libc::flock` call, and failures map to the same `io::Error`. Using the
        // safe std wrapper keeps this crate free of direct FFI here.
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Acquire the lock, giving up after `limit` instead of waiting forever.
    ///
    /// The blocking [`acquire`](Self::acquire) is correct wherever an indefinite
    /// wait is merely a delay — a background stage, a durable counter, a
    /// publication transaction that returns in millis. It is NOT correct on the
    /// LAUNCH path. The apply lock is taken at the top of `main`, before the
    /// window exists, so a holder that is SIGSTOPped, paused under a debugger, or
    /// wedged on an unplugged volume turns "aterm starts a moment later" into
    /// "aterm never starts", with no window to say why and nothing to click.
    ///
    /// Windows has had a bounded wait since it was written, for exactly this
    /// reason (see the branch below); unix had none, purely because `flock`
    /// offers blocking as its default. Callers map the timeout to a normal launch
    /// on the build already installed, which is always a safe outcome.
    #[cfg(unix)]
    pub fn acquire_within(path: &Path, limit: std::time::Duration) -> io::Result<Self> {
        // Ascription load-bearing — census File evidence, as in `acquire`.
        let file: std::fs::File = Self::open_lock_file(path)?;
        let deadline = std::time::Instant::now() + limit;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                // Held by someone else: the ONLY retryable outcome.
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
                // A real error (EBADF, ENOLCK, an unsupported filesystem) is
                // PERMANENT — retrying it just burns the budget before surfacing
                // the same failure, exactly as the Windows branch reasons.
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }

    /// Windows: the lock is already a bounded retry loop, so the bounded and
    /// blocking spellings are the same operation. `limit` is accepted for a
    /// single call shape across platforms.
    #[cfg(windows)]
    pub fn acquire_within(path: &Path, _limit: std::time::Duration) -> io::Result<Self> {
        Self::acquire(path)
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

#[cfg(all(test, unix))]
mod bounded_lock_tests {
    use super::FileLock;
    use std::time::{Duration, Instant};

    /// THE FIX. A held lock must surface as a timeout, not as a launch that never
    /// finishes. `flock` is per open-file-description, so a second handle in this
    /// same process contends exactly as another process would.
    #[test]
    fn acquire_within_times_out_while_another_holder_has_it() {
        let dir = std::env::temp_dir().join(format!("aterm-bounded-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("apply.lock");

        let held = FileLock::acquire(&path).expect("first holder takes the lock");
        let started = Instant::now();
        let error = match FileLock::acquire_within(&path, Duration::from_millis(200)) {
            Ok(_) => panic!("a held lock must not be acquirable"),
            Err(error) => error,
        };
        let waited = started.elapsed();

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
        assert!(waited >= Duration::from_millis(150), "gave up too early: {waited:?}");
        assert!(waited < Duration::from_secs(5), "waited {waited:?}: it blocked");

        // And once the holder goes, the same call succeeds immediately.
        drop(held);
        let reacquired = Instant::now();
        FileLock::acquire_within(&path, Duration::from_secs(5)).expect("free lock is takeable");
        assert!(reacquired.elapsed() < Duration::from_secs(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An UNCONTENDED lock must cost essentially nothing — the bounded path must
    /// not introduce a poll-interval floor on the common case.
    #[test]
    fn acquire_within_is_immediate_when_the_lock_is_free() {
        let dir = std::env::temp_dir().join(format!("aterm-bounded-lock-free-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("apply.lock");
        let started = Instant::now();
        let _lock = FileLock::acquire_within(&path, Duration::from_secs(10)).expect("free lock");
        assert!(started.elapsed() < Duration::from_millis(50));
        let _ = std::fs::remove_dir_all(&dir);
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
