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
// Skip: the body is `probe_while_locked`'s, which bottoms out at FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
pub fn probe(path: &Path) -> Owner {
    probe_while_locked(path, || {})
}

/// [`probe`], running `while_locked` at the one moment the probe's own
/// descriptor holds the lock: after its `flock` took it, before its `LOCK_UN`
/// (never, when the lock was not free). The shipped probe runs nothing there.
/// The tests fork there, which is the only way to put a child between `fork`
/// and `exec` holding a copy of the probe's descriptor on every run, and so the
/// only way a test can see that the `LOCK_UN` is there (module docs).
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
// Skip: bottoms out at the `open`/`flock`/`close` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_while_locked(path: &Path, while_locked: impl FnOnce()) -> Owner {
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
        while_locked();
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

// Two attributes, not `cfg(all(test, ..))`: the wasm-process census (OB-12)
// masks unshipped items by the exact spelling `#[cfg(test)]`, and this module
// spawns a thread, which that census refuses anywhere it cannot see is test-only.
#[cfg(test)]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

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

    /// What the parked child execs once it is let go: the POSIX shell, told to
    /// exit 0. `/bin/sh` is where every unix this module is wired for (macOS,
    /// Linux — NixOS included, which has no `/usr/bin/true`) keeps it, and the
    /// crate's other spawning tests use it too.
    const SHELL: &str = "/bin/sh";

    /// A CHILD PARKED BETWEEN `fork` AND `exec`, holding a copy of every
    /// descriptor this process had open when it forked — the state every
    /// sibling test that spawns through `pre_exec` puts this process in for up
    /// to milliseconds at a time (`spawnfd`'s inheritance strip walks up to
    /// 65 536 descriptors there). Parking one makes that interleaving a test's
    /// every run instead of its occasional one.
    ///
    /// Letting it go is unconditional: [`Parked::release`] does it and returns
    /// the child's exit status, and `Drop` does it on every other path — a
    /// failed assertion unwinding past a parked child lets it exec and exit at
    /// once instead of holding every descriptor for its 10 s read timeout. The
    /// child only ever ends by exiting: it execs `/bin/sh -c 'exit 0'` whether
    /// it was answered, reached end of file, or timed out, and nothing here
    /// makes its write raise `SIGPIPE` (its socket's peer is never shut for
    /// reading, and the child holds a copy of that peer itself).
    struct Parked {
        ours: std::os::unix::net::UnixStream,
        spawner: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
    }

    impl Parked {
        /// Fork a child and return once it is parked. The child writes one
        /// byte, then waits for one. The read timeout is on the socket both
        /// processes share, so even a parent that dies cannot strand the child
        /// in `pre_exec` past 10 s.
        fn park() -> Self {
            use std::os::fd::AsRawFd as _;
            use std::os::unix::process::CommandExt as _;
            let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("pair");
            theirs
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .expect("timeout");
            let theirs_fd = theirs.as_raw_fd();
            let spawner = std::thread::spawn(move || {
                let mut cmd = std::process::Command::new(SHELL);
                cmd.args(["-c", "exit 0"])
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                // SAFETY: runs in the forked child before `exec` and calls only
                // `write(2)` and `read(2)` on a descriptor the parent keeps open
                // (`theirs`, alive until `status` returns), both
                // async-signal-safe.
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
            // Built before the wait, so a child that never parks is still
            // let go and joined by `Drop` when the wait panics.
            let mut parked = Self {
                ours,
                spawner: Some(spawner),
            };
            let mut byte = [0u8];
            parked
                .ours
                .read_exact(&mut byte)
                .expect("the child parked between fork and exec");
            parked
        }

        /// Let the child exec and exit, and return how it exited.
        fn release(mut self) -> std::process::ExitStatus {
            self.let_go();
            let spawner = self.spawner.take().expect("released once");
            match spawner.join().expect("the spawner thread") {
                Ok(status) => status,
                Err(e) => panic!(
                    "spawning `{SHELL} -c 'exit 0'` for the parked child failed: {e} \
                     (the child forks and parks before its exec, so this is the exec)"
                ),
            }
        }

        /// Answer the child's read, then end the stream so a read that missed
        /// the byte sees end of file. Errors are ignored: a child that already
        /// exited needs nothing.
        fn let_go(&mut self) {
            let _ = self.ours.write_all(b"g");
            let _ = self.ours.shutdown(std::net::Shutdown::Write);
        }
    }

    impl Drop for Parked {
        fn drop(&mut self) {
            if let Some(spawner) = self.spawner.take() {
                self.let_go();
                let _ = spawner.join();
            }
        }
    }

    /// A lock this process holds through another open is LIVE; once released
    /// it is DEAD; a missing file and a symlink are UNKNOWN.
    ///
    /// The release runs while a child this process forked is parked between
    /// `fork` and `exec` holding a copy of the holder's descriptor
    /// ([`Parked`]).
    #[test]
    fn the_probe_reads_the_owner_lock() {
        use std::os::fd::AsRawFd as _;
        let dir = std::env::temp_dir().join(format!("ownerlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        let file = std::fs::File::create(&path).expect("create");
        // SAFETY: a descriptor this test owns; LOCK_EX|LOCK_NB never waits.
        assert_eq!(unsafe { flock(file.as_raw_fd(), 2 | 4) }, 0);
        assert_eq!(probe(&path), Owner::Live, "held through another open");

        let parked = Parked::park();
        release(file);
        assert_eq!(probe(&path), Owner::Dead, "the holder released it");
        let status = parked.release();
        assert!(status.success(), "{status}");

        assert_eq!(probe(&path), Owner::Dead, "the probe released its own lock");
        assert_eq!(probe(&dir.join("absent.log")), Owner::Unknown);
        let link = dir.join("link.log");
        std::os::unix::fs::symlink(&path, &link).expect("symlink");
        assert_eq!(probe(&link), Owner::Unknown, "a symlink is never followed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// THE PROBE LETS GO OF ITS OWN LOCK BEFORE IT CLOSES (module docs). A
    /// child forked while the probe holds the lock keeps a copy of the probe's
    /// open file description until it execs, and a probe that only closed would
    /// leave the lock held on that copy: the next probe would read a marker no
    /// process holds as LIVE. The fork lands at exactly that moment here — from
    /// inside the probe, between its `flock` and its `LOCK_UN` — and the next
    /// probe runs while the child is still parked holding the copy.
    #[test]
    fn a_child_forked_mid_probe_does_not_keep_the_probes_lock() {
        let dir = std::env::temp_dir().join(format!("ownerlock-mid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("m.log");
        drop(std::fs::File::create(&path).expect("create"));

        let mut parked = None;
        let first = probe_while_locked(&path, || parked = Some(Parked::park()));
        assert_eq!(first, Owner::Dead, "nobody holds it");
        let parked = parked.expect("the probe took the free lock and forked holding it");
        assert_eq!(
            probe(&path),
            Owner::Dead,
            "the child forked mid-probe kept the probe's lock: the probe closed without LOCK_UN"
        );
        let status = parked.release();
        assert!(status.success(), "{status}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
