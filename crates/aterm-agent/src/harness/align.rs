// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bounded child runner, [`capture_bounded`]: a child read to a deadline
//! (the `claude auth` probes of [`super::cli`]).

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::mpsc;
#[cfg(all(test, unix))]
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::{Duration, Instant};

/// On non-Unix hosts the reader still uses a blocking pipe.
#[cfg(not(unix))]
const READER_CLOSE_GRACE: Duration = Duration::from_millis(500);
/// The process may close stdout before it exits.
const REAP_CHECK: Duration = Duration::from_millis(20);

/// Per-invocation witness for the Tier-1 lifecycle bind: it records the real
/// worker exit and child reap rather than inferring them from a successful
/// return.
#[cfg(all(test, unix))]
#[derive(Default)]
struct CaptureLifecycleObservation {
    child_reaped: AtomicBool,
    reader_done: AtomicBool,
    /// Run after spawn, and the deadline taken again after it, so what a
    /// test's child must have done by then (an escape test's holder leaving
    /// for its own session) happens before the deadline under any load.
    after_spawn: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Run after the deadline is taken and before the spawn: a spawn that
    /// takes this long, charged to the budget.
    before_spawn: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

/// Take and run `step` of `observer`, if a test set one: `true` when one ran.
#[cfg(all(test, unix))]
fn observed_step(
    observer: &CaptureObserver,
    step: impl FnOnce(
        &CaptureLifecycleObservation,
    ) -> &std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
) -> bool {
    let Some(observer) = observer else {
        return false;
    };
    let slot = step(observer);
    let run = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    run.map(|run| run()).is_some()
}

#[cfg(all(test, unix))]
type CaptureObserver = Option<Arc<CaptureLifecycleObservation>>;
#[cfg(not(all(test, unix)))]
#[derive(Clone, Default)]
struct CaptureObserver {
    _marker: (),
}

fn mark_capture_child_reaped(observer: &CaptureObserver) {
    #[cfg(all(test, unix))]
    if let Some(observer) = observer {
        observer.child_reaped.store(true, Ordering::Release);
    }
    #[cfg(not(all(test, unix)))]
    let _ = observer;
}

/// What one bounded child did: the bytes it printed, and how it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// Its stdout, capped at the caller's byte budget.
    pub stdout: Vec<u8>,
    /// Its exit code; `None` when a signal took it.
    pub code: Option<i32>,
}

impl Captured {
    /// Did it exit 0?
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }
}

/// Run one bounded child and hand back its stdout, or the sentence that says
/// why there is none: a deadline-free `wait_with_output` would hang on a stale
/// mount or a looping command.
///
/// # Errors
///
/// The child will not spawn, its stdout cannot be captured, it ran past
/// `budget`, or it closed its output and did not exit.
pub fn capture_bounded(
    cmd: Command,
    budget: Duration,
    max_stdout: usize,
) -> Result<Captured, String> {
    match capture_bounded_impl(cmd, budget, max_stdout, None, CaptureObserver::default())? {
        Ended::Exited(got) => Ok(got),
        // No stop text was watched for, so no capture ends on one.
        Ended::Stopped => Err(String::from("it was stopped")),
    }
}

/// How a watched capture ([`capture_bounded_until`]) ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ended {
    /// The child exited and closed its stdout before the deadline, as [`capture_bounded`] reports.
    Exited(Captured),
    /// The child printed the stop text (on stdout or, on Unix, stderr) and its whole process
    /// group was killed then, without waiting for the deadline.
    Stopped,
}

/// [`capture_bounded`] that also watches the child's stdout and (on Unix) its stderr for `stop`,
/// ending the read the moment either prints it: a child that would otherwise wait out its budget
/// on something only a person can do (a sign-in prompt) is stopped as soon as it says so. On
/// other hosts stderr is not read and stdout is checked as it is read.
///
/// # Errors
///
/// As [`capture_bounded`]: the child will not spawn, its output cannot be captured, it ran past
/// `budget` without printing `stop`, or it closed its output and did not exit.
pub fn capture_bounded_until(
    cmd: Command,
    budget: Duration,
    max_stdout: usize,
    stop: &'static [u8],
) -> Result<Ended, String> {
    capture_bounded_impl(
        cmd,
        budget,
        max_stdout,
        (!stop.is_empty()).then_some(stop),
        CaptureObserver::default(),
    )
}

/// How often a watched capture drains the child's stderr while it waits.
#[cfg(unix)]
const STDERR_WATCH: Duration = Duration::from_millis(25);

/// The child's stderr under watch for a stop text: a non-blocking pipe the waiting thread
/// drains, keeping only the tail a match can straddle, so the child never blocks on it and no
/// second reader thread exists to outlive a return.
#[cfg(unix)]
struct StderrWatch {
    pipe: std::io::PipeReader,
    stop: &'static [u8],
    tail: Vec<u8>,
}

#[cfg(unix)]
impl StderrWatch {
    /// Drain what the child has written so far: `true` once `stop` was among it.
    fn seen(&mut self) -> bool {
        let mut chunk = [0u8; 8192];
        loop {
            match self.pipe.read(&mut chunk) {
                Ok(0) => return false,
                Ok(n) => {
                    if append_and_find(&mut self.tail, &chunk[..n], self.stop) {
                        return true;
                    }
                    let keep = self.stop.len().saturating_sub(1);
                    let cut = self.tail.len().saturating_sub(keep);
                    self.tail.drain(..cut);
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
    }
}

/// The non-Unix stdout reader: blocking reads of at most `max_stdout` bytes, each chunk checked
/// for `stop` as it arrives.
#[cfg(not(unix))]
fn read_blocking_until(
    stdout: std::process::ChildStdout,
    max_stdout: usize,
    stop: Option<&[u8]>,
) -> std::io::Result<(Vec<u8>, bool)> {
    let mut stdout = stdout.take(max_stdout as u64);
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stdout.read(&mut chunk) {
            Ok(0) => return Ok((bytes, false)),
            Ok(n) => {
                let seen = match stop {
                    Some(stop) => append_and_find(&mut bytes, &chunk[..n], stop),
                    None => {
                        bytes.extend_from_slice(&chunk[..n]);
                        false
                    }
                };
                if seen {
                    return Ok((bytes, true));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Append `chunk` to `seen` and say whether `stop` now occurs in the part that `chunk` could
/// have completed.
fn append_and_find(seen: &mut Vec<u8>, chunk: &[u8], stop: &[u8]) -> bool {
    let from = seen.len().saturating_sub(stop.len().saturating_sub(1));
    seen.extend_from_slice(chunk);
    !stop.is_empty() && seen[from..].windows(stop.len()).any(|w| w == stop)
}

fn capture_bounded_impl(
    mut cmd: Command,
    budget: Duration,
    max_stdout: usize,
    stop: Option<&'static [u8]>,
    observer: CaptureObserver,
) -> Result<Ended, String> {
    let mut keep: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (name, value) in cmd.get_envs() {
        if let Some(value) = value {
            keep.push((name.to_os_string(), value.to_os_string()));
        }
    }
    cmd.env_clear().stdin(Stdio::null()).stdout(Stdio::piped());
    #[cfg(unix)]
    let mut stderr_watch = match stop {
        Some(stop) => {
            let (pipe, writer) =
                std::io::pipe().map_err(|e| format!("its stderr could not be watched: {e}"))?;
            capture_pipe_nonblocking(pipe.as_raw_fd())
                .map_err(|e| format!("its stderr could not be watched: {e}"))?;
            cmd.stderr(Stdio::from(writer));
            Some(StderrWatch {
                pipe,
                stop,
                tail: Vec::new(),
            })
        }
        None => {
            cmd.stderr(Stdio::null());
            None
        }
    };
    #[cfg(not(unix))]
    cmd.stderr(Stdio::null());
    for pass in ["PATH", "HOME", "TMPDIR"] {
        if let Some(value) = std::env::var_os(pass) {
            cmd.env(pass, value);
        }
    }
    for (name, value) in keep {
        cmd.env(name, value);
    }
    #[cfg(unix)]
    cmd.process_group(0);
    let after_now = |d: Duration| {
        let now = Instant::now();
        now.checked_add(d)
            .unwrap_or_else(|| now + Duration::from_secs(3600))
    };
    let deadline = after_now(budget);
    #[cfg(all(test, unix))]
    observed_step(&observer, |o| &o.before_spawn);
    let mut child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    // The parent's copy of the stderr pipe's write end goes with the command, so the watch sees
    // the pipe end once the child's group has closed it.
    drop(cmd);
    #[cfg(all(test, unix))]
    let deadline = if observed_step(&observer, |o| &o.after_spawn) {
        after_now(budget)
    } else {
        deadline
    };
    let Some(stdout) = child.stdout.take() else {
        kill_capture_child(&mut child, &observer);
        return Err(String::from("its stdout could not be captured"));
    };
    let (tx, rx) = mpsc::channel();
    // A watched capture can end on stderr while the reader still waits on stdout: dropping
    // `cancel_writer` wakes it.
    #[cfg(unix)]
    let (cancel_reader, cancel_writer) = match stop {
        Some(_) => match std::io::pipe() {
            Ok((r, w)) => (Some(r), Some(w)),
            Err(e) => {
                kill_capture_child(&mut child, &observer);
                return Err(format!("its stderr could not be watched: {e}"));
            }
        },
        None => (None, None),
    };
    #[cfg(all(test, unix))]
    let reader_observer = observer.clone();
    let reader = std::thread::spawn(move || {
        #[cfg(unix)]
        let read = read_capture_pipe_until(
            stdout,
            max_stdout,
            deadline,
            stop,
            cancel_reader.as_ref().map(AsRawFd::as_raw_fd),
        );
        #[cfg(not(unix))]
        let read = read_blocking_until(stdout, max_stdout, stop);
        #[cfg(all(test, unix))]
        if let Some(observer) = reader_observer {
            observer.reader_done.store(true, Ordering::Release);
        }
        let _ = tx.send(read);
    });
    #[cfg(unix)]
    let waited = loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let Some(watch) = stderr_watch.as_mut() else {
            break rx.recv_timeout(remaining);
        };
        match rx.recv_timeout(remaining.min(STDERR_WATCH)) {
            Err(mpsc::RecvTimeoutError::Timeout) if !remaining.is_zero() => {
                if watch.seen() {
                    kill_capture_child(&mut child, &observer);
                    drop(cancel_writer);
                    let _ = reader.join();
                    return Ok(Ended::Stopped);
                }
            }
            other => break other,
        }
    };
    #[cfg(not(unix))]
    let waited = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let bytes = match waited {
        Ok(Ok((_, true))) => {
            let _ = reader.join();
            kill_capture_child(&mut child, &observer);
            return Ok(Ended::Stopped);
        }
        Ok(Ok((bytes, false))) => {
            let _ = reader.join();
            bytes
        }
        Ok(Err(error)) => {
            let _ = reader.join();
            kill_capture_child(&mut child, &observer);
            if error.kind() == std::io::ErrorKind::TimedOut {
                return Err(format!(
                    "it ran past its {} ms deadline",
                    budget.as_millis()
                ));
            }
            return Err(format!("its output was unreadable: {error}"));
        }
        Err(_) => {
            kill_capture_child(&mut child, &observer);
            #[cfg(unix)]
            let _ = reader.join();
            #[cfg(not(unix))]
            {
                let _ = rx.recv_timeout(READER_CLOSE_GRACE);
                if reader.is_finished() {
                    let _ = reader.join();
                }
            }
            return Err(format!(
                "it ran past its {} ms deadline",
                budget.as_millis()
            ));
        }
    };
    let status = loop {
        #[cfg(unix)]
        if stderr_watch.as_mut().is_some_and(StderrWatch::seen) {
            kill_capture_child(&mut child, &observer);
            return Ok(Ended::Stopped);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                mark_capture_child_reaped(&observer);
                break status;
            }
            Ok(None) => {}
            Err(error) => {
                kill_capture_child(&mut child, &observer);
                return Err(format!("it could not be reaped: {error}"));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            kill_capture_child(&mut child, &observer);
            return Err(String::from(
                "it closed its output and did not exit before its deadline",
            ));
        }
        std::thread::sleep(REAP_CHECK.min(remaining));
    };
    // What the child wrote to stderr before it exited is still in the pipe.
    #[cfg(unix)]
    if stderr_watch.as_mut().is_some_and(StderrWatch::seen) {
        return Ok(Ended::Stopped);
    }
    Ok(Ended::Exited(Captured {
        stdout: bytes,
        code: status.code(),
    }))
}

#[cfg(unix)]
fn capture_pipe_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: the worker exclusively owns this pipe end until it drops it;
    // `fcntl` changes only this end's open-file-description flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// The waiting thread dropped the cancel pipe's write end: the capture already ended.
#[cfg(unix)]
fn capture_cancelled() -> std::io::Error {
    std::io::Error::other("capture cancelled")
}

#[cfg(unix)]
fn capture_pipe_deadline() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "capture pipe deadline")
}

#[cfg(unix)]
fn capture_pipe_ready(
    fd: std::os::fd::RawFd,
    cancel: Option<std::os::fd::RawFd>,
    deadline: Instant,
) -> std::io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(capture_pipe_deadline());
        }
        let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        let mut ready = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                // A negative fd is ignored by poll: no cancel pipe, nothing watched.
                fd: cancel.unwrap_or(-1),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `ready` is a live stack array of two pollfds; each fd belongs to this
        // worker and remains open for the duration of this call (or is -1, which poll skips).
        match unsafe { libc::poll(ready.as_mut_ptr(), 2, millis) } {
            n if n > 0 && ready[1].revents != 0 => return Err(capture_cancelled()),
            n if n > 0 => return Ok(()),
            0 => {}
            _ => {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }
    }
}

#[cfg(unix)]
pub(super) fn read_capture_pipe(
    pipe: std::process::ChildStdout,
    max_stdout: usize,
    deadline: Instant,
) -> std::io::Result<Vec<u8>> {
    read_capture_pipe_until(pipe, max_stdout, deadline, None, None).map(|(bytes, _)| bytes)
}

/// [`read_capture_pipe`] that returns early, with `true`, once `stop` occurs in what was read,
/// and with an error once `cancel` (the read end of a pipe whose writer the waiting thread holds)
/// becomes readable or hangs up: a capture stopped on stderr joins this reader at once even
/// while a holder outside the killed group keeps stdout open.
#[cfg(unix)]
fn read_capture_pipe_until(
    mut pipe: std::process::ChildStdout,
    max_stdout: usize,
    deadline: Instant,
    stop: Option<&[u8]>,
    cancel: Option<std::os::fd::RawFd>,
) -> std::io::Result<(Vec<u8>, bool)> {
    if max_stdout == 0 {
        return Ok((Vec::new(), false));
    }
    capture_pipe_nonblocking(pipe.as_raw_fd())?;
    let mut bytes = Vec::with_capacity(max_stdout.min(8192));
    let mut chunk = [0u8; 8192];
    while bytes.len() < max_stdout {
        if Instant::now() >= deadline {
            return Err(capture_pipe_deadline());
        }
        let ask = chunk.len().min(max_stdout - bytes.len());
        match pipe.read(&mut chunk[..ask]) {
            Ok(0) => return Ok((bytes, false)),
            Ok(n) => {
                let seen = match stop {
                    Some(stop) => append_and_find(&mut bytes, &chunk[..n], stop),
                    None => {
                        bytes.extend_from_slice(&chunk[..n]);
                        false
                    }
                };
                if seen {
                    return Ok((bytes, true));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                capture_pipe_ready(pipe.as_raw_fd(), cancel, deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok((bytes, false))
}

/// Stop the child and descendants that inherited its stdout, then reap the
/// direct child.
fn kill_capture_child(child: &mut Child, observer: &CaptureObserver) {
    // `process_group(0)` made the child's PID its private PGID before exec, and the unreaped
    // Child still reserves it, so the group id cannot have been reused by an unrelated process.
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        #[cfg(not(test))]
        let _ = aterm_uds::process::kill_group_until_empty(pgid);
        // A unit test's group must empty, a child its shell forked as it was killed included,
        // and a loaded machine can keep that shell from exiting within the product's 5 s.
        #[cfg(test)]
        let _ =
            aterm_uds::process::kill_group_until_empty_within(pgid, aterm_spec::wait::MUST_HAPPEN);
    }
    let _ = child.kill();
    if child.wait().is_ok() {
        mark_capture_child_reaped(observer);
    }
}

#[path = "align_tests.rs"]
#[cfg(test)]
mod tests;
