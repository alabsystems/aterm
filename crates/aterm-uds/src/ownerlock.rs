// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Who holds a file's owner lock: the probe the PTY keeper reads a dead window's crash marker
//! with (`docs/DESIGN-pty-keeper-2026-09-26.md` §5.4 row 3), on the same files as aterm-gui's
//! `crash_signal::markers::probe`. It opens read-only without following a symlink or blocking
//! on a FIFO, tries `flock(LOCK_EX|LOCK_NB)`, and unlocks a lock it took before closing,
//! through std (`File::try_lock`, `File::unlock`) so the workspace's flock lint sees it
//! (`clippy.toml`).
//!
//! A marker's owner holds `LOCK_EX` for its whole life and the kernel drops it when the owner
//! dies, so a lock this probe can take says no process holds the marker open.
//!
//! `LOCK_UN` before `close` matters: `flock` binds to the open file description, so a child
//! forked during the probe would otherwise keep the lock on its inherited copy and the next
//! probe would read the marker as live.

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

/// [`probe`], running `while_locked` while the probe's own descriptor holds the lock (never,
/// when the lock was not free). The tests fork there to check that the `LOCK_UN` happens.
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
    // `try_lock` is `flock(LOCK_EX|LOCK_NB)` here, and only EWOULDBLOCK is
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

// Two attributes, not `cfg(all(test, ..))`: the wasm-process census masks unshipped items only
// by the exact spelling `#[cfg(test)]`, and this module spawns a thread.
#[cfg(test)]
#[cfg(any(target_vendor = "apple", target_os = "linux"))]
pub(crate) mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    unsafe extern "C" {
        fn read(fd: i32, buf: *mut core::ffi::c_void, count: usize) -> isize;
        fn write(fd: i32, buf: *const core::ffi::c_void, count: usize) -> isize;
    }

    /// The holder lets go as the probe does, `LOCK_UN` then close: a bare close frees the lock
    /// only when no copy of the description remains, and a sibling test's spawn may hold one.
    fn release(file: std::fs::File) {
        file.unlock().expect("LOCK_UN");
        drop(file);
    }

    /// What the parked child execs once let go (NixOS has no `/usr/bin/true`).
    const SHELL: &str = "/bin/sh";

    /// A child parked between `fork` and `exec`, holding a copy of every descriptor this
    /// process had open: the state a sibling test's `pre_exec` spawn puts this process in for
    /// milliseconds at a time, made deterministic.
    ///
    /// [`Parked::release`] lets it go and returns its exit status; `Drop` lets it go on every
    /// other path. The child always ends by exec'ing `/bin/sh -c 'exit 0'`.
    pub(crate) struct Parked {
        ours: std::os::unix::net::UnixStream,
        spawner: Option<std::thread::JoinHandle<std::io::Result<std::process::ExitStatus>>>,
    }

    impl Parked {
        /// Fork a child and return once it is parked. The child writes one byte, then waits
        /// for one, under a 10 s timeout on the shared socket, so even a dead parent cannot
        /// strand it in `pre_exec`.
        pub(crate) fn park() -> Self {
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
                // SAFETY: runs in the forked child before `exec` and calls only the
                // async-signal-safe `write(2)` and `read(2)`, on a descriptor the parent keeps
                // open (`theirs`, alive until `status` returns).
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
            // Built before the wait, so `Drop` lets go of a child that never parks.
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
        pub(crate) fn release(mut self) -> std::process::ExitStatus {
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

        /// Answer the child's read, then end the stream so a read that missed the byte sees
        /// end of file. A child that already exited needs nothing.
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

    /// Held through another open is `Live`, released is `Dead` (while a forked child holds a
    /// copy of the holder's descriptor), missing or a symlink is `Unknown`, and a FIFO is
    /// answered without waiting for a writer (`Unknown` on Darwin, whose `flock` refuses one).
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

    /// A child forked between the probe's `flock` and its `LOCK_UN` holds the probe's open
    /// file description, and the next probe still reads `Dead`.
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
