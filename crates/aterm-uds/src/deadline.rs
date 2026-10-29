// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Deadlines for blocking in-process calls and child processes.

use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Output;

use crate::spawnfd::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

#[cfg(unix)]
mod children;

mod complete;
pub use complete::{
    CompleteOutputError, output_until_complete_stdout, output_until_complete_stdout_from,
};

/// Maximum duration macOS may hold the first exec of a new binary in `syspolicyd`.
pub const FIRST_EXEC_HANG: Duration = Duration::from_secs(10 * 60);

/// Marker filename written by a fresh copy immediately after exec.
pub const RAN: &str = "ran";

/// Marker filename written by [`stamp_exec`] before exec with the awake-clock reading.
pub const EXEC_STAMP: &str = "exec";

/// Writes the current awake-clock reading to [`EXEC_STAMP`] inside `home` before exec.
pub fn stamp_exec(home: &std::path::Path) {
    if let Err(e) = try_stamp_exec(home) {
        eprintln!("cannot stamp {} before its exec: {e}", home.display());
    }
}

/// [`stamp_exec`] for a caller that must know whether the stamp was written.
/// # Errors
/// The stamp could not be written; a write cut short can leave it blank, a
/// stamp [`exec_stamp`] reads no reading from.
pub fn try_stamp_exec(home: &std::path::Path) -> std::io::Result<()> {
    let stamp = crate::process::awake_ms()
        .map(|ms| ms.to_string())
        .unwrap_or_default();
    std::fs::write(home.join(EXEC_STAMP), stamp)
}

/// The awake-clock reading [`EXEC_STAMP`] inside `home` holds: none when it is
/// missing, unreadable or blank.
#[must_use]
pub fn exec_stamp(home: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(home.join(EXEC_STAMP))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
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

fn copy_wait_left_at(
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

#[cfg(any(test, debug_assertions))]
mod silent;
#[cfg(any(test, debug_assertions))]
pub use silent::{
    CODESIGN_HANG, ECHO_HANG, KeptUntilRan, ad_hoc_copy, held_words, require_disk_image_service,
    require_trust_service, sign_ad_hoc, stand_in, trust_service_silent,
};
#[cfg(all(unix, any(test, debug_assertions)))]
pub use silent::{cat_echoes, prints};

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
                .wait_timeout_while(slot, within, |slot| matches!(slot, Slot::Pending))
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

/// Reason [`output_until`] failed to produce output.
#[derive(Debug)]
pub enum RunError {
    /// Child process or stream reader thread could not be spawned.
    Spawn(io::Error),
    /// Waiting for the child failed; the child was killed and reaped.
    Wait(io::Error),
    /// Child did not finish before the deadline and was killed and reaped.
    TimedOut,
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "could not start: {e}"),
            Self::Wait(e) => write!(f, "could not be waited for: {e}"),
            Self::TimedOut => f.write_str("did not finish in time"),
        }
    }
}

impl std::error::Error for RunError {}

const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(25);

const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// The bound on reading on past [`DRAIN_GRACE`] a pipe no writer holds: its end is a read
/// away, so its reader waits on nothing but this process's scheduling (a test binary at full
/// load left one unscheduled 650 ms and more, and `ps`'s line was lost).
#[cfg(unix)]
const SLOW_READER: Duration = Duration::from_secs(60);

struct Grace {
    by: Instant,
    #[cfg_attr(not(unix), allow(dead_code))]
    until: Instant,
    /// The exited child's pid, which led its process group.
    #[cfg_attr(not(unix), allow(dead_code))]
    group: Option<u32>,
}

impl Grace {
    /// A grace that ends at `by`, for a child whose group is not known.
    #[cfg(test)]
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

/// Runs `cmd` with closed stdin, draining stdout/stderr and killing its process group at
/// `deadline`.
///
/// # Errors
/// Returns [`RunError::Spawn`], [`RunError::Wait`], or [`RunError::TimedOut`].
pub fn output_until(cmd: &mut Command, deadline: Instant) -> Result<Output, RunError> {
    output_until_from(cmd, Stdio::null(), deadline)
}

/// [`output_until`] with a relative timeout `within` from now.
///
/// # Errors
/// Returns the same errors as [`output_until`].
pub fn output_within(cmd: &mut Command, within: Duration) -> Result<Output, RunError> {
    output_until(cmd, Instant::now() + within)
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
    output_by(cmd, stdin, &mut || {
        deadline.saturating_duration_since(deadline_now())
    })
}

#[cfg(test)]
thread_local! {
    /// The clock a test reads this thread's deadlines on, in place of the real one.
    static DEADLINE_CLOCK_FOR_TEST: std::cell::RefCell<Option<Box<dyn Fn() -> Instant>>> =
        const { std::cell::RefCell::new(None) };
}

/// Installs or clears the clock [`deadline_now`] reads on this thread.
#[cfg(all(test, unix))]
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
    let tick = PROGRESS_TICK.min(window / 4).max(Duration::from_millis(1));
    let mut last: Option<(u64, Instant)> = None;
    output_by(cmd, Stdio::null(), &mut || {
        let reading = progress();
        let now = Instant::now();
        let since = match last {
            Some((seen, since)) if seen == reading => since,
            _ => {
                last = Some((reading, now));
                now
            }
        };
        let quiet = now.saturating_duration_since(since);
        window.saturating_sub(quiet).min(tick)
    })
}

fn output_by(
    cmd: &mut Command,
    stdin: Stdio,
    left: &mut dyn FnMut() -> Duration,
) -> Result<Output, RunError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let stdout = child.stdout.take().map(|s| {
        let pipe = pipe_end(&s);
        drain(s, pipe, "stdout")
    });
    let stderr = child.stderr.take().map(|s| {
        let pipe = pipe_end(&s);
        drain(s, pipe, "stderr")
    });
    let (stdout, stderr) = match (stdout.transpose(), stderr.transpose()) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        (Err(e), _) | (_, Err(e)) => {
            end(&mut child);
            return Err(RunError::Spawn(e));
        }
    };
    let mut exit = ExitWait::new(&child);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let remaining = left();
                if remaining.is_zero() {
                    end(&mut child);
                    return Err(RunError::TimedOut);
                }
                exit.wait(remaining);
            }
            Err(e) => {
                end(&mut child);
                return Err(RunError::Wait(e));
            }
        }
    };
    let grace = Grace {
        by: Instant::now() + DRAIN_GRACE,
        until: Instant::now() + left(),
        group: Some(child.id()),
    };
    let collect = |drain: Option<Drain>| drain.map(|d| d.collect(&grace)).unwrap_or_default();
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
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
    /// The fallback sleep, doubled up to [`POLL_MAX`].
    tick: Duration,
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
        }
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
        self.tick = (self.tick * 2).min(POLL_MAX);
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

fn end(child: &mut Child) {
    #[cfg(unix)]
    children::kill_group(child);
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

/// `SIGKILL` `child` and reap it within `hang`, waiting on its exit as the kernel reports it. One still there at `hang` is left to its handle's drop, which hands it to the reaper: whether a program macOS's program check holds takes its signal is unmeasured.
pub fn kill_within(child: &mut Child, hang: Duration) {
    let _ = child.kill();
    reap_within(child, None, Instant::now() + hang);
}

fn reap_within(child: &mut Child, exit: Option<&mut ExitWait>, by: Instant) {
    let mut fallback;
    let exit = match exit {
        Some(exit) => exit,
        None => {
            fallback = ExitWait::new(child);
            &mut fallback
        }
    };
    loop {
        match child.try_wait() {
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
            reap_within(&mut child, None, Instant::now() + reap);
            return Err(RunError::Spawn(error));
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
    ended: mpsc::Receiver<()>,
    pipe: Option<PipeEnd>,
}

impl Drain {
    fn collect(self, grace: &Grace) -> Vec<u8> {
        let ended = self
            .ended
            .recv_timeout(grace.by.saturating_duration_since(Instant::now()))
            .is_ok();
        if !ended && let Some(wait) = grace.read_on(self.pipe.as_ref()) {
            let _ = self.ended.recv_timeout(wait);
        }
        std::mem::take(&mut *self.tail.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Read `stream` to its end on a thread of its own, keeping the tail; `pipe` is its
/// [`PipeEnd`].
#[cfg(not(target_arch = "wasm32"))]
fn drain(
    mut stream: impl Read + Send + 'static,
    pipe: Option<PipeEnd>,
    which: &str,
) -> io::Result<Drain> {
    let tail = Arc::new(Mutex::new(Vec::new()));
    let theirs = Arc::clone(&tail);
    let (tx, ended) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(format!("child-{which}"))
        .spawn(move || {
            let mut chunk = [0u8; 8192];
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                let mut tail = theirs.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend_from_slice(&chunk[..n]);
                if tail.len() > OUTPUT_KEEP {
                    let cut = tail.len() - OUTPUT_KEEP;
                    tail.drain(..cut);
                }
            }
            let _ = tx.send(());
        })?;
    Ok(Drain { tail, ended, pipe })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[cfg(target_vendor = "apple")]
    use super::Held;
    #[cfg(unix)]
    use super::{
        ExitWait, OUTPUT_KEEP, RunError, output_until, output_while_progressing, outputs_until,
    };
    use super::{Gate, Refused};

    #[test]
    fn a_copy_ages_on_the_awake_clock_from_its_exec() {
        use super::{
            EXEC_STAMP, FIRST_EXEC_HANG, RAN, copy_wait_left_at, exec_stamp, stamp_exec,
            try_stamp_exec,
        };
        let copy_may_go_at =
            |home: &std::path::Path, now, awake| copy_wait_left_at(home, now, awake).is_none();
        let hang = u64::try_from(FIRST_EXEC_HANG.as_millis()).expect("ms");
        let dir = aterm_tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("1-2");
        std::fs::create_dir(&home).expect("mkdir");
        let now = std::time::SystemTime::now();
        let later = now + FIRST_EXEC_HANG + Duration::from_secs(60);
        assert!(!copy_may_go_at(&home, now, Some(5)));
        assert!(copy_may_go_at(&home, later, Some(5)));
        assert_eq!(exec_stamp(&home), None, "no stamp");
        std::fs::write(home.join(EXEC_STAMP), "1000\n").expect("stamp");
        assert_eq!(exec_stamp(&home), Some(1000));
        assert!(
            !copy_may_go_at(&home, later, Some(1000 + hang - 1)),
            "slept through"
        );
        assert!(copy_may_go_at(&home, now, Some(1000 + hang)));
        assert!(copy_may_go_at(&home, now, Some(999)), "a later boot");
        assert_eq!(
            copy_wait_left_at(&home, later, Some(1000 + 60_000)),
            Some(FIRST_EXEC_HANG - Duration::from_secs(60)),
            "what is left is awake time too"
        );
        assert!(!copy_may_go_at(&home, now, None));
        assert!(copy_may_go_at(&home, later, None));
        std::fs::write(home.join(EXEC_STAMP), "").expect("blank stamp");
        assert_eq!(exec_stamp(&home), None, "a blank stamp holds no reading");
        assert!(!copy_may_go_at(&home, now, Some(1000 + hang)));
        assert!(copy_may_go_at(&home, later, Some(1000 + hang)));
        std::fs::write(home.join(RAN), "").expect("ran");
        assert!(copy_may_go_at(&home, now, Some(0)));
        let fresh = dir.path().join("3-4");
        std::fs::create_dir(&fresh).expect("mkdir");
        stamp_exec(&fresh);
        let text = std::fs::read_to_string(fresh.join(EXEC_STAMP)).expect("stamped");
        assert!(text.is_empty() || text.parse::<u64>().is_ok(), "{text:?}");
        let gone = dir.path().join("5-6");
        assert!(try_stamp_exec(&gone).is_err(), "nowhere to write it");
        stamp_exec(&gone);
        assert!(!gone.exists(), "a stamp that failed made nothing");
        assert!(try_stamp_exec(&fresh).is_ok());
        let first = crate::process::awake_ms();
        let second = crate::process::awake_ms();
        assert!(first <= second, "{first:?} then {second:?}");
    }

    const DEADLINE: Duration = Duration::from_millis(200);
    const HANG: Duration = Duration::from_secs(60);

    /// A reader that first waits for its release, as one the scheduler has not run yet.
    #[cfg(unix)]
    pub(super) struct Late<R> {
        pub(super) held: mpsc::Receiver<()>,
        pub(super) reader: R,
    }

    /// The pid of a child that led a process group of its own, exited and reaped: a group
    /// nothing is left in.
    #[cfg(unix)]
    pub(super) fn an_empty_group() -> u32 {
        let mut child = crate::spawnfd::command("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .expect("a child");
        child.wait().expect("reaped");
        child.id()
    }

    /// A child that led a process group of its own and exited, left unreaped: a group whose
    /// one member is a zombie, which Darwin answers a signal for with `EPERM`, not `ESRCH`.
    /// Dropping it reaps it.
    #[cfg(target_vendor = "apple")]
    pub(super) fn a_group_left_only_a_zombie() -> crate::spawnfd::Child {
        const P_PID: i32 = 1;
        const WNOHANG: i32 = 0x01;
        const WEXITED: i32 = 0x04;
        const WNOWAIT: i32 = 0x20;
        unsafe extern "C" {
            fn waitid(idtype: i32, id: u32, info: *mut u8, options: i32) -> i32;
        }
        let child = crate::spawnfd::command("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .expect("a child");
        let since = Instant::now();
        loop {
            let mut info = [0u8; 128];
            // SAFETY: `info` is writable and larger than Darwin's 104-byte `siginfo_t`, which `waitid` fills and does not retain; `WNOWAIT` leaves the child waitable, so its `Child` still reaps it.
            let rc = unsafe {
                waitid(
                    P_PID,
                    child.id(),
                    info.as_mut_ptr(),
                    WEXITED | WNOWAIT | WNOHANG,
                )
            };
            assert_eq!(rc, 0, "waitid: {}", std::io::Error::last_os_error());
            if info[12..16] != [0; 4] {
                return child;
            }
            assert!(since.elapsed() < HANG, "the child never exited");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// `pipe`'s writer writes `more` and closes a moment from now: a holder outside the
    /// child's group letting go at its exec.
    #[cfg(unix)]
    pub(super) fn lets_go(mut writer: std::io::PipeWriter, more: &'static [u8]) {
        use std::io::Write as _;
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let _ = writer.write_all(more);
        });
    }

    #[cfg(unix)]
    impl<R: std::io::Read> std::io::Read for Late<R> {
        fn read(&mut self, into: &mut [u8]) -> std::io::Result<usize> {
            let _ = self.held.recv_timeout(HANG);
            self.reader.read(into)
        }
    }

    /// A pipe no writer holds is read to its end past the grace, however late its reader
    /// runs: the grace is for a descendant holding the pipe open, and a reader a loaded
    /// test binary left unscheduled lost `ps`'s one line. Negative control: a `collect`
    /// that returns at `by` whatever the pipe gives returns nothing here.
    #[cfg(unix)]
    #[test]
    fn a_pipe_no_writer_holds_is_read_to_its_end_past_the_grace() {
        use std::io::Write as _;
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer
            .write_all(b"Thu Oct 22 16:44:35 2026\n")
            .expect("write");
        drop(writer);
        let pipe = super::pipe_end(&reader);
        let (release, held) = mpsc::channel();
        let drain = super::drain(Late { held, reader }, pipe, "late").expect("reader");
        let grace = super::Grace {
            by: Instant::now(),
            until: Instant::now() + HANG,
            group: Some(an_empty_group()),
        };
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let _ = release.send(());
        });
        assert_eq!(drain.collect(&grace), b"Thu Oct 22 16:44:35 2026\n");
    }

    /// A pipe only something outside the exited child's group holds is read until it lets
    /// go, within the call's deadline: a child another thread of this process forked holds
    /// it until its exec, and a loaded test binary kept one past the grace. Negative
    /// control: a drain that stops at `by` here returns at most `one `.
    #[cfg(unix)]
    #[test]
    fn a_pipe_held_outside_the_childs_group_is_read_until_it_lets_go() {
        use std::io::Write as _;
        let (reader, mut writer) = std::io::pipe().expect("pipe");
        writer.write_all(b"one ").expect("write");
        let pipe = super::pipe_end(&reader);
        let drain = super::drain(reader, pipe, "outside").expect("reader");
        let grace = super::Grace {
            by: Instant::now(),
            until: Instant::now() + HANG,
            group: Some(an_empty_group()),
        };
        lets_go(writer, b"two");
        assert_eq!(drain.collect(&grace), b"one two");
    }

    /// CONTROL: a pipe held while the exited child's group still has a member (its
    /// descendant) gives at `by` what it gave by then.
    #[cfg(unix)]
    #[test]
    fn a_pipe_held_while_the_childs_group_lives_gives_what_it_gave_by_the_grace() {
        let mut member = crate::spawnfd::command("/bin/sleep")
            .arg("600")
            .process_group(0)
            .spawn()
            .expect("a member of the group");
        let (reader, writer) = std::io::pipe().expect("pipe");
        let pipe = super::pipe_end(&reader);
        let drain = super::drain(reader, pipe, "descendant").expect("reader");
        let begun = Instant::now();
        let grace = super::Grace {
            by: Instant::now(),
            until: Instant::now() + HANG,
            group: Some(member.id()),
        };
        let got = drain.collect(&grace);
        let waited = begun.elapsed();
        let _ = member.kill();
        let _ = member.wait();
        drop(writer);
        assert!(got.is_empty(), "{got:?}");
        assert!(waited < HANG, "waited on a pipe a descendant holds");
    }

    fn witness() -> (impl FnOnce() + Send + 'static, mpsc::Receiver<()>) {
        let (ran, seen) = mpsc::channel::<()>();
        (
            move || {
                let _ = ran.send(());
            },
            seen,
        )
    }

    fn never_ran(seen: &mpsc::Receiver<()>) -> bool {
        seen.recv_timeout(HANG) == Err(mpsc::RecvTimeoutError::Disconnected)
    }

    fn drained(gate: &Gate) {
        let since = Instant::now();
        while gate.running() != 0 {
            assert!(since.elapsed() < HANG, "a released helper never returned");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn started(gate: &Gate, n: usize) {
        let since = Instant::now();
        while gate.running() < n {
            assert!(since.elapsed() < HANG, "a helper never started");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn work_that_never_answers_is_refused_at_the_deadline_and_piles_up_no_threads() {
        static GATE: Gate = Gate::new("test-gate");

        let (release, held) = mpsc::channel::<()>();
        let begun = Instant::now();
        let first = GATE.run(HANG, DEADLINE, move || {
            let _ = held.recv();
            "late"
        });
        let waited = begun.elapsed();
        assert_eq!(first, Err(Refused::Unanswered));
        assert!(
            waited >= DEADLINE,
            "returned before the deadline: {waited:?}"
        );
        assert!(waited < HANG, "the call did not come back: {waited:?}");
        assert_eq!(GATE.running(), 1, "the stuck helper is still running");

        let (work, seen) = witness();
        assert_eq!(GATE.run(HANG, DEADLINE, work), Err(Refused::Unanswered));
        assert!(
            never_ran(&seen),
            "a second helper started behind a stuck one"
        );
        assert_eq!(GATE.running(), 1);

        release.send(()).expect("the stuck helper is still waiting");
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
    }

    #[test]
    fn calls_that_arrive_after_the_suspect_age_start_no_helper_while_the_first_still_waits() {
        const SUSPECT: Duration = Duration::from_millis(100);
        static GATE: Gate = Gate::new("test-gate");

        let (release, held) = mpsc::channel::<()>();
        let first = std::thread::spawn(|| {
            GATE.run(SUSPECT, HANG, move || {
                let _ = held.recv();
            })
        });
        started(&GATE, 1);
        for _ in 0..2 {
            std::thread::sleep(SUSPECT + Duration::from_millis(20));
            let (work, seen) = witness();
            let asked = Instant::now();
            assert_eq!(GATE.run(HANG, HANG, work), Err(Refused::Unanswered));
            assert!(
                asked.elapsed() < HANG,
                "a later call waited instead of refusing"
            );
            assert!(never_ran(&seen), "a later call started a helper");
            assert_eq!(GATE.running(), 1, "only the first call has a helper");
        }

        release.send(()).expect("the stuck helper is still waiting");
        assert_eq!(first.join().expect("the first caller"), Ok(()));
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || "answered"), Ok("answered"));
    }

    #[test]
    fn a_long_honest_call_does_not_make_a_short_one_refuse_until_it_is_overdue_itself() {
        static GATE: Gate = Gate::new("test-gate");
        const SHORT: Duration = Duration::from_millis(50);

        let (release, held) = mpsc::channel::<()>();
        let slow = std::thread::spawn(|| {
            GATE.run(HANG, HANG, move || {
                let _ = held.recv();
                "hashed"
            })
        });
        started(&GATE, 1);
        std::thread::sleep(SHORT * 3);
        assert_eq!(GATE.run(SHORT, HANG, || "quick"), Ok("quick"));

        release.send(()).expect("the slow helper is still waiting");
        assert_eq!(slow.join().expect("the slow caller"), Ok("hashed"));
        drained(&GATE);
    }

    #[test]
    fn a_started_call_past_its_suspect_age_is_still_its_callers_answer_and_refuses_the_others() {
        const SUSPECT: Duration = Duration::from_millis(50);
        static GATE: Gate = Gate::new("test-gate");

        let (release, held) = mpsc::channel::<()>();
        let mut pending = GATE
            .start(SUSPECT, move || {
                let _ = held.recv();
                "answered"
            })
            .expect("the gate is idle");
        std::thread::sleep(SUSPECT * 2);
        for _ in 0..3 {
            pending = match pending.wait(Duration::from_millis(5)) {
                Err(still) => still,
                Ok(answer) => panic!("answered before its work returned: {answer:?}"),
            };
        }
        let (work, seen) = witness();
        assert_eq!(GATE.run(HANG, HANG, work), Err(Refused::Unanswered));
        assert!(never_ran(&seen), "a call started behind an overdue one");

        release.send(()).expect("the helper is still waiting");
        let answer = pending.wait(HANG);
        assert!(matches!(answer, Ok(Ok("answered"))), "{answer:?}");
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
    }

    #[test]
    fn work_that_panics_is_a_failure_and_does_not_read_as_a_wedged_service() {
        static GATE: Gate = Gate::new("test-gate");
        let result: Result<(), Refused> = GATE.run(HANG, HANG, || panic!("stand-in failure"));
        assert!(matches!(result, Err(Refused::Failed(_))), "{result:?}");
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || "answered"), Ok("answered"));
    }

    #[cfg(unix)]
    #[test]
    fn a_child_that_finishes_gives_its_status_and_both_streams() {
        let out = output_until(
            crate::spawnfd::command("/bin/sh")
                .arg("-c")
                .arg("printf out; printf err >&2; exit 3"),
            Instant::now() + HANG,
        )
        .expect("ran");
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
    }

    /// SIGKILLs `pid` if it is a number (a test's own stray, by pid).
    #[cfg(unix)]
    fn kill_stray(pid: Option<u32>) {
        if let Some(pid) = pid {
            let _ = crate::spawnfd::command("/bin/kill")
                .args(["-KILL", &pid.to_string()])
                .status();
        }
    }

    /// The pid a shell's `echo` wrote to `file`, once its whole line is there.
    #[cfg(unix)]
    fn pid_in(file: &std::path::Path) -> Option<u32> {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.strip_suffix('\n')?.parse::<u32>().ok())
    }

    /// Waits (under the hang detector) until a shell has written its pid to `file`.
    #[cfg(unix)]
    fn wrote_pid(file: &std::path::Path) {
        let since = Instant::now();
        while pid_in(file).is_none() && since.elapsed() < HANG {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// How often a run under a [`ClockHeld`] looks at its clock while it is held.
    #[cfg(unix)]
    const LOOK: Duration = Duration::from_millis(10);

    /// This thread's deadline clock, held at the moment it was made until `ready()` holds or
    /// the hang detector runs out, then the real one; dropped, the real one.
    #[cfg(unix)]
    struct ClockHeld {
        at: Instant,
        /// The first reading past the hold.
        ran: std::rc::Rc<std::cell::Cell<Option<Instant>>>,
    }

    #[cfg(unix)]
    impl ClockHeld {
        fn until(ready: impl Fn() -> bool + 'static) -> Self {
            let at = Instant::now();
            let ran = std::rc::Rc::new(std::cell::Cell::new(None));
            let theirs = std::rc::Rc::clone(&ran);
            super::set_deadline_clock_for_test(Some(Box::new(move || {
                if theirs.get().is_none() && at.elapsed() < HANG && !ready() {
                    return at;
                }
                let now = Instant::now();
                theirs.set(theirs.get().or(Some(now)));
                now
            })));
            Self { at, ran }
        }

        /// A deadline one look past the held clock.
        fn deadline(&self) -> Instant {
            self.at + LOOK
        }

        /// How long since the clock ran past its hold; zero if it never did.
        fn since_it_ran(&self) -> Duration {
            self.ran.get().map_or(Duration::ZERO, |ran| ran.elapsed())
        }
    }

    #[cfg(unix)]
    impl Drop for ClockHeld {
        fn drop(&mut self) {
            super::set_deadline_clock_for_test(None);
        }
    }

    /// Waits (under the hang detector) until `pid` is gone; its new parent reaps it.
    #[cfg(unix)]
    fn gone(pid: u32) -> bool {
        let since = Instant::now();
        while crate::process::pid_alive(pid) {
            if since.elapsed() >= HANG {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        true
    }

    /// The call returns after the drain grace, not at the deadline, with what the child wrote.
    #[cfg(unix)]
    #[test]
    fn a_child_that_exits_while_a_descendant_holds_its_pipe_returns_what_it_wrote() {
        let dir = std::env::temp_dir().join(format!("uds-deadline-gc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let pid_file = dir.join("pid");
        let mut cmd = crate::spawnfd::command("/bin/sh");
        cmd.arg("-c")
            .arg("sleep 600 & echo $! > \"$0\"; echo hi")
            .arg(&pid_file);
        let begun = Instant::now();
        let out = output_until(&mut cmd, Instant::now() + HANG * 2);
        let waited = begun.elapsed();
        kill_stray(pid_in(&pid_file));
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.expect("ran");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"hi\n");
        assert!(waited < HANG, "waited out the deadline: {waited:?}");
    }

    /// A child past its deadline is killed and reaped, its descendants with it. The deadline's
    /// clock stands short of it until the shell has written its descendant's pid and its own,
    /// so both are checked on every run. Negative control: a `kill_group` that signals only
    /// the child leaves the descendant running past the hang detector.
    #[cfg(unix)]
    #[test]
    fn a_child_past_its_deadline_is_killed_and_reaped() {
        let dir = scratch("past");
        let pid_file = dir.join("pid");
        let grandchild_file = dir.join("grandchild");
        let mut cmd = sh(
            "sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600",
            &[&pid_file, &grandchild_file],
        );
        let both = [pid_file.clone(), grandchild_file.clone()];
        let clock = ClockHeld::until(move || both.iter().all(|file| pid_in(file).is_some()));
        let result = output_until(&mut cmd, clock.deadline());
        let waited = clock.since_it_ran();
        drop(clock);
        let child = pid_in(&pid_file);
        let grandchild = pid_in(&grandchild_file);
        let _ = std::fs::remove_dir_all(&dir);
        let (Some(child), Some(grandchild)) = (child, grandchild) else {
            panic!("the shell wrote no pids in a minute: {child:?} {grandchild:?}");
        };
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(waited < HANG, "the call did not come back: {waited:?}");
        assert!(
            !crate::process::pid_alive(child),
            "pid {child} outlived the call"
        );
        let dead = gone(grandchild);
        kill_stray((!dead).then_some(grandchild));
        assert!(
            dead,
            "the child's descendant {grandchild} outlived the call"
        );
    }

    /// A child still forking at its deadline is killed with every child it forks. The shell
    /// forks a `sleep` and kills it, over and over, so the deadline's group `SIGKILL` lands while
    /// it forks, and on XNU the child of that fork joins the group unsignalled
    /// ([`crate::process::kill_group_until_empty`]). Negative control: a `kill_group` that
    /// signals the group once leaves a `sleep 600` in it past the hang detector.
    #[cfg(unix)]
    #[test]
    fn a_group_still_forking_at_its_deadline_is_killed_whole() {
        let dir = scratch("forking");
        let pid_file = dir.join("pid");
        let mut cmd = sh(
            "echo $$ > \"$0\"; while :; do sleep 600 & kill $!; done",
            &[&pid_file],
        );
        let ready = pid_file.clone();
        let clock = ClockHeld::until(move || pid_in(&ready).is_some());
        let result = output_until(&mut cmd, clock.deadline());
        drop(clock);
        let group = pid_in(&pid_file);
        let _ = std::fs::remove_dir_all(&dir);
        let Some(group) = group.and_then(|pid| i32::try_from(pid).ok()) else {
            panic!("the shell wrote no pid in a minute");
        };
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(
            group_emptied(group),
            "a child forked as group {group} was killed ran on"
        );
    }

    /// Whether process group `group`, whose leader is reaped, has emptied (`ESRCH`) within the
    /// hang detector. One that has not is sent `SIGKILL`, so a failing run leaves nothing behind.
    #[cfg(unix)]
    fn group_emptied(group: i32) -> bool {
        let since = Instant::now();
        loop {
            // SAFETY: a plain syscall; signal 0 delivers nothing.
            if unsafe { libc::kill(-group, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                return true;
            }
            if since.elapsed() >= HANG {
                // SAFETY: a plain syscall to the group this test's child made, which a member
                // still holds (the id of a group with a member is no new process's).
                unsafe { libc::kill(-group, libc::SIGKILL) };
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_fails_at_spawn() {
        assert!(matches!(
            output_until(
                &mut crate::spawnfd::command("/nonexistent/tool"),
                Instant::now() + HANG
            ),
            Err(RunError::Spawn(_))
        ));
    }

    /// A shell running `script` with `args` as `$0`, `$1`, ….
    #[cfg(unix)]
    fn sh(script: &str, args: &[&std::path::Path]) -> crate::spawnfd::Command {
        let mut cmd = crate::spawnfd::command("/bin/sh");
        cmd.arg("-c").arg(script).args(args);
        cmd
    }

    /// A scratch directory of this test's own.
    #[cfg(unix)]
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("uds-deadline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    /// The wait ends at the exit, not at its timeout (the hang detector).
    #[cfg(unix)]
    #[test]
    fn an_exit_wait_ends_at_the_exit() {
        let mut child = crate::spawnfd::command("/bin/sh")
            .args(["-c", "exit 4"])
            .spawn()
            .expect("spawn");
        let mut exit = ExitWait::new(&child);
        let begun = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().expect("wait") {
                break status;
            }
            exit.wait(HANG);
        };
        assert!(begun.elapsed() < HANG, "the wait outlived the exit");
        assert_eq!(status.code(), Some(4));
    }

    /// The window the two progress tests hold a child to: short, so a stall is named in well
    /// under a second.
    #[cfg(unix)]
    const WINDOW: Duration = Duration::from_millis(300);

    /// A child that keeps making progress outlives its window and answers: the probe's
    /// reading moves every quarter window, and after three windows the probe drops the flag
    /// the child waits for.
    #[cfg(unix)]
    #[test]
    fn a_child_that_keeps_making_progress_outlives_its_window() {
        let dir = scratch("progressing");
        let flag = dir.join("flag");
        let mut cmd = sh(
            "while [ ! -e \"$0\" ]; do sleep 0.02; done; printf done",
            &[&flag],
        );
        let begun = Instant::now();
        let mut dropped = false;
        let mut probe = || {
            let age = begun.elapsed().min(HANG);
            if age >= WINDOW * 3 && !dropped {
                std::fs::write(&flag, b"").expect("the flag");
                dropped = true;
            }
            u64::try_from(age.as_millis() / (WINDOW / 4).as_millis()).unwrap_or(u64::MAX)
        };
        let out = output_while_progressing(&mut cmd, WINDOW, &mut probe);
        let waited = begun.elapsed();
        let _ = std::fs::remove_dir_all(&dir);
        let out = out.expect("a child making progress is no hang");
        assert!(out.status.success(), "{out:?}");
        assert_eq!(out.stdout, b"done");
        assert!(
            waited >= WINDOW * 3,
            "the child ran past three windows: {waited:?}"
        );
    }

    /// A child that stops making progress is killed, with its group, one whole window after
    /// its LAST step, not one window after it started, and the probe's last reading names the
    /// step it stopped after: the reading moves on each of the probe's first five looks and
    /// never again, while the child sleeps for ten minutes. The first look waits for the
    /// shell's pid, so the kill is checked on every run. Negative control: an `output_by` that
    /// returns at the stall without ending the child leaves it running.
    #[cfg(unix)]
    #[test]
    fn a_child_that_stops_making_progress_is_killed_one_window_after_its_last_step() {
        const STEPS: u64 = 5;
        let dir = scratch("stalled");
        let pid_file = dir.join("pid");
        let mut cmd = sh("echo $$ > \"$0\"; exec sleep 600", &[&pid_file]);
        let begun = Instant::now();
        let mut reading = 0;
        let mut last_step = begun;
        let mut probe = || {
            if reading == 0 {
                wrote_pid(&pid_file);
            }
            if reading < STEPS {
                reading += 1;
                last_step = Instant::now();
            }
            reading
        };
        let result = output_while_progressing(&mut cmd, WINDOW, &mut probe);
        let killed = Instant::now();
        let child = pid_in(&pid_file);
        let _ = std::fs::remove_dir_all(&dir);
        let child = child.expect("the shell wrote no pid in a minute");
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert_eq!(reading, STEPS, "the stall is named by the last step");
        let quiet = killed.duration_since(last_step);
        assert!(
            quiet >= WINDOW,
            "killed {quiet:?} after the last step, inside the window"
        );
        assert!(
            killed.duration_since(last_step) < HANG,
            "the call did not come back"
        );
        assert!(
            !crate::process::pid_alive(child),
            "pid {child} outlived the call"
        );
    }

    /// The first child cannot finish until the third has run, which a one-after-another run
    /// would reach only at the deadline.
    #[cfg(unix)]
    #[test]
    fn several_children_run_at_once_and_each_answers_for_itself() {
        let dir = scratch("many");
        let flag = dir.join("flag");
        let holder_file = dir.join("holder");
        let mut waits = sh(
            "while [ ! -e \"$0\" ]; do sleep 0.01; done; printf first; printf e1 >&2; exit 3",
            &[&flag],
        );
        let mut missing = crate::spawnfd::command("/nonexistent/tool");
        let mut signals = sh(": > \"$0\"; printf second", &[&flag]);
        let mut leaves = sh("sleep 600 & echo $! > \"$0\"; echo hi", &[&holder_file]);
        let begun = Instant::now();
        let results = outputs_until(
            &mut [&mut waits, &mut missing, &mut signals, &mut leaves],
            Instant::now() + HANG * 2,
            Duration::from_secs(1),
        );
        let waited = begun.elapsed();
        kill_stray(pid_in(&holder_file));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(waited < HANG, "waited out the deadline: {waited:?}");
        let mut results = results.into_iter();
        let first = results.next().expect("one").expect("the first ran");
        assert_eq!(first.status.code(), Some(3));
        assert_eq!(first.stdout, b"first");
        assert_eq!(first.stderr, b"e1");
        assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
        let third = results.next().expect("three").expect("the third ran");
        assert!(third.status.success());
        assert_eq!(third.stdout, b"second");
        let leaves = results.next().expect("four").expect("the fourth ran");
        assert!(leaves.status.success());
        assert_eq!(leaves.stdout, b"hi\n");
        assert!(results.next().is_none());
    }

    /// A child writing more than a pipe holds, on both streams, is drained while it runs and
    /// keeps the tail past [`OUTPUT_KEEP`].
    #[cfg(unix)]
    #[test]
    fn a_child_writing_more_than_a_pipe_holds_is_drained_while_it_runs() {
        let mut chatty = sh(
            "head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2; exit 0",
            &[],
        );
        let mut flood = sh("head -c 1100000 /dev/zero | tr '\\0' a; printf END", &[]);
        let results = outputs_until(
            &mut [&mut chatty, &mut flood],
            Instant::now() + HANG,
            Duration::from_secs(1),
        );
        let mut results = results.into_iter();
        let chatty = results.next().expect("one").expect("ran");
        assert!(chatty.status.success());
        assert_eq!(chatty.stdout.len(), 300_000);
        assert_eq!(chatty.stderr.len(), 300_000);
        let flood = results.next().expect("two").expect("ran");
        assert_eq!(flood.stdout.len(), OUTPUT_KEEP);
        assert!(flood.stdout.ends_with(b"aaaEND"), "the tail is kept");
    }

    /// At the deadline a running child is killed with its group and reaped within `reap`; the
    /// others keep their own answers (a tool that could not start is a spawn error, not a
    /// timeout). The deadline's clock stands short of it until the shell has written its
    /// descendant's pid and its own, so both are checked on every run. Negative control: a
    /// `kill_group` that signals only the child leaves the descendant running past the hang
    /// detector.
    #[cfg(unix)]
    #[test]
    fn at_the_deadline_the_running_child_is_killed_and_reaped_and_the_rest_keep_their_answers() {
        let dir = scratch("many-deadline");
        let pid_file = dir.join("pid");
        let grandchild_file = dir.join("grandchild");
        let mut wedged = sh(
            "sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600",
            &[&pid_file, &grandchild_file],
        );
        let mut missing = crate::spawnfd::command("/nonexistent/tool");
        let both = [pid_file.clone(), grandchild_file.clone()];
        let clock = ClockHeld::until(move || both.iter().all(|file| pid_in(file).is_some()));
        let results = outputs_until(&mut [&mut wedged, &mut missing], clock.deadline(), HANG);
        let waited = clock.since_it_ran();
        drop(clock);
        let child = pid_in(&pid_file);
        let grandchild = pid_in(&grandchild_file);
        let _ = std::fs::remove_dir_all(&dir);
        let (Some(child), Some(grandchild)) = (child, grandchild) else {
            panic!("the shell wrote no pids in a minute: {child:?} {grandchild:?}");
        };
        assert!(waited < HANG, "the call did not come back: {waited:?}");
        let mut results = results.into_iter();
        assert!(matches!(results.next(), Some(Err(RunError::TimedOut))));
        assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
        assert!(
            !crate::process::pid_alive(child),
            "pid {child} outlived the call"
        );
        let dead = gone(grandchild);
        kill_stray((!dead).then_some(grandchild));
        assert!(
            dead,
            "the child's descendant {grandchild} outlived the call"
        );
    }

    /// **A HELD CHILD RUNS ONLY ONCE RESUMED, TO ITS OUTPUT.** Held, the shell is exec'd and
    /// stopped before its first instruction (`ps`: `T`) and has left no marker;
    /// [`Held::output_until`] runs it to its status and both streams, as
    /// [`output_until`] does from a spawn.
    #[cfg(target_vendor = "apple")]
    #[test]
    fn a_held_child_runs_only_once_resumed_and_one_dropped_never_runs() {
        let state = |pid: u32| {
            let out = crate::spawnfd::command("/bin/ps")
                .args(["-o", "stat=", "-p", &pid.to_string()])
                .output()
                .expect("run ps");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let dir = scratch("held");
        let marker = dir.join("ran");
        let held = Held::start(&mut sh(
            "printf ran > \"$0\"; printf out; printf err >&2; exit 3",
            &[&marker],
        ))
        .expect("held");
        assert!(state(held.id()).starts_with('T'), "{:?}", state(held.id()));
        assert!(!marker.exists(), "a held child ran");
        let out = held
            .output_until(Instant::now() + HANG, Duration::from_secs(1))
            .expect("ran");
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert!(marker.exists(), "the resumed child ran");

        let unrun = dir.join("unrun");
        let dropped = Held::start(&mut sh("printf ran > \"$0\"", &[&unrun])).expect("held");
        let pid = dropped.id();
        drop(dropped);
        assert!(gone(pid), "the dropped held child {pid} outlived its drop");
        assert!(!unrun.exists(), "a held child dropped unresumed ran");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
