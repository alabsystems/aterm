// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned input, bounded capture and streaming output on one Unix poll loop.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::process::Output;
use std::time::{Duration, Instant};

use super::children::{PollFd, poll};
use super::{ChildOutput, ExitWait, POLL_MAX, deadline_now};
use crate::secret::Secret;
use crate::spawnfd::{Child, Command, Stdio};

const POLLIN: i16 = 0x1;
const POLLOUT: i16 = 0x4;
const REAP_TICK: Duration = Duration::from_millis(20);

struct Capture {
    pipe: Option<File>,
    stream: ChildOutput,
}

impl Capture {
    fn of(fd: OwnedFd, stream: ChildOutput) -> io::Result<Self> {
        let pipe = File::from(fd);
        nonblocking(&pipe)?;
        Ok(Self {
            pipe: Some(pipe),
            stream,
        })
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
            Ok(0) => self.pipe = None,
            Ok(n) => {
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
struct Run<'a> {
    child: Child,
    input: Option<Secret>,
    to_child: Option<File>,
    sent: usize,
    reaped: bool,
    retire: Option<&'a mut dyn FnMut()>,
}

impl Run<'_> {
    fn settle(&mut self) {
        self.settle_with(Child::wait);
    }

    fn settle_with(
        &mut self,
        mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
    ) {
        self.close_input();
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
                Ok(_) => {
                    self.reaped = true;
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

    fn close_input(&mut self) {
        self.input = None;
        self.to_child = None;
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

impl Drop for Run<'_> {
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
    run_with_input_until_watching(cmd, None, None, deadline, spawned, output, stopped, None)
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
) -> io::Result<Option<std::process::ExitStatus>> {
    if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned input requires an unreaped child exit reader on Darwin or Linux",
        ));
    }
    if deadline.is_some_and(|deadline| deadline_now() >= deadline) {
        return Err(timed_out());
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
        input,
        to_child: None,
        sent: 0,
        reaped: false,
        retire,
    };
    spawned(run.child.id());
    if run.input.is_some() {
        let stdin = run.child.stdin.take().ok_or_else(missing_pipe)?;
        let stdin = File::from(OwnedFd::from(stdin));
        nonblocking(&stdin)?;
        run.to_child = Some(stdin);
    }
    let stdout = run.child.stdout.take().ok_or_else(missing_pipe)?;
    let stderr = run.child.stderr.take().ok_or_else(missing_pipe)?;
    let mut streams = [
        Capture::of(OwnedFd::from(stdout), ChildOutput::Stdout)?,
        Capture::of(OwnedFd::from(stderr), ChildOutput::Stderr)?,
    ];
    if run.input.as_ref().is_some_and(Secret::is_empty) {
        run.close_input();
    }
    let mut exit = ExitWait::new(&run.child);
    let mut exited = false;
    loop {
        if !exited && run.child.exited()? {
            run.close_input();
            // exited() uses WNOWAIT: this owned root still reserves the group ID, and
            // its natural exit status cannot be changed by signalling descendants now.
            super::children::kill_group(&mut run.child);
            exited = true;
        }
        if exited && streams.iter().all(|stream| stream.pipe.is_none()) {
            let status = run.child.wait()?;
            run.reaped = true;
            return Ok(Some(status));
        }
        if stopped() {
            // No output-completion claim accompanies cancellation. Close our own pipes
            // before retaining this waiter until its actual root is positively reaped.
            for stream in &mut streams {
                stream.pipe = None;
            }
            run.settle();
            return Ok(None);
        }
        let left = deadline.map_or(POLL_MAX, |deadline| {
            deadline.saturating_duration_since(deadline_now())
        });
        if left.is_zero() {
            return Err(timed_out());
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
        for (index, stream) in streams.iter().enumerate() {
            if let Some(pipe) = &stream.pipe {
                fds.push(PollFd {
                    fd: pipe.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                });
                owners.push(index + 1);
            }
        }
        if !exited && let Some(fd) = exit.reporter() {
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
                1 | 2 => streams[owner - 1].read_ready(&mut output)?,
                _ => exit.reported(),
            }
        }
    }
}

fn timed_out() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "child input, execution or output exceeded its deadline",
    )
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
