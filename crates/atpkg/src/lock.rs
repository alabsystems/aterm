// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The store-wide single-writer lock — atpkg's **single-writer-per-store contract**.
//!
//! The coherence-group transaction ([`crate::flow`]) stages into the SAME
//! `staging/<program>/` and `store/<program>/<build>/` paths in every process, and an
//! aborting transaction DISCARDS the build dirs it staged
//! ([`crate::store::discard_build`]). That discard — and the check-then-act windows in
//! [`crate::linkmode`] reconciliation and the pin file's read-modify-write — are only
//! sound while **one process at a time mutates a store**. Two concurrent mutators (the
//! GUI Settings ▸ Packages worker, the 6-hour update loop, a second aterm instance, a
//! manual CLI invocation) could otherwise have one process's abort delete the very
//! build the other just activated: dangling shims, a wedged coherence tuple.
//!
//! So every verb that MUTATES the store TRY-acquires this advisory lock — a
//! `store.lock` file (`0600`) directly under the hardened pkg prefix — at the CLI edge
//! ([`crate::cli::main_entry`]) and holds it for the whole verb. Contention is
//! **fail-closed and LOUD**: the verb refuses with exit 1 naming the lock path (the
//! GUI page surfaces the child's stderr; the 6-hour loop simply retries next pass).
//! Read-only verbs never touch the lock. The kernel releases the lock when the holder
//! exits, so a crashed mutator can never wedge the store.
//!
//! The TRY semantics are why this does not reuse `aterm_update_core::FileLock`
//! directly: that primitive's `acquire` BLOCKS (`flock(LOCK_EX)`) — right for the
//! floor file's millisecond critical section ([`crate::sig::Floor`]), wrong for a
//! whole multi-minute install that must refuse, not queue. This is the same std
//! `flock` wrapper (`File::try_lock` IS `flock(fd, LOCK_EX | LOCK_NB)` on Unix,
//! `LockFileEx(LOCKFILE_FAIL_IMMEDIATELY)` on Windows) with the non-blocking flag,
//! and the same open discipline as that primitive (create `0600`, never truncate —
//! a lock file is a rendezvous, not data; std opens close-on-exec by default).

use std::fs::File;
use std::io;
use std::path::PathBuf;

use crate::store::Layout;

/// The store-wide writer lock, held for the lifetime of the value. Dropping it (or
/// the process exiting) releases it — `flock` is associated with the open file
/// description, so the kernel always cleans up after a crashed holder.
pub struct StoreLock {
    _file: File,
}

/// Why the store lock could not be taken. Both variants are refusals at the CLI
/// edge (fail-closed): a mutating verb never proceeds without the lock.
#[derive(Debug)]
pub enum StoreLockError {
    /// Another atpkg process holds the lock (the one-line loud refusal, §single-writer).
    Contended(PathBuf),
    /// The lock file could not be created/opened/locked for a non-contention reason
    /// (unwritable prefix, I/O error). Still a refusal — mutating without the lock
    /// would silently reopen the concurrent-transaction hazard.
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for StoreLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreLockError::Contended(path) => write!(
                f,
                "another atpkg process holds the store lock at {} — refusing to mutate \
                 the store concurrently (retry when it exits)",
                path.display()
            ),
            StoreLockError::Io(path, e) => write!(
                f,
                "cannot take the store lock at {}: {e} — refusing to mutate the store \
                 without it",
                path.display()
            ),
        }
    }
}

impl std::error::Error for StoreLockError {}

/// TRY-acquire the store-wide writer lock for `layout`'s store. Never blocks: a held
/// lock is [`StoreLockError::Contended`] immediately. Creates the (vetted) prefix
/// `0700` first — the lock must be takeable before a first install has built the
/// store — exactly as every other prefix writer does ([`crate::pin::set_pinned`]).
///
/// Factored off the CLI edge so lock contention is unit-testable in-process: two
/// `Layout`s over one prefix contend exactly like two processes do (`flock` treats
/// separate open file descriptions independently, same-process or not).
pub fn try_lock_store(layout: &Layout) -> Result<StoreLock, StoreLockError> {
    let path = layout.store_lock();
    if let Err(e) = crate::platform::ensure_private_dir(&layout.prefix) {
        return Err(StoreLockError::Io(path, e));
    }
    let mut opts = std::fs::OpenOptions::new();
    // Never truncate: a lock file is a rendezvous, not data (the FileLock discipline).
    opts.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let file = match opts.open(&path) {
        Ok(f) => f,
        Err(e) => return Err(StoreLockError::Io(path, e)),
    };
    match file.try_lock() {
        Ok(()) => Ok(StoreLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(StoreLockError::Contended(path)),
        Err(std::fs::TryLockError::Error(e)) => Err(StoreLockError::Io(path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn temp_layout(label: &str) -> Layout {
        let p = std::env::temp_dir().join(format!("atpkg-lock-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        Layout { prefix: p }
    }

    /// The single-writer contract, in-process: with the lock held (one `Layout`),
    /// a second acquisition over the SAME prefix (a second `Layout`, a distinct
    /// open file description — exactly what a second process presents) refuses
    /// IMMEDIATELY and LOUDLY, naming the lock path and the holder; dropping the
    /// guard releases it for the next acquisition.
    #[test]
    fn contended_store_lock_refuses_loudly_and_releases_on_drop() {
        let a = temp_layout("contend");
        let b = Layout {
            prefix: a.prefix.clone(),
        };
        let guard = try_lock_store(&a).expect("first acquisition succeeds");
        let err = match try_lock_store(&b) {
            Ok(_) => panic!("a held store lock must refuse a second mutator"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Contended(ref p) if *p == a.store_lock()),
            "contention names the lock path: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("another atpkg process holds the store lock"),
            "the refusal names the holder: {msg}"
        );
        assert!(
            msg.contains(&a.store_lock().display().to_string()),
            "the refusal names the lock path: {msg}"
        );
        drop(guard);
        let reacquired = try_lock_store(&b).expect("released lock is takeable again");
        drop(reacquired);
        let _ = std::fs::remove_dir_all(&a.prefix);
    }

    /// First contact: the prefix does not exist yet (nothing installed) — the lock
    /// must still be takeable (the first `install` needs it), and both the created
    /// prefix and the lock file carry the hardened modes.
    #[test]
    fn store_lock_creates_the_prefix_and_hardens_the_lock_file() {
        let l = temp_layout("fresh");
        assert!(!l.prefix.exists(), "fixture: no prefix yet");
        let guard = try_lock_store(&l).expect("lockable before the store exists");
        assert!(
            l.store_lock().is_file(),
            "store.lock created under the prefix"
        );
        #[cfg(unix)]
        {
            let dir_mode = std::fs::metadata(&l.prefix).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700, "prefix hardened 0700");
            let mode = std::fs::metadata(l.store_lock())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "store.lock is 0600");
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&l.prefix);
    }

    /// An unusable prefix (a plain FILE where the directory should be) is a
    /// fail-closed `Io` refusal, never a silent lock-free pass.
    #[test]
    fn unusable_prefix_refuses_instead_of_passing_lock_free() {
        let l = temp_layout("badprefix");
        std::fs::write(&l.prefix, b"not a directory").unwrap();
        let err = match try_lock_store(&l) {
            Ok(_) => panic!("a file-shaped prefix cannot yield a store lock"),
            Err(e) => e,
        };
        assert!(
            matches!(err, StoreLockError::Io(..)),
            "refusal is Io: {err:?}"
        );
        assert!(
            err.to_string().contains("refusing to mutate the store"),
            "the Io refusal is fail-closed too: {err}"
        );
        let _ = std::fs::remove_file(&l.prefix);
    }
}
