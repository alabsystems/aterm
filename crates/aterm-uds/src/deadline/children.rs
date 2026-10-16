// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix: the `poll(2)` under [`super::ExitWait`], and several children run at
//! once on one thread ([`super::outputs_until`]), which watches every child's
//! streams and exit with one `poll(2)` and so starts no thread.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsRawFd as _, OwnedFd, RawFd};
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

use super::{DRAIN_GRACE, ExitWait, OUTPUT_KEEP, POLL_MAX, POLL_MIN, RunError};
use crate::spawnfd::{Child, Command, Stdio};

/// `struct pollfd`: the same layout on Darwin, the BSDs and Linux.
#[repr(C)]
struct PollFd {
    fd: RawFd,
    events: i16,
    revents: i16,
}

/// `POLLIN`: the same bit on Darwin, the BSDs and Linux. A pipe whose writer
/// closed reports `POLLHUP` in `revents` whatever was asked, which any
/// nonzero `revents` covers.
const POLLIN: i16 = 0x1;

/// `poll(2)` over `fds` for at most `timeout` (rounded UP to a millisecond,
/// so a wait never returns early and spins), retrying `EINTR` on what is left.
/// `Ok(false)` when the timeout ran out with nothing ready.
// Skip: a blocking `poll(2)` wait (FFI, unverifiable body).
#[cfg_attr(trust_verify, trust::skip)]
fn poll(fds: &mut [PollFd], timeout: Duration) -> io::Result<bool> {
    // `nfds_t`: `unsigned long` in glibc and musl, `unsigned int` on Darwin,
    // the BSDs and Android's bionic.
    #[cfg(target_os = "linux")]
    type Nfds = core::ffi::c_ulong;
    #[cfg(not(target_os = "linux"))]
    type Nfds = core::ffi::c_uint;
    unsafe extern "C" {
        fn poll(fds: *mut PollFd, nfds: Nfds, timeout: i32) -> i32;
    }
    let until = Instant::now() + timeout;
    loop {
        let left = until.saturating_duration_since(Instant::now());
        let ms = i32::try_from(left.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX);
        let n = Nfds::try_from(fds.len()).unwrap_or(Nfds::MAX);
        // SAFETY: `fds` is a live, initialized array of `struct pollfd` of the
        // length passed, which `poll` reads and writes and does not retain.
        let ready = unsafe { poll(fds.as_mut_ptr(), n, ms) };
        if ready >= 0 {
            return Ok(ready > 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Whether the kernel has reported the exit `fd` ([`crate::exitwatch::exit_fd`])
/// watches, waiting at most `timeout` for it.
pub(super) fn exit_reported(fd: &OwnedFd, timeout: Duration) -> io::Result<bool> {
    let mut fds = [PollFd {
        fd: fd.as_raw_fd(),
        events: POLLIN,
        revents: 0,
    }];
    poll(&mut fds, timeout)
}

/// `SIGKILL` the process group `child` leads, and the child itself.
pub(super) fn kill_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        // SAFETY: a plain syscall. The child is not yet reaped, so its pid —
        // and with it the group id it leads — cannot have been reused.
        unsafe { kill(-pid, SIGKILL) };
    }
    let _ = child.kill();
}

/// One stream of one child: the pipe while it is open, and its tail so far.
struct Stream {
    pipe: Option<File>,
    tail: Vec<u8>,
}

impl Stream {
    fn new(pipe: Option<OwnedFd>) -> Self {
        Self {
            pipe: pipe.map(File::from),
            tail: Vec::new(),
        }
    }

    /// One read of a pipe `poll` reported ready, which therefore cannot block:
    /// keep the tail, and close the pipe at its end (or on an error).
    fn read_ready(&mut self) {
        let Some(pipe) = self.pipe.as_mut() else {
            return;
        };
        let mut chunk = [0u8; 8192];
        match pipe.read(&mut chunk) {
            // A signal before the read: still open, read on the next report.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Ok(0) | Err(_) => self.pipe = None,
            Ok(n) => {
                self.tail.extend_from_slice(&chunk[..n]);
                if self.tail.len() > OUTPUT_KEEP {
                    let cut = self.tail.len() - OUTPUT_KEEP;
                    self.tail.drain(..cut);
                }
            }
        }
    }
}

/// One child of [`outputs_until`].
struct Run {
    /// `Err` when it could not be started.
    child: Result<Child, io::Error>,
    exit: Option<ExitWait>,
    status: Option<ExitStatus>,
    exited_at: Option<Instant>,
    streams: [Stream; 2],
    failed: Option<RunError>,
}

impl Run {
    fn start(cmd: &mut Command) -> Self {
        cmd.process_group(0);
        let spawned = cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        match spawned {
            Ok(mut child) => {
                let exit = Some(ExitWait::new(&child));
                let streams = [
                    Stream::new(child.stdout.take().map(OwnedFd::from)),
                    Stream::new(child.stderr.take().map(OwnedFd::from)),
                ];
                Self {
                    child: Ok(child),
                    exit,
                    status: None,
                    exited_at: None,
                    streams,
                    failed: None,
                }
            }
            Err(error) => Self {
                child: Err(error),
                exit: None,
                status: None,
                exited_at: None,
                streams: [Stream::new(None), Stream::new(None)],
                failed: None,
            },
        }
    }

    /// Started, not yet exited, not given up on.
    fn running(&self) -> bool {
        self.child.is_ok() && self.status.is_none() && self.failed.is_none()
    }

    /// Nothing left to wait for.
    fn settled(&self) -> bool {
        self.child.is_err()
            || self.failed.is_some()
            || (self.status.is_some() && self.streams.iter().all(|s| s.pipe.is_none()))
    }

    fn close_streams(&mut self) {
        for stream in &mut self.streams {
            stream.pipe = None;
        }
    }

    /// Reap it if it has exited; a wait that fails kills it.
    fn observe(&mut self, now: Instant, reap_by: Instant) {
        let Ok(child) = self.child.as_mut() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.status = Some(status);
                self.exited_at = Some(now);
                self.exit = None;
            }
            Ok(None) => {}
            Err(error) => {
                kill_group(child);
                reap_within(child, self.exit.as_mut(), reap_by);
                self.failed = Some(RunError::Wait(error));
                self.close_streams();
            }
        }
    }

    fn into_result(self) -> Result<Output, RunError> {
        if let Some(failed) = self.failed {
            return Err(failed);
        }
        match (self.child, self.status) {
            (Err(error), _) => Err(RunError::Spawn(error)),
            (Ok(_), Some(status)) => {
                let [stdout, stderr] = self.streams;
                Ok(Output {
                    status,
                    stdout: stdout.tail,
                    stderr: stderr.tail,
                })
            }
            (Ok(_), None) => Err(RunError::TimedOut),
        }
    }
}

/// Reap a killed `child` by `by`, waiting on its exit as the kernel reports
/// it. One the kernel has not let go by then is left: dropping its handle
/// hands it to [`crate::reaper`].
fn reap_within(child: &mut Child, exit: Option<&mut ExitWait>, by: Instant) {
    let mut fallback;
    let exit = match exit {
        Some(exit) => exit,
        None => {
            fallback = ExitWait::new(child);
            &mut fallback
        }
    };
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                exit.wait(left);
            }
        }
    }
}

/// Several children at once, on this one thread. See [`super::outputs_until`].
pub(super) fn outputs_until(
    cmds: &mut [&mut Command],
    deadline: Instant,
    reap: Duration,
) -> Vec<Result<Output, RunError>> {
    let mut runs: Vec<Run> = cmds.iter_mut().map(|cmd| Run::start(cmd)).collect();
    // The fallback tick, for a child with no exit descriptor.
    let mut tick = POLL_MIN;
    loop {
        let now = Instant::now();
        for run in runs.iter_mut().filter(|run| run.running()) {
            run.observe(now, now + reap);
        }
        // A child that exited longer than the grace ago: what its streams
        // gave by then is its report, whatever descendant still holds one.
        for run in &mut runs {
            if run.exited_at.is_some_and(|at| now >= at + DRAIN_GRACE) {
                run.close_streams();
            }
        }
        if runs.iter().all(Run::settled) {
            break;
        }
        if now >= deadline {
            // Kill every child still running first, then reap them all
            // within the one reserve, so `reap` bounds the whole cleanup.
            let reap_by = Instant::now() + reap;
            for run in runs.iter_mut().filter(|run| run.running()) {
                if let Ok(child) = run.child.as_mut() {
                    kill_group(child);
                }
            }
            for run in runs.iter_mut().filter(|run| run.running()) {
                if let Ok(child) = run.child.as_mut() {
                    reap_within(child, run.exit.as_mut(), reap_by);
                }
                run.failed = Some(RunError::TimedOut);
                run.close_streams();
            }
            break;
        }
        let mut wake = deadline;
        // A running child with no exit descriptor: wake on the tick for it.
        let mut ticking = false;
        let mut fds = Vec::new();
        // Which run and stream (0, 1) or exit (2) each pollfd stands for.
        let mut owners = Vec::new();
        for (index, run) in runs.iter().enumerate() {
            if run.settled() {
                continue;
            }
            for (which, stream) in run.streams.iter().enumerate() {
                if let Some(pipe) = &stream.pipe {
                    fds.push(PollFd {
                        fd: pipe.as_raw_fd(),
                        events: POLLIN,
                        revents: 0,
                    });
                    owners.push((index, which));
                }
            }
            if run.running() {
                match run.exit.as_ref().and_then(ExitWait::reporter) {
                    Some(fd) => {
                        fds.push(PollFd {
                            fd,
                            events: POLLIN,
                            revents: 0,
                        });
                        owners.push((index, 2));
                    }
                    None => {
                        ticking = true;
                        wake = wake.min(now + tick);
                    }
                }
            }
            if let Some(at) = run.exited_at {
                wake = wake.min(at + DRAIN_GRACE);
            }
        }
        if ticking {
            tick = (tick * 2).min(POLL_MAX);
        }
        let timeout = wake.saturating_duration_since(Instant::now());
        match poll(&mut fds, timeout) {
            Ok(true) => {}
            Ok(false) => continue,
            // A `poll` that cannot run: wait a tick, and every child is
            // still reaped and every stream still read on the next turn.
            Err(_) => {
                std::thread::sleep(tick.min(timeout));
                tick = (tick * 2).min(POLL_MAX);
                continue;
            }
        }
        for (fd, &(index, which)) in fds.iter().zip(&owners) {
            if fd.revents == 0 {
                continue;
            }
            let run = &mut runs[index];
            match which {
                0 | 1 => run.streams[which].read_ready(),
                // Reported: `observe` reaps it next turn; stop polling it.
                _ => {
                    if let Some(exit) = run.exit.as_mut() {
                        exit.reported();
                    }
                }
            }
        }
    }
    runs.into_iter().map(Run::into_result).collect()
}
