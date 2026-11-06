// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete-stdout capture for child processes with bounded output.

use super::{
    DRAIN_GRACE, ExitWait, Grace, OUTPUT_KEEP, PipeEnd, RunError, drain, pipe_end, reap_within,
};
use crate::spawnfd::{Child, Command, Stdio};
use std::io::{self, Read};
use std::process::Output;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Hard maximum for an explicitly selected complete-stdout capture.
/// The default APIs retain their existing one-MiB limit.
pub const MAX_COMPLETE_STDOUT_BYTES: usize = 16 * 1024 * 1024;

/// Why a call requiring complete stdout cannot use the child's output.
#[derive(Debug)]
pub enum CompleteOutputError {
    /// The child creation, post-creation capture, wait or deadline refusal.
    Run(RunError),
    /// Reading stdout failed; bytes read before it are not a complete result.
    StdoutRead(io::Error),
    /// At least one stdout byte exceeded the bounded complete capture.
    StdoutTooLarge {
        /// The inclusive maximum complete stdout length.
        limit: usize,
    },
    /// Stdout did not prove EOF by the collection deadline, or its reader disappeared.
    StdoutIncomplete,
}

impl std::fmt::Display for CompleteOutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(error) => std::fmt::Display::fmt(error, f),
            Self::StdoutRead(error) => write!(f, "stdout could not be read completely: {error}"),
            Self::StdoutTooLarge { limit } => {
                write!(
                    f,
                    "stdout exceeds the complete-capture limit of {limit} bytes"
                )
            }
            Self::StdoutIncomplete => f.write_str("stdout did not reach proven EOF"),
        }
    }
}

impl std::error::Error for CompleteOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(error) => Some(error),
            Self::StdoutRead(error) => Some(error),
            Self::StdoutTooLarge { .. } | Self::StdoutIncomplete => None,
        }
    }
}

/// A complete-output refusal together with custody of the original child and retained guards.
/// The cause is independent of whether the child has positively reaped.
#[cfg(unix)]
pub struct RetainedOutputError<T: Send + 'static = ()> {
    error: CompleteOutputError,
    custody: Box<super::ChildCustody<T>>,
}

#[cfg(unix)]
impl<T: Send + 'static> RetainedOutputError<T> {
    /// Recover the unchanged cause and its actual-child custody.
    pub fn into_parts(self) -> (CompleteOutputError, super::ChildCustody<T>) {
        (self.error, *self.custody)
    }
}

#[cfg(unix)]
impl<T: Send + 'static> std::fmt::Debug for RetainedOutputError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedOutputError")
            .field("error", &self.error)
            .field("custody", &self.custody)
            .finish()
    }
}

#[cfg(unix)]
impl<T: Send + 'static> std::fmt::Display for RetainedOutputError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

#[cfg(unix)]
impl<T: Send + 'static> std::error::Error for RetainedOutputError<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Capture complete stdout while retaining `retained` through every pending owning reap.
/// Success returns both output and the original guards. Failure returns the unchanged cause
/// and actual child custody; explicitly leaving that custody to the reaper proves no exit.
///
/// `cleanup` bounds the combined repeated group-signalling and reap observation after failure.
/// Synchronous process creation and individual kernel calls remain outside that polling bound.
/// Only leader completion is established: guards requiring descendant drain or an executable
/// start witness still need their stronger owner. Stderr retains the bounded-tail policy.
///
/// # Errors
/// Child refusal, capture/wait/deadline failure, or stdout without proven complete EOF.
#[cfg(unix)]
pub fn output_until_complete_stdout_retained<T: Send + 'static>(
    cmd: &mut Command,
    deadline: Instant,
    cleanup: Duration,
    retained: T,
) -> Result<(Output, T), RetainedOutputError<T>> {
    capture_retained(cmd, deadline, cleanup, retained, drain_stdout)
}

#[cfg(unix)]
fn capture_retained<T: Send + 'static>(
    cmd: &mut Command,
    deadline: Instant,
    cleanup: Duration,
    retained: T,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
) -> Result<(Output, T), RetainedOutputError<T>> {
    cmd.process_group(0);
    let child = match cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            return Err(RetainedOutputError {
                error: CompleteOutputError::Run(RunError::Spawn(error)),
                custody: Box::new(super::ChildCustody::not_started(retained)),
            });
        }
    };
    let mut owned = RetainedCapture {
        child: Some(child),
        retained: Some(retained),
        cleanup,
    };
    let mut reaped = false;
    match collect_started(
        owned.child.as_mut().expect("capture owns its child"),
        deadline,
        stdout_reader,
        &mut reaped,
    ) {
        Ok(output) => owned.finish(output),
        Err(error) => Err(RetainedOutputError {
            error,
            custody: Box::new(owned.take_custody()),
        }),
    }
}

/// Own the guards before reading begins, including through an unwind in a reader or wait.
#[cfg(unix)]
struct RetainedCapture<T: Send + 'static> {
    child: Option<Child>,
    retained: Option<T>,
    cleanup: Duration,
}

#[cfg(unix)]
impl<T: Send + 'static> RetainedCapture<T> {
    fn take_custody(&mut self) -> super::ChildCustody<T> {
        let mut child = self.child.take().expect("capture owns its child");
        let retained = self.retained.take().expect("capture retains its guards");
        let by = super::saturating_after(Instant::now(), self.cleanup);
        if !child.is_reaped() {
            if let Ok(group) = i32::try_from(child.id()) {
                let _ = crate::process::kill_group_until_empty_within(
                    group,
                    by.saturating_duration_since(Instant::now()),
                );
            }
            let _ = child.kill();
        }
        let mut custody = super::ChildCustody::from_child(child, retained);
        // No positive observation (including an error) can release the original guards.
        let _ = custody.observe_until(by);
        custody
    }

    fn finish(mut self, output: Output) -> Result<(Output, T), RetainedOutputError<T>> {
        let child = self.child.take().expect("capture owns its child");
        let retained = self.retained.take().expect("capture retains its guards");
        let mut custody = super::ChildCustody::from_child(child, retained);
        let error = match custody.observe_until(Instant::now()) {
            Ok(Some(status)) if status == output.status => {
                return match custody.try_into_retained() {
                    Ok(retained) => Ok((output, retained)),
                    Err(custody) => Err(RetainedOutputError {
                        error: CompleteOutputError::Run(RunError::Wait(io::Error::other(
                            "positive child reap did not permit guard recovery",
                        ))),
                        custody,
                    }),
                };
            }
            Err(error) => error,
            _ => io::Error::other("complete output has no matching positive owning reap"),
        };
        Err(RetainedOutputError {
            error: CompleteOutputError::Run(RunError::Wait(error)),
            custody: Box::new(custody),
        })
    }
}

#[cfg(unix)]
impl<T: Send + 'static> Drop for RetainedCapture<T> {
    fn drop(&mut self) {
        if self.child.is_some() {
            self.take_custody().reap_in_background();
        }
    }
}

/// Runs `cmd` with closed stdin and requires complete stdout up to [`OUTPUT_KEEP`] bytes.
///
/// # Errors
/// A child refusal or stdout whose EOF, read or size cannot be vouched for.
pub fn output_until_complete_stdout(
    cmd: &mut Command,
    deadline: Instant,
) -> Result<Output, CompleteOutputError> {
    output_until_complete_stdout_observed(cmd, deadline, || {})
}

/// Capture complete stdout with an explicit inclusive limit no larger than
/// [`MAX_COMPLETE_STDOUT_BYTES`]. Stderr retains the existing bounded-tail policy.
/// A zero limit accepts only empty stdout with proven EOF.
///
/// # Errors
/// An `InvalidInput` spawn refusal before dispatch for an excessive limit;
/// otherwise the same refusals as [`output_until_complete_stdout`].
pub fn output_until_complete_stdout_with_limit(
    cmd: &mut Command,
    deadline: Instant,
    limit: usize,
) -> Result<Output, CompleteOutputError> {
    if limit > MAX_COMPLETE_STDOUT_BYTES {
        return Err(CompleteOutputError::Run(RunError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "complete stdout limit exceeds hard maximum",
        ))));
    }
    capture(
        cmd,
        Stdio::null(),
        deadline,
        || {},
        move |pipe, stream| drain_stdout_with_limit(pipe, stream, limit),
    )
}

/// As [`output_until_complete_stdout`], reporting successful process creation before any
/// output reader starts. A later reader-start error must not be treated as no dispatch.
/// If the observer unwinds, cleanup signals the owned child and makes a bounded reap attempt
/// before unwinding continues. An unreaped child remains subject to the Child drop reaper;
/// this is not a receipt that reaping or descendant drain completed.
///
/// # Errors
/// The same errors as [`output_until_complete_stdout`].
pub fn output_until_complete_stdout_observed(
    cmd: &mut Command,
    deadline: Instant,
    on_started: impl FnOnce(),
) -> Result<Output, CompleteOutputError> {
    output_until_complete_stdout_from_observed(cmd, Stdio::null(), deadline, on_started)
}

/// Runs `cmd` with `stdin` until `deadline`, requiring complete stdout up to [`OUTPUT_KEEP`].
///
/// # Errors
/// [`CompleteOutputError`] for a child refusal, stdout read failure, overflow, or missing EOF.
pub fn output_until_complete_stdout_from(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
) -> Result<Output, CompleteOutputError> {
    output_until_complete_stdout_from_observed(cmd, stdin, deadline, || {})
}

/// As [`output_until_complete_stdout_from`], with the successful process-creation witness
/// described by [`output_until_complete_stdout_observed`].
///
/// # Errors
/// The same errors as [`output_until_complete_stdout_from`].
pub fn output_until_complete_stdout_from_observed(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    on_started: impl FnOnce(),
) -> Result<Output, CompleteOutputError> {
    capture(cmd, stdin, deadline, on_started, drain_stdout)
}

fn capture(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    on_started: impl FnOnce(),
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
) -> Result<Output, CompleteOutputError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CompleteOutputError::Run(RunError::Spawn(error)))?;
    observe_started(&mut child, on_started);
    let mut reaped = false;
    let result = collect_started(&mut child, deadline, stdout_reader, &mut reaped);
    if result.is_err() && !reaped {
        end_bounded(&mut child);
    }
    result
}

fn collect_started(
    child: &mut Child,
    deadline: Instant,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    reaped: &mut bool,
) -> Result<Output, CompleteOutputError> {
    let stdout = match child.stdout.take() {
        Some(stream) => match stdout_reader(pipe_end(&stream), stream) {
            Ok(stdout) => stdout,
            Err(error) => {
                return Err(CompleteOutputError::Run(RunError::Capture(error)));
            }
        },
        None => {
            return Err(CompleteOutputError::StdoutIncomplete);
        }
    };
    let stderr = match child
        .stderr
        .take()
        .map(|stream| {
            let pipe = pipe_end(&stream);
            drain(stream, pipe, "stderr")
        })
        .transpose()
    {
        Ok(stderr) => stderr,
        Err(error) => {
            return Err(CompleteOutputError::Run(RunError::Capture(error)));
        }
    };
    let mut exit = ExitWait::new(child);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                *reaped = true;
                break status;
            }
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(CompleteOutputError::Run(RunError::TimedOut));
                }
                exit.wait(remaining);
            }
            Err(error) => {
                return Err(CompleteOutputError::Run(RunError::Wait(error)));
            }
        }
    };
    let grace = Grace {
        by: Instant::now() + DRAIN_GRACE,
        until: deadline,
        group: Some(child.id()),
    };
    let stdout = stdout.collect(&grace)?;
    let stderr = stderr
        .map(|stream| stream.collect(&grace))
        .unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn observe_started(child: &mut Child, observer: impl FnOnce()) {
    struct Guard<'a> {
        child: &'a mut Child,
        armed: bool,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            if self.armed {
                end_bounded(self.child);
            }
        }
    }
    let mut guard = Guard { child, armed: true };
    observer();
    guard.armed = false;
}

fn end_bounded(child: &mut Child) {
    #[cfg(unix)]
    super::children::kill_group(child);
    #[cfg(not(unix))]
    let _ = child.kill();
    reap_within(child, None, Instant::now() + Duration::from_secs(60));
}

struct CompleteDrain {
    ended: mpsc::Receiver<Result<Vec<u8>, CompleteOutputError>>,
    pipe: Option<PipeEnd>,
}

impl CompleteDrain {
    fn collect(self, grace: &Grace) -> Result<Vec<u8>, CompleteOutputError> {
        let ended = match self
            .ended
            .recv_timeout(grace.by.saturating_duration_since(Instant::now()))
        {
            Err(mpsc::RecvTimeoutError::Timeout) => match grace.read_on(self.pipe.as_ref()) {
                Some(wait) => self.ended.recv_timeout(wait),
                None => Err(mpsc::RecvTimeoutError::Timeout),
            },
            ended => ended,
        };
        ended.map_err(|_| CompleteOutputError::StdoutIncomplete)?
    }
}

fn read_stdout(mut stream: impl Read, limit: usize) -> Result<Vec<u8>, CompleteOutputError> {
    let mut bytes = Vec::new();
    let mut too_large = false;
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => {
                return if too_large {
                    Err(CompleteOutputError::StdoutTooLarge { limit })
                } else {
                    Ok(bytes)
                };
            }
            Ok(n) => {
                let keep = n.min(limit.saturating_sub(bytes.len()));
                bytes.extend_from_slice(&chunk[..keep]);
                too_large |= keep != n;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(CompleteOutputError::StdoutRead(error)),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_stdout(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
) -> io::Result<CompleteDrain> {
    drain_stdout_with_limit(pipe, stream, OUTPUT_KEEP)
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_stdout_with_limit(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
    limit: usize,
) -> io::Result<CompleteDrain> {
    let (finished, ended) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("child-complete-stdout".into())
        .spawn(move || {
            let result = read_stdout(stream, limit);
            let _ = finished.send(result);
        })?;
    Ok(CompleteDrain { ended, pipe })
}

#[cfg(test)]
#[path = "complete_tests.rs"]
mod tests;
