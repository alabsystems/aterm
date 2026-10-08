// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! WHO HOLDS A FILE'S OWNER LOCK — the probe the PTY keeper reads a dead
//! window's crash marker with (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.4 row
//! 3). It is the same probe `aterm-gui`'s `crash_signal::markers::probe` makes on
//! the same files: `open` read-only, NOT following a symlink and NOT blocking
//! on a FIFO, `flock(LOCK_EX|LOCK_NB)`, `LOCK_UN` when it was taken, `close`
//! — through std (`File::try_lock`, `File::unlock`), so the workspace's flock
//! lint sees it (`clippy.toml`).
//! A marker's owner holds `LOCK_EX` on it for its whole life and the kernel
//! drops the lock when the owner dies, however it dies, so a lock this probe
//! can take says no process holds the marker open any more.
//!
//! `LOCK_UN` before `close` matters: `flock` binds to the open file
//! description, so a child forked during the probe would otherwise keep the
//! lock on its inherited copy and the next probe would read the marker as live.

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
/// (never, when the lock was not free). The shipped probe runs nothing there;
/// the tests fork there to check that the `LOCK_UN` happens.
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
// Skip: bottoms out at std's `open`/`flock` FFI calls.
#[cfg_attr(trust_verify, trust::skip)]
fn probe_while_locked(path: &Path, while_locked: impl FnOnce()) -> Owner {
    use std::os::unix::fs::OpenOptionsExt as _;
    // std adds O_CLOEXEC to every open.
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
    // `try_lock` IS `flock(LOCK_EX|LOCK_NB)` here, and only EWOULDBLOCK is
    // `WouldBlock`.
    #[expect(
        clippy::disallowed_methods,
        reason = "a probe: a lock it takes is let go by unlock() before its close"
    )]
    let taken = file.try_lock();
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

// Two attributes, not `cfg(all(test, ..))`: the wasm-process census masks
// unshipped items only by the exact spelling `#[cfg(test)]`, and this module
// spawns a thread.
#[cfg(test)]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    unsafe extern "C" {
        fn read(fd: i32, buf: *mut core::ffi::c_void, count: usize) -> isize;
        fn write(fd: i32, buf: *const core::ffi::c_void, count: usize) -> isize;
    }

    /// The holder lets go of its lock: `LOCK_UN`, then close, as the probe
    /// does. `flock` binds to the open file description, so a bare close frees
    /// the lock only when no copy remains, and a sibling test's `pre_exec`
    /// spawn may hold one until its child execs.
    fn release(file: std::fs::File) {
        file.unlock().expect("LOCK_UN");
        drop(file);
    }

    /// What the parked child execs once let go: `/bin/sh -c 'exit 0'` (`/bin/sh`
    /// exists on every Unix this module is wired for; NixOS has no
    /// `/usr/bin/true`).
    const SHELL: &str = "/bin/sh";

    /// A CHILD PARKED BETWEEN `fork` AND `exec`, holding a copy of every
    /// descriptor this process had open when it forked — the state every
    /// sibling test that spawns through `pre_exec` puts this process in for up
    /// to milliseconds at a time (`spawnfd::InheritStrip` walks up to
    /// `kern.maxfilesperproc` descriptors there). Parking one makes that
    /// interleaving a test's every run instead of its occasional one.
    ///
    /// [`Parked::release`] lets it go and returns its exit status; `Drop` lets
    /// it go on every other path, so a failed assertion does not leave it
    /// holding descriptors for its 10 s read timeout. The child always ends by
    /// exec'ing `/bin/sh -c 'exit 0'`, whether answered, at end of file, or
    /// timed out, and its write cannot raise `SIGPIPE`.
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

    /// A lock this process holds through another open is `Live`; once released
    /// it is `Dead`; a missing file and a symlink are `Unknown`. The release
    /// runs while a forked child holding a copy of the holder's descriptor is
    /// parked ([`Parked`]).
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

    /// The probe unlocks before it closes: a child forked from inside the probe,
    /// between its `flock` and its `LOCK_UN`, stays parked holding the probe's
    /// open file description while the next probe runs, and the marker must
    /// still read `Dead`.
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
