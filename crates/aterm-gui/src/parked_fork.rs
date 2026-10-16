// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only: makes the fork→exec window deterministic.
//!
//! A `flock` belongs to the open file description, and a child forked by any thread holds a
//! copy of every descriptor (close-on-exec ones included) until it execs. A lock released only
//! by closing its `File` stays held in that copy for the child's fork→exec window, so a
//! `try_lock` in that window is refused although nobody holds the lock. `LOCK_UN`
//! ([`crate::native_document_host::HeldAdvisoryLock`]) strips the lock from the description
//! itself, the child's copy included.
//!
//! [`ParkedChild::fork`] forks `/bin/sh` from its own thread and parks it short of `execve`
//! until [`ParkedChild::release`]. [`arm_at`] makes the `n`-th [`fork_if_armed`] site (each
//! sits just after a lock is taken) on the arming thread fork the child while that lock is held.

use std::cell::RefCell;
use std::io::{PipeWriter, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{ExitStatus, Stdio};
use std::thread::JoinHandle;

/// A child forked from another thread and parked in its pre-exec hook: it
/// holds a copy of every descriptor this process had open when it forked.
pub(crate) struct ParkedChild {
    /// Closing (or writing to) this lets the child exec.
    go: Option<PipeWriter>,
    /// Returns once the child has exec'd and exited — never while it is parked.
    spawner: Option<JoinHandle<std::io::Result<ExitStatus>>>,
}

impl ParkedChild {
    /// Fork the child and return once it exists.
    pub(crate) fn fork() -> Self {
        // `forked`: the child says it exists. `go`: the test lets it exec.
        let (mut forked_r, forked_w) = std::io::pipe().expect("pipe");
        let (go_r, go_w) = std::io::pipe().expect("pipe");
        let (forked_fd, go_fd, go_w_fd) =
            (forked_w.as_raw_fd(), go_r.as_raw_fd(), go_w.as_raw_fd());
        let spawner = std::thread::spawn(move || {
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", "exit 0"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            // SAFETY: the hook runs in the forked child before exec and calls only
            // async-signal-safe functions (close, write, read, errno) on descriptors this closure
            // keeps open across the spawn. It closes its copy of `go`'s write end first, so the
            // parent's end closing (a panic included) lets it exec rather than wait.
            unsafe {
                cmd.pre_exec(move || {
                    let interrupted = || {
                        std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
                    };
                    libc::close(go_w_fd);
                    if libc::write(forked_fd, b"f".as_ptr().cast(), 1) != 1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let mut b = 0u8;
                    while libc::read(go_fd, (&raw mut b).cast(), 1) < 0 {
                        if !interrupted() {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    Ok(())
                });
            }
            let status = cmd.status();
            // A child that failed before its byte: the parent's read ends.
            drop((forked_w, go_r));
            status
        });
        let mut b = [0u8; 1];
        forked_r.read_exact(&mut b).expect("the child forked");
        Self {
            go: Some(go_w),
            spawner: Some(spawner),
        }
    }

    /// The child has not exec'd yet: its spawn returns only at the exec, which
    /// waits for [`Self::release`].
    pub(crate) fn still_parked(&self) -> bool {
        self.spawner.as_ref().is_some_and(|s| !s.is_finished())
    }

    /// Let the child exec, and reap it.
    pub(crate) fn release(mut self) {
        self.go
            .take()
            .expect("parked")
            .write_all(b"g")
            .expect("let the child exec");
        let status = self
            .spawner
            .take()
            .expect("spawned")
            .join()
            .expect("the spawner")
            .expect("the child ran");
        assert!(status.success(), "{status:?}");
    }
}

impl Drop for ParkedChild {
    /// A test that panicked with the child parked: closing `go` lets it exec.
    fn drop(&mut self) {
        drop(self.go.take());
        if let Some(spawner) = self.spawner.take() {
            let _ = spawner.join();
        }
    }
}

enum Arm {
    /// This many more armed sites to pass before the one that forks.
    Waiting(usize),
    Forked(ParkedChild),
    Spent,
}

thread_local! {
    static ARMED: RefCell<Option<Arm>> = const { RefCell::new(None) };
}

/// While this lives, the `n`-th (1-based) [`fork_if_armed`] site this thread
/// reaches forks a [`ParkedChild`], once.
pub(crate) struct Armed(());

/// Arm this thread (see [`Armed`]).
pub(crate) fn arm_at(n: usize) -> Armed {
    assert!(n >= 1, "sites are counted from 1");
    ARMED.with(|armed| *armed.borrow_mut() = Some(Arm::Waiting(n - 1)));
    Armed(())
}

impl Armed {
    /// The child the armed site forked, still parked. Panics when no armed site
    /// was reached — a test whose fork never happened proves nothing.
    pub(crate) fn child(&self) -> ParkedChild {
        ARMED.with(|armed| {
            let mut armed = armed.borrow_mut();
            match armed.replace(Arm::Spent) {
                Some(Arm::Forked(child)) => child,
                _ => panic!("no armed lock site forked a child"),
            }
        })
    }
}

impl Drop for Armed {
    fn drop(&mut self) {
        // An untaken child is released by its own drop.
        let left = ARMED.with(|armed| armed.borrow_mut().take());
        drop(left);
    }
}

/// A lock site's hook, placed just after the lock is taken: when this thread
/// is armed and this is the site it counts to, fork a [`ParkedChild`] while the
/// lock is held.
pub(crate) fn fork_if_armed() {
    ARMED.with(|armed| {
        let mut armed = armed.borrow_mut();
        match armed.as_mut() {
            Some(Arm::Waiting(0)) => *armed = Some(Arm::Forked(ParkedChild::fork())),
            Some(Arm::Waiting(more)) => *more -= 1,
            _ => {}
        }
    });
}
