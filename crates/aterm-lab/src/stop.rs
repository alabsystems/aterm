// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal interruption handling and hard-exit cleanup for long lab verbs.

use std::cell::Cell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

static STOPPED: AtomicI32 = AtomicI32::new(0);

/// The cleanups under way on every thread ([`Grace`]), and those a stop of the run will start
/// ([`Ahead`]).
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
/// third while a cleanup is under way or ahead ([`Grace`], [`Ahead`]). Every act registered is
/// held, none replaces another: they run newest first, as `atexit` runs its functions, each once
/// per registration, so a caller that registers once per run registers once (behind a `Once` or
/// a `OnceLock`'s first `set`).
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
                // A second interrupt while a cleanup is under way or ahead cuts that cleanup's
                // waits short, so it can still say what it left; any other, or a third, ends the
                // run.
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

/// Whether a second interrupt has cut this thread's cleanup short ([`Grace`]); never outside one.
/// A command a cleanup runs ends on this, where the stop that asked for the cleanup must not end
/// it.
pub fn cut() -> bool {
    HELD.with(Cell::get) > 0 && CUT.load(Ordering::SeqCst) != 0
}

/// A cleanup the run's stop will start ([`Grace`]), from [`cleanup_ahead`] until it drops: a
/// second interrupt cuts that cleanup short even when it comes before the cleanup has begun (two
/// signals at once, or a second while the run has yet to act on the first), instead of ending the
/// run at once. It sees through no stop itself; only a [`Grace`] does.
#[must_use = "a second interrupt cuts the cleanup ahead short only while this is held"]
pub struct Ahead {
    _noted: (),
}

/// Note a cleanup the run's stop will start ([`Ahead`]).
pub fn cleanup_ahead() -> Ahead {
    GRACES.fetch_add(1, Ordering::SeqCst);
    Ahead { _noted: () }
}

impl Drop for Ahead {
    fn drop(&mut self) {
        GRACES.fetch_sub(1, Ordering::SeqCst);
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
#[path = "stop_tests.rs"]
mod tests;
