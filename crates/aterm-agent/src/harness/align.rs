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
    capture_bounded_impl(cmd, budget, max_stdout, CaptureObserver::default())
}

fn capture_bounded_impl(
    mut cmd: Command,
    budget: Duration,
    max_stdout: usize,
    observer: CaptureObserver,
) -> Result<Captured, String> {
    let mut keep: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (name, value) in cmd.get_envs() {
        if let Some(value) = value {
            keep.push((name.to_os_string(), value.to_os_string()));
        }
    }
    cmd.env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
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
    let deadline = Instant::now() + budget;
    #[cfg(all(test, unix))]
    observed_step(&observer, |o| &o.before_spawn);
    let mut child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    #[cfg(all(test, unix))]
    let deadline = if observed_step(&observer, |o| &o.after_spawn) {
        Instant::now() + budget
    } else {
        deadline
    };
    let Some(stdout) = child.stdout.take() else {
        kill_capture_child(&mut child, &observer);
        return Err(String::from("its stdout could not be captured"));
    };
    let (tx, rx) = mpsc::channel();
    #[cfg(all(test, unix))]
    let reader_observer = observer.clone();
    let reader = std::thread::spawn(move || {
        #[cfg(unix)]
        let read = read_capture_pipe(stdout, max_stdout, deadline);
        #[cfg(not(unix))]
        let mut buf = Vec::new();
        #[cfg(not(unix))]
        let read = stdout
            .take(max_stdout as u64)
            .read_to_end(&mut buf)
            .map(|_| buf);
        #[cfg(all(test, unix))]
        if let Some(observer) = reader_observer {
            observer.reader_done.store(true, Ordering::Release);
        }
        let _ = tx.send(read);
    });
    let bytes = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(bytes)) => {
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
    Ok(Captured {
        stdout: bytes,
        code: status.code(),
    })
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

#[cfg(unix)]
fn capture_pipe_deadline() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "capture pipe deadline")
}

#[cfg(unix)]
fn capture_pipe_ready(fd: std::os::fd::RawFd, deadline: Instant) -> std::io::Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(capture_pipe_deadline());
        }
        let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `ready` is a live stack pollfd; its fd belongs to this
        // worker and remains open for the duration of this call.
        match unsafe { libc::poll(&mut ready, 1, millis) } {
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
    mut pipe: std::process::ChildStdout,
    max_stdout: usize,
    deadline: Instant,
) -> std::io::Result<Vec<u8>> {
    if max_stdout == 0 {
        return Ok(Vec::new());
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
            Ok(0) => return Ok(bytes),
            Ok(n) => bytes.extend_from_slice(&chunk[..n]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                capture_pipe_ready(pipe.as_raw_fd(), deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

/// Stop the child and descendants that inherited its stdout, then reap the
/// direct child.
fn kill_capture_child(child: &mut Child, observer: &CaptureObserver) {
    // `process_group(0)` made the child's PID its private PGID before exec, and the unreaped
    // Child still reserves it, so the group id cannot have been reused by an unrelated process.
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id()) {
        let _ = aterm_uds::process::kill_group_until_empty(pgid);
    }
    let _ = child.kill();
    if child.wait().is_ok() {
        mark_capture_child_reaped(observer);
    }
}

#[path = "align_tests.rs"]
#[cfg(test)]
mod tests;
