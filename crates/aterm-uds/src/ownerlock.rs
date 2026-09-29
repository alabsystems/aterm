// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO HOLDS A FILE'S OWNER LOCK — the probe the PTY keeper reads a dead
//! window's crash marker with (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.4 row
//! 3). It is the same probe `aterm-gui`'s `crash_signal::markers::probe` makes on
//! the same files: `open` read-only, NOT following a symlink and NOT blocking
//! on a FIFO, `flock(LOCK_EX|LOCK_NB)`, `LOCK_UN` when it was taken, `close`.
//! A marker's owner holds `LOCK_EX` on it for its whole life and the kernel
//! drops the lock when the owner dies, however it dies, so a lock this probe
//! can take says no process holds the marker open any more.
//!
//! `LOCK_UN` before `close` matters: `flock` binds to the open file
//! DESCRIPTION, and a child forked in the probe's microseconds would otherwise
//! keep the lock on the copy it inherited, and the next probe would read the
//! dead marker as live.

use std::path::Path;

/// What trying a file's owner lock says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Someone holds it: a live process has the file open with the lock.
    Live,
    /// The lock was free.
    Dead,
    /// The file could not be opened or locked for another reason (missing, a
    /// symlink, a platform with no wiring): no evidence either way.
    Unknown,
}

/// Probe `path`'s owner lock without waiting. See the module docs.
#[must_use]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
// Skip: bottoms out at the `open`/`flock`/`close` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn probe(path: &Path) -> Owner {
    use std::os::unix::ffi::OsStrExt as _;
    unsafe extern "C" {
        fn open(path: *const core::ffi::c_char, flags: i32, ...) -> i32;
        fn flock(fd: i32, op: i32) -> i32;
        fn close(fd: i32) -> i32;
    }
    #[cfg(target_vendor = "apple")]
    const FLAGS: i32 = 0x0100 /* O_NOFOLLOW */ | 0x0004 /* O_NONBLOCK */ | 0x0100_0000 /* O_CLOEXEC */;
    #[cfg(all(target_os = "linux", any(target_arch = "aarch64", target_arch = "arm")))]
    const FLAGS: i32 = 0o100_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */ | 0o2_000_000 /* O_CLOEXEC */;
    #[cfg(all(
        target_os = "linux",
        not(any(target_arch = "aarch64", target_arch = "arm"))
    ))]
    const FLAGS: i32 = 0o400_000 /* O_NOFOLLOW */ | 0o4000 /* O_NONBLOCK */ | 0o2_000_000 /* O_CLOEXEC */;
    #[cfg(target_vendor = "apple")]
    const EWOULDBLOCK: i32 = 35;
    #[cfg(target_os = "linux")]
    const EWOULDBLOCK: i32 = 11;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    const LOCK_UN: i32 = 8;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Owner::Unknown;
    };
    // SAFETY: `c_path` is a live NUL-terminated string for the call; O_RDONLY
    // (0) with no O_CREAT takes no mode argument.
    let fd = unsafe { open(c_path.as_ptr(), FLAGS) };
    if fd < 0 {
        return Owner::Unknown;
    }
    // SAFETY: `fd` was opened above and is closed below; LOCK_NB never waits.
    let locked = unsafe { flock(fd, LOCK_EX | LOCK_NB) } == 0;
    let err = std::io::Error::last_os_error().raw_os_error();
    if locked {
        // SAFETY: `fd` is the descriptor opened above; LOCK_UN never waits.
        unsafe { flock(fd, LOCK_UN) };
    }
    // SAFETY: closes the descriptor opened above exactly once.
    unsafe { close(fd) };
    if locked {
        Owner::Dead
    } else if err == Some(EWOULDBLOCK) {
        Owner::Live
    } else {
        Owner::Unknown
    }
}

/// Every other platform: no wiring, no evidence.
#[must_use]
#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
pub fn probe(_path: &Path) -> Owner {
    Owner::Unknown
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
mod tests {
    use super::*;

    unsafe extern "C" {
        fn flock(fd: i32, op: i32) -> i32;
        fn read(fd: i32, buf: *mut core::ffi::c_void, count: usize) -> isize;
        fn write(fd: i32, buf: *const core::ffi::c_void, count: usize) -> isize;
    }

    /// The holder lets go of its lock: `LOCK_UN`, THEN close — the order the
    /// probe itself keeps, for the same reason (module docs). `flock` binds to
    /// the open file DESCRIPTION, so a bare close frees the lock only when this
    /// descriptor was the description's last copy. In a test binary it often
    /// is not: a sibling test's `pre_exec` spawn holds a copy of every
    /// descriptor this process has open until its child execs, and a close
    /// landing in that window left the lock held — the probe read LIVE where
    /// DEAD was expected, 39 of 200 full `aterm-uds` runs (2026-09-29).
    fn release(file: std::fs::File) {
        use std::os::fd::AsRawFd as _;
        // SAFETY: a descriptor this test owns; LOCK_UN never waits.
        assert_eq!(unsafe { flock(file.as_raw_fd(), 8) }, 0, "LOCK_UN");
        drop(file);
    }

    /// A lock this process holds through another open is LIVE; once released
    /// it is DEAD; a missing file and a symlink are UNKNOWN.
    ///
    /// The release runs while a child this process forked is parked between
    /// `fork` and `exec` holding a copy of the holder's descriptor — the state
    /// every sibling test that spawns through `pre_exec` puts this process in
    /// for up to milliseconds at a time (`spawnfd`'s inheritance strip walks up
    /// to 65 536 descriptors there). Parking one here makes that interleaving
    /// the test's every run instead of its occasional one.
    #[test]
    fn the_probe_reads_the_owner_lock() {
        use std::io::{Read as _, Write as _};
        use std::os::fd::AsRawFd as _;
        use std::os::unix::process::CommandExt as _;
        let dir = std::env::temp_dir().join(format!("ownerlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        let file = std::fs::File::create(&path).expect("create");
        // SAFETY: a descriptor this test owns; LOCK_EX|LOCK_NB never waits.
        assert_eq!(unsafe { flock(file.as_raw_fd(), 2 | 4) }, 0);
        assert_eq!(probe(&path), Owner::Live, "held through another open");

        // PARK A CHILD between fork and exec: it writes one byte, then waits for
        // one. The read timeout is on the socket both processes share, so a
        // parent that never answers cannot strand the child in `pre_exec`.
        let (mut ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
        theirs
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .expect("timeout");
        let theirs_fd = theirs.as_raw_fd();
        let spawner = std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("/usr/bin/true");
            cmd.stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            // SAFETY: runs in the forked child before `exec` and calls only
            // `write(2)` and `read(2)` on a descriptor the parent keeps open
            // (`theirs`, alive until `status` returns), both async-signal-safe.
            unsafe {
                cmd.pre_exec(move || {
                    let mut byte = 0u8;
                    write(theirs_fd, (&raw const byte).cast(), 1);
                    read(theirs_fd, (&raw mut byte).cast(), 1);
                    Ok(())
                });
            }
            let status = cmd.status();
            drop(theirs);
            status
        });
        let mut parked = [0u8];
        ours.read_exact(&mut parked).expect("the child parked");
        release(file);
        // Read now, assert after the child is let go: a failed assertion here
        // must not leave it parked.
        let after_release = probe(&path);
        ours.write_all(b"g").expect("let the child exec");
        let status = spawner.join().expect("spawner").expect("spawn");
        assert!(status.success(), "{status}");
        assert_eq!(after_release, Owner::Dead, "the holder released it");

        assert_eq!(probe(&path), Owner::Dead, "the probe released its own lock");
        assert_eq!(probe(&dir.join("absent.log")), Owner::Unknown);
        let link = dir.join("link.log");
        std::os::unix::fs::symlink(&path, &link).expect("symlink");
        assert_eq!(probe(&link), Owner::Unknown, "a symlink is never followed");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
