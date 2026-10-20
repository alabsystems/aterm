// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A deadline on a call that can block forever: in this process ([`Gate`]) or in a child
//! ([`output_until`], [`output_while_progressing`], [`outputs_until`]).
//!
//! The case is macOS's `trustd`. Security.framework's certificate and code-signing checks
//! (`SecTrustEvaluateWithError`, `SecCodeCheckValidity`, `SecStaticCodeCheckValidity` on
//! certificate-chained code) and `/usr/bin/codesign` are synchronous Mach round-trips to it;
//! with `trustd` silent they wait forever (no socket timeout reaches a thread parked in a Mach
//! receive, and there is no cancellation API). `SecCodeCopySelf`, designated-requirement reads,
//! `SecStaticCodeCreateWithPath`, a validity check of ad-hoc code, `spctl` and `security` still
//! answer.
//!
//! # In this process: [`Gate::run`]
//!
//! The call runs on a helper thread and the caller stops waiting at `deadline`:
//!
//! * `work` is `'static + Send`, so a helper that outlives its caller cannot reach the
//!   caller's state. They share one reference-counted slot; a caller that gives up marks it
//!   abandoned, and the helper drops its result when it returns.
//! * Every running helper carries its own suspect age: how long that kind of call may honestly
//!   take (a TLS evaluation a few seconds, hashing a large bundle a minute). While any helper is
//!   past its suspect age, a new call is refused at once without starting a helper, so the
//!   stuck helpers never grow past those that started before the first went overdue. When they
//!   return the next call tries the service again: recovery needs no restart. The check and
//!   the start are one step under the list's lock.
//! * Per-call suspect ages keep one site's honest slowness from refusing another; an overdue
//!   call refuses every site sharing the gate, which is why every in-process wait on `trustd`
//!   goes through the one [`TRUST`] gate.
//! * A panic settles as [`Refused::Failed`], never as a service that did not answer.
//!
//! [`Gate::start`] is the same call with the wait left to the caller: a [`Pending`] it waits
//! for in slices, watching other things between them, for as long as it chooses. Its suspect
//! age still says when it starts refusing the other calls; dropped unanswered, it is abandoned
//! as [`Gate::run`] abandons a call at its deadline.
//!
//! # In a child: [`output_until`]
//!
//! `codesign`, `spctl`, `security`: run to completion by a deadline, with the child's process
//! group killed and reaped at the deadline. Children need no gate. The exit is waited for as
//! the kernel reports it ([`ExitWait`]: a kqueue on Darwin, a pidfd on Linux), not on a sleep
//! tick, so a 40 ms tool returns at 40 ms.
//!
//! A child that does a long run of small steps is held to its progress instead
//! ([`output_while_progressing`]): killed once a caller's probe of it has not moved for one
//! window, however long the whole run takes.
//!
//! # Several children at once: [`outputs_until`]
//!
//! On Unix every child starts before any is waited for, so a check that needs five tools waits
//! for the slowest, not the sum. One `poll(2)` on the calling thread drains their streams and
//! watches their exits, so it starts no thread (the boot apply runs it before the window
//! exists).
//!
//! # A child started now, run later: [`Held`]
//!
//! On Darwin a child can be started stopped at its exec and run to its output once the caller
//! decides it may: the exec, and the work the system does at one, overlap the caller's own
//! work, and nothing of the program runs before the decision.
//!
//! # Tests
//!
//! Test and debug builds (never a release one) add [`require_trust_service`] and
//! [`require_disk_image_service`]: a test that needs the service ends COULD NOT RUN when it
//! does not answer ([`crate::host::could_not_run`]).

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

#[cfg(any(test, debug_assertions))]
mod silent;
#[cfg(any(test, debug_assertions))]
pub use silent::{
    CODESIGN_HANG, ad_hoc_copy, require_disk_image_service, require_trust_service, sign_ad_hoc,
    trust_service_silent,
};

/// The gate for every in-process call that can wait on macOS's `trustd` (TLS certificate
/// evaluation, the PTY keeper's peer and on-disk code checks): one process-wide gate, so a
/// wedge any of them finds refuses all of them and the stuck threads stay at about one.
pub static TRUST: Gate = Gate::new("aterm-trust-check");

/// Why [`Gate::run`] produced no result.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    /// The work did not finish within the deadline, or an earlier call's work
    /// has been running past its suspect age and still has not.
    Unanswered,
    /// The work could not be run at all (no thread could be started) or it
    /// panicked. Carries what went wrong, for the error text.
    Failed(String),
}

/// What the caller and the helper share.
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

/// Runs work on a helper thread under a deadline; see the module docs.
#[derive(Debug)]
pub struct Gate {
    /// The helper threads' name.
    name: &'static str,
    helpers: Mutex<Helpers>,
}

impl Gate {
    /// A gate whose helper threads are called `name`.
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

    /// Helpers that have not yet returned.
    pub fn running(&self) -> usize {
        self.helpers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
            .len()
    }

    /// `work()`, on a helper thread, if it finishes within `deadline`.
    ///
    /// Refused at once, without starting a helper, while an earlier call's work has run longer
    /// than its suspect age. This call's suspect age is `suspect` (capped at `deadline`).
    ///
    /// # Errors
    /// [`Refused::Unanswered`] past the deadline or while another call is
    /// overdue; [`Refused::Failed`] when no thread could start or `work`
    /// panicked.
    // Redundant to the compiler (the module is native-only); read by the wasm census's OB-12
    // spawn sweep, which does not evaluate `lib.rs`'s gate.
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
        // Still pending at the deadline: dropping it marks the slot abandoned.
        self.start(suspect.min(deadline), work)?
            .wait(deadline)
            .unwrap_or(Err(Refused::Unanswered))
    }

    /// `work()`, started on a helper thread, its answer left to the caller to wait for
    /// ([`Pending::wait`]) for as long as it chooses.
    ///
    /// Refused at once, without starting a helper, while an earlier call's work has run longer
    /// than its suspect age. This call's suspect age is `suspect`: past it, while the caller
    /// still waits, every other call through the gate is refused at once.
    ///
    /// # Errors
    /// [`Refused::Unanswered`] while another call is overdue; [`Refused::Failed`] when no
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
        // The record is in before the helper exists, and the guard is gone before
        // it starts: the helper's own `forget` never runs under `helpers`.
        let spawned = std::thread::Builder::new()
            .name(self.name.to_owned())
            .spawn(move || {
                // Caught so the slot always settles: an unsettled one reads as no answer.
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

    /// The helper `id` has returned.
    fn forget(&self, id: u64) {
        let mut helpers = self.helpers.lock().unwrap_or_else(PoisonError::into_inner);
        helpers.running.retain(|helper| helper.id != id);
    }
}

/// A call [`Gate::start`] began whose answer nobody has taken yet. Dropped before then, it is
/// abandoned: the helper drops its result when it returns.
pub struct Pending<T> {
    slot: Arc<(Mutex<Slot<std::thread::Result<T>>>, Condvar)>,
}

impl<T> Pending<T> {
    /// The call's answer, if it comes within `within`; the call back, still pending, if not.
    ///
    /// # Errors
    /// The pending call itself while its work has not returned. Its answer is
    /// [`Refused::Failed`] when the work panicked.
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
                // Only this call's own drop abandons it, and that has not run.
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

/// Why [`output_until`] produced no output.
#[derive(Debug)]
pub enum RunError {
    /// The child could not be started (or a reader thread for it could not).
    Spawn(io::Error),
    /// Waiting for the child failed; it was killed and reaped.
    Wait(io::Error),
    /// The child did not finish by the deadline; it was killed and reaped.
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

/// The first sleep tick of an [`ExitWait`] with no exit descriptor, doubled up to
/// [`POLL_MAX`] so a tool that takes tens of milliseconds is not rounded up to a fixed tick.
const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(25);

/// How long [`output_until`] reads the streams after the child exits: a pipe still open past
/// that is held by a descendant, not part of the child's report.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// How much of each stream [`output_until`] keeps: the tail, so a tool's last diagnostic is
/// never the part cut; far more than any tool it runs prints.
pub const OUTPUT_KEEP: usize = 1024 * 1024;

/// `Command::output` with a deadline. Stdin is closed; stdout and stderr are drained on their
/// own threads, so a chatty child cannot wedge on a full pipe (the last [`OUTPUT_KEEP`] bytes
/// of each kept). On Unix the child leads a new process group, and at the deadline the group
/// is killed (`SIGKILL`) and the child reaped, so nothing it started outlives the call. After
/// the child exits its streams are read for at most [`DRAIN_GRACE`] more, in case a descendant
/// still holds a pipe.
///
/// Build `cmd` with [`crate::spawnfd::command`]: the child holds its stdio and nothing else
/// and, on Darwin, starts without a fork.
///
/// # Errors
/// [`RunError::Spawn`], [`RunError::Wait`] or [`RunError::TimedOut`]; never a
/// partial output.
pub fn output_until(cmd: &mut Command, deadline: Instant) -> Result<Output, RunError> {
    output_until_from(cmd, Stdio::null(), deadline)
}

/// [`output_until`] with `stdin` as the child's standard input (a file the caller opened),
/// for a tool that reads its request there.
///
/// # Errors
/// As [`output_until`].
pub fn output_until_from(
    cmd: &mut Command,
    stdin: Stdio,
    deadline: Instant,
) -> Result<Output, RunError> {
    output_by(cmd, stdin, &mut || {
        deadline.saturating_duration_since(Instant::now())
    })
}

/// The most time [`output_while_progressing`] lets pass between two reads of its progress
/// probe (a quarter of its window, when that is shorter): a stall is named within one tick of
/// its window, and a probe that reads a file's length costs nothing at this rate.
const PROGRESS_TICK: Duration = Duration::from_millis(100);

/// [`output_until`] held to the child's PROGRESS, not to one deadline: for a child that does a
/// long run of small steps (one shell running a batch of calls), where a deadline on the whole
/// run times the batch rather than a hang, and a loaded machine fails an honest run.
///
/// `progress` is the caller's measure of how far the child has got (the length of a file it
/// appends to is the usual one), read when the child is first found running and then at least
/// every [`PROGRESS_TICK`] (a quarter of `window` when that is shorter); any change in what it
/// returns is progress. The child runs as long as it keeps making some, and once `progress`
/// has answered the same for `window` the child's process group is killed (`SIGKILL`) and the
/// child reaped, as [`output_until`] does at its deadline. A reading taken late (a descheduled
/// caller) still counts what the child did meanwhile. Stdin, the kept tails, the process group
/// and [`DRAIN_GRACE`] are as in [`output_until`].
///
/// # Errors
/// As [`output_until`]. [`RunError::TimedOut`] here means no progress for `window`: the
/// probe's last reading is where the child stopped, which the caller can name.
pub fn output_while_progressing(
    cmd: &mut Command,
    window: Duration,
    progress: &mut dyn FnMut() -> u64,
) -> Result<Output, RunError> {
    let tick = PROGRESS_TICK.min(window / 4).max(Duration::from_millis(1));
    // The reading, and when it last changed: taken at the first look, after the spawn.
    let mut last: Option<(u64, Instant)> = None;
    output_by(cmd, Stdio::null(), &mut || {
        // The reading first, then the clock: a quiet stretch is never counted from before
        // the reading that ended the last one.
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

/// The one child run behind [`output_until_from`] and [`output_while_progressing`]: each time
/// the child is found still running, `left` says how long to wait for its exit before looking
/// again, and zero ends the run (the group is killed and the child reaped,
/// [`RunError::TimedOut`]).
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
    let stdout = child.stdout.take().map(|s| drain(s, "stdout"));
    let stderr = child.stderr.take().map(|s| drain(s, "stderr"));
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
    // The drains finish promptly unless a descendant still holds a pipe; then what was read by
    // the grace is the child's report.
    let by = Instant::now() + DRAIN_GRACE;
    let collect = |drain: Option<Drain>| drain.map(|d| d.collect(by)).unwrap_or_default();
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Waits for one child's exit as the kernel reports it.
///
/// It blocks on a descriptor the kernel makes readable at the exit (Darwin: a kqueue
/// `EVFILT_PROC`/`NOTE_EXIT`; Linux: a pidfd; see [`crate::exitwatch::exit_fd`]), so the wait
/// ends at the exit, or at the caller's timeout. A sleep tick from 1 ms doubling to 25 ms
/// would add about half again to a 30-70 ms `codesign`. Without such a descriptor (Windows, a
/// kernel without `pidfd_open`) it sleeps that backoff tick.
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
    /// A wait on `child`'s exit.
    #[must_use]
    pub fn new(child: &Child) -> Self {
        #[cfg(not(unix))]
        let _ = child;
        Self {
            #[cfg(unix)]
            fd: crate::exitwatch::exit_fd(child.id()),
            tick: POLL_MIN,
        }
    }

    /// Block until the kernel reports the child's exit or `timeout` passes (without a
    /// descriptor, for one backoff tick at most); the caller's `try_wait` says which.
    pub fn wait(&mut self, timeout: Duration) {
        #[cfg(unix)]
        if let Some(fd) = &self.fd {
            match children::exit_reported(fd, timeout) {
                Ok(false) => return,
                // Reported: the next `try_wait` reaps it. Ticks from here on.
                Ok(true) => {
                    self.reported();
                    return;
                }
                // A `poll` that cannot run (no memory): fall back to ticks.
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

/// Kill (`SIGKILL`; on Unix the child's whole process group, which
/// [`output_until`] made it lead) and reap.
fn end(child: &mut Child) {
    #[cfg(unix)]
    children::kill_group(child);
    #[cfg(not(unix))]
    let _ = child.kill();
    let _ = child.wait();
}

/// Reap a killed `child` by `by`, waiting on its exit as the kernel reports
/// it. On Unix, one the kernel has not let go by then is left for the
/// dropped spawnfd handle to hand to [`crate::reaper`]. This function does
/// not certify that the child has exited when its reserve runs out.
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

/// [`output_until`] for each of `cmds`, all started before any is waited for and held to the
/// one `deadline`; the results come back in `cmds`' order. Stdin, the kept tails, the process
/// group and [`DRAIN_GRACE`] are as in [`output_until`].
///
/// Unlike [`output_until`], the cleanup at the deadline is bounded: every child still running
/// has its group killed (`SIGKILL`) and all are reaped within `reap`. One the kernel has not
/// let go by then is left to [`crate::reaper`] and answers [`RunError::TimedOut`] either way,
/// so a caller on a deadline of its own cannot park on a process wedged past its `SIGKILL`.
///
/// On Unix no thread is started: one `poll(2)` drains every stream and watches every exit.
/// Elsewhere the children run one after another through [`output_until`].
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

/// A child STARTED STOPPED AT ITS EXEC ([`crate::spawnfd::Command::start_suspended`]), for
/// [`Self::output_until`] to run later. Its exec — and whatever the system does at one, a
/// first-exec assessment included — proceeds while the caller does other work, and not one
/// instruction of the program runs before the caller resumes it.
///
/// Dropped unrun, it is killed (`SIGKILL`, its process group) without ever running and reaped
/// within [`HELD_REAP`]; one the kernel has not let go by then is left to [`crate::reaper`].
/// If this process dies holding it, it dies unrun too: it leads its own process group, which
/// the kernel hangs up when orphaned, with `SIGHUP` at its default
/// ([`crate::spawnfd::Command::start_suspended`]).
#[cfg(unix)]
#[derive(Debug)]
pub struct Held {
    /// `None` once [`Self::output_until`] has taken it.
    child: Option<Child>,
}

#[cfg(unix)]
impl Held {
    /// Start `cmd` stopped at its exec, set up as [`outputs_until`] runs a child: leading a new
    /// process group, stdin `/dev/null`, stdout and stderr piped.
    ///
    /// # Errors
    /// The spawn's, the exec's own included: `Unsupported` off Darwin, with nothing started.
    pub fn start(cmd: &mut Command) -> io::Result<Self> {
        let child = children::prepare(cmd).start_suspended(true).spawn()?;
        Ok(Self { child: Some(child) })
    }

    /// The held child's pid.
    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.as_ref().map_or(0, Child::id)
    }

    /// Resume the child and run it to its output by `deadline`, exactly as [`outputs_until`]
    /// runs one child from its spawn (the reaping reserve `reap` included).
    ///
    /// # Errors
    /// As [`outputs_until`]'s, and [`RunError::Spawn`] for a child that could not be resumed
    /// (killed and reaped within `reap`; it never ran).
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

/// One stream being read on a thread of its own: the tail so far, and a
/// signal when the stream has ended.
struct Drain {
    tail: Arc<Mutex<Vec<u8>>>,
    ended: mpsc::Receiver<()>,
}

impl Drain {
    /// The tail, once the stream ends or at `by`, whichever is first.
    fn collect(self, by: Instant) -> Vec<u8> {
        let _ = self
            .ended
            .recv_timeout(by.saturating_duration_since(Instant::now()));
        std::mem::take(&mut *self.tail.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// Read `stream` to its end on a thread of its own, keeping the tail.
#[cfg(not(target_arch = "wasm32"))]
fn drain(mut stream: impl Read + Send + 'static, which: &str) -> io::Result<Drain> {
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
    Ok(Drain { tail, ended })
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

    /// Short: the work never answers on its own; the assertion is that the call comes back.
    const DEADLINE: Duration = Duration::from_millis(200);
    /// A wait for something that must happen: a hang detector, not a budget.
    const HANG: Duration = Duration::from_secs(60);

    /// Work that reports, on a channel, whether it ever ran.
    fn witness() -> (impl FnOnce() + Send + 'static, mpsc::Receiver<()>) {
        let (ran, seen) = mpsc::channel::<()>();
        (
            move || {
                let _ = ran.send(());
            },
            seen,
        )
    }

    /// Its work was dropped unrun: the channel closed without a message.
    fn never_ran(seen: &mpsc::Receiver<()>) -> bool {
        seen.recv_timeout(HANG) == Err(mpsc::RecvTimeoutError::Disconnected)
    }

    /// Waits (under the hang detector) until no helper is running.
    fn drained(gate: &Gate) {
        let since = Instant::now();
        while gate.running() != 0 {
            assert!(since.elapsed() < HANG, "a released helper never returned");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits (under the hang detector) until `n` helpers are running.
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

        // A `trustd` that never replies: the work blocks until the test releases it.
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

        // While it is stuck, a second call is refused without starting a helper.
        let (work, seen) = witness();
        assert_eq!(GATE.run(HANG, DEADLINE, work), Err(Refused::Unanswered));
        assert!(
            never_ran(&seen),
            "a second helper started behind a stuck one"
        );
        assert_eq!(GATE.running(), 1);

        // Released, the helper leaves the list and the next call runs.
        release.send(()).expect("the stuck helper is still waiting");
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
    }

    #[test]
    fn calls_that_arrive_after_the_suspect_age_start_no_helper_while_the_first_still_waits() {
        // The first caller's deadline is long; later calls past its suspect age must refuse
        // at once, not each start a helper and wait out a deadline of their own.
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

        // The first caller still holds its own deadline; release the service
        // and it gets its answer.
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
        // Older than the short call's suspect age, but within its own.
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
        // Waited for in slices, it stays pending, and its caller keeps it.
        for _ in 0..3 {
            pending = match pending.wait(Duration::from_millis(5)) {
                Err(still) => still,
                Ok(answer) => panic!("answered before its work returned: {answer:?}"),
            };
        }
        // Past its suspect age it refuses every other call at once.
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

    /// The pid a shell wrote to `file`, if it got that far.
    #[cfg(unix)]
    fn pid_in(file: &std::path::Path) -> Option<u32> {
        std::fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
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

    /// A child past its deadline is killed and reaped, its descendants with it.
    #[cfg(unix)]
    #[test]
    fn a_child_past_its_deadline_is_killed_and_reaped() {
        let dir = std::env::temp_dir().join(format!("uds-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let pid_file = dir.join("pid");
        let mut cmd = crate::spawnfd::command("/bin/sh");
        // `exec`, so the pid the shell writes is sleep's own; the background
        // sleep is a descendant that must die with it.
        let grandchild_file = dir.join("grandchild");
        cmd.arg("-c")
            .arg("sleep 600 & echo $! > \"$1\"; echo $$ > \"$0\"; exec sleep 600")
            .arg(&pid_file)
            .arg(&grandchild_file);
        let begun = Instant::now();
        let result = output_until(&mut cmd, Instant::now() + Duration::from_secs(2));
        let waited = begun.elapsed();
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert!(waited < HANG, "the call did not come back: {waited:?}");
        // A zombie still answers `kill(pid, 0)`, so gone means killed and reaped. A machine
        // too loaded to start the shell in two seconds leaves no pid to check.
        if let Some(pid) = std::fs::read_to_string(&pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<u32>().ok())
        {
            assert!(
                !crate::process::pid_alive(pid),
                "pid {pid} outlived the call"
            );
        }
        if let Some(pid) = pid_in(&grandchild_file) {
            let dead = gone(pid);
            kill_stray((!dead).then_some(pid));
            assert!(dead, "the child's descendant {pid} outlived the call");
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    /// under a second. Each test's progress is the probe's own (a reading the test moves), so
    /// neither verdict rests on how fast a loaded machine runs the child.
    #[cfg(unix)]
    const WINDOW: Duration = Duration::from_millis(300);

    /// A child that keeps making progress outlives its window and answers: the probe's
    /// reading moves every quarter window, and after three windows the probe drops the flag
    /// the child waits for. A deadline of one window would have killed it. The reading stops
    /// moving at the hang detector, so a child that never sees the flag still fails.
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
    /// never again, while the child sleeps for ten minutes.
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
        assert!(matches!(result, Err(RunError::TimedOut)), "{result:?}");
        assert_eq!(reading, STEPS, "the stall is named by the last step");
        let quiet = killed.duration_since(last_step);
        assert!(
            quiet >= WINDOW,
            "killed {quiet:?} after the last step, inside the window"
        );
        assert!(
            killed.duration_since(begun) < HANG,
            "the call did not come back"
        );
        // A zombie still answers `kill(pid, 0)`, so gone means killed and reaped. A machine
        // too loaded to start the shell in time leaves no pid to check.
        if let Some(pid) = child {
            assert!(
                !crate::process::pid_alive(pid),
                "pid {pid} outlived the call"
            );
        }
    }

    /// The first child cannot finish until the third has run, which a one-after-another run
    /// would reach only at the deadline. Each answers with its own status and streams, in
    /// order; a tool that cannot start stops nothing else; one that leaves a descendant on its
    /// pipe answers after the grace.
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
    /// timeout).
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
        let begun = Instant::now();
        let results = outputs_until(
            &mut [&mut wedged, &mut missing],
            Instant::now() + Duration::from_secs(2),
            Duration::from_secs(1),
        );
        assert!(begun.elapsed() < HANG, "the call did not come back");
        let mut results = results.into_iter();
        assert!(matches!(results.next(), Some(Err(RunError::TimedOut))));
        assert!(matches!(results.next(), Some(Err(RunError::Spawn(_)))));
        // A zombie still answers `kill(pid, 0)`, so gone means killed and reaped. A machine
        // too loaded to start the shell in two seconds leaves no pid to check.
        if let Some(pid) = pid_in(&pid_file) {
            assert!(
                !crate::process::pid_alive(pid),
                "pid {pid} outlived the call"
            );
        }
        if let Some(pid) = pid_in(&grandchild_file) {
            let dead = gone(pid);
            kill_stray((!dead).then_some(pid));
            assert!(dead, "the child's descendant {pid} outlived the call");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A HELD CHILD RUNS ONLY ONCE RESUMED, TO ITS OUTPUT.** Held, the shell is exec'd and
    /// stopped before its first instruction (`ps`: `T`) and has left no marker;
    /// [`Held::output_until`] runs it to its status and both streams, as
    /// [`output_until`] does from a spawn. Dropped unresumed, a held child is killed and
    /// gone without having run.
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
