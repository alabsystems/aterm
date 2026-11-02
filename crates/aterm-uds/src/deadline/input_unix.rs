// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Owned input and complete bounded output on the deadline module's Unix poll loop.

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::process::Output;
use std::time::{Duration, Instant};

use super::children::{PollFd, poll};
use super::{ExitWait, POLL_MAX, deadline_now, reap_within, saturating_after};
use crate::secret::Secret;
use crate::spawnfd::{Child, Command, Stdio};

const POLLIN: i16 = 0x1;
const POLLOUT: i16 = 0x4;
const REAP: Duration = Duration::from_secs(60);

struct Capture {
    pipe: Option<File>,
    bytes: Vec<u8>,
    name: &'static str,
}

impl Capture {
    fn of(fd: OwnedFd, name: &'static str) -> io::Result<Self> {
        let pipe = File::from(fd);
        nonblocking(&pipe)?;
        Ok(Self {
            pipe: Some(pipe),
            bytes: Vec::new(),
            name,
        })
    }

    fn read_ready(&mut self, limit: usize) -> io::Result<()> {
        let Some(pipe) = self.pipe.as_mut() else {
            return Ok(());
        };
        let mut chunk = [0_u8; 8192];
        match pipe.read(&mut chunk) {
            Ok(0) => self.pipe = None,
            Ok(n) => {
                if n > limit.saturating_sub(self.bytes.len()) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "{} exceeds its complete-output bound of {limit} bytes",
                            self.name
                        ),
                    ));
                }
                self.bytes.extend_from_slice(&chunk[..n]);
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

/// The root is never reaped before its group's termination is requested. Its reserved PID is
/// the authority for every group signal, including a normal exit with inherited pipes open.
struct Run {
    child: Child,
    input: Option<Secret>,
    to_child: Option<File>,
    sent: usize,
    reaped: bool,
}

impl Run {
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

impl Drop for Run {
    fn drop(&mut self) {
        self.close_input();
        if !self.reaped {
            super::children::kill_group(&mut self.child);
            reap_within(
                &mut self.child,
                None,
                saturating_after(Instant::now(), REAP),
            );
        }
    }
}

pub(super) fn output_with_input_until(
    cmd: &mut Command,
    input: Secret,
    deadline: Instant,
    max_output: usize,
) -> io::Result<Output> {
    if !cfg!(any(target_vendor = "apple", target_os = "linux")) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "owned input requires an unreaped child exit reader on Darwin or Linux",
        ));
    }
    if deadline_now() >= deadline {
        return Err(timed_out());
    }
    let child = cmd
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut run = Run {
        child,
        input: Some(input),
        to_child: None,
        sent: 0,
        reaped: false,
    };
    let stdin = run.child.stdin.take().ok_or_else(missing_pipe)?;
    let stdin = File::from(OwnedFd::from(stdin));
    nonblocking(&stdin)?;
    run.to_child = Some(stdin);
    let stdout = run.child.stdout.take().ok_or_else(missing_pipe)?;
    let stderr = run.child.stderr.take().ok_or_else(missing_pipe)?;
    let mut streams = [
        Capture::of(OwnedFd::from(stdout), "stdout")?,
        Capture::of(OwnedFd::from(stderr), "stderr")?,
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
            let [stdout, stderr] = streams;
            return Ok(Output {
                status,
                stdout: stdout.bytes,
                stderr: stderr.bytes,
            });
        }
        let left = deadline.saturating_duration_since(deadline_now());
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
                1 | 2 => streams[owner - 1].read_ready(max_output)?,
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
