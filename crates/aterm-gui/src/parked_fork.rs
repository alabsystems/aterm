// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only helper that parks a forked child in its pre-exec hook.

use std::cell::RefCell;
use std::io::{PipeWriter, Read as _, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::process::CommandExt as _;
use std::process::{ExitStatus, Stdio};
use std::thread::JoinHandle;

/// Child process parked in its pre-exec hook while holding inherited descriptors.
pub(crate) struct ParkedChild {
    /// Closing (or writing to) this lets the child exec.
    go: Option<PipeWriter>,
    /// Returns once the child has exec'd and exited — never while it is parked.
    spawner: Option<JoinHandle<std::io::Result<ExitStatus>>>,
}

impl ParkedChild {
    /// Fork the child and return once it exists.
    pub(crate) fn fork() -> Self {
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
            // keeps open across the spawn.
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

    /// Returns whether the child is still parked before `exec`.
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

/// Guard that arms the `n`-th (1-based) [`fork_if_armed`] site on this thread.
pub(crate) struct Armed(());

/// Arm this thread (see [`Armed`]).
pub(crate) fn arm_at(n: usize) -> Armed {
    assert!(n >= 1, "sites are counted from 1");
    ARMED.with(|armed| *armed.borrow_mut() = Some(Arm::Waiting(n - 1)));
    Armed(())
}

impl Armed {
    /// Takes the [`ParkedChild`] forked at the armed site, panicking if none forked.
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
        let left = ARMED.with(|armed| armed.borrow_mut().take());
        drop(left);
    }
}

/// Forks a [`ParkedChild`] when this thread is armed and reaches its target site.
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
