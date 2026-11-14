// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Complete capture for child processes with bounded output.

use super::{
    DRAIN_GRACE, ExitWait, Grace, OUTPUT_KEEP, PipeEnd, ProcessProgressProbe, ProgressWindow,
    RunError, STOP_TICK, deadline_now, drain, pipe_end,
};
use crate::spawnfd::{Child, Command, Stdio};
use std::io::{self, Read};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Hard maximum for an explicitly selected complete-stdout capture.
/// The default APIs retain their existing one-MiB limit.
pub const MAX_COMPLETE_STDOUT_BYTES: usize = 16 * 1024 * 1024;

/// Hard maximum for [`output_until_complete_listing`]: 64 MiB, which holds Spotlight's listing
/// of every page of several toolchains' docs (`aterm doctor`'s `spotlight:` row reads one of
/// some 22 MB on a Mac with three, and refuses one past this rather than count it short).
pub const MAX_COMPLETE_LISTING_BYTES: usize = 64 * 1024 * 1024;

/// Why a call requiring complete output cannot use the child's output. Stdout failures
/// have their own variants; stderr proof failures use [`RunError::Capture`].
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

/// Why a process-progress-bounded complete-stdout child did not produce an output.
#[derive(Debug)]
pub enum CompleteProgressError {
    /// Ordinary complete-output capture failure.
    Complete(CompleteOutputError),
    /// One whole evidence window passed without captured bytes or descendant CPU/I/O progress.
    Stalled {
        /// The inactivity evidence window.
        window: Duration,
        /// The operation the caller was waiting for, without secret argv or environment values.
        waiting_on: String,
    },
}

impl std::fmt::Display for CompleteProgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Complete(error) => std::fmt::Display::fmt(error, f),
            Self::Stalled { window, waiting_on } => write!(
                f,
                "produced no stdout/stderr bytes and its descendant tree made no CPU or I/O \
                 progress for {window:?} while waiting on {waiting_on}"
            ),
        }
    }
}

impl std::error::Error for CompleteProgressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Complete(error) => Some(error),
            Self::Stalled { .. } => None,
        }
    }
}

impl From<CompleteOutputError> for CompleteProgressError {
    fn from(error: CompleteOutputError) -> Self {
        Self::Complete(error)
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
        &mut None,
        StderrCapture::Tail,
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

/// Runs `cmd` with closed stdin while captured output bytes or a complete descendant-tree
/// CPU/I/O generation advances within `window`, requiring complete stdout up to [`OUTPUT_KEEP`].
/// An unread or incomparable process observation disarms the inactivity window.
///
/// # Errors
/// The same capture errors as [`output_until_complete_stdout`], or
/// [`CompleteProgressError::Stalled`] after one whole evidence window.
pub fn output_while_process_progressing_complete_stdout(
    cmd: &mut Command,
    window: Duration,
    waiting_on: impl Into<String>,
) -> Result<Output, CompleteProgressError> {
    capture_progress(cmd, window, waiting_on.into(), drain_stdout, None)
}

/// As [`output_while_process_progressing_complete_stdout`], observing `stopped` at least every
/// [`STOP_TICK`]. A cancelled child is ended and reaped, publishes no partial output, and is a
/// post-spawn [`RunError::TimedOut`] inside [`CompleteProgressError::Complete`], distinct from a
/// child whose process and output evidence actually stalled.
///
/// # Errors
/// As [`output_while_process_progressing_complete_stdout`], or cancellation after process
/// creation.
pub fn output_while_process_progressing_complete_stdout_unless(
    cmd: &mut Command,
    window: Duration,
    waiting_on: impl Into<String>,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteProgressError> {
    capture_progress(cmd, window, waiting_on.into(), drain_stdout, Some(stopped))
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
    capped(cmd, deadline, limit, MAX_COMPLETE_STDOUT_BYTES, None)
}

/// As [`output_until_complete_stdout_with_limit`], observing `stopped` at least every
/// [`STOP_TICK`] as [`output_until_complete_stdout_from_unless`] does: a cancelled child is
/// ended and reaped, publishes no partial output, and is a post-spawn [`RunError::TimedOut`].
///
/// # Errors
/// As [`output_until_complete_stdout_with_limit`], or cancellation after process creation.
pub fn output_until_complete_stdout_with_limit_unless(
    cmd: &mut Command,
    deadline: Instant,
    limit: usize,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteOutputError> {
    capped(
        cmd,
        deadline,
        limit,
        MAX_COMPLETE_STDOUT_BYTES,
        Some(stopped),
    )
}

/// As [`output_until_complete_stdout_with_limit`], for a listing that runs to tens of MiB: an
/// explicit inclusive limit no larger than [`MAX_COMPLETE_LISTING_BYTES`], so the callers sized
/// by [`MAX_COMPLETE_STDOUT_BYTES`] keep their bound.
///
/// # Errors
/// An `InvalidInput` spawn refusal before dispatch for an excessive limit;
/// otherwise the same refusals as [`output_until_complete_stdout`].
pub fn output_until_complete_listing(
    cmd: &mut Command,
    deadline: Instant,
    limit: usize,
) -> Result<Output, CompleteOutputError> {
    capped(cmd, deadline, limit, MAX_COMPLETE_LISTING_BYTES, None)
}

/// As [`output_until_complete_listing`], ending `within` from now.
///
/// # Errors
/// The same errors as [`output_until_complete_listing`].
pub fn output_within_complete_listing(
    cmd: &mut Command,
    within: Duration,
    limit: usize,
) -> Result<Output, CompleteOutputError> {
    output_until_complete_listing(cmd, super::saturating_after(Instant::now(), within), limit)
}

/// A complete capture of at most `limit` bytes of stdout, refused before dispatch past `cap`,
/// cancelled once `stopped` says so.
fn capped(
    cmd: &mut Command,
    deadline: Instant,
    limit: usize,
    cap: usize,
    stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Output, CompleteOutputError> {
    if limit > cap {
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
        stopped,
    )
}

/// Capture complete stdout read as `separator`-ended records, keeping only those `keep`
/// accepts: the run reads its whole stdout to proven EOF however long it is, while what it holds
/// stays within `limit` (at most [`MAX_COMPLETE_STDOUT_BYTES`]). The answer's stdout is the kept
/// records, each with its separator; a last record with none is offered too and kept as it ends.
/// Stderr retains the bounded-tail policy.
///
/// # Errors
/// An `InvalidInput` spawn refusal before dispatch for an excessive limit;
/// [`CompleteOutputError::StdoutTooLarge`] once the kept records, or one record being read,
/// pass `limit`; otherwise the same refusals as [`output_until_complete_stdout`].
pub fn output_within_complete_stdout_records(
    cmd: &mut Command,
    within: Duration,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
) -> Result<Output, CompleteOutputError> {
    output_within_complete_stdout_records_from(cmd, Stdio::null(), within, separator, limit, keep)
}

/// As [`output_within_complete_stdout_records`], with `stdin` as the child's standard input.
///
/// # Errors
/// The same errors as [`output_within_complete_stdout_records`].
pub fn output_within_complete_stdout_records_from(
    cmd: &mut Command,
    stdin: Stdio,
    within: Duration,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
) -> Result<Output, CompleteOutputError> {
    if limit > MAX_COMPLETE_STDOUT_BYTES {
        return Err(CompleteOutputError::Run(RunError::Spawn(io::Error::new(
            io::ErrorKind::InvalidInput,
            "complete stdout limit exceeds hard maximum",
        ))));
    }
    let deadline = super::saturating_after(Instant::now(), within);
    capture(
        cmd,
        stdin,
        deadline,
        || {},
        move |pipe, stream| drain_stdout_records(pipe, stream, separator, limit, keep),
        None,
    )
}

/// Complete separator-terminated records with explicit cancellation. `keep` may
/// stream records into owned storage while returning false to retain no heap
/// output. A final unterminated record is refused. This collector owns its
/// direct child with process-group cancellation. This establishes no descendant-
/// drain proof; cleanup may outlive the work deadline.
///
/// # Errors
/// The same errors as [`output_within_complete_stdout_records_from`], a stop,
/// or an unterminated final record. Cancellation uses the same owning collector.
pub fn output_within_complete_stdout_records_from_unless(
    cmd: &mut Command,
    stdin: Stdio,
    within: Duration,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteOutputError> {
    if limit > MAX_COMPLETE_STDOUT_BYTES {
        return Err(CompleteOutputError::StdoutTooLarge {
            limit: MAX_COMPLETE_STDOUT_BYTES,
        });
    }
    capture(
        cmd,
        stdin,
        super::saturating_after(Instant::now(), within),
        || {},
        move |pipe, stream| drain_stdout_records_policy(pipe, stream, separator, limit, keep, true),
        Some(stopped),
    )
}

/// Progress-bounded twin of [`output_within_complete_stdout_records_from_unless`].
/// Input is null, as for the existing progress-bounded complete-stdout collector.
///
/// # Errors
/// Complete-record, explicit-stop and native-progress failures are preserved.
pub fn output_while_process_progressing_complete_stdout_records_unless(
    cmd: &mut Command,
    window: Duration,
    waiting_on: impl Into<String>,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteProgressError> {
    if limit > MAX_COMPLETE_STDOUT_BYTES {
        return Err(CompleteOutputError::StdoutTooLarge {
            limit: MAX_COMPLETE_STDOUT_BYTES,
        }
        .into());
    }
    capture_progress(
        cmd,
        window,
        waiting_on.into(),
        move |pipe, stream| drain_stdout_records_policy(pipe, stream, separator, limit, keep, true),
        Some(stopped),
    )
}

/// As [`output_until_complete_stdout`], reporting successful process creation before any
/// output reader starts. A later reader-start error must not be treated as no dispatch.
/// Every post-spawn return or unwind retains the original child until its positive owning
/// wait. Cleanup may outlive the work deadline. This establishes no descendant-drain receipt.
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
    capture(cmd, stdin, deadline, on_started, drain_stdout, None)
}

/// As [`output_until_complete_stdout_from`], observing `stopped` at least every [`STOP_TICK`]
/// while waiting for the child or output. Cancellation publishes no partial output and is a
/// post-spawn [`RunError::TimedOut`], not a spawn refusal. The original child stays owned
/// through cleanup until a positive owning wait, even on an unwind or wait error; cleanup may
/// outlive the work deadline. No group is signalled after its leader has been reaped, and no
/// descendant-drain receipt is supplied.
///
/// # Errors
/// As [`output_until_complete_stdout_from`], or cancellation after process creation.
pub fn output_until_complete_stdout_from_unless(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteOutputError> {
    capture(cmd, stdin, deadline, || {}, drain_stdout, Some(stopped))
}

/// Capture both stdout and stderr to proven EOF, each within [`OUTPUT_KEEP`] bytes.
/// Cancellation, deadlines and child ownership follow
/// [`output_until_complete_stdout_from_unless`]. No partial stream is published.
///
/// # Errors
/// The same child and stdout refusals as [`output_until_complete_stdout_from_unless`].
/// A stderr read failure, overflow or missing EOF is a post-spawn [`RunError::Capture`].
pub fn output_until_complete_from_unless(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, CompleteOutputError> {
    capture_streams(
        cmd,
        stdin,
        deadline,
        || {},
        drain_stdout,
        Some(stopped),
        StderrCapture::Complete,
    )
}

#[derive(Clone, Copy)]
enum StderrCapture {
    Tail,
    Complete,
}

fn capture(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    on_started: impl FnOnce(),
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Output, CompleteOutputError> {
    capture_streams(
        cmd,
        stdin,
        deadline,
        on_started,
        stdout_reader,
        stopped,
        StderrCapture::Tail,
    )
}

fn capture_streams(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    on_started: impl FnOnce(),
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    mut stopped: Option<&mut dyn FnMut() -> bool>,
    stderr_capture: StderrCapture,
) -> Result<Output, CompleteOutputError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CompleteOutputError::Run(RunError::Spawn(error)))?;
    let owned = CaptureGuard { child: &mut child };
    observe_started(owned.child, on_started);
    let mut reaped = false;
    collect_started(
        owned.child,
        deadline,
        stdout_reader,
        &mut reaped,
        &mut stopped,
        stderr_capture,
    )
}

fn capture_progress(
    cmd: &mut Command,
    window: Duration,
    waiting_on: String,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Output, CompleteProgressError> {
    let mut probe = ProcessProgressProbe::default();
    capture_progress_with(
        cmd,
        window,
        waiting_on,
        stdout_reader,
        stopped,
        &mut deadline_now,
        &mut |pid| probe.read(pid),
    )
}

/// [`capture_progress`] on the clock `now`, reading the child's tree through `work`.
fn capture_progress_with(
    cmd: &mut Command,
    window: Duration,
    waiting_on: String,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    mut stopped: Option<&mut dyn FnMut() -> bool>,
    now: &mut dyn FnMut() -> Instant,
    work: &mut dyn FnMut(u32) -> Option<u64>,
) -> Result<Output, CompleteProgressError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| CompleteOutputError::Run(RunError::Spawn(error)))?;
    let owned = CaptureGuard { child: &mut child };
    let mut reaped = false;
    let mut left = ProgressWindow::collector(window, now, work);
    let mut timed_out = || RunError::TimedOut;
    let can_stop = stopped.is_some();
    let cancelled = std::cell::Cell::new(false);
    let mut observe_stop = || {
        let stop = stopped.as_mut().is_some_and(|stop| stop());
        cancelled.set(cancelled.get() || stop);
        stop
    };
    let mut stop: Option<&mut dyn FnMut() -> bool> =
        can_stop.then_some(&mut observe_stop as &mut dyn FnMut() -> bool);
    match collect_started_with(
        owned.child,
        stdout_reader,
        &mut reaped,
        &mut stop,
        &mut left,
        &mut timed_out,
        StderrCapture::Tail,
    ) {
        Err(error @ CompleteOutputError::Run(RunError::TimedOut)) if cancelled.get() => {
            Err(CompleteProgressError::Complete(error))
        }
        Err(CompleteOutputError::Run(RunError::TimedOut)) => {
            Err(CompleteProgressError::Stalled { window, waiting_on })
        }
        Err(error) => Err(CompleteProgressError::Complete(error)),
        Ok(output) => Ok(output),
    }
}

/// Hold the actual child across reader creation, callbacks and output collection. A failure
/// cannot release its caller's cleanup lifetime while the owning wait remains unproven.
struct CaptureGuard<'a> {
    child: &'a mut Child,
}

impl Drop for CaptureGuard<'_> {
    fn drop(&mut self) {
        super::settle_child(self.child);
    }
}

fn collect_started(
    child: &mut Child,
    deadline: Instant,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    reaped: &mut bool,
    stopped: &mut Option<&mut dyn FnMut() -> bool>,
    stderr_capture: StderrCapture,
) -> Result<Output, CompleteOutputError> {
    let mut left = |_, _| deadline.saturating_duration_since(deadline_now());
    let mut timed_out = || RunError::TimedOut;
    collect_started_with(
        child,
        stdout_reader,
        reaped,
        stopped,
        &mut left,
        &mut timed_out,
        stderr_capture,
    )
}

fn collect_started_with(
    child: &mut Child,
    stdout_reader: impl FnOnce(Option<PipeEnd>, std::process::ChildStdout) -> io::Result<CompleteDrain>,
    reaped: &mut bool,
    stopped: &mut Option<&mut dyn FnMut() -> bool>,
    left: &mut dyn FnMut(Option<u32>, u64) -> Duration,
    overrun: &mut dyn FnMut() -> RunError,
    stderr_capture: StderrCapture,
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
    let stderr = match StderrDrain::start(child.stderr.take(), stderr_capture) {
        Ok(stderr) => stderr,
        Err(error) => {
            return Err(CompleteOutputError::Run(RunError::Capture(error)));
        }
    };
    let mut exit = ExitWait::new(child);
    let captured = |stdout: &CompleteDrain, stderr: &StderrDrain| {
        stdout.bytes().saturating_add(stderr.bytes())
    };
    let status = loop {
        if stopped.as_mut().is_some_and(|stop| stop()) {
            return Err(CompleteOutputError::Run(RunError::TimedOut));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                *reaped = true;
                break status;
            }
            Ok(None) => {
                let remaining = left(Some(child.id()), captured(&stdout, &stderr));
                if remaining.is_zero() {
                    return Err(CompleteOutputError::Run(overrun()));
                }
                exit.wait(if stopped.is_some() {
                    remaining.min(STOP_TICK)
                } else {
                    remaining
                });
            }
            Err(error) => {
                return Err(CompleteOutputError::Run(RunError::Wait(error)));
            }
        }
    };
    let grace = Grace {
        by: Instant::now() + DRAIN_GRACE,
        until: super::saturating_after(Instant::now(), left(None, captured(&stdout, &stderr))),
        group: Some(child.id()),
    };
    let stdout = stdout.collect_unless(&grace, stopped)?;
    let stderr = stderr.collect_unless(&grace, stopped)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

enum StderrDrain {
    Tail(Option<super::Drain>),
    Complete(CompleteDrain),
}

impl StderrDrain {
    fn start(stream: Option<std::process::ChildStderr>, mode: StderrCapture) -> io::Result<Self> {
        match (mode, stream) {
            (StderrCapture::Tail, stream) => stream
                .map(|stream| {
                    let pipe = pipe_end(&stream);
                    drain(stream, pipe, "stderr")
                })
                .transpose()
                .map(Self::Tail),
            (StderrCapture::Complete, Some(stream)) => {
                drain_complete_with_limit(pipe_end(&stream), stream, OUTPUT_KEEP, "stderr")
                    .map(Self::Complete)
            }
            (StderrCapture::Complete, None) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stderr pipe is absent",
            )),
        }
    }

    fn bytes(&self) -> u64 {
        match self {
            Self::Tail(stream) => stream.as_ref().map_or(0, super::Drain::bytes),
            Self::Complete(stream) => stream.bytes(),
        }
    }

    fn collect_unless(
        self,
        grace: &Grace,
        stopped: &mut Option<&mut dyn FnMut() -> bool>,
    ) -> Result<Vec<u8>, CompleteOutputError> {
        match self {
            Self::Tail(stream) => stream
                .map(|stream| stream.collect_unless(grace, stopped))
                .transpose()
                .map(Option::unwrap_or_default)
                .map_err(CompleteOutputError::Run),
            Self::Complete(stream) => stream.collect_unless(grace, stopped).map_err(|error| {
                let error = match error {
                    CompleteOutputError::Run(error) => return CompleteOutputError::Run(error),
                    CompleteOutputError::StdoutRead(error) => io::Error::new(
                        error.kind(),
                        format!("stderr could not be read completely: {error}"),
                    ),
                    CompleteOutputError::StdoutTooLarge { limit } => io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("stderr exceeds the complete-capture limit of {limit} bytes"),
                    ),
                    CompleteOutputError::StdoutIncomplete => io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "stderr did not reach proven EOF",
                    ),
                };
                CompleteOutputError::Run(RunError::Capture(error))
            }),
        }
    }
}

fn observe_started(child: &mut Child, observer: impl FnOnce()) {
    struct Guard<'a> {
        child: &'a mut Child,
        armed: bool,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            if self.armed {
                super::settle_child(self.child);
            }
        }
    }
    let mut guard = Guard { child, armed: true };
    observer();
    guard.armed = false;
}

struct CompleteDrain {
    ended: mpsc::Receiver<Result<Vec<u8>, CompleteOutputError>>,
    total: Arc<AtomicU64>,
    pipe: Option<PipeEnd>,
}

impl CompleteDrain {
    fn bytes(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn collect(self, grace: &Grace) -> Result<Vec<u8>, CompleteOutputError> {
        self.collect_unless(grace, &mut None)
    }

    fn collect_unless(
        self,
        grace: &Grace,
        stopped: &mut Option<&mut dyn FnMut() -> bool>,
    ) -> Result<Vec<u8>, CompleteOutputError> {
        let receive = |wait, stopped: &mut Option<&mut dyn FnMut() -> bool>| {
            super::receive_unless(&self.ended, wait, stopped).map_err(CompleteOutputError::Run)
        };
        let ended = match receive(grace.by.saturating_duration_since(Instant::now()), stopped)? {
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // A live group can close its writer before the caller's budget ends.
                // The short diagnostic-tail grace cannot establish incomplete protocol data.
                let wait = grace
                    .read_on(self.pipe.as_ref())
                    .unwrap_or_else(|| grace.until.saturating_duration_since(Instant::now()));
                receive(wait, stopped)?
            }
            ended => ended,
        };
        ended.map_err(|_| CompleteOutputError::StdoutIncomplete)?
    }
}

#[cfg(test)]
fn read_stdout(stream: impl Read, limit: usize) -> Result<Vec<u8>, CompleteOutputError> {
    read_stdout_counted(stream, limit, &AtomicU64::new(0))
}

fn read_stdout_counted(
    mut stream: impl Read,
    limit: usize,
    total: &AtomicU64,
) -> Result<Vec<u8>, CompleteOutputError> {
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
                total.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
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
    drain_complete_with_limit(pipe, stream, limit, "stdout")
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_complete_with_limit(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
    limit: usize,
    name: &str,
) -> io::Result<CompleteDrain> {
    let (finished, ended) = mpsc::sync_channel(1);
    let total = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&total);
    std::thread::Builder::new()
        .name(format!("child-complete-{name}"))
        .spawn(move || {
            let result = read_stdout_counted(stream, limit, &counted);
            let _ = finished.send(result);
        })?;
    Ok(CompleteDrain { ended, total, pipe })
}

/// `stream` read to its end as `separator`-ended records, the ones `keep` accepts kept with
/// their separators, within `limit` held bytes. Past `limit` it reads on to the end, keeping
/// nothing, so the refusal still waits on the writer's EOF.
#[cfg(test)]
fn read_records(
    stream: impl Read,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool,
) -> Result<Vec<u8>, CompleteOutputError> {
    read_records_counted(stream, separator, limit, &AtomicU64::new(0), keep)
}

#[cfg(test)]
fn read_records_counted(
    stream: impl Read,
    separator: u8,
    limit: usize,
    total: &AtomicU64,
    keep: impl FnMut(&[u8]) -> bool,
) -> Result<Vec<u8>, CompleteOutputError> {
    read_records_counted_policy(stream, separator, limit, total, keep, false)
}

fn read_records_counted_policy(
    mut stream: impl Read,
    separator: u8,
    limit: usize,
    total: &AtomicU64,
    mut keep: impl FnMut(&[u8]) -> bool,
    require_terminator: bool,
) -> Result<Vec<u8>, CompleteOutputError> {
    let mut kept = Vec::new();
    let mut record = Vec::new();
    let mut too_large = false;
    let mut chunk = [0u8; 8192];
    let mut offer = |record: &[u8], body: &[u8], kept: &mut Vec<u8>| {
        if !keep(body) {
            return true;
        }
        if kept.len() + record.len() > limit {
            return false;
        }
        kept.extend_from_slice(record);
        true
    };
    loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                total.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                n
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(CompleteOutputError::StdoutRead(error)),
        };
        if too_large {
            continue;
        }
        for piece in chunk[..n].split_inclusive(|b| *b == separator) {
            record.extend_from_slice(piece);
            // `limit` bounds a record's body; its separator is not held against it.
            if record.len() - usize::from(record.last() == Some(&separator)) > limit {
                too_large = true;
                break;
            }
            if record.last() == Some(&separator) {
                let body = &record[..record.len() - 1];
                if !offer(&record, body, &mut kept) {
                    too_large = true;
                    break;
                }
                record.clear();
            }
        }
        if too_large {
            // Free what was held, not just empty it: nothing reads it again while the rest drains.
            drop(std::mem::take(&mut record));
            drop(std::mem::take(&mut kept));
        }
    }
    if require_terminator && !record.is_empty() && !too_large {
        return Err(CompleteOutputError::StdoutIncomplete);
    }
    if too_large || (!record.is_empty() && !offer(&record, &record, &mut kept)) {
        return Err(CompleteOutputError::StdoutTooLarge { limit });
    }
    Ok(kept)
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_stdout_records(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
) -> io::Result<CompleteDrain> {
    drain_stdout_records_policy(pipe, stream, separator, limit, keep, false)
}

#[cfg(not(target_arch = "wasm32"))]
fn drain_stdout_records_policy(
    pipe: Option<PipeEnd>,
    stream: impl Read + Send + 'static,
    separator: u8,
    limit: usize,
    keep: impl FnMut(&[u8]) -> bool + Send + 'static,
    require_terminator: bool,
) -> io::Result<CompleteDrain> {
    let (finished, ended) = mpsc::sync_channel(1);
    let total = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&total);
    std::thread::Builder::new()
        .name("child-complete-records".into())
        .spawn(move || {
            let result = read_records_counted_policy(
                stream,
                separator,
                limit,
                &counted,
                keep,
                require_terminator,
            );
            let _ = finished.send(result);
        })?;
    Ok(CompleteDrain { ended, total, pipe })
}

#[cfg(test)]
#[path = "complete_tests.rs"]
mod tests;
