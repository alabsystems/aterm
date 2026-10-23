// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete bounded child output with an exact stdin request, without staging input.

use super::{Cmd, Live, Ran, RunFailed, fixed_command};
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
    let (program, args) = live.argv(cmd)?;
    let mut command = fixed_command(&program, &args, &live.child_env(cmd), cmd.cwd.as_deref());
    let (reader, writer) =
        io::pipe().map_err(|_| RunFailed::Spawn("input pipe creation failed".into()))?;
    nonblocking(&writer).map_err(|_| RunFailed::Spawn("input pipe configuration failed".into()))?;
    let deadline = Instant::now() + cmd.budget;
    let cancelled = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let writer = std::thread::Builder::new()
            .name("child-complete-input".into())
            .spawn_scoped(scope, || write_input(writer, input, deadline, &cancelled))
            .map_err(|_| RunFailed::Spawn("input writer could not start".into()))?;
        let output = aterm_uds::deadline::output_until_complete_stdout_from(
            &mut command,
            Stdio::from(reader),
            deadline,
        );
        // A failed spawn can leave the Command owning its supplied read descriptor;
        // drop it before joining. Nonblocking writes plus cancellation also cover a
        // descendant retaining stdin after its direct parent exits.
        cancelled.store(true, Ordering::Release);
        drop(command);
        let written = writer.join();
        let output = output.map_err(|error| match error {
            CompleteOutputError::Run(RunError::TimedOut) => RunFailed::TimedOut(cmd.budget),
            CompleteOutputError::Run(RunError::Spawn(_) | RunError::Wait(_)) => {
                RunFailed::Spawn("child spawn or wait failed".into())
            }
            CompleteOutputError::StdoutRead(_) => {
                RunFailed::Refused("complete stdout read failed".into())
            }
            CompleteOutputError::StdoutTooLarge { limit } => {
                RunFailed::Refused(format!("complete stdout exceeds {limit} bytes"))
            }
            CompleteOutputError::StdoutIncomplete => {
                RunFailed::Refused("complete stdout did not reach EOF".into())
            }
        })?;
        if !matches!(written, Ok(Ok(()))) {
            return Err(RunFailed::Refused(
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
                // A short bounded poll lets capture failure cancel the writer even if
                // a descendant keeps the pipe open without reading it.
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
mod tests {
    use super::*;
    use crate::port::{Fake, System};
    use std::time::Duration;

    #[test]
    fn complete_input_streams_beyond_pipe_capacity_without_a_file_or_extra_newline() {
        let input = vec![b'q'; 256 * 1024];
        let out = Live::new()
            .run_complete_stdout_input(&Cmd::new("/bin/cat"), &input)
            .unwrap();
        assert!(out.success());
        assert_eq!(out.stdout, input);
    }

    #[test]
    fn complete_input_refuses_output_overflow_and_an_unread_request_at_its_deadline() {
        let live = Live::new();
        let input = vec![b'x'; aterm_uds::deadline::OUTPUT_KEEP + 1];
        let error = live
            .run_complete_stdout_input(&Cmd::new("/bin/cat"), &input)
            .unwrap_err();
        assert!(error.to_string().contains("complete stdout exceeds"));
        let error = live
            .run_complete_stdout_input(
                &Cmd::new("/bin/sleep").arg("60").budget(Duration::ZERO),
                &input,
            )
            .unwrap_err();
        assert!(matches!(error, RunFailed::TimedOut(_)));
    }

    #[test]
    fn complete_input_is_absent_from_fake_calls_fed_records_and_failure_diagnostics() {
        let sys = Fake::root();
        let input = b"private-input-must-never-be-recorded";
        let directory = aterm_tempfile::tempdir().unwrap();
        let cmd = Cmd::new(directory.path().join("absent-input"));
        sys.on(&cmd.display(), Ran::ok("done"));
        assert_eq!(
            sys.run_complete_stdout_input(&cmd, input).unwrap().stdout,
            b"done"
        );
        assert_eq!(sys.calls(), [cmd.display()]);
        assert!(sys.fed().is_empty());
        let error = Live::new()
            .run_complete_stdout_input(&cmd, input)
            .unwrap_err();
        assert!(
            !error
                .to_string()
                .contains(std::str::from_utf8(input).unwrap())
        );
        let file_stdin = cmd.stdin_file(directory.path().join("unopened-input-file"));
        let before = sys.calls();
        assert!(sys.run_complete_stdout_input(&file_stdin, input).is_err());
        assert_eq!(sys.calls(), before);
    }
}
