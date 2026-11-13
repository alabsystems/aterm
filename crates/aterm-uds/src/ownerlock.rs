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
#[path = "ownerlock_tests.rs"]
mod tests;
