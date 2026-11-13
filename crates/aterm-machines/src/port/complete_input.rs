// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete bounded child output with an exact stdin request, without staging input.

use super::{Cmd, Live, Ran, RunFailed, System, fixed_command};
use aterm_uds::deadline::{CompleteOutputError, RunError};
use aterm_uds::spawnfd::Stdio;
use std::io::{self, Write as _};
use std::os::fd::AsRawFd as _;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

pub(super) fn admit(cmd: &Cmd) -> Result<(), RunFailed> {
    if !cmd.program.is_absolute() {
        return Err(RunFailed::Refused(
            "piped input requires an absolute program".into(),
        ));
    }
    if cmd.keep_on_timeout {
        return Err(RunFailed::Refused(
            "complete stdout cannot keep timed-out partial output".into(),
        ));
    }
    if cmd.stdin.is_some() {
        return Err(RunFailed::Refused(
            "the complete-input request reserves stdin for its private pipe".into(),
        ));
    }
    Ok(())
}

pub(super) fn run(live: &Live, cmd: &Cmd, input: &[u8]) -> Result<Ran, RunFailed> {
    admit(cmd)?;
    live.capture_current()?;
    let (program, args) = live.argv(cmd)?;
    let command = fixed_command(&program, &args, &live.child_env(cmd), cmd.cwd.as_deref());
    let (reader, writer) =
        io::pipe().map_err(|_| RunFailed::Spawn("input pipe creation failed".into()))?;
    nonblocking(&writer).map_err(|_| RunFailed::Spawn("input pipe configuration failed".into()))?;
    let deadline = super::deadline_after(cmd.budget);
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        // Keep the command's pipe reader on this closure's stack, so unwinding closes it
        // before scope auto-joins the writer. The cancellation lease drops before both.
        let mut command = command;
        let writer = std::thread::Builder::new()
            .name("child-complete-input".into())
            .spawn_scoped(scope, || write_input(writer, input, deadline, &cancelled))
            .map_err(|_| RunFailed::Spawn("input writer could not start".into()))?;
        struct Cancel<'a>(&'a AtomicBool);
        impl Drop for Cancel<'_> {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let cancel = Cancel(&cancelled);
        let output = aterm_uds::deadline::output_until_complete_stdout_from_unless(
            &mut command,
            Stdio::from(reader),
            deadline,
            &mut || live.stopped().is_some(),
        );
        drop(cancel);
        drop(command);
        let written = writer.join();
        let output = output.map_err(|error| {
            live.capture_failure(match error {
                CompleteOutputError::Run(RunError::TimedOut) => RunFailed::TimedOut(cmd.budget),
                CompleteOutputError::Run(RunError::Spawn(_)) => {
                    RunFailed::Spawn("child spawn failed".into())
                }
                CompleteOutputError::Run(RunError::Capture(_)) => {
                    RunFailed::AfterStart("child output capture setup failed".into())
                }
                CompleteOutputError::Run(RunError::Wait(_)) => {
                    RunFailed::AfterStart("child wait failed".into())
                }
                CompleteOutputError::StdoutRead(_) => {
                    RunFailed::AfterStart("complete stdout read failed".into())
                }
                CompleteOutputError::StdoutTooLarge { limit } => {
                    RunFailed::AfterStart(format!("complete stdout exceeds {limit} bytes"))
                }
                CompleteOutputError::StdoutIncomplete => {
                    RunFailed::AfterStart("complete stdout did not reach EOF".into())
                }
            })
        })?;
        if !matches!(written, Ok(Ok(()))) {
            return Err(RunFailed::AfterStart(
                "the input pipe did not accept the complete request".into(),
            ));
        }
        Ok(Ran {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    })
}

fn nonblocking(writer: &io::PipeWriter) -> io::Result<()> {
    let fd = writer.as_raw_fd();
    // SAFETY: the writer owns this live descriptor. These operations affect only its
    // status flags; the descriptor is never borrowed by the child or another writer.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn write_input(
    mut writer: io::PipeWriter,
    mut input: &[u8],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    while !input.is_empty() {
        if cancelled.load(Ordering::Acquire) || Instant::now() >= deadline {
            return Err(io::ErrorKind::Interrupted.into());
        }
        match writer.write(input) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => input = &input[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(Instant::now());
                let ms = i32::try_from(left.as_millis().saturating_add(1).min(100)).unwrap_or(100);
                let mut poll = libc::pollfd {
                    fd: writer.as_raw_fd(),
                    events: libc::POLLOUT,
                    revents: 0,
                };
                // SAFETY: one initialized pollfd with a live owned descriptor; poll
                // retains no pointer and cannot close that descriptor.
                if unsafe { libc::poll(&mut poll, 1, ms) } < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() != io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "complete_input_tests.rs"]
mod tests;
