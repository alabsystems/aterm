// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Test-only helper that parks a forked child before its exec.

use std::cell::RefCell;
use std::io::{PipeWriter, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, RawFd};
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::thread::JoinHandle;

/// Child process parked before its exec while holding inherited descriptors.
pub(crate) struct ParkedChild {
    /// Closing (or writing to) this lets the child exec.
    go: Option<PipeWriter>,
    /// Returns once the child has exec'd and exited — never while it is parked.
    spawner: Option<JoinHandle<std::io::Result<ExitStatus>>>,
}

impl ParkedChild {
    /// Fork the child and return once it exists.
    ///
    /// The fork holds the spawn lock (`aterm_uds::spawn_lock::spawning`) across itself alone, as
    /// every spawn does, so the child copies no neighbour's descriptor between its creation and
    /// its close-on-fork mark (`aterm_uds::kept_out`); the parked child holds nothing of it.
    pub(crate) fn fork() -> Self {
        let (mut forked_r, forked_w) = std::io::pipe().expect("pipe");
        let (go_r, go_w) = std::io::pipe().expect("pipe");
        let (forked_fd, go_fd, go_w_fd) =
            (forked_w.as_raw_fd(), go_r.as_raw_fd(), go_w.as_raw_fd());
        let spawner = std::thread::spawn(move || {
            let argv = [
                c"/bin/sh".as_ptr(),
                c"-c".as_ptr(),
                c"exit 0".as_ptr(),
                std::ptr::null(),
            ];
            let envp = [std::ptr::null()];
            let spawning = aterm_uds::spawn_lock::spawning();
            // SAFETY: the child branch runs only `parked`, which calls async-signal-safe
            // functions on descriptors and arrays made before the fork and never returns.
            let pid = unsafe { libc::fork() };
            if pid == 0 {
                // SAFETY: the forked child; see `parked`.
                unsafe { parked(go_w_fd, forked_fd, go_fd, &argv, &envp) }
            }
            let forked = if pid < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(pid)
            };
            drop(spawning);
            drop((forked_w, go_r));
            reap(forked?)
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

/// The forked child: lets go of `go_w`, says it forked on `forked`, parks until `go` is written
/// or closed, then execs `argv` with `envp`; exits 127 when it cannot.
///
/// # Safety
/// Only in the child of a `fork`, which may be of a multithreaded process: it calls only
/// async-signal-safe functions (`close`, `write`, `read`, `execve`, `_exit`) and reads `errno`.
unsafe fn parked(
    go_w: RawFd,
    forked: RawFd,
    go: RawFd,
    argv: &[*const libc::c_char; 4],
    envp: &[*const libc::c_char; 1],
) -> ! {
    // SAFETY: as the caller promises; each pointer is to a live local of the forking thread,
    // copied into this child, and both arrays end in null.
    unsafe {
        libc::close(go_w);
        if libc::write(forked, b"f".as_ptr().cast(), 1) == 1 {
            let mut b = 0u8;
            let released = loop {
                if libc::read(go, (&raw mut b).cast(), 1) >= 0 {
                    break true;
                }
                if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                    break false;
                }
            };
            if released {
                libc::execve(argv[0], argv.as_ptr(), envp.as_ptr());
            }
        }
        libc::_exit(127)
    }
}

/// Waits for `pid`, a child of this process, and returns how it ended.
fn reap(pid: libc::pid_t) -> std::io::Result<ExitStatus> {
    let mut status = 0;
    loop {
        // SAFETY: `pid` is this process's own child, reaped once here into a local slot.
        if unsafe { libc::waitpid(pid, &raw mut status, 0) } == pid {
            return Ok(ExitStatus::from_raw(status));
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}
