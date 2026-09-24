// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE BOUNDED CHILD RUNNER — [`capture_bounded`], and nothing else.
//!
//! This module used to be the packaging contract and the alignment verdict
//! (design `docs/DESIGN-aterm-wrapper-2026-09-17.md` §1.2, §3.1-§3.8):
//! `harness.toml` admission, the probes, the per-capability verdict and the
//! signed × local intersection, rendered by `aterm harness align|caps`. No
//! publisher of a harness tree ever existed, no verdict it computed gated
//! anything a session did, and the verbs and the row vocabulary in
//! `atpkg::harness` were deleted with the second harness stack on 2026-09-23
//! (§0.4 of that design). What stays is the one piece something still runs:
//! the bounded subprocess [`super::disk`] reads `df` through. The name is
//! kept because `disk` calls it by this path.
//!
//! # No background polling
//!
//! [`capture_bounded`] parks on stdout until its deadline, then checks a child
//! that closed stdout but has not exited at a bounded 20 ms cadence. That
//! short-lived reap check is what lets the caller keep the child handle and
//! kill it at the same deadline; there is no resident timer or background
//! scanner.
//!
//! STATUS (docs/README.md honesty ratchet): unit-tested, and the worker
//! lifecycle is Tier-1 bound to `harness_capture_worker_lifecycle_model` in
//! `aterm-spec` (the tests in `align_tests.rs` drive the real runner with an
//! escaped descendant holding each pipe).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};
#[cfg(unix)]
use std::{
    os::fd::AsRawFd,
    sync::{Arc, atomic::AtomicBool, atomic::Ordering},
};

/// On non-Unix hosts the reader still uses a blocking pipe. Do not join it
/// indefinitely if a descendant kept the write end open after the child died.
#[cfg(not(unix))]
const READER_CLOSE_GRACE: Duration = Duration::from_millis(500);
/// The process may close stdout before it exits. Check its exit without
/// spinning, and never wait beyond the caller's absolute deadline.
const REAP_CHECK: Duration = Duration::from_millis(20);
/// An outgoing pipe with no reader parks in `poll`, but should notice that the
/// child has already exited without charging the next footer a full deadline.
#[cfg(unix)]
const WRITER_CANCEL_CHECK: Duration = Duration::from_millis(50);

/// Per-invocation witness for the Tier-1 lifecycle bind. It records the real
/// worker exits and child reap, rather than inferring them from a successful
/// return. Compiled only into Unix tests; production carries a zero-sized marker.
#[cfg(all(test, unix))]
#[derive(Default)]
struct CaptureLifecycleObservation {
    child_reaped: AtomicBool,
    reader_done: AtomicBool,
    writer_done: AtomicBool,
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

/// Run one bounded child and hand back its stdout, or the SENTENCE that says why
/// there is none — the shape a harness subprocess borrows, because a
/// deadline-free `wait_with_output` is a hang waiting for a stale mount or a
/// user command that loops.
///
/// The environment is CLEARED and rebuilt from `PATH`, `HOME`, `TMPDIR` plus
/// whatever the caller already set on `cmd`, so a child cannot be steered by
/// whatever the session happened to export. One thread reads stdout while
/// this one blocks in `recv_timeout`; after EOF this thread checks `try_wait`
/// only until the same absolute deadline. `stdin` is written on a thread of
/// its own so a child that never reads cannot block the caller on a full pipe.
/// On Unix both pipe workers use nonblocking descriptors and `poll` with this
/// same deadline. A descendant that escapes the child's process group cannot
/// leave a reader or writer thread behind by retaining an inherited pipe.
/// The child gets a private process group so ordinary descendants are killed
/// together at timeout.
///
/// # Errors
///
/// The child will not spawn, its stdout cannot be captured, it ran past
/// `budget`, or it closed its output and did not exit.
pub fn capture_bounded(
    cmd: Command,
    budget: Duration,
    stdin: Option<Vec<u8>>,
    max_stdout: usize,
) -> Result<Captured, String> {
    capture_bounded_impl(cmd, budget, stdin, max_stdout, CaptureObserver::default())
}

fn capture_bounded_impl(
    mut cmd: Command,
    budget: Duration,
    stdin: Option<Vec<u8>>,
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
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
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
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    let deadline = Instant::now() + budget;
    let mut writer = CaptureWriter::new(stdin.zip(child.stdin.take()), deadline, observer.clone());
    let Some(stdout) = child.stdout.take() else {
        kill_capture_child(&mut child, &observer);
        writer.finish();
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
            writer.finish();
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
                // A non-Unix blocking read cannot be interrupted if an
                // escaped descendant still owns the pipe.
                let _ = rx.recv_timeout(READER_CLOSE_GRACE);
                if reader.is_finished() {
                    let _ = reader.join();
                }
            }
            writer.finish();
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
                writer.finish();
                return Err(format!("it could not be reaped: {error}"));
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            kill_capture_child(&mut child, &observer);
            writer.finish();
            return Err(String::from(
                "it closed its output and did not exit before its deadline",
            ));
        }
        std::thread::sleep(REAP_CHECK.min(remaining));
    };
    writer.finish();
    Ok(Captured {
        stdout: bytes,
        code: status.code(),
    })
}

/// Own the child's stdin until it has seen EOF, or until the common deadline
/// or completion of the child cancels the write. The caller joins the Unix
/// worker before returning, including when a detached descendant retained
/// the read end. Other platforms retain the previous fail-soft detached path.
struct CaptureWriter {
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    stop: Arc<AtomicBool>,
}

impl CaptureWriter {
    fn new(
        stdin: Option<(Vec<u8>, std::process::ChildStdin)>,
        deadline: Instant,
        observer: CaptureObserver,
    ) -> Self {
        #[cfg(unix)]
        let stop = Arc::new(AtomicBool::new(false));
        #[cfg(not(unix))]
        let _ = deadline;
        #[cfg(all(test, unix))]
        if stdin.is_none()
            && let Some(observer) = &observer
        {
            observer.writer_done.store(true, Ordering::Release);
        }
        #[cfg(not(all(test, unix)))]
        let _ = observer;
        let thread = stdin.map(|(bytes, mut pipe)| {
            #[cfg(unix)]
            let worker_stop = Arc::clone(&stop);
            #[cfg(all(test, unix))]
            let writer_observer = observer.clone();
            std::thread::spawn(move || {
                // The moved handle closes here, so a reader that consumes all
                // input observes EOF without a second pipe or relay thread.
                #[cfg(unix)]
                let _ = write_capture_pipe(&mut pipe, &bytes, deadline, &worker_stop);
                #[cfg(not(unix))]
                let _ = pipe.write_all(&bytes);
                #[cfg(all(test, unix))]
                if let Some(observer) = writer_observer {
                    observer.writer_done.store(true, Ordering::Release);
                }
            })
        });
        Self {
            thread,
            #[cfg(unix)]
            stop,
        }
    }

    fn finish(&mut self) {
        #[cfg(unix)]
        {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
        #[cfg(not(unix))]
        {
            // The non-Unix writer may still be in a blocking `write_all`.
            let _ = self.thread.take();
        }
    }
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
fn capture_pipe_ready(
    fd: std::os::fd::RawFd,
    event: libc::c_short,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> std::io::Result<()> {
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "capture pipe cancelled",
            ));
        }
        let mut remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(capture_pipe_deadline());
        }
        if stop.is_some() {
            remaining = remaining.min(WRITER_CANCEL_CHECK);
        }
        // A positive sub-millisecond wait still needs one kernel sleep.
        let millis = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
        let mut ready = libc::pollfd {
            fd,
            events: event,
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
fn read_capture_pipe(
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
                capture_pipe_ready(pipe.as_raw_fd(), libc::POLLIN, deadline, None)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

#[cfg(unix)]
fn write_capture_pipe(
    pipe: &mut std::process::ChildStdin,
    bytes: &[u8],
    deadline: Instant,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    capture_pipe_nonblocking(pipe.as_raw_fd())?;
    let mut written = 0;
    while written < bytes.len() {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(capture_pipe_deadline());
        }
        match pipe.write(&bytes[written..]) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "capture stdin accepted no bytes",
                ));
            }
            Ok(n) => written += n,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                capture_pipe_ready(pipe.as_raw_fd(), libc::POLLOUT, deadline, Some(stop))?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Stop the child AND descendants that inherited its stdout, then reap the
/// direct child. Killing only the direct child leaves a reader thread parked
/// when a shell has already spawned a background process holding the pipe.
fn kill_capture_child(child: &mut Child, observer: &CaptureObserver) {
    #[cfg(unix)]
    if let Ok(pgid) = libc::pid_t::try_from(child.id())
        && pgid > 1
    {
        // SAFETY: `process_group(0)` made this child's PID its private PGID
        // before exec. The unreaped Child still reserves the PID, so the
        // negative group id cannot have been reused by an unrelated process.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    if child.wait().is_ok() {
        mark_capture_child_reaped(observer);
    }
}

#[path = "align_tests.rs"]
#[cfg(test)]
mod tests;
