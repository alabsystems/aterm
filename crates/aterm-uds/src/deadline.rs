// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadlines for blocking in-process calls and child processes.

#[cfg(any(unix, windows))]
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::io::{self, Read, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Output;

use crate::spawnfd::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

/// A poll(2) timeout: round positive millisecond fragments up and saturate at its signed limit.
/// Rounding down would turn the last fragment of a deadline into a busy zero-timeout poll.
#[must_use]
pub fn poll_timeout_ms(left: Duration) -> i32 {
    i32::try_from(left.as_nanos().div_ceil(1_000_000)).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "deadline_poll_timeout_tests.rs"]
mod poll_timeout_tests;

#[cfg(unix)]
mod children;

#[cfg(unix)]
mod input_unix;
#[cfg(windows)]
mod input_windows;
#[cfg(windows)]
pub use input_windows::{JobHold, spawn_in_job};

mod complete;
#[cfg(unix)]
mod custody;
pub use complete::{
    CompleteOutputError, CompleteProgressError, MAX_COMPLETE_LISTING_BYTES,
    MAX_COMPLETE_STDOUT_BYTES, output_until_complete_from_unless, output_until_complete_listing,
    output_until_complete_stdout, output_until_complete_stdout_from,
    output_until_complete_stdout_from_observed, output_until_complete_stdout_from_unless,
    output_until_complete_stdout_observed, output_until_complete_stdout_with_limit,
    output_until_complete_stdout_with_limit_unless,
    output_while_process_progressing_complete_stdout,
    output_while_process_progressing_complete_stdout_records_unless,
    output_while_process_progressing_complete_stdout_unless, output_within_complete_listing,
    output_within_complete_stdout_records, output_within_complete_stdout_records_from,
    output_within_complete_stdout_records_from_unless,
};
#[cfg(unix)]
pub use complete::{RetainedOutputError, output_until_complete_stdout_retained};
#[cfg(unix)]
pub use custody::{ChildCompletion, ChildCustody};

/// Maximum duration macOS may hold the first exec of a new binary in `syspolicyd`.
pub const FIRST_EXEC_HANG: Duration = Duration::from_secs(10 * 60);

/// Marker filename written by a fresh copy immediately after exec.
pub const RAN: &str = "ran";

/// Writes the [`RAN`] marker inside `home` only into a regular file (`O_NOFOLLOW | O_NONBLOCK |
/// O_CLOEXEC` and mode `0o600` on Unix).
/// # Errors
/// [`RAN`] inside `home` could not be opened or is not a regular file.
pub fn try_mark_ran(home: &std::path::Path) -> std::io::Result<()> {
    let ran_path = home.join(RAN);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&ran_path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", ran_path.display()),
            ));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&ran_path, b"")
    }
}

/// Marker filename written by [`stamp_exec`] before exec with the awake-clock reading.
pub const EXEC_STAMP: &str = "exec";

/// Writes the current awake-clock reading to [`EXEC_STAMP`] inside `home` before exec.
pub fn stamp_exec(home: &std::path::Path) {
    if let Err(e) = try_stamp_exec(home) {
        eprintln!("cannot stamp {} before its exec: {e}", home.display());
    }
}

/// [`stamp_exec`] for a caller that must know whether the stamp was written. It writes only into
/// a regular file: the open creates [`EXEC_STAMP`] or opens what is there without truncating it,
/// and the stamp replaces the file's text only once the opened file has passed every check. On
/// Unix the open follows no final symbolic link (`O_NOFOLLOW`) and does not wait for a FIFO's
/// reader (`O_NONBLOCK`), and a file with more than one name (a hard link) is refused, so a file
/// another program planted there is neither waited on nor written through. Elsewhere neither
/// kind of link is refused: a symbolic link there is followed and a regular file it names is
/// written, as is a file with other names.
/// # Errors
/// The stamp could not be written, or [`EXEC_STAMP`] there is anything but a regular file; on
/// Unix also a symbolic link, a FIFO with no reader, or a regular file with more than one name.
/// One refused is left as it was; a write cut short can leave the stamp blank, a stamp
/// [`exec_stamp`] reads no reading from.
pub fn try_stamp_exec(home: &std::path::Path) -> std::io::Result<()> {
    let stamp = crate::process::awake_ms()
        .map(|ms| ms.to_string())
        .unwrap_or_default();
    let path = home.join(EXEC_STAMP);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options.open(&path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return Err(io::Error::other(format!(
                "{} has {} names, not one",
                path.display(),
                metadata.nlink()
            )));
        }
    }
    file.set_len(0)?;
    file.write_all(stamp.as_bytes())
}

/// The longest [`EXEC_STAMP`] [`exec_stamp`] reads, in bytes: an awake-clock reading in
/// milliseconds has at most 20 digits.
const STAMP_MAX: u64 = 64;

/// The awake-clock reading [`EXEC_STAMP`] inside `home` holds
/// ([`crate::atomic_file::read_regular_text`]): none when it is missing or unreadable, is anything
/// but a regular file once a link there is followed, is longer than [`STAMP_MAX`] bytes or not
/// UTF-8, or is blank, anything but decimal digits once trimmed, or a number past [`u64::MAX`].
/// On Unix its open does not wait, so a FIFO another program planted there, or a link to one,
/// reads no stamp, as a blank one does, rather than hang the sweep that reads it. A link to a
/// regular stamp is read: a caller may keep its stamp behind one.
#[must_use]
pub fn exec_stamp(home: &std::path::Path) -> Option<u64> {
    crate::atomic_file::read_regular_text(&home.join(EXEC_STAMP), STAMP_MAX)
        .ok()
        .and_then(|text| {
            let s = text.trim();
            (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                .then(|| s.parse::<u64>().ok())
                .flatten()
        })
}

/// Whether the binary copy directory `home` is safe to remove at `now`.
#[must_use]
pub fn copy_may_go(home: &std::path::Path, now: std::time::SystemTime) -> bool {
    copy_wait_left(home, now).is_none()
}

/// Remaining duration before the binary copy in `home` may be removed at `now`.
#[must_use]
pub fn copy_wait_left(home: &std::path::Path, now: std::time::SystemTime) -> Option<Duration> {
    copy_wait_left_at(home, now, crate::process::awake_ms())
}

/// [`copy_wait_left`] at the awake-clock reading `awake_now` ([`crate::process::awake_ms`]).
pub(crate) fn copy_wait_left_at(
    home: &std::path::Path,
    now: std::time::SystemTime,
    awake_now: Option<u64>,
) -> Option<Duration> {
    if home.join(RAN).exists() {
        return None;
    }
    let age = match (exec_stamp(home), awake_now) {
        (Some(at), Some(awake)) => Some(Duration::from_millis(awake.checked_sub(at)?)),
        _ => std::fs::symlink_metadata(home)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| now.duration_since(at).ok()),
    };
    let left = FIRST_EXEC_HANG.saturating_sub(age.unwrap_or_default());
    (!left.is_zero()).then_some(left)
}

/// The words a wait ends COULD NOT RUN with when its child `pid` is held at macOS's program check past `waited`, its hang detector: `held` is [`crate::process::at_program_check`]'s answer, asked before the child is killed. `None` unless it is held, so any other silence stays the waiter's own failure. It stands outside the test seams below because a bench built with debug assertions off ends a wait this way too.
#[must_use]
pub fn held_words(pid: u32, waited: Duration, held: Option<bool>) -> Option<String> {
    (held == Some(true)).then(|| {
        format!(
            "macOS's program check held this test's program (pid {pid}) past its {waited:?} \
             hang detector; nothing was verified"
        )
    })
}

/// A test's pause between two looks at what must happen, in a poll its own hang detector bounds:
/// it gives the processor away (`std::thread::yield_now`). Nothing that waits for latency polls
/// this way: on macOS a yield while other work is runnable sets the thread aside for a whole
/// 10 ms scheduling quantum, so shipped code waits on a wake (clippy.toml: a wait that wakes).
#[cfg(any(test, debug_assertions))]
pub fn poll_yield() {
    #[expect(
        clippy::disallowed_methods,
        reason = "a test poll's one yield: its hang detector bounds it and no deadline waits on it"
    )]
    std::thread::yield_now();
}

#[cfg(any(test, debug_assertions))]
mod released;
#[cfg(any(test, debug_assertions))]
pub use released::once_released;

#[cfg(any(test, debug_assertions))]
mod silent;
#[cfg(all(test, any(target_vendor = "apple", target_os = "linux")))]
pub(crate) use silent::copy_regular_fresh;
#[cfg(all(unix, test))]
pub(crate) use silent::std_ends_within;
#[cfg(any(test, debug_assertions))]
pub use silent::{
    CODESIGN_HANG, ECHO_HANG, KeptUntilRan, ad_hoc_copy, require_disk_image_service,
    require_trust_service, sign_ad_hoc, stand_in, trust_service_silent,
};
#[cfg(all(unix, any(test, debug_assertions)))]
pub use silent::{cat_echoes, ends_within, prints};

#[cfg(all(
    any(target_vendor = "apple", target_os = "linux"),
    any(test, debug_assertions)
))]
mod execd;
#[cfg(all(
    any(target_vendor = "apple", target_os = "linux"),
    any(test, debug_assertions)
))]
pub use execd::execd;

#[cfg(all(unix, any(test, debug_assertions)))]
mod stopped;
#[cfg(all(unix, any(test, debug_assertions)))]
pub use stopped::{StopReport, await_stopped, spin_stopped, stop_state};

/// Shared gate for in-process calls that can block on macOS `trustd`.
pub static TRUST: Gate = Gate::new("aterm-trust-check");

/// Reason [`Gate::run`] or [`Gate::start`] produced no result.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// Work timed out, or an earlier call on the gate has exceeded its suspect age.
    Unanswered,
    /// Helper thread could not be spawned or panicked.
    Failed(String),
}

enum Slot<T> {
    /// The helper is still working and the caller is still waiting.
    Pending,
    /// The helper finished in time; the caller takes the value.
    Done(T),
    /// The caller stopped waiting. A helper that finds this drops its value.
    Abandoned,
}

/// One helper that has not yet returned.
#[derive(Debug)]
struct Running {
    id: u64,
    began: Instant,
    /// Past this age it means the service is not answering.
    suspect: Duration,
}

#[derive(Debug)]
struct Helpers {
    next: u64,
    running: Vec<Running>,
}

/// Runs closures on a named helper thread under a deadline.
#[derive(Debug)]
pub struct Gate {
    /// The helper threads' name.
    name: &'static str,
    helpers: Mutex<Helpers>,
}

impl Gate {
    /// Creates a gate whose helper threads use `name`.
    #[must_use]
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            helpers: Mutex::new(Helpers {
                next: 0,
                running: Vec::new(),
            }),
        }
    }

    /// Number of helper threads currently running on this gate.
    pub fn running(&self) -> usize {
        self.helpers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
            .len()
    }

    /// Runs `work` on a helper thread if it completes within `deadline`.
    ///
    /// # Errors
    /// Returns [`Refused::Unanswered`] on timeout or overdue gate, or [`Refused::Failed`] on spawn
    /// failure or panic.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run<T, F>(
        &'static self,
        suspect: Duration,
        deadline: Duration,
        work: F,
    ) -> Result<T, Refused>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        self.start(suspect.min(deadline), work)?
            .wait(deadline)
            .unwrap_or(Err(Refused::Unanswered))
    }

    /// Starts `work` on a helper thread and returns a [`Pending`] handle to wait on.
    ///
    /// # Errors
    /// Returns [`Refused::Unanswered`] if an earlier call is overdue, or [`Refused::Failed`] if no
    /// thread could start.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start<T, F>(&'static self, suspect: Duration, work: F) -> Result<Pending<T>, Refused>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let ours = Arc::new((
            Mutex::new(Slot::<std::thread::Result<T>>::Pending),
            Condvar::new(),
        ));
        let theirs = Arc::clone(&ours);
        let id = {
            let mut helpers = self.helpers.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            if helpers
                .running
                .iter()
                .any(|helper| now.saturating_duration_since(helper.began) > helper.suspect)
            {
                return Err(Refused::Unanswered);
            }
            let id = helpers.next;
            helpers.next = helpers.next.wrapping_add(1);
            helpers.running.push(Running {
                id,
                began: now,
                suspect,
            });
            id
        };
        let spawned = std::thread::Builder::new()
            .name(self.name.to_owned())
            .spawn(move || {
                let value = catch_unwind(AssertUnwindSafe(work));
                self.forget(id);
                let (lock, answered) = &*theirs;
                let mut slot = lock.lock().unwrap_or_else(PoisonError::into_inner);
                if !matches!(*slot, Slot::Abandoned) {
                    *slot = Slot::Done(value);
                    answered.notify_one();
                }
            });
        if let Err(error) = spawned {
            self.forget(id);
            return Err(Refused::Failed(format!(
                "no thread could be started: {error}"
            )));
        }
        Ok(Pending { slot: ours })
    }

    fn forget(&self, id: u64) {
        let mut helpers = self.helpers.lock().unwrap_or_else(PoisonError::into_inner);
        helpers.running.retain(|helper| helper.id != id);
    }
}

/// In-flight call started by [`Gate::start`]; abandoned if dropped before completion.
pub struct Pending<T> {
    slot: Arc<(Mutex<Slot<std::thread::Result<T>>>, Condvar)>,
}

impl<T> Pending<T> {
    /// Waits up to `within` for the call's answer, returning `Err(self)` if still pending.
    ///
    /// # Errors
    /// Returns `Err(self)` if the work has not yet finished within `within`.
    pub fn wait(self, within: Duration) -> Result<Result<T, Refused>, Self> {
        let answer = {
            let (lock, answered) = &*self.slot;
            let slot = lock.lock().unwrap_or_else(PoisonError::into_inner);
            let (mut slot, _) = answered
                .wait_timeout_while(slot, within.min(MAX_WITHIN), |slot| {
                    matches!(slot, Slot::Pending)
                })
                .unwrap_or_else(PoisonError::into_inner);
            match std::mem::replace(&mut *slot, Slot::Abandoned) {
                Slot::Done(Ok(value)) => Some(Ok(value)),
                Slot::Done(Err(_panic)) => {
                    Some(Err(Refused::Failed("the check panicked".to_owned())))
                }
                Slot::Pending => {
                    *slot = Slot::Pending;
                    None
                }
                Slot::Abandoned => Some(Err(Refused::Unanswered)),
            }
        };
        answer.ok_or(self)
    }
}

impl<T> Drop for Pending<T> {
    fn drop(&mut self) {
        let (lock, _) = &*self.slot;
        *lock.lock().unwrap_or_else(PoisonError::into_inner) = Slot::Abandoned;
    }
}

impl<T> std::fmt::Debug for Pending<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending")
    }
}

/// Why a collector could not produce output, independently of child completion.
///
/// `Spawn` is a refusal before an owned Child was returned. `Capture` and `Wait` occur after
/// process creation, so child effects may already exist. Cleanup follows each collector's
/// contract: the legacy single-child collector can wait without a reap bound, while bounded
/// collectors can hand an unreaped child to the reaper. These causes are not reap receipts.
#[derive(Debug)]
pub enum RunError {
    /// Command setup or process creation failed before an owned Child was returned.
    Spawn(io::Error),
    /// A Child was created, but preparing its capture or resuming it failed. Dispatch may
    /// already have occurred; this is not a no-child spawn refusal.
    Capture(io::Error),
    /// Waiting for the created child failed; cleanup is collector-specific.
    Wait(io::Error),
    /// The collection deadline or stop condition expired; this alone proves no reap.
    TimedOut,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start: {e}"),
            Self::Capture(e) => write!(
                f,
                "capture could not be prepared after process creation: {e}"
            ),
            Self::Wait(e) => write!(f, "could not be waited for: {e}"),
            Self::TimedOut => f.write_str("did not finish in time"),
        }
    }
}

impl std::error::Error for RunError {}

/// Why a process-progress-bounded child did not produce an output.
#[derive(Debug)]
pub enum ProgressError {
    /// Ordinary spawn, capture or wait failure.
    Run(RunError),
    /// One whole evidence window passed without captured bytes or descendant CPU/I/O progress.
    Stalled {
        /// The inactivity evidence window.
        window: Duration,
        /// The operation the caller was waiting for, without secret argv or environment values.
        waiting_on: String,
    },
}

impl std::fmt::Display for ProgressError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Run(error) => std::fmt::Display::fmt(error, f),
            Self::Stalled { window, waiting_on } => write!(
                f,
                "produced no stdout/stderr bytes and its descendant tree made no CPU or I/O \
                 progress for {window:?} while waiting on {waiting_on}"
            ),
        }
    }
}

impl std::error::Error for ProgressError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Run(error) => Some(error),
            Self::Stalled { .. } => None,
        }
    }
}

impl From<RunError> for ProgressError {
    fn from(error: RunError) -> Self {
        Self::Run(error)
    }
}

const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(25);
const MAX_WITHIN: Duration = Duration::from_secs(10 * 365 * 24 * 3600);

pub(crate) fn saturating_after(now: Instant, within: Duration) -> Instant {
    now.checked_add(within).unwrap_or_else(|| now + MAX_WITHIN)
}

const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// The bound on reading on past [`DRAIN_GRACE`] a pipe no writer holds: its end is a read
/// away, so its reader waits on nothing but this process's scheduling (a test binary at full
/// load left one unscheduled 650 ms and more, and `ps`'s line was lost).
#[cfg(unix)]
const SLOW_READER: Duration = Duration::from_secs(60);

struct Grace {
    by: Instant,
    /// Past `by`, when a drain stops reading a pipe only something outside the child's process
    /// group holds: the reap plus its wait's post-reap `left` ([`drained_owned`]'s or
    /// `complete::collect_started_with`'s), a tick for a process-progress collector
    /// ([`ProgressWindow::exited`]).
    #[cfg_attr(not(unix), allow(dead_code))]
    until: Instant,
    /// The exited child's pid, which led its process group.
    #[cfg_attr(not(unix), allow(dead_code))]
    group: Option<u32>,
}

impl Grace {
    /// A grace that ends at `by`, for a child whose group is not known.
    #[cfg(any(test, all(unix, debug_assertions)))]
    fn at(by: Instant) -> Self {
        Self {
            by,
            until: by,
            group: None,
        }
    }

    fn read_on(&self, pipe: Option<&PipeEnd>) -> Option<Duration> {
        #[cfg(unix)]
        {
            use std::os::fd::AsFd as _;
            let pipe = pipe?.as_fd();
            if children::writers_gone(pipe) {
                Some(SLOW_READER)
            } else if children::descendant_may_hold(pipe, self.group) {
                None
            } else {
                Some(self.until.saturating_duration_since(Instant::now()))
            }
        }
        #[cfg(not(unix))]
        {
            pipe.map(|pipe| match *pipe {})
        }
    }
}

/// A second descriptor on a child's pipe, which a drain asks past [`DRAIN_GRACE`] whether
/// anything still holds the pipe open. Nothing asks off Unix.
#[cfg(unix)]
type PipeEnd = std::os::fd::OwnedFd;
#[cfg(not(unix))]
type PipeEnd = std::convert::Infallible;

/// `stream`'s [`PipeEnd`].
#[cfg(unix)]
fn pipe_end(stream: &impl std::os::fd::AsFd) -> Option<PipeEnd> {
    stream.as_fd().try_clone_to_owned().ok()
}

/// `stream`'s [`PipeEnd`].
#[cfg(not(unix))]
fn pipe_end<T>(_stream: &T) -> Option<PipeEnd> {
    None
}

/// Maximum tail bytes retained per output stream by [`output_until`].
pub const OUTPUT_KEEP: usize = 1024 * 1024;

/// Runs `cmd` with owned secret input, requiring complete stdout and stderr, each at most
/// `max_output` bytes. The absolute deadline covers input, child execution and both drains.
/// Direct exit or refusal closes input and requests termination of the owned process scope
/// (Unix process group, Windows Job). Unix cleanup retains this call until its owning root
/// wait succeeds; the deadline bounds work, not retirement. Windows cleanup remains bounded.
/// A direct exit's status survives descendant cleanup. Input is scrubbed when sent or stopped.
/// A process the kernel will not let exit can outlive the termination request; lost Unix
/// wait custody blocks retirement rather than authorizing another numeric PID operation.
///
/// # Errors
/// Spawn, input, output or wait errors; `InvalidData` if either output exceeds its bound;
/// `TimedOut` if execution or complete EOF outlives the deadline. An early stdin `BrokenPipe`
/// preserves the direct child's result. Unsupported owned-process operations are refused.
pub fn output_with_input_until(
    cmd: &mut Command,
    input: crate::secret::Secret,
    deadline: Instant,
    max_output: usize,
) -> io::Result<Output> {
    #[cfg(unix)]
    {
        input_unix::output_with_input_until(cmd, input, deadline, max_output)
    }
    #[cfg(windows)]
    {
        input_windows::output_with_input_until(cmd, input, deadline, max_output)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (cmd, input, deadline, max_output);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "bounded child input requires Unix or Windows",
        ))
    }
}

/// Complete bounded stdout and stderr, or an explicit external stop on Unix.
/// `None` supplies no output or no-execution proof. Once a child starts, every return
/// and unwind retains its owner until a positive owning root wait; cleanup can outlive
/// the work deadline indefinitely. No descendant-drain guarantee is supplied.
///
/// # Errors
/// As [`output_with_input_until`]. The stop callback runs only on the owning waiter;
/// an unwind settles that actual root before propagating.
#[cfg(unix)]
pub fn output_with_input_until_watching(
    cmd: &mut Command,
    input: crate::secret::Secret,
    deadline: Instant,
    max_output: usize,
    stopped: impl FnMut() -> bool,
) -> io::Result<Option<Output>> {
    input_unix::output_with_input_until_watching(cmd, input, deadline, max_output, stopped)
}

/// Which output pipe supplied a chunk to [`stream_until_stopped`].
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildOutput {
    /// The child's stdout.
    Stdout,
    /// The child's stderr.
    Stderr,
}

/// Run with closed stdin, streaming borrowed output chunks without retaining or limiting totals.
/// `spawned` observes the owned root before output is delivered. Callbacks run on this waiter;
/// they must return promptly so it can poll cancellation and an optional absolute deadline.
/// `None` means stopped and supplies no EOF claim; `Some(status)` requires both output EOFs.
/// Every return or unwind after spawn retains the actual root until its owning wait succeeds.
/// The deadline bounds work, not retirement. Normal root exit requests descendant termination
/// while its group ID is still reserved. Escaped descendants supply no drain guarantee.
///
/// # Errors
/// Unsupported ownership operations, spawn, read, wait or callback errors, or `TimedOut`
/// when execution or output outlives the supplied deadline. Cleanup can outlive that deadline.
#[cfg(unix)]
pub fn stream_until_stopped(
    cmd: &mut Command,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    stopped: impl FnMut() -> bool,
) -> io::Result<Option<std::process::ExitStatus>> {
    input_unix::stream_until_stopped(cmd, deadline, spawned, output, stopped)
}

/// Streaming with explicit stdin custody, for a cooperative operation's cancellation pipe.
/// The caller owns the writer's lifetime. Output completion and retirement follow
/// [`stream_until_stopped`]; this runner neither writes to nor replaces the supplied input.
/// With `retire`, error/unwind retirement requests cooperative stop and waits for the actual
/// root instead of killing its group. Such a root must finish cleanup with output pipes
/// closed. A callback that cannot stop its root retains this call indefinitely.
///
/// # Errors
/// As [`stream_until_stopped`].
#[cfg(unix)]
pub fn stream_with_stdin_until_stopped(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Option<Instant>,
    spawned: impl FnOnce(u32),
    output: impl FnMut(ChildOutput, &[u8]) -> io::Result<()>,
    stopped: impl FnMut() -> bool,
    retire: Option<&mut dyn FnMut()>,
) -> io::Result<Option<std::process::ExitStatus>> {
    input_unix::stream_with_stdin_until_stopped(
        cmd, stdin, deadline, spawned, output, stopped, retire,
    )
}

/// Runs `cmd` with closed stdin, draining stdout/stderr and killing its process group at
/// `deadline`.
///
/// # Errors
/// Returns [`RunError::Spawn`] before a Child is returned, [`RunError::Capture`] after
/// process creation when capture setup fails, [`RunError::Wait`], or [`RunError::TimedOut`].
pub fn output_until(cmd: &mut Command, deadline: Instant) -> Result<Output, RunError> {
    output_until_from(cmd, Stdio::null(), deadline)
}

/// [`output_until`] with a relative timeout `within` from now.
///
/// # Errors
/// Returns the same errors as [`output_until`].
pub fn output_within(cmd: &mut Command, within: Duration) -> Result<Output, RunError> {
    output_until(cmd, saturating_after(Instant::now(), within))
}

/// [`output_until_from`] with a relative timeout `within` from now.
///
/// # Errors
/// Returns the same errors as [`output_until`].
pub fn output_within_from(
    cmd: &mut Command,
    stdin: Stdio,
    within: Duration,
) -> Result<Output, RunError> {
    output_until_from(cmd, stdin, saturating_after(Instant::now(), within))
}

/// [`output_until`] with `stdin` attached as the child's standard input.
///
/// # Errors
/// Returns the same errors as [`output_until`].
pub fn output_until_from(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
) -> Result<Output, RunError> {
    output_by(
        cmd,
        stdin,
        &mut || deadline.saturating_duration_since(deadline_now()),
        &mut settle_child,
        None,
    )
}

/// [`output_until_from`] that also ends `cmd` once `stopped()` reads true: read at least every
/// [`STOP_TICK`] while it runs or its output is collected. A running child and its group are
/// killed and the child positively reaped; after the child is reaped its former group is never
/// signalled. Cancellation publishes no output. Either end answers [`RunError::TimedOut`]; the
/// caller tells its stop from its deadline by its own reading.
///
/// # Errors
/// Returns the same errors as [`output_until`].
pub fn output_until_from_unless(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, RunError> {
    output_by(
        cmd,
        stdin,
        &mut || deadline.saturating_duration_since(deadline_now()),
        &mut settle_child,
        Some(stopped),
    )
}

/// How often [`output_until_from_unless`] and [`outputs_until_unless`] read their stop while a
/// child runs.
pub const STOP_TICK: Duration = Duration::from_millis(100);

#[cfg(test)]
thread_local! {
    /// The clock a test reads this thread's deadlines on, in place of the real one.
    static DEADLINE_CLOCK_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn() -> Instant>>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs or clears the clock [`deadline_now`] reads on this thread.
#[cfg(test)]
fn set_deadline_clock_for_test(clock: Option<Box<dyn Fn() -> Instant>>) {
    DEADLINE_CLOCK_FOR_TEST.with(|slot| *slot.borrow_mut() = clock);
}

/// Now, as [`output_until_from`] and the children's runs read the deadline they were given.
fn deadline_now() -> Instant {
    #[cfg(test)]
    if let Some(now) =
        DEADLINE_CLOCK_FOR_TEST.with(|slot| slot.borrow().as_ref().map(|clock| clock()))
    {
        return now;
    }
    Instant::now()
}

const PROGRESS_TICK: Duration = Duration::from_millis(100);
/// The m16 descendant-progress detector's cadence: frequent enough inside a one-minute evidence
/// window without turning a network operation into a continuous process-table sweep. A
/// [`ProgressWindow`] reads the tree once per tick however often its collector wakes (one that
/// holds a stop wakes every [`STOP_TICK`]), and between ticks only to judge a window that would
/// close before the next reading.
const PROCESS_PROGRESS_TICK: Duration = Duration::from_millis(500);

/// More descendants than this make a process-tree reading incomplete. Ordinary Git and local
/// metadata children stay many orders below it.
#[cfg(any(unix, windows))]
const PROCESS_PROGRESS_TREE_CAP: usize = 4_096;

/// One identity-bracketed member of a complete descendant-tree progress reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ProcessProgressMember {
    pid: u32,
    start: u64,
    /// Own CPU plus CPU of descendants this member already reaped.
    cpu_ns: u64,
    /// Storage I/O where the platform supplies both counters.
    io_bytes: Option<u64>,
}

/// Select `root` and every current descendant from one complete process-table sweep. Missing
/// identity, CPU, reaped-child CPU, truncation or ambiguous pids prove no lack of progress.
#[cfg(any(unix, windows))]
fn process_progress_tree(root: u32) -> Option<Vec<ProcessProgressMember>> {
    let sweep = aterm_sysprobe::processes()?;
    if sweep.truncated {
        return None;
    }
    let mut by_pid = BTreeMap::new();
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for process in &sweep.procs {
        if by_pid.insert(process.pid, process).is_some() {
            return None;
        }
        children.entry(process.ppid).or_default().push(process.pid);
    }
    if !by_pid.contains_key(&root) {
        return None;
    }
    let mut open = VecDeque::from([root]);
    let mut seen = BTreeSet::new();
    let mut members = Vec::new();
    while let Some(pid) = open.pop_front() {
        if !seen.insert(pid) || members.len() >= PROCESS_PROGRESS_TREE_CAP {
            return None;
        }
        let process = by_pid.get(&pid)?;
        members.push(ProcessProgressMember {
            pid,
            start: process.start?,
            cpu_ns: process.cpu_ns?.saturating_add(process.child_cpu_ns?),
            io_bytes: process
                .disk_read
                .zip(process.disk_written)
                .map(|(read, written)| read.saturating_add(written)),
        });
        if let Some(next) = children.get(&pid) {
            open.extend(next.iter().copied());
        }
    }
    members.sort_unstable();
    Some(members)
}

fn exclude_supervisor_work(members: &mut [ProcessProgressMember], root: u32) {
    if let Some(root) = members.iter_mut().find(|member| member.pid == root) {
        root.cpu_ns = 0;
        root.io_bytes = None;
    }
}

#[cfg(not(any(unix, windows)))]
fn process_progress_tree(_root: u32) -> Option<Vec<ProcessProgressMember>> {
    None
}

/// Monotone generation of CPU or I/O progress over comparable complete descendant-tree reads.
#[derive(Debug, Default)]
pub struct ProcessProgressProbe {
    seen: Vec<ProcessProgressMember>,
    generation: u64,
    observed: bool,
}

impl ProcessProgressProbe {
    /// Observe the root and its complete descendant tree's CPU and I/O. An unread or
    /// incomparable tree returns `None`, never evidence that the child stopped progressing.
    pub fn read(&mut self, root: u32) -> Option<u64> {
        self.read_over(process_progress_tree(root))
    }

    /// Observe descendant CPU and I/O, excluding the supervisor's own polling work.
    /// Incomplete or incomparable readings return `None`, never evidence of a stall.
    pub fn read_descendants(&mut self, root: u32) -> Option<u64> {
        let reading = process_progress_tree(root).map(|mut members| {
            exclude_supervisor_work(&mut members, root);
            members
        });
        self.read_over(reading)
    }

    fn read_over(&mut self, now: Option<Vec<ProcessProgressMember>>) -> Option<u64> {
        let now = now?;
        if !self.observed {
            self.seen = now;
            self.observed = true;
            return Some(self.generation);
        }
        let identities_match = self.seen.len() == now.len()
            && self
                .seen
                .iter()
                .zip(&now)
                .all(|(before, after)| (before.pid, before.start) == (after.pid, after.start));
        let counters = identities_match.then(|| {
            self.seen
                .iter()
                .zip(&now)
                .try_fold(false, |grew, (before, after)| -> Option<bool> {
                    if before.cpu_ns > after.cpu_ns {
                        return None;
                    }
                    let io_grew = match (before.io_bytes, after.io_bytes) {
                        (None, None) => false,
                        (Some(before), Some(after)) if before <= after => before < after,
                        _ => return None,
                    };
                    Some(grew || before.cpu_ns < after.cpu_ns || io_grew)
                })
        });
        self.seen = now;
        let Some(Some(grew)) = counters else {
            return None;
        };
        if grew {
            self.generation = self.generation.saturating_add(1);
        }
        Some(self.generation)
    }
}

/// Evidence window over captured bytes and a complete descendant-tree progress generation.
/// An unread or incomparable tree disarms the window until another complete baseline exists.
/// [`ProgressWindow::observe`] reads the tree once a tick, and afresh before it judges the
/// window closed.
#[derive(Clone, Copy, Debug)]
struct ProgressWindow {
    window: Duration,
    last: Instant,
    bytes: u64,
    work: u64,
    armed: bool,
    /// When [`ProgressWindow::observe`] last read the tree; `None` before its first reading.
    read_at: Option<Instant>,
}

impl ProgressWindow {
    fn new(window: Duration, now: Instant) -> Self {
        Self {
            window,
            last: now,
            bytes: 0,
            work: 0,
            armed: false,
            read_at: None,
        }
    }

    /// How often the tree is read: [`PROCESS_PROGRESS_TICK`], or a quarter of a shorter window.
    fn tick(&self) -> Duration {
        PROCESS_PROGRESS_TICK
            .min(self.window / 4)
            .max(Duration::from_millis(1))
    }

    /// How long until the collector looks again: what is left of the window at `now`, but at
    /// most a tick, and between readings no later than the next reading is due. The tree is read
    /// through `read` only when a tick has passed since its last reading or the window would
    /// close without one: a collector that wakes more often (one holding a stop) does not sweep
    /// the process table on each wake, one that waits the whole answer (one without a stop)
    /// wakes when the next reading is due, and a stall is never named on a stale reading.
    /// Between readings captured bytes still count.
    fn observe(
        &mut self,
        now: Instant,
        bytes: u64,
        read: impl FnOnce() -> Option<u64>,
    ) -> Duration {
        if let Some(at) = self.read_at {
            let tick = self.tick();
            let since = now.saturating_duration_since(at);
            if since < tick {
                let left = self.remaining(now, bytes, self.armed.then_some(self.work));
                if !left.is_zero() {
                    return left.min(tick.saturating_sub(since));
                }
            }
        }
        self.read_at = Some(now);
        self.remaining(now, bytes, read())
    }

    /// How long past a reaped child's exit its streams are read on when only something outside
    /// its process group holds a pipe ([`Grace`]'s `until`): a tick, wherever the last reading
    /// fell. A reaped root has no tree left to read, so none is read.
    fn exited(&self) -> Duration {
        self.tick()
    }

    /// A process-progress collector's `left` ([`drained_owned`]'s, and the complete-stdout
    /// collector's): over a window opened at `now()`, [`ProgressWindow::observe`] at `now()`
    /// while the child runs (`Some(pid)`), reading its tree through `work`, and
    /// [`ProgressWindow::exited`] once it is reaped (`None`). Both collectors build theirs here.
    fn collector<'a>(
        window: Duration,
        now: &'a mut dyn FnMut() -> Instant,
        work: &'a mut dyn FnMut(u32) -> Option<u64>,
    ) -> impl FnMut(Option<u32>, u64) -> Duration + 'a {
        let mut progress = Self::new(window, now());
        move |pid, bytes| match pid {
            Some(pid) => progress.observe(now(), bytes, || work(pid)),
            None => progress.exited(),
        }
    }

    fn remaining(&mut self, now: Instant, bytes: u64, work: Option<u64>) -> Duration {
        let tick = self.tick();
        let Some(work) = work else {
            self.armed = false;
            self.last = now;
            self.bytes = bytes;
            return tick;
        };
        if !self.armed || self.bytes != bytes || self.work != work {
            self.armed = true;
            self.last = now;
            self.bytes = bytes;
            self.work = work;
        }
        self.window
            .saturating_sub(now.saturating_duration_since(self.last))
            .min(tick)
    }
}

/// Runs `cmd` while `progress()` keeps advancing at least once every `window`.
///
/// # Errors
/// Returns [`RunError::TimedOut`] if `progress()` stays unchanged for `window`, or spawn/wait
/// errors.
pub fn output_while_progressing(
    cmd: &mut Command,
    window: Duration,
    progress: &mut dyn FnMut() -> u64,
) -> Result<Output, RunError> {
    progressing_until(cmd, window, None, progress)
}

/// Runs while native progress advances within `window`, without extending
/// the original absolute deadline when progress occurs.
/// # Errors
/// The original deadline expires, progress stalls, or native spawn/wait fails.
pub fn output_while_progressing_until(
    cmd: &mut Command,
    window: Duration,
    deadline: Instant,
    progress: &mut dyn FnMut() -> u64,
) -> Result<Output, RunError> {
    if deadline_now() >= deadline {
        return Err(RunError::TimedOut);
    }
    progressing_until(cmd, window, Some(deadline), progress)
}

fn progressing_until(
    cmd: &mut Command,
    window: Duration,
    deadline: Option<Instant>,
    progress: &mut dyn FnMut() -> u64,
) -> Result<Output, RunError> {
    let tick = PROGRESS_TICK.min(window / 4).max(Duration::from_millis(1));
    let mut last: Option<(u64, Instant)> = None;
    output_by(
        cmd,
        Stdio::null(),
        &mut || {
            let reading = progress();
            let now = deadline_now();
            let since = match last {
                Some((seen, since)) if seen == reading => since,
                _ => {
                    last = Some((reading, now));
                    now
                }
            };
            let quiet = now.saturating_duration_since(since);
            let left = window.saturating_sub(quiet).min(tick);
            deadline.map_or(left, |limit| left.min(limit.saturating_duration_since(now)))
        },
        &mut settle_child,
        None,
    )
}

/// Runs `cmd` with closed stdin while captured output bytes or a complete descendant-tree
/// CPU/I/O generation advances at least once every `window`. An unread or incomparable process
/// observation disarms the inactivity window instead of turning missing evidence into zero.
/// That includes platforms whose process reader cannot supply cumulative reaped-child CPU
/// (currently Windows); release cuts use macOS's complete reading.
///
/// # Errors
/// Spawn, capture or wait failures, or [`ProgressError::Stalled`] after one whole evidence
/// window.
pub fn output_while_process_progressing(
    cmd: &mut Command,
    window: Duration,
    waiting_on: impl Into<String>,
) -> Result<Output, ProgressError> {
    output_from_while_process_progressing(cmd, Stdio::null(), window, waiting_on)
}

/// As [`output_while_process_progressing`], with explicit owned `stdin`.
///
/// # Errors
/// The same errors as [`output_while_process_progressing`].
pub fn output_from_while_process_progressing(
    cmd: &mut Command,
    stdin: Stdio,
    window: Duration,
    waiting_on: impl Into<String>,
) -> Result<Output, ProgressError> {
    let waiting_on = waiting_on.into();
    let mut probe = ProcessProgressProbe::default();
    let mut now = deadline_now;
    output_from_while_process_progressing_with(
        cmd,
        stdin,
        window,
        waiting_on,
        &mut now,
        &mut |pid| probe.read(pid),
        None,
    )
}

/// As [`output_from_while_process_progressing`], also observing an explicit stop every
/// [`STOP_TICK`]. A stop retires the owned child/group through the same collector as a stall,
/// publishing no output. The caller distinguishes its stop from a stall by its own reading.
/// Escaped descendants and termination of this owner are outside this collector's contract.
///
/// # Errors
/// The same errors as [`output_from_while_process_progressing`].
pub fn output_from_while_process_progressing_unless(
    cmd: &mut Command,
    stdin: Stdio,
    window: Duration,
    waiting_on: impl Into<String>,
    stopped: &mut dyn FnMut() -> bool,
) -> Result<Output, ProgressError> {
    let mut probe = ProcessProgressProbe::default();
    output_from_while_process_progressing_with(
        cmd,
        stdin,
        window,
        waiting_on.into(),
        &mut deadline_now,
        &mut |pid| probe.read(pid),
        Some(stopped),
    )
}

fn output_from_while_process_progressing_with(
    cmd: &mut Command,
    stdin: Stdio,
    window: Duration,
    waiting_on: String,
    now: &mut dyn FnMut() -> Instant,
    work: &mut dyn FnMut(u32) -> Option<u64>,
    stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Output, ProgressError> {
    let mut left = ProgressWindow::collector(window, now, work);
    let (child, pipes, group) = started(cmd, stdin)?;
    drained_owned(
        child,
        pipes,
        group,
        &mut left,
        &mut settle_child,
        true,
        stopped,
    )?
    .map_err(|_| ProgressError::Stalled { window, waiting_on })
}

/// `cmd` run with `stdin` in a process group of its own (on Unix), its output drained, until it
/// ends or `left()` reads zero; then `overrun` ends it (for `output_until` and
/// `output_while_progressing`, [`settle_child`]: it and its group killed, and it reaped).
fn output_by(
    cmd: &mut Command,
    stdin: Stdio,
    left: &mut dyn FnMut() -> Duration,
    overrun: &mut dyn FnMut(&mut Child),
    stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Output, RunError> {
    let (child, pipes, group) = started(cmd, stdin)?;
    let mut unobserved = |_, _| left();
    drained_owned(child, pipes, group, &mut unobserved, overrun, true, stopped)?
        .map_err(|_| RunError::TimedOut)
}

/// A child's stdout and stderr, taken for a drained wait ([`drained_owned`]).
type Pipes = (
    Option<std::process::ChildStdout>,
    Option<std::process::ChildStderr>,
);

/// `cmd` started with `stdin` in a process group of its own (on Unix), with its stdout and
/// stderr piped and taken for a drained wait, and the group it leads.
fn started(cmd: &mut Command, stdin: Stdio) -> Result<(Child, Pipes, Option<u32>), RunError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let pipes = (child.stdout.take(), child.stderr.take());
    let group = Some(child.id());
    Ok((child, pipes, group))
}

/// A started child a drained wait ([`drained_owned`]) reads: this crate's, or, for a test that
/// launches its own, `std`'s.
trait Waited {
    /// Its process id.
    fn pid(&self) -> u32;
    /// Its exit status once it has exited, reaping it.
    fn poll_exit(&mut self) -> io::Result<Option<std::process::ExitStatus>>;
    /// Kill it (and the group it leads, when it leads one) and reap it, however long that takes.
    fn end(&mut self);
    /// Let it go, reaped or handed to [`crate::reaper`] (`spawnfd`'s handle does so on its drop).
    fn release(self);
}

impl Waited for Child {
    fn pid(&self) -> u32 {
        self.id()
    }

    fn poll_exit(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }

    fn end(&mut self) {
        settle_child(self);
    }

    fn release(self) {}
}

#[cfg(all(unix, test))]
impl Waited for std::process::Child {
    fn pid(&self) -> u32 {
        self.id()
    }

    fn poll_exit(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        self.try_wait()
    }

    /// The child alone: it is in a group `std` chose, often the test binary's own.
    fn end(&mut self) {
        let _ = self.kill();
        let _ = self.wait();
    }

    fn release(mut self) {
        if matches!(self.try_wait(), Ok(None)) {
            crate::reaper::hand_off(move || !matches!(self.try_wait(), Ok(None)));
        }
    }
}

/// What a drained wait leaves of a run past its time once `overrun` has ended it: its drains,
/// which hold what it wrote by its end ([`Late::wrote`]).
#[cfg_attr(not(all(unix, any(test, debug_assertions))), allow(dead_code))]
struct Late {
    stdout: Option<Drain>,
    stderr: Option<Drain>,
}

impl Late {
    /// What the run wrote, read until its pipes close or [`DRAIN_GRACE`] has passed.
    #[cfg(all(unix, any(test, debug_assertions)))]
    fn wrote(self) -> (Vec<u8>, Vec<u8>) {
        let grace = Grace::at(saturating_after(Instant::now(), DRAIN_GRACE));
        let collect = |drain: Option<Drain>| drain.map(|d| d.collect(&grace)).unwrap_or_default();
        (collect(self.stdout), collect(self.stderr))
    }
}

/// `child`, started with `pipes` (its stdout and stderr), waited on with its output drained until
/// it ends or `left(Some(pid), bytes)` reads zero, `bytes` being what both streams have captured;
/// once it is reaped, `left(None, bytes)` is how long past the reap a pipe only something
/// outside its group holds is read on ([`Grace`]). `group` is its process group when it leads
/// one. Past its time `overrun` ends it while it still runs, and the answer is [`Late`]; on a
/// failure it is [`Waited::end`]ed. Ordinary captures also keep their child on unwind; the silent
/// lab selects its explicit bounded cleanup policy instead.
fn drained_owned<C: Waited>(
    child: C,
    pipes: Pipes,
    group: Option<u32>,
    left: &mut dyn FnMut(Option<u32>, u64) -> Duration,
    overrun: &mut dyn FnMut(&mut C),
    settle_on_unwind: bool,
    mut stopped: Option<&mut dyn FnMut() -> bool>,
) -> Result<Result<Output, Late>, RunError> {
    struct Owner<C: Waited> {
        child: Option<C>,
        settle: bool,
    }
    impl<C: Waited> Drop for Owner<C> {
        fn drop(&mut self) {
            if self.settle
                && let Some(child) = &mut self.child
            {
                child.end();
            }
        }
    }
    let mut owned = Owner {
        child: Some(child),
        settle: settle_on_unwind,
    };
    let child = owned.child.as_mut().expect("capture owns its child");
    let stdout = pipes.0.map(|s| {
        let pipe = pipe_end(&s);
        drain(s, pipe, "stdout")
    });
    let stderr = pipes.1.map(|s| {
        let pipe = pipe_end(&s);
        drain(s, pipe, "stderr")
    });
    let (stdout, stderr) = match (stdout.transpose(), stderr.transpose()) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(e), _) | (_, Err(e)) => {
            child.end();
            return Err(RunError::Capture(e));
        }
    };
    let mut exit = ExitWait::on_pid(child.pid());
    let captured = |stdout: &Option<Drain>, stderr: &Option<Drain>| {
        stdout
            .as_ref()
            .map_or(0, Drain::bytes)
            .saturating_add(stderr.as_ref().map_or(0, Drain::bytes))
    };
    let status = loop {
        match child.poll_exit() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let remaining = if stopped.as_mut().is_some_and(|stop| stop()) {
                    Duration::ZERO
                } else {
                    left(Some(child.pid()), captured(&stdout, &stderr))
                };
                if remaining.is_zero() {
                    overrun(child);
                    owned
                        .child
                        .take()
                        .expect("capture owns its child")
                        .release();
                    return Ok(Err(Late { stdout, stderr }));
                }
                exit.wait(if stopped.is_some() {
                    remaining.min(STOP_TICK)
                } else {
                    remaining
                });
            }
            Err(e) => {
                child.end();
                return Err(RunError::Wait(e));
            }
        }
    };
    let grace = Grace {
        by: saturating_after(Instant::now(), DRAIN_GRACE),
        until: saturating_after(Instant::now(), left(None, captured(&stdout, &stderr))),
        group,
    };
    let mut collect = |drain: Option<Drain>| {
        drain
            .map(|d| {
                if stopped.is_some() {
                    d.collect_unless(&grace, &mut stopped)
                } else {
                    Ok(d.collect(&grace))
                }
            })
            .transpose()
            .map(Option::unwrap_or_default)
    };
    Ok(Ok(Output {
        status,
        stdout: collect(stdout)?,
        stderr: collect(stderr)?,
    }))
}

/// Kernel-backed wait handle for a single child process's exit.
///
/// Make it while the child is unreaped (right after the spawn), then alternate `try_wait`
/// with [`ExitWait::wait`]:
///
/// ```no_run
/// use std::{io, process::ExitStatus, time::Instant};
/// use aterm_uds::{deadline::ExitWait, spawnfd::Child};
///
/// // On timeout the caller still owns the live child and decides how to end it.
/// fn wait_until(child: &mut Child, deadline: Instant) -> io::Result<Option<ExitStatus>> {
///     let mut exit = ExitWait::new(child);
///     loop {
///         if let Some(status) = child.try_wait()? {
///             return Ok(Some(status));
///         }
///         let remaining = deadline.saturating_duration_since(Instant::now());
///         if remaining.is_zero() {
///             return Ok(None);
///         }
///         exit.wait(remaining);
///     }
/// }
/// ```
#[derive(Debug)]
pub struct ExitWait {
    /// Readable once the child has exited; `None` without such a descriptor, or once it has
    /// reported (it stays readable, and a child not yet reapable must not spin on it).
    #[cfg(unix)]
    fd: Option<std::os::fd::OwnedFd>,
    /// The fallback sleep, doubled up to `tick_max`.
    tick: Duration,
    /// The longest fallback sleep: [`POLL_MAX`] unless [`ExitWait::ticking_at_most`] lowers it.
    tick_max: Duration,
}

impl ExitWait {
    /// Creates an exit wait handle for `child`.
    #[must_use]
    pub fn new(child: &Child) -> Self {
        Self::on_pid(child.id())
    }

    /// Creates an exit wait handle for whatever process currently holds `pid`.
    #[must_use]
    pub fn on_pid(pid: u32) -> Self {
        #[cfg(not(unix))]
        let _ = pid;
        Self {
            #[cfg(unix)]
            fd: crate::exitwatch::exit_fd(pid),
            tick: POLL_MIN,
            tick_max: POLL_MAX,
        }
    }

    /// Caps the fallback sleep at `most` (no less than [`POLL_MIN`]). The fallback is the wait
    /// where the kernel gives no word of the exit: no exit descriptor could be made, or it has
    /// reported and the child is not yet reapable. The cap is for a caller whose reap must come
    /// within `most` of the exit there, at the cost of more wakeups than [`POLL_MAX`] gives.
    #[must_use]
    pub fn ticking_at_most(mut self, most: Duration) -> Self {
        self.tick_max = most.max(POLL_MIN);
        self.tick = self.tick.min(self.tick_max);
        self
    }

    /// Blocks until the kernel reports the process's exit or `timeout` elapses.
    pub fn wait(&mut self, timeout: Duration) {
        #[cfg(unix)]
        if let Some(fd) = &self.fd {
            match children::exit_reported(fd, timeout) {
                Ok(false) => return,
                Ok(true) => {
                    self.reported();
                    return;
                }
                Err(_) => self.reported(),
            }
        }
        std::thread::sleep(self.tick.min(timeout));
        self.tick = (self.tick * 2).min(self.tick_max);
    }

    /// The descriptor to poll beside others, while it can still report.
    #[cfg(unix)]
    fn reporter(&self) -> Option<std::os::fd::RawFd> {
        self.fd.as_ref().map(std::os::fd::AsRawFd::as_raw_fd)
    }

    /// The exit has been reported: wait on ticks from here on.
    #[cfg(unix)]
    fn reported(&mut self) {
        self.fd = None;
    }
}

/// End this owned child's process group on Unix, or the child on other platforms, and retain
/// it until a positive owning wait. Already reaped children cause no signal. A child whose
/// numeric custody was lost stays retained without signalling a possibly reused identity.
/// Cleanup may outlive any work deadline; this establishes no descendant-drain receipt.
pub fn settle_child(child: &mut Child) {
    settle_child_with(child, Child::wait);
}

/// Keep the actual child until an owning wait succeeds, including on cleanup errors. An
/// unknown child is retained without signalling a number that may have been reused.
#[cfg(unix)]
fn settle_child_with(
    child: &mut Child,
    mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
) {
    if child.is_reaped() {
        return;
    }
    if child.owns_unreaped() {
        children::kill_group(child);
    }
    let mut exit = child.owns_unreaped().then(|| ExitWait::new(child));
    loop {
        if !child.owns_unreaped() && !child.is_reaped() {
            exit = None;
            std::thread::sleep(STOP_TICK);
            continue;
        }
        if wait(child).is_ok() {
            return;
        }
        if !child.owns_unreaped() {
            exit = None;
        }
        let at = Instant::now();
        if let Some(exit) = &mut exit {
            exit.wait(STOP_TICK);
        }
        let left = STOP_TICK.saturating_sub(at.elapsed());
        if !left.is_zero() {
            std::thread::sleep(left);
        }
    }
}

/// Off Unix, `Child` retains the native process handle, so ending and waiting never use a
/// potentially reused process id. Wait failures retain that same handle until completion.
#[cfg(not(unix))]
fn settle_child_with(
    child: &mut Child,
    mut wait: impl FnMut(&mut Child) -> io::Result<std::process::ExitStatus>,
) {
    let _ = child.kill();
    while wait(child).is_err() {
        std::thread::sleep(STOP_TICK);
    }
}

/// `SIGKILL` `child` and reap it within `hang`, waiting on its exit as the kernel reports it. One still there at `hang` is left to its handle's drop, which hands it to the reaper: whether a program macOS's program check holds takes its signal is unmeasured.
pub fn kill_within(child: &mut Child, hang: Duration) {
    let _ = child.kill();
    reap_within(child, None, saturating_after(Instant::now(), hang));
}

/// `child`'s exit status once it exits within `hang`, without ending it; `None` while it still
/// runs at `hang`, or when it cannot be waited on.
#[cfg(all(unix, any(test, debug_assertions)))]
pub(crate) fn exit_within(child: &mut Child, hang: Duration) -> Option<std::process::ExitStatus> {
    reap_within(child, None, saturating_after(Instant::now(), hang));
    child.try_wait().ok().flatten()
}

fn reap_within<C: Waited>(child: &mut C, exit: Option<&mut ExitWait>, by: Instant) {
    let mut fallback;
    let exit = match exit {
        Some(exit) => exit,
        None => {
            fallback = ExitWait::on_pid(child.pid());
            &mut fallback
        }
    };
    loop {
        match child.poll_exit() {
            Ok(Some(_)) | Err(_) => return,
            Ok(None) => {
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return;
                }
                exit.wait(left);
            }
        }
    }
}

/// Spawns all `cmds` in parallel and collects their outputs under a shared `deadline`.
#[must_use]
pub fn outputs_until(
    cmds: &mut [&mut Command],
    deadline: Instant,
    reap: Duration,
) -> Vec<Result<Output, RunError>> {
    #[cfg(unix)]
    {
        children::outputs_until(cmds, deadline, reap)
    }
    #[cfg(not(unix))]
    {
        let _ = reap;
        cmds.iter_mut()
            .map(|cmd| output_until(cmd, deadline))
            .collect()
    }
}

/// [`outputs_until`] that also ends the children still running once `stopped()` reads true:
/// read at least every [`STOP_TICK`] while any runs, and then each killed with its group and
/// reaped within `reap`, as at `deadline`. Either end answers [`RunError::TimedOut`] for a child
/// it ended; the caller tells its stop from its deadline by its own reading.
#[must_use]
pub fn outputs_until_unless(
    cmds: &mut [&mut Command],
    deadline: Instant,
    reap: Duration,
    stopped: &mut dyn FnMut() -> bool,
) -> Vec<Result<Output, RunError>> {
    #[cfg(unix)]
    {
        children::outputs_until_unless(cmds, deadline, reap, stopped)
    }
    #[cfg(not(unix))]
    {
        let _ = reap;
        cmds.iter_mut()
            .map(|cmd| output_until_from_unless(cmd, Stdio::null(), deadline, &mut *stopped))
            .collect()
    }
}

/// How long a [`Held`] child dropped unrun is given to be reaped after its `SIGKILL`.
#[cfg(unix)]
const HELD_REAP: Duration = Duration::from_millis(250);

/// Child process started suspended at exec ([`crate::spawnfd::Command::start_suspended`]).
#[cfg(unix)]
#[derive(Debug)]
pub struct Held {
    /// `None` once [`Self::output_until`] has taken it.
    child: Option<Child>,
}

#[cfg(unix)]
impl Held {
    /// Spawns `cmd` suspended at exec in a new process group with piped output.
    ///
    /// # Errors
    /// Returns an error if spawning fails or suspended start is unsupported on this platform.
    pub fn start(cmd: &mut Command) -> io::Result<Self> {
        let child = children::prepare(cmd).start_suspended(true).spawn()?;
        Ok(Self { child: Some(child) })
    }

    /// Process ID of the held child.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Resumes the held child and collects its output until `deadline`.
    ///
    /// # Errors
    /// Returns [`RunError`] on resume failure, wait failure, or timeout.
    pub fn output_until(mut self, deadline: Instant, reap: Duration) -> Result<Output, RunError> {
        let Some(mut child) = self.child.take() else {
            return Err(RunError::TimedOut);
        };
        if let Err(error) = child.resume() {
            children::kill_group(&mut child);
            reap_within(&mut child, None, saturating_after(Instant::now(), reap));
            return Err(RunError::Capture(error));
        }
        children::output_of_until(child, deadline, reap)
    }
}

#[cfg(unix)]
impl Drop for Held {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            children::kill_group(&mut child);
            reap_within(&mut child, None, Instant::now() + HELD_REAP);
        }
    }
}

struct Drain {
    tail: Arc<Mutex<Vec<u8>>>,
    total: Arc<AtomicU64>,
    ended: mpsc::Receiver<()>,
    pipe: Option<PipeEnd>,
}

impl Drain {
    fn bytes(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    fn collect(self, grace: &Grace) -> Vec<u8> {
        self.collect_unless(grace, &mut None)
            .expect("an unobserved capture cannot be cancelled")
    }

    fn collect_unless(
        self,
        grace: &Grace,
        stopped: &mut Option<&mut dyn FnMut() -> bool>,
    ) -> Result<Vec<u8>, RunError> {
        let ended = receive_unless(
            &self.ended,
            grace.by.saturating_duration_since(Instant::now()),
            stopped,
        )?
        .is_ok();
        if !ended && let Some(wait) = grace.read_on(self.pipe.as_ref()) {
            let _ = receive_unless(&self.ended, wait, stopped)?;
        }
        Ok(std::mem::take(
            &mut *self.tail.lock().unwrap_or_else(PoisonError::into_inner),
        ))
    }
}

/// Keep ordinary captures' single wait; cancellation-aware captures reuse the existing tick.
fn receive_unless<T>(
    receiver: &mpsc::Receiver<T>,
    wait: Duration,
    stopped: &mut Option<&mut dyn FnMut() -> bool>,
) -> Result<Result<T, mpsc::RecvTimeoutError>, RunError> {
    if stopped.is_none() {
        return Ok(receiver.recv_timeout(wait));
    }
    let by = saturating_after(Instant::now(), wait);
    loop {
        if stopped.as_mut().is_some_and(|stop| stop()) {
            return Err(RunError::TimedOut);
        }
        let left = by.saturating_duration_since(Instant::now());
        match receiver.recv_timeout(left.min(STOP_TICK)) {
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < by => {}
            got => return Ok(got),
        }
    }
}

#[cfg(all(test, unix))]
type DrainStartFailure = Box<dyn FnOnce() -> io::Error>;

#[cfg(all(test, unix))]
thread_local! {
    static DRAIN_START_FAILURE: std::cell::RefCell<Option<DrainStartFailure>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(all(test, unix))]
struct DrainStartFailureGuard(Option<DrainStartFailure>);

#[cfg(all(test, unix))]
impl Drop for DrainStartFailureGuard {
    fn drop(&mut self) {
        DRAIN_START_FAILURE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

#[cfg(all(test, unix))]
fn fail_next_drain_start(failure: impl FnOnce() -> io::Error + 'static) -> DrainStartFailureGuard {
    let previous = DRAIN_START_FAILURE.with(|slot| slot.borrow_mut().replace(Box::new(failure)));
    DrainStartFailureGuard(previous)
}

/// Read `stream` to its end on a thread of its own, keeping the tail; `pipe` is its
/// [`PipeEnd`].
#[cfg(not(target_arch = "wasm32"))]
fn drain(
    mut stream: impl Read + Send + 'static,
    pipe: Option<PipeEnd>,
    which: &str,
) -> io::Result<Drain> {
    #[cfg(all(test, unix))]
    if let Some(failure) = DRAIN_START_FAILURE.with(|slot| slot.borrow_mut().take()) {
        return Err(failure());
    }
    let tail = Arc::new(Mutex::new(Vec::new()));
    let theirs = Arc::clone(&tail);
    let total = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&total);
    let (tx, ended) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("child-{which}"))
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                counted.fetch_add(u64::try_from(n).unwrap_or(u64::MAX), Ordering::Relaxed);
                let mut tail = theirs.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > OUTPUT_KEEP {
                    let cut = tail.len() - OUTPUT_KEEP;
                    tail.drain(..cut);
                }
            }
            let _ = tx.send(());
        })?;
    Ok(Drain {
        tail,
        total,
        ended,
        pipe,
    })
}

#[cfg(test)]
#[path = "deadline_tests.rs"]
mod tests;
