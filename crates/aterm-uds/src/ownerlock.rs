// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Non-blocking `flock` owner-lock probe for crash marker files.

use std::path::Path;

/// Result of probing a file's owner lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// A live process holds the lock.
    Live,
    /// No holder the probe looks for: [`probe`] found no exclusive holder, [`probe_any`] none at
    /// all.
    Dead,
    /// The file could not be opened or locked as a regular file, or this platform has no wiring.
    Unknown,
}

/// Which lock a probe tries for.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[derive(Clone, Copy)]
enum Take {
    /// `LOCK_SH`: only an exclusive holder refuses it, so one probe never refuses another.
    Shared,
    /// `LOCK_EX`: any holder refuses it, another probe included.
    Exclusive,
}

/// Probes `path`'s owner lock without blocking, for a lock whose owner holds it exclusively, as
/// each of aterm's own does: `Live` means such an owner. It tries for the lock shared, so two
/// processes reading one file at once never read each other as its owner. An owner taking the
/// lock non-blocking while a probe holds it is still refused, for the microseconds of that hold.
#[must_use]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[cfg_attr(trust_verify, trust::skip)]
pub fn probe(path: &Path) -> Owner {
    probe_while_locked(path, Take::Shared, || {})
}

/// [`probe`] for a lock another program keeps, which may hold it shared (Cargo's build locks, a
/// Codex writer's): `Live` means any lock at all, so another prober's own reads `Live` too.
#[must_use]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[cfg_attr(trust_verify, trust::skip)]
pub fn probe_any(path: &Path) -> Owner {
    probe_while_locked(path, Take::Exclusive, || {})
}

/// A probe taking `take`, running `while_locked` while its own descriptor holds the lock (never,
/// when the lock was not free). The tests fork there to check that the `LOCK_UN` happens.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
#[cfg_attr(trust_verify, trust::skip)]
fn probe_while_locked(path: &Path, take: Take, while_locked: impl FnOnce()) -> Owner {
    use std::os::unix::fs::OpenOptionsExt as _;
    #[cfg(target_vendor = "apple")]
    const FLAGS: i32 = 0x0100 /* O_NOFOLLOW */ | 0x0004 /* O_NONBLOCK */;
    #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
    const FLAGS: i32 = 0o100_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm"))
    ))]
    const FLAGS: i32 = 0o400_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */;
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS)
        .open(path)
    else {
        return Owner::Unknown;
    };
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return Owner::Unknown;
    }
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe: a lock it takes is let go by unlock() before its close"
    )]
    let taken = match take {
        Take::Shared => file.try_lock_shared(),
        Take::Exclusive => file.try_lock(),
    };
    match taken {
        Ok(()) => {
            while_locked();
            let _ = file.unlock();
            Owner::Dead
        }
        Err(std::fs::TryLockError::WouldBlock) => Owner::Live,
        Err(std::fs::TryLockError::Error(_)) => Owner::Unknown,
    }
}

/// Every other platform: no wiring, no evidence.
#[must_use]
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn probe(_path: &Path) -> Owner {
    Owner::Unknown
}

/// Every other platform: no wiring, no evidence.
#[must_use]
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn probe_any(_path: &Path) -> Owner {
    Owner::Unknown
}

#[cfg(test)]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
mod tests {
    use super::*;
    use crate::fork_window::Parked;

    fn release(file: std::fs::File) {
        file.unlock().expect("LOCK_UN");
        drop(file);
    }

    #[test]
    fn the_probe_reads_the_owner_lock() {
        let dir = std::env::temp_dir().join(format!("ownerlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        let file = std::fs::File::create(&path).expect("create");
        #[expect(
            clippy::disallowed_methods,
            reason = "the test's stand-in owner lets go by `release`: LOCK_UN, then the close"
        )]
        file.try_lock().expect("LOCK_EX|LOCK_NB");
        assert_eq!(probe(&path), Owner::Live, "held through another open");
        assert_eq!(probe_any(&path), Owner::Live, "held through another open");

        let parked = Parked::park();
        release(file);
        assert_eq!(probe(&path), Owner::Dead, "the holder released it");
        let status = parked.release();
        assert!(status.success(), "{status}");

        assert_eq!(probe(&path), Owner::Dead, "the probe released its own lock");
        assert_eq!(probe(&dir.join("absent.log")), Owner::Unknown);
        assert_eq!(probe(&dir), Owner::Unknown, "a directory is no owner lock");
        let link = dir.join("link.log");
        std::os::unix::fs::symlink(&path, &link).expect("symlink");
        assert_eq!(probe(&link), Owner::Unknown, "a symlink is never followed");
        let fifo = dir.join("fifo.log");
        let c_fifo = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).expect("path");
        // SAFETY: a NUL-terminated path that outlives the call.
        assert_eq!(unsafe { libc::mkfifo(c_fifo.as_ptr(), 0o600) }, 0, "mkfifo");
        let answer = probe(&fifo);
        if cfg!(target_vendor = "apple") {
            assert_eq!(answer, Owner::Unknown, "Darwin's flock refuses a FIFO");
        } else {
            assert_ne!(answer, Owner::Live, "nobody holds a FIFO's lock");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_child_forked_mid_probe_does_not_keep_the_probes_lock() {
        let dir = std::env::temp_dir().join(format!("ownerlock-mid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        drop(std::fs::File::create(&path).expect("create"));

        for take in [Take::Shared, Take::Exclusive] {
            let mut parked = None;
            let first = probe_while_locked(&path, take, || parked = Some(Parked::park()));
            assert_eq!(first, Owner::Dead, "nobody holds it");
            let parked = parked.expect("the probe took the free lock and forked holding it");
            assert_eq!(
                probe_any(&path),
                Owner::Dead,
                "the child forked mid-probe kept the probe's lock: the probe closed without \
                 LOCK_UN"
            );
            let status = parked.release();
            assert!(status.success(), "{status}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_probe_under_way_is_no_owner_to_another_probe() {
        let dir = std::env::temp_dir().join(format!("ownerlock-two-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        drop(std::fs::File::create(&path).expect("create"));

        let mut second = None;
        let first = probe_while_locked(&path, Take::Shared, || second = Some(probe(&path)));
        assert_eq!(first, Owner::Dead, "nobody holds it");
        assert_eq!(
            second,
            Some(Owner::Dead),
            "the first probe read as an owner"
        );

        let mut any = None;
        probe_while_locked(&path, Take::Shared, || any = Some(probe_any(&path)));
        assert_eq!(any, Some(Owner::Live), "probe_any reads any lock at all");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
