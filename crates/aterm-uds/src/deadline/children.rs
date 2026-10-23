// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Unix: the `poll(2)` under [`super::ExitWait`], and several children run at
//! once on one thread ([`super::outputs_until`], and one held child resumed,
//! [`super::Held`]), which watches every child's streams and exit with one
//! `poll(2)` and so starts no thread.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd, OwnedFd, RawFd};
use std::process::{ExitStatus, Output};
use std::time::{Duration, Instant};

use super::{DRAIN_GRACE, ExitWait, OUTPUT_KEEP, POLL_MAX, POLL_MIN, RunError, reap_within};
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

/// `POLLHUP`: the same bit on Darwin, the BSDs and Linux.
const POLLHUP: i16 = 0x10;

/// `ESRCH`: the same number on Darwin, the BSDs and Linux.
const ESRCH: i32 = 3;

/// Whether every writer of the pipe `fd` reads has closed it: what it still holds is all it
/// will ever hold, so its end is a read away.
pub(super) fn writers_gone(fd: BorrowedFd<'_>) -> bool {
    let mut fds = [PollFd {
        fd: fd.as_raw_fd(),
        events: POLLIN,
        revents: 0,
    }];
    matches!(poll(&mut fds, Duration::ZERO), Ok(true)) && fds[0].revents & POLLHUP != 0
}

/// Whether a descendant of an exited child may still hold the pipe `fd` reads: a writer
/// holds it and the process group the child led (`group`, its pid) still has a member, or
/// the group is unknown. A descendant that leaves the group is not seen.
pub(super) fn descendant_may_hold(fd: BorrowedFd<'_>, group: Option<u32>) -> bool {
    !writers_gone(fd) && group.is_none_or(group_left)
}

/// Whether any process is left in the process group `group`.
fn group_left(group: u32) -> bool {
    let Ok(group) = i32::try_from(group) else {
        return true;
    };
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: a plain syscall; signal 0 delivers nothing.
    let none =
        unsafe { kill(-group, 0) } != 0 && io::Error::last_os_error().raw_os_error() == Some(ESRCH);
    !none
}

/// `poll(2)` over `fds` for at most `timeout` (rounded UP to a millisecond,
/// so a wait never returns early and spins), retrying `EINTR` while time remains.
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
        if Instant::now() >= until {
            return Ok(false);
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

/// `cmd` set up as every child here runs: leading a new process group, stdin
/// `/dev/null`, stdout and stderr piped.
pub(super) fn prepare(cmd: &mut Command) -> &mut Command {
    cmd.process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
}

impl Run {
    fn start(cmd: &mut Command) -> Self {
        Self::of(prepare(cmd).spawn())
    }

    /// A child [`prepare`]d and started (or why it was not), from here on run
    /// like the others.
    fn of(spawned: io::Result<Child>) -> Self {
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

    /// Close the streams a descendant may still hold open; one nothing else holds is read
    /// on, to its end or the deadline (as [`super::Grace`] says).
    fn close_held_streams(&mut self) {
        let group = self.child.as_ref().ok().map(Child::id);
        for stream in &mut self.streams {
            if stream
                .pipe
                .as_ref()
                .is_some_and(|pipe| descendant_may_hold(pipe.as_fd(), group))
            {
                stream.pipe = None;
            }
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

/// Several children at once, on this one thread. See [`super::outputs_until`].
pub(super) fn outputs_until(
    cmds: &mut [&mut Command],
    deadline: Instant,
    reap: Duration,
) -> Vec<Result<Output, RunError>> {
    run_until(
        cmds.iter_mut().map(|cmd| Run::start(cmd)).collect(),
        deadline,
        reap,
    )
}

/// One child already started from a [`prepare`]d command — a held one, just
/// resumed ([`super::Held::output_until`]) — run as [`outputs_until`] runs each.
pub(super) fn output_of_until(
    child: Child,
    deadline: Instant,
    reap: Duration,
) -> Result<Output, RunError> {
    run_until(vec![Run::of(Ok(child))], deadline, reap)
        .pop()
        .unwrap_or(Err(RunError::TimedOut))
}

/// The runs to their ends, or to `deadline` and the `reap` reserve after it.
fn run_until(
    mut runs: Vec<Run>,
    deadline: Instant,
    reap: Duration,
) -> Vec<Result<Output, RunError>> {
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
                run.close_held_streams();
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
            if let Some(at) = run.exited_at
                && now < at + DRAIN_GRACE
            {
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::AsFd as _;

    use super::writers_gone;

    /// A pipe is writer-less once its last writer closes it, with data left in it or not.
    /// A child another test's thread forks meanwhile holds a copy until its exec, so the
    /// last look waits (a hang detector) for it to let go.
    #[test]
    fn a_pipe_is_writer_less_once_its_last_writer_closes() {
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        assert!(!writers_gone(reader.as_fd()), "open and empty");
        writer.write_all(b"x").expect("write");
        assert!(!writers_gone(reader.as_fd()), "open with data");
        let second = writer.try_clone().expect("a second writer");
        drop(writer);
        assert!(!writers_gone(reader.as_fd()), "a second writer holds it");
        drop(second);
        let hang = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !writers_gone(reader.as_fd()) {
            assert!(std::time::Instant::now() < hang, "closed with data left");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}
