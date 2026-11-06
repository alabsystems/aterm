// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal interruption handling and hard-exit cleanup for long lab verbs.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

static STOPPED: AtomicI32 = AtomicI32::new(0);

/// The cleanups under way on every thread ([`Grace`]).
static GRACES: AtomicUsize = AtomicUsize::new(0);

/// The second interrupt, which cut the run's cleanups short ([`Grace`]); 0 while none has.
static CUT: AtomicI32 = AtomicI32::new(0);

thread_local! {
    /// The [`Grace`]s this thread holds.
    static HELD: Cell<usize> = const { Cell::new(0) };
}

fn bell() -> &'static (Mutex<()>, Condvar) {
    static B: OnceLock<(Mutex<()>, Condvar)> = OnceLock::new();
    B.get_or_init(|| (Mutex::new(()), Condvar::new()))
}

/// A run cut short by a signal; its exit status is 128 + the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped(pub i32);

static CAUGHT: AtomicBool = AtomicBool::new(false);

/// Every act registered with [`at_hard_exit`], in the order registered.
static HARD_EXIT: Mutex<Vec<fn()>> = Mutex::new(Vec::new());

/// Run `undo` when an interrupt ends the process at once, before it exits: a second one, or a
/// third while a cleanup is under way ([`Grace`]). Every act
/// registered is held, none replaces another: they run newest first, as `atexit` runs its
/// functions, each once per registration, so a caller that registers once per run registers
/// once (behind a `Once` or a `OnceLock`'s first `set`).
pub fn at_hard_exit(undo: fn()) {
    HARD_EXIT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(undo);
}

/// Run every act [`at_hard_exit`] holds, newest first, outside its lock (an act that registers
/// another cannot deadlock the end).
fn run_hard_exit_acts() {
    let acts = HARD_EXIT.lock().unwrap_or_else(|p| p.into_inner()).clone();
    for undo in acts.into_iter().rev() {
        undo();
    }
}

/// Installs signal handlers for unignored `INT`, `TERM`, and `HUP` signals.
pub fn install() {
    if CAUGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(pipe) = crate::sys::SignalPipe::install(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP])
    else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            for sig in pipe.wait() {
                let again = STOPPED.swap(sig, Ordering::SeqCst) != 0;
                // A second interrupt while a cleanup is under way cuts that cleanup's waits
                // short, so it can still say what it left; any other, or a third, ends the run.
                let cuts = again
                    && GRACES.load(Ordering::SeqCst) > 0
                    && CUT
                        .compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok();
                if again && !cuts {
                    run_hard_exit_acts();
                    std::process::exit(128 + sig);
                }
                let (lock, cv) = bell();
                let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
                cv.notify_all();
            }
        }
    });
}

/// Set by [`note`]: an interrupt ended one of this run's waits.
static WAIT_ENDED: AtomicBool = AtomicBool::new(false);

/// Note `sig` as an interrupt of this run, as the handler [`install`] sets would: a wait for
/// capacity or a lock that ended on it ([`aterm_toolchain::interrupt`]) hands it on, so the run's
/// own interrupt path ([`check`]) ends what it started, and a second interrupt ends it at once
/// (or cuts a cleanup under way short, [`Grace`]). The run then ends NOT RUN ([`ended_a_wait`]).
pub fn note(sig: i32) {
    WAIT_ENDED.store(true, Ordering::SeqCst);
    let _ = STOPPED.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
    ring();
}

/// Whether an interrupt ended one of this run's waits ([`note`]): what waited started nothing,
/// so the run is NOT RUN ([`status`]).
pub fn ended_a_wait() -> bool {
    WAIT_ENDED.load(Ordering::SeqCst)
}

/// The run's exit status for a verb that ended with `code`: NOT RUN once an interrupt ended one
/// of its waits ([`ended_a_wait`]), whatever its verb made of that (a refusal, a window that
/// never started, rows cut short, the interrupt's own 128 + signal), unless its own verdict was
/// a failure it found (`found`: it returned 1), which stays.
pub fn status(code: i32, found: bool) -> i32 {
    if ended_a_wait() && !found {
        crate::linux::NOT_RUN
    } else {
        code
    }
}

/// `Err` once an interrupt arrived; on a thread holding a [`Grace`], once a second one cut its
/// cleanup short.
///
/// # Errors
/// The run was interrupted (in a cleanup, its cleanup was cut short).
pub fn check() -> Result<(), Stopped> {
    let stopped = if HELD.with(Cell::get) > 0 {
        &CUT
    } else {
        &STOPPED
    };
    match stopped.load(Ordering::SeqCst) {
        0 => Ok(()),
        sig => Err(Stopped(sig)),
    }
}

/// A cleanup under way on this thread, from [`grace`] until it drops. The interrupt that stops
/// the run asks for this cleanup, so its waits ([`check`], [`sleep`], [`sleep_unless`]) see
/// through that interrupt and end at their own bounds: a stop hastens the cleanup instead of
/// voiding it. A second interrupt cuts them short instead of ending the run at once, and a third
/// ends the run.
#[must_use = "a cleanup's waits see through the stop only while its Grace is held"]
pub struct Grace {
    /// It counts the waits of the thread that took it, so it stays on that thread.
    _thread: PhantomData<*const ()>,
}

/// Start a cleanup on this thread ([`Grace`]).
pub fn grace() -> Grace {
    HELD.with(|held| held.set(held.get() + 1));
    GRACES.fetch_add(1, Ordering::SeqCst);
    Grace {
        _thread: PhantomData,
    }
}

impl Drop for Grace {
    fn drop(&mut self) {
        GRACES.fetch_sub(1, Ordering::SeqCst);
        HELD.with(|held| held.set(held.get() - 1));
    }
}

/// Compute `Instant::now() + d`, saturating at one year if `d` would overflow.
pub(crate) fn deadline_after(d: Duration) -> Instant {
    let now = Instant::now();
    now.checked_add(d)
        .unwrap_or_else(|| now + Duration::from_secs(365 * 24 * 3600))
}

/// Wait `d`, or less when an interrupt arrives ([`check`]).
///
/// # Errors
/// The run was interrupted (in a cleanup, its cleanup was cut short).
pub fn sleep(d: Duration) -> Result<(), Stopped> {
    let end = deadline_after(d);
    let (lock, cv) = bell();
    let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        check()?;
        let now = Instant::now();
        if now >= end {
            return Ok(());
        }
        g = cv
            .wait_timeout(g, end - now)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
}

/// Wait `d`, or less when `done` is set (and [`ring`] rung) or an interrupt arrives ([`check`]): `Ok(true)` when `done` ended it.
///
/// # Errors
/// The run was interrupted (in a cleanup, its cleanup was cut short) before `done` was set.
pub fn sleep_unless(d: Duration, done: &AtomicBool) -> Result<bool, Stopped> {
    let end = deadline_after(d);
    let (lock, cv) = bell();
    let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        if done.load(Ordering::SeqCst) {
            return Ok(true);
        }
        check()?;
        let now = Instant::now();
        if now >= end {
            return Ok(false);
        }
        g = cv
            .wait_timeout(g, end - now)
            .unwrap_or_else(|p| p.into_inner())
            .0;
    }
}

/// Wake every [`sleep`] and [`sleep_unless`] to look again.
pub fn ring() {
    let (lock, cv) = bell();
    let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
    cv.notify_all();
}

/// Wait until the wall clock reads `t` (seconds since the epoch).
///
/// # Errors
/// The run was interrupted.
pub fn until(t: f64) -> Result<(), Stopped> {
    let d = t - crate::util::now();
    if d > 0.0 {
        sleep(Duration::try_from_secs_f64(d).unwrap_or(Duration::MAX))
    } else {
        check()
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    const PROBE: &str = "ATERM_LAB_STOP_PROBE";

    fn probe(test: &str, mode: &str) -> (Option<i32>, String) {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(PROBE, mode);
        let out =
            aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_mins(2))
                .expect("the probe ends within the hang detector");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }

    fn named(h: libc::sighandler_t) -> &'static str {
        match h {
            libc::SIG_IGN => "ignored",
            libc::SIG_DFL => "default",
            _ => "caught",
        }
    }

    #[test]
    fn a_hang_up_started_ignored_stays_ignored() {
        const TEST: &str = "stop::tests::a_hang_up_started_ignored_stays_ignored";
        if let Some(mode) = std::env::var_os(PROBE) {
            crate::sys::start_signal(libc::SIGHUP, mode == "ignored");
            super::install();
            let hup = crate::sys::disposition(libc::SIGHUP).expect("HUP");
            let term = crate::sys::disposition(libc::SIGTERM).expect("TERM");
            println!("PROBE hup={} term={}", named(hup), named(term));
            std::process::exit(0);
        }
        let (code, out) = probe(TEST, "ignored");
        assert_eq!(code, Some(0), "{out}");
        assert!(out.contains("PROBE hup=ignored term=caught"), "{out}");
        let (code, out) = probe(TEST, "control");
        assert_eq!(code, Some(0), "{out}");
        assert!(out.contains("PROBE hup=caught term=caught"), "{out}");
    }

    #[test]
    fn a_ring_after_done_ends_the_wait_and_the_deadline_ends_it_otherwise() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let done = Arc::new(AtomicBool::new(false));
        let t = Instant::now();
        assert_eq!(
            super::sleep_unless(Duration::from_millis(30), &done),
            Ok(false)
        );
        assert!(t.elapsed() >= Duration::from_millis(30));
        let (tx, rx) = std::sync::mpsc::channel();
        {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let _ = tx.send(super::sleep_unless(Duration::from_mins(10), &done));
            });
        }
        std::thread::sleep(Duration::from_millis(20));
        done.store(true, Ordering::SeqCst);
        super::ring();
        assert_eq!(
            rx.recv_timeout(Duration::from_mins(1)),
            Ok(Ok(true)),
            "the ring woke the waiter within the one-minute hang detector (its own deadline is 10 minutes)"
        );
        assert_eq!(super::sleep_unless(Duration::MAX, &done), Ok(true));
        assert_eq!(super::until(f64::NAN), Ok(()));
        assert_eq!(super::until(0.0), Ok(()));
    }

    fn earlier_act() {
        let _ = crate::sys::write_all(1, b"PROBE the earlier act ran\n");
    }

    fn later_act() {
        let _ = crate::sys::write_all(1, b"PROBE the later act ran\n");
    }

    /// A SECOND INTERRUPT RUNS EVERY ACT REGISTERED, NEWEST FIRST, AND ENDS THE RUN (128 + the
    /// signal): two acts registered, both run, the later one first. CONTROL: with an act that
    /// replaces the one before it, as `at_hard_exit` held one, the earlier act never runs.
    #[test]
    fn a_second_interrupt_runs_every_hard_exit_act_newest_first_and_ends_the_run() {
        const TEST: &str = "stop::tests::a_second_interrupt_runs_every_hard_exit_act_newest_first_and_ends_the_run";
        if std::env::var_os(PROBE).is_some() {
            crate::sys::start_signal(libc::SIGINT, false);
            super::install();
            super::at_hard_exit(earlier_act);
            super::at_hard_exit(later_act);
            let me = i32::try_from(std::process::id()).expect("pid");
            crate::sys::kill(me, libc::SIGINT).expect("the first interrupt");
            assert_eq!(
                super::sleep(Duration::from_mins(1)),
                Err(super::Stopped(libc::SIGINT)),
                "the first interrupt is noted"
            );
            crate::sys::kill(me, libc::SIGINT).expect("the second interrupt");
            std::thread::sleep(Duration::from_mins(1));
            println!("PROBE still running");
            std::process::exit(0);
        }
        let (code, out) = probe(TEST, "twice");
        assert_eq!(code, Some(128 + libc::SIGINT), "{out}");
        let at = |line: &str| out.find(line);
        let (earlier, later) = (
            at("PROBE the earlier act ran"),
            at("PROBE the later act ran"),
        );
        assert!(earlier.is_some() && later.is_some(), "every act ran: {out}");
        assert!(later < earlier, "the later act ran first: {out}");
        assert!(!out.contains("PROBE still running"), "{out}");
    }

    #[test]
    fn a_cleanup_waits_through_the_stop_on_its_own_thread_and_a_third_interrupt_ends_the_run() {
        const TEST: &str = "stop::tests::a_cleanup_waits_through_the_stop_on_its_own_thread_and_a_third_interrupt_ends_the_run";
        if std::env::var_os(PROBE).is_some() {
            crate::sys::start_signal(libc::SIGINT, false);
            super::install();
            super::at_hard_exit(earlier_act);
            super::at_hard_exit(later_act);
            let me = i32::try_from(std::process::id()).expect("pid");
            let interrupt = |what| crate::sys::kill(me, libc::SIGINT).expect(what);
            interrupt("the stop");
            assert_eq!(
                super::sleep(Duration::from_mins(1)),
                Err(super::Stopped(libc::SIGINT))
            );
            let grace = super::grace();
            assert_eq!(super::sleep(Duration::from_millis(30)), Ok(()));
            let elsewhere = std::thread::spawn(super::check).join().expect("thread");
            assert_eq!(elsewhere, Err(super::Stopped(libc::SIGINT)));
            interrupt("the cut");
            assert_eq!(
                super::sleep(Duration::from_mins(1)),
                Err(super::Stopped(libc::SIGINT))
            );
            drop(grace);
            println!("PROBE the second interrupt cut the cleanup short");
            interrupt("the end");
            std::thread::sleep(Duration::from_mins(1));
            println!("PROBE still running");
            std::process::exit(0);
        }
        let (code, out) = probe(TEST, "thrice");
        assert_eq!(code, Some(128 + libc::SIGINT), "{out}");
        assert!(
            out.contains("PROBE the second interrupt cut the cleanup short")
                && out.contains("PROBE the earlier act ran")
                && out.contains("PROBE the later act ran"),
            "{out}"
        );
    }
}
