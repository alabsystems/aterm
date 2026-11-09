// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The bounded child runner, [`capture_bounded`]: a child read to a deadline
//! (the `claude auth` probes of [`super::cli`]).
//! On Unix, the work deadline does not expire ownership: return and unwind retain
//! the caller stack until the actual child is positively reaped and any reader joined.

use aterm_uds::spawnfd::{Child, Command, Stdio};
use std::io::Read;
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::mpsc;
#[cfg(all(test, unix))]
use std::sync::{Arc, atomic::AtomicBool, atomic::Ordering};
use std::time::Duration;
use std::time::Instant;

#[cfg(unix)]
use super::wake_deadline::SLEEP_CHECK;

/// On non-Unix hosts the reader still uses a blocking pipe.
#[cfg(not(unix))]
const READER_CLOSE_GRACE: Duration = Duration::from_millis(500);
/// The process may close stdout before it exits.
const REAP_CHECK: Duration = Duration::from_millis(20);

#[cfg(all(test, unix))]
type ChildReady = Box<dyn FnOnce(&mut Child) + Send>;

#[cfg(all(test, unix))]
type OwningWait = Box<dyn FnMut(&mut Child) -> std::io::Result<std::process::ExitStatus> + Send>;

#[cfg(all(test, unix))]
type GroupSignal = Box<dyn FnOnce(i32) -> std::io::Result<()> + Send>;

#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum CaptureCustody {
    Owned,
    Reaped(std::process::ExitStatus),
    Lost,
}

#[cfg(unix)]
#[derive(Clone, Copy)]
struct WaitDiagnostic {
    stage: &'static str,
    kind: std::io::ErrorKind,
    os: Option<i32>,
}

#[cfg(unix)]
impl CaptureCustody {
    fn owns_unreaped(&self) -> bool {
        matches!(self, Self::Owned)
    }

    fn reaped(&self) -> Option<std::process::ExitStatus> {
        match self {
            Self::Reaped(status) => Some(*status),
            _ => None,
        }
    }

    fn observe(&mut self, result: &std::io::Result<Option<std::process::ExitStatus>>) {
        if !self.owns_unreaped() {
            return;
        }
        match result {
            Ok(Some(status)) => *self = Self::Reaped(*status),
            Err(error) => self.observe_error(error),
            _ => {}
        }
    }

    fn observe_error(&mut self, error: &std::io::Error) {
        if self.owns_unreaped() && error.raw_os_error() == Some(libc::ECHILD) {
            *self = Self::Lost;
        }
    }
}

/// On Unix, owns settlement before caller admission guards or staging can unwind.
/// Group signaling is attempted only while the original child is unreaped; it is
/// not a receipt of descendant quiescence. An uncertain owning wait retains this stack.
struct CaptureOwner {
    child: Child,
    #[cfg(unix)]
    reader: Option<std::thread::JoinHandle<()>>,
    #[cfg(unix)]
    cancel: Option<std::io::PipeWriter>,
    #[cfg(unix)]
    owned: Option<aterm_uds::tether::Watched>,
    #[cfg(unix)]
    custody: CaptureCustody,
    #[cfg(unix)]
    uncertainty: Option<WaitDiagnostic>,
    #[cfg(unix)]
    uncertainty_reported: bool,
    observer: CaptureObserver,
    /// Non-Unix keeps the incoming child-only end semantics; no reader-join guarantee.
    #[cfg(not(unix))]
    done: bool,
}

#[cfg(unix)]
impl CaptureOwner {
    fn new(child: Child, observer: CaptureObserver) -> Self {
        Self {
            child,
            reader: None,
            cancel: None,
            owned: None,
            custody: CaptureCustody::Owned,
            uncertainty: None,
            uncertainty_reported: false,
            observer,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        if !self.custody.owns_unreaped() {
            return self
                .custody
                .reaped()
                .map(Some)
                .ok_or_else(|| std::io::Error::from_raw_os_error(libc::ECHILD));
        }
        let result = self.child.try_wait();
        self.custody.observe(&result);
        if let Err(error) = &result {
            self.note_uncertainty("natural-wait", error);
        }
        if matches!(result, Ok(Some(_))) {
            mark_capture_child_reaped(&self.observer);
        }
        result
    }

    /// Read owned-child identity only after this owner already protects setup/unwind.
    fn watch_owned(&mut self) {
        self.owned = i32::try_from(self.child.id())
            .ok()
            .and_then(aterm_uds::process::read_lineage)
            .map(|lineage| aterm_uds::tether::Watched::of(&lineage));
    }

    fn cancel_output(&mut self) {
        self.cancel.take();
    }

    fn note_uncertainty(&mut self, stage: &'static str, error: &std::io::Error) {
        self.uncertainty.get_or_insert(WaitDiagnostic {
            stage,
            kind: error.kind(),
            os: error.raw_os_error(),
        });
    }

    fn report_uncertainty(&mut self, report: bool) {
        if !report || std::thread::panicking() || self.uncertainty_reported {
            return;
        }
        let Some(seen) = self.uncertainty else {
            return;
        };
        self.uncertainty_reported = true;
        // Explicit cleanup only: logging may block, but the original ownership stays held.
        // Unwind settlement never invokes the installed logger's callbacks.
        if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            aterm_log::info!(
                "capture ownership unsettled stage={} kind={:?} os={:?}",
                seen.stage,
                seen.kind,
                seen.os
            );
        })) {
            discard_capture_panic(payload);
        }
    }

    fn join_reader(&mut self) -> Result<(), String> {
        let Some(reader) = self.reader.take() else {
            return Ok(());
        };
        let joined = reader.join();
        #[cfg(test)]
        if let Some(observer) = &self.observer {
            observer.reader_joined.store(true, Ordering::Release);
            if let Some(witness) = &observer.cleanup_witness {
                witness.reader_joined.store(true, Ordering::Release);
            }
        }
        match joined {
            Ok(()) => Ok(()),
            Err(payload) => {
                discard_capture_panic(payload);
                Err("its output reader panicked".to_owned())
            }
        }
    }

    fn stop(&mut self, report: bool) -> Result<(), String> {
        // The original unreaped child pins its process group. Never signal an id after
        // the owning child implementation has reaped it, even if foreground restoration failed.
        let group = if self.custody.owns_unreaped() && !self.child.is_reaped() {
            if let Some(watched) = self.owned.take() {
                // Birth-checked TERM/KILL grace ends only this child and does not reap it.
                let _ = end_owned(watched);
            }
            let signal = if let Ok(pgid) = libc::pid_t::try_from(self.child.id()) {
                #[cfg(test)]
                if let Some(observer) = &self.observer {
                    observer.group_kills.fetch_add(1, Ordering::AcqRel);
                }
                #[cfg(not(test))]
                let result = aterm_uds::process::kill_group_until_empty(pgid);
                #[cfg(test)]
                let result = match self
                    .observer
                    .as_ref()
                    .and_then(|observer| observer.group_signal.lock().unwrap().take())
                {
                    Some(signal) => signal(pgid),
                    None => aterm_uds::process::kill_group_until_empty_within(
                        pgid,
                        aterm_spec::wait::MUST_HAPPEN,
                    ),
                };
                // `kill_group_until_empty` has already settled a group of the ended. What is
                // left is `ESRCH`, or an `EPERM` it could not settle: on Apple a group it could
                // not list, elsewhere a real refusal. Accept these errors only after the owning
                // wait below positively settles the child's exit; a one-shot WNOHANG probe here
                // races it.
                match result {
                    Ok(()) => Ok(()),
                    Err(error)
                        if matches!(error.raw_os_error(), Some(libc::EPERM | libc::ESRCH)) =>
                    {
                        Ok(())
                    }
                    Err(_) => Err("its capture group could not be signalled".to_owned()),
                }
            } else {
                Err("its capture group is unavailable".to_owned())
            };
            if self.custody.owns_unreaped() && !self.child.is_reaped() {
                let _ = self.child.kill();
            }
            signal
        } else {
            Ok(())
        };
        self.cancel_output();
        let joined = self.join_reader();
        self.report_uncertainty(report);
        let mut wait_error = false;
        if self.custody.reaped().is_none() {
            let mut exit = self
                .custody
                .owns_unreaped()
                .then(|| aterm_uds::deadline::ExitWait::new(&self.child));
            loop {
                if !self.custody.owns_unreaped() {
                    // waitpid(pid) after ECHILD could name a reused PID. Neither another
                    // signal nor a later status can restore this original ownership proof.
                    std::thread::sleep(REAP_CHECK);
                    continue;
                }
                let result = capture_owning_wait(&mut self.child, &self.observer).map(Some);
                self.custody.observe(&result);
                match result {
                    Ok(_) => {
                        mark_capture_child_reaped(&self.observer);
                        break;
                    }
                    Err(error) => {
                        // Work has stopped, but ownership is unsettled. ECHILD is not a
                        // positive owning wait. Keep the original caller stack and retry paced.
                        wait_error = true;
                        self.note_uncertainty("cleanup-wait", &error);
                        if !self.custody.owns_unreaped() {
                            exit = None;
                        }
                        discard_capture_value(error);
                        self.report_uncertainty(report);
                        let retry_at = Instant::now();
                        if let Some(exit) = &mut exit {
                            exit.wait(REAP_CHECK);
                        }
                        // A latched exit descriptor or ECHILD may make the watch immediately
                        // readable. Ownership polling still has a minimum monotonic interval.
                        let left = REAP_CHECK.saturating_sub(retry_at.elapsed());
                        if !left.is_zero() {
                            std::thread::sleep(left);
                        }
                    }
                }
            }
        }
        group?;
        joined?;
        if wait_error {
            return Err("its capture child could not be reaped on the first wait".to_owned());
        }
        Ok(())
    }
}

impl Drop for CaptureOwner {
    fn drop(&mut self) {
        #[cfg(unix)]
        if self.custody.reaped().is_none() || self.reader.is_some() {
            let _ = self.stop(false);
        }
        #[cfg(not(unix))]
        if !self.done {
            let _ = self.stop(false);
        }
    }
}

#[cfg(not(unix))]
impl CaptureOwner {
    fn new(child: Child, observer: CaptureObserver) -> Self {
        Self {
            child,
            observer,
            done: false,
        }
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let result = self.child.try_wait();
        match result {
            Ok(Some(_)) => {
                self.done = true;
                mark_capture_child_reaped(&self.observer);
            }
            Err(_) => self.done = true,
            Ok(None) => {}
        }
        result
    }

    fn stop(&mut self, _report: bool) -> Result<(), String> {
        if self.done {
            return Ok(());
        }
        self.done = true;
        let _ = self.child.kill();
        let reaped = capture_owning_wait(&mut self.child, &self.observer)
            .map_err(|_| "its capture child could not be reaped".to_owned());
        if reaped.is_ok() {
            mark_capture_child_reaped(&self.observer);
        }
        reaped.map(|_| ())
    }
}

type CaptureChild = CaptureOwner;

/// Per-invocation witness for the Tier-1 lifecycle bind: it records the real
/// worker exit and child reap rather than inferring them from a successful
/// return.
#[cfg(all(test, unix))]
#[derive(Default)]
struct CaptureLifecycleObservation {
    child_reaped: AtomicBool,
    reader_done: AtomicBool,
    reader_joined: AtomicBool,
    /// How many times the capture signalled its child's group.
    group_kills: std::sync::atomic::AtomicUsize,
    group_signal: std::sync::Mutex<Option<GroupSignal>>,
    /// Run after spawn, and the deadline taken again after it, so what a
    /// test's child must have done by then (an escape test's holder leaving
    /// for its own session) happens before the deadline under any load.
    after_spawn: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Run after the deadline is taken and before the spawn: a spawn that
    /// takes this long, charged to the budget.
    before_spawn: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// A nonreaping actual-child observation before starting the output waiter.
    child_ready: std::sync::Mutex<Option<ChildReady>>,
    owning_wait: std::sync::Mutex<Option<OwningWait>>,
    cleanup_witness: Option<Arc<CaptureCleanupWitness>>,
    after_reader: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    reader_step: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// How long, in nanoseconds, the host is taken to have slept since the capture began: added
    /// to the clock that counts through sleep ([`Deadline`]), which no test can make the host do.
    slept_ns: std::sync::atomic::AtomicU64,
    /// Run once, as the capture's wait first starts waiting, after it has taken what is left of
    /// the deadline: what happens while it waits.
    in_wait: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
    /// Run at an actual full stdin pipe, before its bounded writable wait.
    in_input_wait: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

#[cfg(all(test, unix))]
#[derive(Default)]
pub(super) struct CaptureCleanupWitness {
    pub(super) child_reaped: AtomicBool,
    pub(super) reader_joined: AtomicBool,
}

/// Exercise the real stop/cleanup path with a per-capture owning-wait fault.
#[cfg(all(test, unix))]
pub(super) fn capture_with_owning_wait(
    cmd: Command,
    wait: impl FnMut(&mut Child) -> std::io::Result<std::process::ExitStatus> + Send + 'static,
    witness: Arc<CaptureCleanupWitness>,
) -> Result<Ended, String> {
    let observed = Arc::new(CaptureLifecycleObservation {
        cleanup_witness: Some(witness),
        ..CaptureLifecycleObservation::default()
    });
    *observed.owning_wait.lock().unwrap() = Some(Box::new(wait));
    capture_with(
        cmd,
        aterm_spec::wait::MUST_HAPPEN,
        1024,
        None,
        Some(observed),
        false,
        Some(&mut |_| true),
    )
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
        if let Some(witness) = &observer.cleanup_witness {
            witness.child_reaped.store(true, Ordering::Release);
        }
    }
    #[cfg(not(all(test, unix)))]
    let _ = observer;
}

use super::auth_deadline::Deadline;

/// The clock that counts through sleep, now ([`Deadline`]), and in a test what the test says the
/// host slept besides.
fn through_sleep_ns(observer: &CaptureObserver) -> Option<u64> {
    let now = aterm_uds::process::continuous_ns()?;
    #[cfg(all(test, unix))]
    if let Some(observer) = observer {
        return Some(now.saturating_add(observer.slept_ns.load(Ordering::Acquire)));
    }
    #[cfg(not(all(test, unix)))]
    let _ = observer;
    Some(now)
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
/// Unix cleanup retains the caller until positive owning reap and reader join;
/// an unresolved owning wait can therefore outlast the work budget.
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
    /// The child exited. Ordinary capture completed stdout; an owned capture can cancel and
    /// drain available stdout at its child's exit while a descendant still holds the pipe.
    Exited(Captured),
    /// The child printed the watched text, or the external watch requested a stop.
    /// The owned cleanup path ran without waiting for the capture deadline.
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

/// [`capture_bounded`] for a child that must not outlive this call or this process, such as a
/// renewal's `claude auth login`, which waits for ever at its paste prompt once nothing answers
/// it. The child is tied to this process ([`tie`]): a quit, an exec in its place (an update's
/// cold lane), a crash or a `SIGKILL` of this process ends it too, by its tether's watcher on
/// macOS (`SIGTERM`, `SIGKILL` after the grace) or the parent-death signal on Linux (`SIGTERM`).
/// That end reaches the child alone, as no watcher signals a group: what it left in its group
/// ends by its own bound (a renewal's opener by its sign-in's, and the browser it drives with the
/// opener's pipe). On macOS the tie runs through this process's executable, so where an update
/// or a rebuild replaced that file the child is not started, and the answer says so. Every way
/// this call stops waiting while the child runs (the deadline, output it cannot read, an unwind)
/// ends it first as that watcher would ([`end_owned`]), then the rest of its process group,
/// before its reap. A child that exits by itself has ended: the call answers its exit then, with
/// what it printed, even while something it left in its group still holds its output, and signals
/// nothing it left there; the opener it started may still be closing its browser, which a kill
/// before that browser's grace would cost the profile's cookies
/// (`headless_oauth::BROWSER_EXIT_GRACE`).
/// Unix cleanup retains its caller until positive owning reap and reader join, even past the
/// work deadline. Group signaling is not descendant-quiescence evidence. Other hosts retain
/// their blocking-reader limitation; they have no new Unix settlement guarantee.
///
/// # Errors
///
/// As [`capture_bounded`], and that this process's executable was replaced since it started.
pub fn capture_bounded_owned(
    mut cmd: Command,
    budget: Duration,
    max_stdout: usize,
) -> Result<Captured, String> {
    tie(&mut cmd)?;
    match capture_with(cmd, budget, max_stdout, None, owned_observer(), true, None)? {
        Ended::Exited(got) => Ok(got),
        // No stop text was watched for, so no capture ends on one.
        Ended::Stopped => Err(String::from("it was stopped")),
    }
}

/// [`capture_bounded_owned`] that also watches an external terminal condition, `stopped`, while
/// the child runs unreaped with piped stdin, as a renewal watches for its browser opener's
/// recorded failure or a hosted OAuth code to inject. Once it says so the child is ended as every
/// other stop ends it, the output reader is cancelled and joined, and the answer is
/// [`Ended::Stopped`]; an exit the capture observes first is answered as that exit. A panic in
/// `stopped` becomes an error after the same end; its payload is not returned.
/// The callback receives only bounded input under the original deadline, not the child's
/// wait or signal authority.
///
/// # Errors
///
/// As [`capture_bounded_owned`], or `stopped` panicked, or the end of the child's group or its
/// reap could not be confirmed.
#[cfg(unix)]
pub(super) fn capture_bounded_owned_watching(
    mut cmd: Command,
    budget: Duration,
    max_stdout: usize,
    mut stopped: impl FnMut(&mut CaptureInput<'_>) -> bool,
) -> Result<Ended, String> {
    tie(&mut cmd)?;
    capture_with(
        cmd,
        budget,
        max_stdout,
        None,
        owned_observer(),
        true,
        Some(&mut stopped),
    )
}

/// Why a child that must be tied to this process is not started: on macOS a tethered spawn runs
/// through this process's executable, which an update or a rebuild replaced since it started.
const UNTIED: &str = "it will not run: this aterm's executable was replaced since it started (an \
                      update), so it could not be tied to this aterm; an aterm started since can \
                      run it";

/// Tie `cmd`'s child to this process ([`aterm_uds::spawnfd::Tether`]), or say why it cannot be:
/// on macOS the tie runs through this process's executable, and once an update or a rebuild put
/// another file at its path a tethered spawn would start the child untied
/// ([`aterm_uds::tether::lost`]). A spawn this tied is checked again by [`tie_held`].
///
/// # Errors
///
/// That this process's executable was replaced.
pub(super) fn tie(cmd: &mut Command) -> Result<(), String> {
    use aterm_uds::spawnfd::Tether as _;
    tie_held()?;
    cmd.tether();
    Ok(())
}

/// After a spawn [`tie`] tied: whether the tie still holds, or why not, the executable replaced
/// since, perhaps before the spawn ran through it, so the child may have started untied; its
/// caller ends that child.
///
/// # Errors
///
/// That this process's executable was replaced.
pub(super) fn tie_held() -> Result<(), String> {
    if aterm_uds::tether::lost() {
        Err(String::from(UNTIED))
    } else {
        Ok(())
    }
}

/// The observer an owned capture runs under: the one a test set for this thread's next owned
/// capture ([`settle_next_owned`]), else none.
fn owned_observer() -> CaptureObserver {
    #[cfg(all(test, unix))]
    {
        NEXT_OWNED.with(std::cell::RefCell::take)
    }
    #[cfg(not(all(test, unix)))]
    {
        CaptureObserver::default()
    }
}

#[cfg(all(test, unix))]
thread_local! {
    /// The observer this thread's next owned capture runs under ([`settle_next_owned`]).
    static NEXT_OWNED: std::cell::RefCell<CaptureObserver> = const { std::cell::RefCell::new(None) };
}

/// TEST SEAM: this thread's next owned capture ([`capture_bounded_owned`]) takes its deadline
/// again once `ready` holds after its spawn, or a minute later, so its child has said what a test
/// reads before the capture can give up on it.
#[cfg(all(test, unix))]
pub(super) fn settle_next_owned(ready: impl Fn() -> bool + Send + 'static) {
    let observed = Arc::new(CaptureLifecycleObservation::default());
    *observed
        .after_spawn
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Box::new(move || {
        let until = Instant::now() + aterm_spec::wait::MUST_HAPPEN;
        while !ready() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }));
    NEXT_OWNED.with(|next| *next.borrow_mut() = Some(observed));
}

/// TEST SEAM: whether a settle [`settle_next_owned`] set is still waiting for an owned capture
/// on this thread.
#[cfg(all(test, unix))]
pub(super) fn next_owned_pending() -> bool {
    NEXT_OWNED.with(|next| next.borrow().is_some())
}

#[cfg(all(test, unix))]
#[path = "align_owned_fixture.rs"]
pub(super) mod owned_fixture;

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
    cmd: Command,
    budget: Duration,
    max_stdout: usize,
    stop: Option<&'static [u8]>,
    observer: CaptureObserver,
) -> Result<Ended, String> {
    capture_with(cmd, budget, max_stdout, stop, observer, false, None)
}

/// The capture: [`capture_bounded_impl`]'s, its child ended as an owned capture's
/// ([`capture_bounded_owned`]) where `owned` says so, and stopped once `watch`, where there is
/// one, says to ([`capture_bounded_owned_watching`]).
fn capture_with(
    mut cmd: Command,
    budget: Duration,
    max_stdout: usize,
    stop: Option<&'static [u8]>,
    observer: CaptureObserver,
    owned: bool,
    mut watch: Option<&mut dyn FnMut(&mut CaptureInput<'_>) -> bool>,
) -> Result<Ended, String> {
    let mut keep: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (name, value) in cmd.get_envs() {
        if let Some(value) = value {
            keep.push((name.to_os_string(), value.to_os_string()));
        }
    }
    cmd.env_clear()
        .stdin(if watch.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped());
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
    let continuous = || through_sleep_ns(&observer);
    let deadline = Deadline::with_clocks(budget, &Instant::now, &continuous);
    #[cfg(all(test, unix))]
    observed_step(&observer, |o| &o.before_spawn);
    let child = cmd.spawn().map_err(|e| format!("it will not run: {e}"))?;
    let mut child = CaptureOwner::new(child, observer.clone());
    #[cfg(unix)]
    if owned {
        child.watch_owned();
    }
    if owned && let Err(why) = tie_held() {
        kill_capture_child(&mut child, &observer);
        return Err(why);
    }
    #[cfg(all(test, unix))]
    if let Some(observer) = &observer
        && let Some(ready) = observer.child_ready.lock().unwrap().take()
    {
        ready(&mut child.child);
    }
    // The parent's copy of the stderr pipe's write end goes with the command, so the watch sees
    // the pipe end once the child's group has closed it.
    drop(cmd);
    #[cfg(all(test, unix))]
    let deadline = if observed_step(&observer, |o| &o.after_spawn) {
        Deadline::with_clocks(budget, &Instant::now, &continuous)
    } else {
        deadline
    };
    let stdout = child.child.stdout.take();
    let Some(stdout) = stdout else {
        kill_capture_child(&mut child, &observer);
        return Err(String::from("its stdout could not be captured"));
    };
    let (tx, rx) = mpsc::channel();
    // Every Unix reader can be cancelled, including during unwinding of an unwatched capture.
    #[cfg(unix)]
    let cancel_reader = match std::io::pipe() {
        Ok((r, w)) => {
            child.cancel = Some(w);
            Some(r)
        }
        Err(e) => {
            kill_capture_child(&mut child, &observer);
            return Err(format!("its output reader could not be cancelled: {e}"));
        }
    };
    #[cfg(all(test, unix))]
    let reader_observer = observer.clone();
    #[cfg(unix)]
    let reader_end = deadline.end();
    let reader = std::thread::Builder::new()
        .name("aterm-capture-output".to_owned())
        .spawn(move || {
            #[cfg(all(test, unix))]
            observed_step(&reader_observer, |o| &o.reader_step);
            #[cfg(unix)]
            let read = read_capture_pipe_until(
                stdout,
                max_stdout,
                reader_end,
                stop,
                cancel_reader.as_ref().map(AsRawFd::as_raw_fd),
            );
            #[cfg(not(unix))]
            let read = read_blocking_until(stdout, max_stdout, stop);
            let _ = tx.send(read);
            // Once what it read is queued for the wait.
            #[cfg(all(test, unix))]
            if let Some(observer) = reader_observer {
                observer.reader_done.store(true, Ordering::Release);
            }
        });
    let mut reader = match reader {
        Ok(reader) => Some(reader),
        Err(error) => {
            kill_capture_child(&mut child, &observer);
            return Err(format!("its output reader could not start: {error}"));
        }
    };
    #[cfg(unix)]
    {
        child.reader = reader.take();
    }
    #[cfg(all(test, unix))]
    observed_step(&observer, |o| &o.after_reader);
    #[cfg(unix)]
    let waited = loop {
        let remaining = deadline.remaining().unwrap_or_default();
        let watching = stderr_watch.is_some() || watch.is_some();
        let tick = match (watching, owned) {
            (true, true) => STDERR_WATCH.min(REAP_CHECK),
            (true, false) => STDERR_WATCH,
            (false, true) => REAP_CHECK,
            // The wait cannot see the clock that counts through sleep, so it looks again.
            (false, false) if deadline.counts_sleep() => SLEEP_CHECK,
            (false, false) => {
                #[cfg(all(test, unix))]
                observed_step(&observer, |o| &o.in_wait);
                break rx.recv_timeout(remaining);
            }
        };
        #[cfg(all(test, unix))]
        observed_step(&observer, |o| &o.in_wait);
        match rx.recv_timeout(remaining.min(tick)) {
            Err(mpsc::RecvTimeoutError::Timeout) if !remaining.is_zero() => {
                if stderr_watch.as_mut().is_some_and(StderrWatch::seen) {
                    kill_capture_child(&mut child, &observer);
                    let _ = join_capture_reader(&mut child, &mut reader);
                    return Ok(Ended::Stopped);
                }
                // An owned child's own exit is its end, even while something it left in its group
                // holds its stdout: what it wrote is in the pipe for the reader to take. It is
                // answered before the watch is asked, as an exit after the output ended is.
                if owned {
                    let exited = child.try_wait();
                    if !matches!(exited, Ok(None)) {
                        // Reaped is recorded before cancellation. Its remaining group is
                        // deliberately left alone while the bounded available output drains.
                        child.cancel_output();
                        let read = rx.recv();
                        let joined = join_capture_reader(&mut child, &mut reader);
                        joined?;
                        return match (exited, read) {
                            (Ok(Some(status)), Ok(Ok((bytes, _)))) => Ok(Ended::Exited(Captured {
                                stdout: bytes,
                                code: status.code(),
                            })),
                            (Err(error), _) => {
                                kill_capture_child(&mut child, &observer);
                                Err(format!("it could not be reaped: {error}"))
                            }
                            (_, Ok(Err(error))) => {
                                Err(format!("its output was unreadable: {error}"))
                            }
                            (_, _) => Err(String::from("its output was unreadable")),
                        };
                    }
                }
                let checked =
                    check_capture_watch(&mut watch, &mut child.child, deadline, &observer);
                if !matches!(checked, Ok(false)) {
                    let cleanup = kill_capture_child_checked(&mut child, &observer);
                    let joined = join_capture_reader(&mut child, &mut reader);
                    cleanup?;
                    joined?;
                    checked?;
                    return Ok(Ended::Stopped);
                }
            }
            other => break other,
        }
    };
    #[cfg(not(unix))]
    let waited = rx.recv_timeout(deadline.remaining().unwrap_or_default());
    let bytes = match waited {
        Ok(Ok((_, true))) => {
            let _ = join_capture_reader(&mut child, &mut reader);
            kill_capture_child(&mut child, &observer);
            return Ok(Ended::Stopped);
        }
        Ok(Ok((bytes, false))) => {
            let _ = join_capture_reader(&mut child, &mut reader);
            bytes
        }
        Ok(Err(error)) => {
            let _ = join_capture_reader(&mut child, &mut reader);
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
            let _ = join_capture_reader(&mut child, &mut reader);
            #[cfg(not(unix))]
            {
                let _ = rx.recv_timeout(READER_CLOSE_GRACE);
                if reader
                    .as_ref()
                    .is_some_and(std::thread::JoinHandle::is_finished)
                {
                    let _ = join_capture_reader(&mut child, &mut reader);
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
                break status;
            }
            Ok(None) => {}
            Err(error) => {
                kill_capture_child(&mut child, &observer);
                return Err(format!("it could not be reaped: {error}"));
            }
        }
        // Natural exit observed after output completion retains its actual exit status.
        // Until then the original unreaped child still pins its private process group.
        let checked = check_capture_watch(&mut watch, &mut child.child, deadline, &observer);
        if !matches!(checked, Ok(false)) {
            kill_capture_child_checked(&mut child, &observer)?;
            checked?;
            return Ok(Ended::Stopped);
        }
        let remaining = deadline.remaining().unwrap_or_default();
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

fn join_capture_reader(
    child: &mut CaptureChild,
    reader: &mut Option<std::thread::JoinHandle<()>>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let _ = reader;
        child.join_reader()
    }
    #[cfg(not(unix))]
    {
        let _ = child;
        let Some(reader) = reader.take() else {
            return Ok(());
        };
        match reader.join() {
            Ok(()) => Ok(()),
            Err(payload) => {
                discard_capture_panic(payload);
                Err("its output reader panicked".to_owned())
            }
        }
    }
}

fn discard_capture_panic(payload: Box<dyn std::any::Any + Send>) {
    discard_capture_value(payload);
}

fn discard_capture_value<T>(value: T) {
    if let Err(payload) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(value))) {
        std::mem::forget(payload);
    }
}

/// Borrowed input to the actual capture child. It conveys no wait, signal or reap right.
pub(super) struct CaptureInput<'a> {
    #[cfg(unix)]
    stdin: Option<&'a mut std::process::ChildStdin>,
    deadline: Deadline<'a>,
    #[cfg(all(test, unix))]
    observer: &'a CaptureObserver,
}

#[cfg(unix)]
impl CaptureInput<'_> {
    /// Writes this slice through the same pipe, retaining progress under the original budget.
    /// # Errors
    /// The pipe is absent, closed or unavailable; work is cancelled; or the original budget
    /// expires. An error can follow partial delivery and never proves no provider effect.
    pub(super) fn write_all(
        &mut self,
        mut bytes: &[u8],
        cancelled: impl Fn() -> bool,
    ) -> std::io::Result<()> {
        use std::io::Write as _;
        if cancelled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "capture input cancelled",
            ));
        }
        self.deadline
            .remaining()
            .map_err(|_| capture_pipe_deadline())?;
        let fd = self
            .stdin
            .as_ref()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?
            .as_raw_fd();
        capture_pipe_nonblocking(fd)?;
        loop {
            if cancelled() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "capture input cancelled",
                ));
            }
            let remaining = self
                .deadline
                .remaining()
                .map_err(|_| capture_pipe_deadline())?;
            if bytes.is_empty() {
                return Ok(());
            }
            match self
                .stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?
                .write(bytes)
            {
                Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    bytes = &bytes[n..];
                    continue;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
            #[cfg(all(test, unix))]
            observed_step(self.observer, |o| &o.in_input_wait);
            // A readiness wake is not permission for another write: the loop rechecks work
            // and the same deadline, including host sleep, before continuing this offset.
            let mut ready = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            let millis = remaining
                .min(REAP_CHECK)
                .as_millis()
                .max(1)
                .min(i32::MAX as u128) as i32;
            // SAFETY: the capture retains its actual child and this borrowed pipe for the
            // entire call; ready is one initialized pollfd, and no handle is reconstructed.
            let polled = unsafe { libc::poll(&mut ready, 1, millis) };
            if polled < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::Interrupted {
                    return Err(error);
                }
            } else if ready.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
        }
    }
}

fn check_capture_watch(
    watch: &mut Option<&mut dyn FnMut(&mut CaptureInput<'_>) -> bool>,
    child: &mut aterm_uds::spawnfd::Child,
    deadline: Deadline<'_>,
    observer: &CaptureObserver,
) -> Result<bool, String> {
    let Some(watch) = watch else {
        return Ok(false);
    };
    let mut input = CaptureInput {
        #[cfg(unix)]
        stdin: child.stdin.as_mut(),
        deadline,
        #[cfg(all(test, unix))]
        observer,
    };
    #[cfg(not(unix))]
    let _ = child;
    #[cfg(not(all(test, unix)))]
    let _ = observer;
    if input.deadline.expired() {
        return Ok(false);
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| watch(&mut input))) {
        Ok(stopped) => Ok(stopped),
        Err(payload) => {
            // A callback's arbitrary panic payload may itself panic on drop.
            // Contain that destructor too before touching the owned cleanup path.
            discard_capture_panic(payload);
            Err("its capture watch panicked".to_owned())
        }
    }
}

#[cfg(unix)]
fn capture_pipe_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    // SAFETY: the caller exclusively owns or borrows this pipe end for this call;
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

/// Waits until `fd` is readable (`true`) or `cancel` is (`false`: the waiting thread dropped the
/// cancel pipe's write end, as the capture ended), within `deadline`.
#[cfg(unix)]
fn capture_pipe_ready(
    fd: std::os::fd::RawFd,
    cancel: Option<std::os::fd::RawFd>,
    deadline: Instant,
) -> std::io::Result<bool> {
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
            n if n > 0 && ready[1].revents != 0 => return Ok(false),
            n if n > 0 => return Ok(true),
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
/// and once `cancel` (the read end of a pipe whose writer the waiting thread holds) becomes
/// readable or hangs up, with what the pipe holds by then: a capture stopped on stderr, or an
/// owned one whose child exited by itself, joins this reader at once even while a holder outside
/// the killed group, or one the child left in its own, keeps stdout open.
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
    let mut cancelled = false;
    while bytes.len() < max_stdout {
        if !cancelled && Instant::now() >= deadline {
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
                if cancelled {
                    return Ok((bytes, false));
                }
                cancelled = !capture_pipe_ready(pipe.as_raw_fd(), cancel, deadline)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok((bytes, false))
}

/// Stop the child and descendants that inherited its stdout, then reap the
/// direct child.
fn kill_capture_child(child: &mut CaptureChild, observer: &CaptureObserver) {
    let _ = kill_capture_child_checked(child, observer);
}

fn kill_capture_child_checked(
    child: &mut CaptureChild,
    observer: &CaptureObserver,
) -> Result<(), String> {
    let _ = observer;
    child.stop(true)
}

fn capture_owning_wait(
    child: &mut Child,
    observer: &CaptureObserver,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(all(test, unix))]
    if let Some(observer) = observer
        && let Some(wait) = observer
            .owning_wait
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_mut()
    {
        return match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| wait(child))) {
            Ok(result) => result,
            Err(payload) => {
                discard_capture_panic(payload);
                Err(std::io::Error::other("its test owning-wait hook panicked"))
            }
        };
    }
    #[cfg(not(all(test, unix)))]
    let _ = observer;
    child.wait()
}

/// End an owned capture's child as its tether's watcher ends it once this process is gone:
/// `SIGTERM`, then `SIGKILL` to one still there [`END_GRACE`] later, each only while its pid still
/// reads the birth and the kernel unique id `watched` read at its spawn and its thread has run
/// ([`aterm_uds::tether::end_watched`]), whose answer this is. A child that already exited, its
/// zombie unreaped, reads as gone and gets nothing. This does not reap or settle capture custody.
/// In this crate's tests the grace is the one-minute hang detector
/// (`aterm_spec::wait::MUST_HAPPEN`, as for the group kill after it): a test's child must answer
/// its `SIGTERM`, and how soon a loaded machine schedules it to is no latency budget
/// (`end_proven`'s own tests hold the grace itself).
///
/// [`END_GRACE`]: aterm_uds::process::END_GRACE
#[cfg(unix)]
fn end_owned(
    watched: aterm_uds::tether::Watched,
) -> Result<aterm_uds::process::Ended, aterm_uds::process::EndRefusal> {
    #[cfg(not(test))]
    let grace = aterm_uds::process::END_GRACE;
    #[cfg(test)]
    let grace = aterm_spec::wait::MUST_HAPPEN;
    aterm_uds::tether::end_watched(
        watched,
        grace,
        &mut aterm_uds::process::EndControlLive,
        |pid| {
            Some(aterm_uds::tether::Proof {
                uniqueid: aterm_uds::process::read_lineage(pid)?.uniqueid,
                ran: aterm_uds::process::thread_ran(pid)?,
            })
        },
    )
}

#[path = "align_tests.rs"]
#[cfg(test)]
mod tests;
