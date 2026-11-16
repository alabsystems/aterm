// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned input, bounded capture and streaming output on one Unix poll loop.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::process::Output;
use std::time::{Duration, Instant};

use super::children::{PollFd, poll};
use super::{ChildOutput, ExitWait, OwnedInputTimeout, POLL_MAX, deadline_now};
use crate::secret::Secret;
use crate::spawnfd::{Child, Command, Stdio};

const POLLIN: i16 = 0x1;
const POLLOUT: i16 = 0x4;
const REAP_TICK: Duration = Duration::from_millis(20);

struct Capture {
    pipe: Option<File>,
    stream: ChildOutput,
    observed: usize,
    eof: bool,
}

impl Capture {
    const fn empty(stream: ChildOutput) -> Self {
        Self {
            pipe: None,
            stream,
            observed: 0,
            eof: false,
        }
    }

    fn attach(&mut self, fd: OwnedFd) -> io::Result<()> {
        // Attach before fallible setup: the Run guard must retain this reader until its
        // final natural-status observation, even when nonblocking setup fails.
        nonblocking(self.pipe.insert(File::from(fd)))
    }

    fn read_ready(
        &mut self,
        output: &mut impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let mut chunk = [0_u8; 8192];
        match pipe.read(&mut chunk) {
            Ok(0) => {
                self.eof = true;
                self.pipe = None;
            }
            Ok(n) => {
                self.observed = self.observed.saturating_add(n);
                output(self.stream, &chunk[..n])?;
            }
            Err(why)
                if matches!(
                    why.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(why) => return Err(why),
        }
        Ok(())
    }
}

/// Forced retirement requests group termination while the unreaped root reserves its ID.
/// Cooperative retirement requests its owner's stop and retains the actual root through cleanup.
struct Run<'retire, 'observe> {
    child: Child,
    input_len: Option<usize>,
    input: Option<Secret>,
    to_child: Option<File>,
    sent: usize,
    reaped: bool,
    natural_exit: bool,
    observed_exit: Option<&'observe mut dyn FnMut(std::process::ExitStatus)>,
    retire: Option<&'retire mut dyn FnMut()>,
    streams: [Capture; 2],
}

impl Run<'_, '_> {
    fn timed_out(&self) -> io::Error {
        timeout_observations(
            self.input_len,
            self.sent,
            self.input.is_some(),
            self.natural_exit,
            &self.streams,
        )
        .into_io_error()
    }

    fn settle(&mut self) {
        self.settle_with(Child::wait);
    }

    fn settle_with(
        &mut self,
        mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
    ) {
        if !self.reaped
            && !self.natural_exit
            && self.observed_exit.is_some()
            && self.child.owns_unreaped()
            && matches!(self.child.exit_status_unreaped(), Ok(Some(_)))
        {
            // This peek precedes stdin EOF, reader closure and every cleanup signal.
            // Report its immutable natural status only after the owning wait below.
            self.natural_exit = true;
        }
        self.close_input();
        self.close_output();
        if self.reaped {
            return;
        }
        if self.child.owns_unreaped() {
            if let Some(retire) = self.retire.as_mut() {
                retire();
            } else {
                super::children::kill_group(&mut self.child);
            }
        }
        let mut exit = self
            .child
            .owns_unreaped()
            .then(|| ExitWait::new(&self.child));
        loop {
            if !self.child.owns_unreaped() && !self.child.is_reaped() {
                // ECHILD forfeits numeric custody permanently; retain the caller's resources.
                exit = None;
                std::thread::sleep(REAP_TICK);
                continue;
            }
            match wait(&mut self.child) {
                Ok(status) => {
                    self.reaped = true;
                    self.report_exit(status);
                    return;
                }
                Err(_) => {
                    if !self.child.owns_unreaped() {
                        exit = None;
                    }
                    let at = Instant::now();
                    if let Some(exit) = &mut exit {
                        exit.wait(REAP_TICK);
                    }
                    let left = REAP_TICK.saturating_sub(at.elapsed());
                    if !left.is_zero() {
                        std::thread::sleep(left);
                    }
                }
            }
        }
    }

    fn report_exit(&mut self, status: std::process::ExitStatus) {
        // WNOWAIT supplies this reserved child's natural status before termination is
        // requested. A cleanup-induced kill is never a previously observed producer result.
        if self.natural_exit
            && let Some(observed) = self.observed_exit.take()
        {
            observed(status);
        }
    }

    fn close_input(&mut self) {
        self.input = None;
        self.to_child = None;
        drop(self.child.stdin.take());
    }

    fn close_output(&mut self) {
        for stream in &mut self.streams {
            stream.pipe = None;
        }
        drop(self.child.stdout.take());
        drop(self.child.stderr.take());
    }

    fn write_ready(&mut self) -> io::Result<()> {
        let (Some(input), Some(pipe)) = (self.input.as_ref(), self.to_child.as_mut()) else {
            return Ok(());
        };
        let end = input.len().min(self.sent.saturating_add(8192));
        match pipe.write(&input.expose()[self.sent..end]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "child stdin stalled",
                ));
            }
            Ok(n) => {
                self.sent += n;
                if self.sent == input.len() {
                    self.close_input();
                }
            }
            Err(why) if why.kind() == io::ErrorKind::BrokenPipe => self.close_input(),
            Err(why)
                if matches!(
                    why.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(why) => return Err(why),
        }
        Ok(())
    }
}

fn timeout_observations(
    input_len: Option<usize>,
    sent: usize,
    input_pending: bool,
    natural_exit: bool,
    streams: &[Capture; 2],
) -> OwnedInputTimeout {
    let mut observed = OwnedInputTimeout::spawned(input_len);
    observed.natural_exit_observed = natural_exit;
    observed.observe_stdin(input_len, sent, input_pending);
    observed.stdout_eof = streams[0].eof;
    observed.stderr_eof = streams[1].eof;
    observed.stdout_bytes_observed = streams[0].observed;
    observed.stderr_bytes_observed = streams[1].observed;
    observed
}

impl Drop for Run<'_, '_> {
    fn drop(&mut self) {
        self.settle();
    }
}

pub(super) fn output_with_input_until(
    cmd: &mut Command,
    input: Secret,
    deadline: Instant,
    max_output: usize,
) -> io::Result<Output> {
    output_with_input_until_watching(cmd, input, deadline, max_output, || false)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::Interrupted, "owned capture was stopped"))
}

pub(super) fn output_with_input_until_watching(
    cmd: &mut Command,
    input: Secret,
    deadline: Instant,
    max_output: usize,
    stopped: impl FnMut() -> bool,
) -> io::Result<Option<Output>> {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run_with_input_until_watching(
        cmd,
        Some(input),
        None,
        Some(deadline),
        |_| {},
        |stream, chunk| {
            let (bytes, name) = match stream {
                ChildOutput::Stdout => (&mut stdout, "stdout"),
                ChildOutput::Stderr => (&mut stderr, "stderr"),
            };
            if chunk.len() > max_output.saturating_sub(bytes.len()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{name} exceeds its complete-output bound of {max_output} bytes"),
                ));
            }
            bytes.extend_from_slice(chunk);
            Ok(())
        },
        stopped,
        None,
        None,
    )?;
    Ok(status.map(|status| Output {
        status,
        stdout,
        stderr,
    }))
}

pub(super) fn stream_until_stopped(
    cmd: &mut Command,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    stopped: impl FnMut() -> bool,
) -> io::Result<Option<std::process::ExitStatus>> {
    run_with_input_until_watching(
        cmd, None, None, deadline, spawned, output, stopped, None, None,
    )
}

pub(super) fn stream_until_stopped_observing_exit(
    cmd: &mut Command,
    stdin: Option<Stdio>,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    stopped: impl FnMut() -> bool,
    mut observed_exit: impl FnMut(std::process::ExitStatus),
) -> io::Result<Option<std::process::ExitStatus>> {
    run_with_input_until_watching(
        cmd,
        None,
        stdin,
        deadline,
        spawned,
        output,
        stopped,
        None,
        Some(&mut observed_exit),
    )
}

pub(super) fn stream_with_stdin_until_stopped(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    stopped: impl FnMut() -> bool,
    retire: Option<&mut dyn FnMut()>,
) -> io::Result<Option<std::process::ExitStatus>> {
    run_with_input_until_watching(
        cmd,
        None,
        Some(stdin),
        deadline,
        spawned,
        output,
        stopped,
        retire,
        None,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one owner shares input, observation, stop and retirement policies"
)]
fn run_with_input_until_watching(
    cmd: &mut Command,
    input: Option<Secret>,
    stdin: Option<Stdio>,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    mut output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    mut stopped: impl FnMut() -> bool,
    retire: Option<&mut dyn FnMut()>,
    observed_exit: Option<&mut dyn FnMut(std::process::ExitStatus)>,
) -> io::Result<Option<std::process::ExitStatus>> {
    if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned input requires an unreaped child exit reader on Darwin or Linux",
        ));
    }
    if deadline.is_some_and(|deadline| deadline_now() >= deadline) {
        return Err(OwnedInputTimeout::before_start().into_io_error());
    }
    if stopped() {
        return Ok(None);
    }
    let child = cmd
        .process_group(0)
        .stdin(stdin.unwrap_or_else(|| {
            if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            }
        }))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut run = Run {
        child,
        input_len: input.as_ref().map(Secret::len),
        input,
        to_child: None,
        sent: 0,
        reaped: false,
        natural_exit: false,
        observed_exit,
        retire,
        streams: [
            Capture::empty(ChildOutput::Stdout),
            Capture::empty(ChildOutput::Stderr),
        ],
    };
    spawned(run.child.id());
    if run.input.is_some() {
        let stdin = run.child.stdin.take().ok_or_else(missing_pipe)?;
        nonblocking(run.to_child.insert(File::from(OwnedFd::from(stdin))))?;
    }
    let stdout = run.child.stdout.take().ok_or_else(missing_pipe)?;
    run.streams[0].attach(OwnedFd::from(stdout))?;
    let stderr = run.child.stderr.take().ok_or_else(missing_pipe)?;
    run.streams[1].attach(OwnedFd::from(stderr))?;
    if run.input.as_ref().is_some_and(Secret::is_empty) {
        run.close_input();
    }
    let mut exit = ExitWait::new(&run.child);
    loop {
        if !run.natural_exit {
            let exited = if run.observed_exit.is_some() {
                if let Some(status) = run.child.exit_status_unreaped()? {
                    run.natural_exit = true;
                    run.report_exit(status);
                    true
                } else {
                    false
                }
            } else {
                run.child.exited()?
            };
            if exited {
                run.natural_exit = true;
                run.close_input();
                // WNOWAIT leaves this owned root reserving the group ID, and
                // its natural exit status cannot be changed by signalling descendants now.
                super::children::kill_group(&mut run.child);
            }
        }
        if run.natural_exit && run.streams.iter().all(|stream| stream.pipe.is_none()) {
            let status = run.child.wait()?;
            run.reaped = true;
            run.report_exit(status);
            return Ok(Some(status));
        }
        if stopped() {
            // No output-completion claim accompanies cancellation. Retirement observes
            // natural status before closing pipes and positively reaps the actual root.
            run.settle();
            return Ok(None);
        }
        let left = deadline.map_or(POLL_MAX, |deadline| {
            deadline.saturating_duration_since(deadline_now())
        });
        if left.is_zero() {
            return Err(run.timed_out());
        }
        let mut fds = Vec::new();
        let mut owners = Vec::new();
        if let Some(pipe) = &run.to_child {
            fds.push(PollFd {
                fd: pipe.as_raw_fd(),
                events: POLLOUT,
                revents: 0,
            });
            owners.push(0);
        }
        for (index, stream) in run.streams.iter().enumerate() {
            if let Some(pipe) = &stream.pipe {
                fds.push(PollFd {
                    fd: pipe.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                });
                owners.push(index + 1);
            }
        }
        if !run.natural_exit
            && let Some(fd) = exit.reporter()
        {
            fds.push(PollFd {
                fd,
                events: POLLIN,
                revents: 0,
            });
            owners.push(3);
        }
        poll(&mut fds, left.min(POLL_MAX))?;
        for (fd, owner) in fds.iter().zip(owners) {
            if fd.revents == 0 {
                continue;
            }
            match owner {
                0 => run.write_ready()?,
                1 | 2 => run.streams[owner - 1].read_ready(&mut output)?,
                _ => exit.reported(),
            }
        }
    }
}

fn missing_pipe() -> io::Error {
    io::Error::other("child exchange pipe is unavailable")
}

fn nonblocking(pipe: &File) -> io::Result<()> {
    let fd = pipe.as_raw_fd();
    // SAFETY: both fcntl calls read/set flags on this owned pipe and retain nothing.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
#[path = "input_unix_tests.rs"]
mod tests;
