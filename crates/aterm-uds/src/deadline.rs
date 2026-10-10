// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A DEADLINE ON A CALL THAT CAN BLOCK FOREVER — in this process ([`Gate`]) or
//! in a child ([`output_until`]).
//!
//! The case that made this necessary is macOS's `trustd`. Security.framework's
//! certificate and code-signing checks (`SecTrustEvaluateWithError`,
//! `SecCodeCheckValidity`, `SecStaticCodeCheckValidity` on certificate-chained
//! code) are synchronous Mach round-trips to it, and `/usr/bin/codesign` makes
//! the same ones. MEASURED on a Mac on 2026-10-01: with `trustd` not answering,
//! every one of them sat in `dispatch_mach_send_and_wait_for_reply` for as long
//! as anyone waited — no socket timeout reaches a thread parked in a Mach
//! receive, and there is no cancellation API — while `SecCodeCopySelf`, the
//! designated-requirement reads, `SecStaticCodeCreateWithPath`, a validity
//! check of ad-hoc code, `spctl` and `security` all still answered.
//!
//! # In this process: [`Gate::run`]
//!
//! The only bound available is to make the call somewhere else and stop
//! waiting for it:
//!
//! * **The helper owns everything it touches.** `work` is `'static + Send`: it
//!   can capture owned data and nothing borrowed, so a helper that outlives its
//!   caller cannot reach the caller's stack or state. The caller and the helper
//!   share one reference-counted slot, which whichever of them finishes last
//!   frees.
//! * **The caller waits at most `deadline`**, then marks the slot ABANDONED and
//!   returns [`Refused::Unanswered`]. The helper is left to finish on its own;
//!   when it does, it finds the mark, drops its result and leaves.
//! * **A wedged service costs a bounded number of threads.** The gate keeps,
//!   for every helper still running, when it started and its OWN suspect age —
//!   how long that kind of call may honestly take (a TLS evaluation a few
//!   seconds, hashing a large bundle a minute). A new call is refused AT ONCE,
//!   without starting a helper, while any helper has been running longer than
//!   its suspect age: the service is not answering, and another helper would
//!   only queue behind it and stick too. So the stuck helpers are at most the
//!   calls that STARTED before the first of them went overdue, and never grow
//!   after; each caller that did start keeps its full deadline. When the stuck
//!   helpers finally return they leave the list, and the next call tries the
//!   service again: recovery needs no restart.
//! * **A panic settles as a failure** ([`Refused::Failed`]), never as a service
//!   that did not answer.
//!
//! The check and the start are one step under the list's lock, so two callers
//! cannot both see an empty list and then both start behind a wedged service.
//!
//! Each call names its own suspect age so that one site's honest slowness is
//! never read as the service wedging for another: a bundle check legitimately
//! hashing for twenty seconds must not make a TLS handshake refuse. A call that
//! IS overdue refuses every site sharing the gate at once — which is why every
//! in-process call that waits on `trustd` goes through the one [`TRUST`] gate.
//!
//! # In a child: [`output_until`]
//!
//! `codesign`, `spctl`, `security`: run to completion by a deadline, both
//! streams drained on their own threads so a chatty child cannot wedge on a
//! full pipe. On Unix the child leads a process group of its own, and at the
//! deadline the whole group is killed (`SIGKILL`) and the child reaped before
//! [`RunError::TimedOut`], so nothing it started outlives the call holding
//! its pipes. A child that exits on time is not waited past a short grace
//! for a descendant still holding a pipe. Children need no gate.
//!
//! # Tests
//!
//! With the `test-support` feature (dev-dependencies only), [`notice_once`]
//! and [`trust_service_silent`]: a test that needs `trustd` to answer asks
//! once per binary, and skips with one line on stderr when it does not;
//! [`disk_image_service_silent`] does the same for an `hdiutil` that timed out.

use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::process::Output;

use crate::spawnfd::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

#[cfg(any(test, feature = "test-support"))]
mod silent;
#[cfg(any(test, feature = "test-support"))]
pub use silent::{
    CODESIGN_HANG, DISK_IMAGE_SKIPPED, SKIPPED, disk_image_service_silent, notice_once,
    sign_ad_hoc, trust_service_silent,
};

/// Every in-process call in this process that can wait on macOS's `trustd`:
/// the TLS certificate evaluation, the PTY keeper's peer and on-disk code
/// checks. One gate — a process-wide `static` — so a wedge discovered by any
/// of them refuses all of them at once, and the stuck threads stay at about
/// one.
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
    /// Refused at once, without starting a helper, while an earlier call's
    /// work has been running longer than ITS suspect age. This call's own
    /// suspect age is `suspect` (or `deadline`, if shorter): how long this kind
    /// of work may honestly take.
    ///
    /// # Errors
    /// [`Refused::Unanswered`] past the deadline or while another call is
    /// overdue; [`Refused::Failed`] when no thread could start or `work`
    /// panicked.
    // Native-only (the module is): redundant to the compiler, read by the
    // wasm census's OB-12 spawn sweep, which does not evaluate `lib.rs`'s gate.
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
                suspect: suspect.min(deadline),
            });
            id
        };
        // The record is in before the helper exists, and the guard is gone before
        // it starts: the helper's own `forget` never runs under `helpers`.
        let spawned = std::thread::Builder::new()
            .name(self.name.to_owned())
            .spawn(move || {
                // A panic is caught so the slot is always settled: an
                // unsettled slot would read as a service that never answered.
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

        let (lock, answered) = &*ours;
        let slot = lock.lock().unwrap_or_else(PoisonError::into_inner);
        let (mut slot, _) = answered
            .wait_timeout_while(slot, deadline, |slot| matches!(slot, Slot::Pending))
            .unwrap_or_else(PoisonError::into_inner);
        match std::mem::replace(&mut *slot, Slot::Abandoned) {
            Slot::Done(Ok(value)) => Ok(value),
            Slot::Done(Err(_panic)) => Err(Refused::Failed("the check panicked".to_owned())),
            // Still pending at the deadline: the slot now says ABANDONED, and
            // the helper drops its value when it finally returns.
            Slot::Pending | Slot::Abandoned => Err(Refused::Unanswered),
        }
    }

    /// The helper `id` has returned.
    fn forget(&self, id: u64) {
        let mut helpers = self.helpers.lock().unwrap_or_else(PoisonError::into_inner);
        helpers.running.retain(|helper| helper.id != id);
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

/// The first child-exit poll, doubled up to [`POLL_MAX`]: a tool that takes
/// tens of milliseconds is not rounded up to a fixed tick.
const POLL_MIN: Duration = Duration::from_millis(1);
const POLL_MAX: Duration = Duration::from_millis(25);

/// How long [`output_until`] waits, once the child has exited, for its streams
/// to close: they close with the child, unless something it started still
/// holds one, and that is not the child's report.
const DRAIN_GRACE: Duration = Duration::from_millis(250);

/// How much of each stream [`output_until`] keeps: the TAIL, so the last
/// diagnostic a tool writes is never the part cut, and far more than any of
/// the tools it runs prints.
pub const OUTPUT_KEEP: usize = 1024 * 1024;

/// `cmd`'s output, if it finishes by `deadline` — `Command::output` with a
/// deadline. Stdin is closed; stdout and stderr are each drained on their own
/// thread (their last [`OUTPUT_KEEP`] bytes kept). On Unix the child leads a
/// new process group; a child still running at the deadline has its group
/// killed (`SIGKILL`) and is reaped. Once the child exits, its streams are
/// read for at most [`DRAIN_GRACE`] more: what it wrote is kept even when a
/// descendant still holds a pipe open.
///
/// `cmd` is a [`crate::spawnfd::Command`] (build it with
/// [`crate::spawnfd::command`]), so the child holds its stdio and nothing
/// else and, on Darwin, is started without a fork.
///
/// # Errors
/// [`RunError::Spawn`], [`RunError::Wait`] or [`RunError::TimedOut`]; never a
/// partial output.
pub fn output_until(cmd: &mut Command, deadline: Instant) -> Result<Output, RunError> {
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd
        .stdin(Stdio::null())
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
    let mut poll = POLL_MIN;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    end(&mut child);
                    return Err(RunError::TimedOut);
                }
                std::thread::sleep(poll.min(remaining));
                poll = (poll * 2).min(POLL_MAX);
            }
            Err(e) => {
                end(&mut child);
                return Err(RunError::Wait(e));
            }
        }
    };
    // The child has exited, so its ends of the pipes are closed and the drains
    // finish promptly — unless a descendant still holds one; then what was
    // read by the grace is the child's report.
    let by = Instant::now() + DRAIN_GRACE;
    let collect = |drain: Option<Drain>| drain.map(|d| d.collect(by)).unwrap_or_default();
    Ok(Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    })
}

/// Kill (`SIGKILL`; on Unix the child's whole process group, which
/// [`output_until`] made it lead) and reap.
fn end(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        unsafe extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        const SIGKILL: i32 = 9;
        // SAFETY: a plain syscall. The child is not yet reaped, so its pid —
        // and with it the group id it leads — cannot have been reused.
        unsafe { kill(-pid, SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
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

    use super::{Gate, Refused};
    #[cfg(unix)]
    use super::{RunError, output_until};

    /// The deadline the stand-in work is held to. Short, because the work never
    /// answers on its own; the assertion is that the CALL comes back.
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

        // Stand-in for a `trustd` that never replies: the work blocks until the
        // test releases it.
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

        // While it is stuck, a second call is refused WITHOUT starting a helper.
        let (work, seen) = witness();
        assert_eq!(GATE.run(HANG, DEADLINE, work), Err(Refused::Unanswered));
        assert!(
            never_ran(&seen),
            "a second helper started behind a stuck one"
        );
        assert_eq!(GATE.running(), 1);

        // The service answers again: the stuck helper returns, leaves the list,
        // and the next call is tried for real.
        release.send(()).expect("the stuck helper is still waiting");
        drained(&GATE);
        assert_eq!(GATE.run(HANG, HANG, || 7), Ok(7));
    }

    #[test]
    fn calls_that_arrive_after_the_suspect_age_start_no_helper_while_the_first_still_waits() {
        // The production shape: the first caller's deadline is long, and the
        // calls that follow arrive while it is still waiting. Past its suspect
        // age they must be refused at once, not each start a helper and wait
        // out a full deadline of their own.
        const SUSPECT: Duration = Duration::from_millis(100);
        static GATE: Gate = Gate::new("test-gate");

        let (release, held) = mpsc::channel::<()>();
        let first = std::thread::spawn(|| {
            GATE.run(SUSPECT, HANG, move || {
                let _ = held.recv();
            })
        });
        started(&GATE, 1);
        for _ in 0..5 {
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
        // One site's honest slowness (hashing a bundle, a long suspect age) is
        // not another site's wedged service: while it runs within its own age,
        // a short call with a short suspect age still runs and answers.
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

    /// Waits (under the hang detector) until `pid` is gone: a killed process
    /// that is not ours is reaped by its new parent, not by us.
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

    /// A child that exits at once while something it started still holds its
    /// stdout: the call returns with what the child wrote, after a short grace
    /// rather than at its (far) deadline.
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

    /// A child past its deadline is killed and REAPED: the call comes back at
    /// the deadline, and the pid it ran as is gone, not a zombie.
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
        // A zombie still answers `kill(pid, 0)`: gone means killed AND reaped.
        // (A machine too loaded to start the shell in two seconds leaves no
        // pid to look at; the timeout above still held.)
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
}
