// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interrupts (`^C`, TERM) as a value the long verbs return through: the
//! first one is noted and every wait of the lab ([`sleep`], the frame
//! recorder's) ends with [`Stopped`], so each verb runs its own cleanup — a
//! lab it brought up comes down, a relay it held is released — and exits
//! 128 + the signal. A second interrupt ends the process at once.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The signal that stopped the run (0: none yet).
static STOPPED: AtomicI32 = AtomicI32::new(0);

fn bell() -> &'static (Mutex<()>, Condvar) {
    static B: OnceLock<(Mutex<()>, Condvar)> = OnceLock::new();
    B.get_or_init(|| (Mutex::new(()), Condvar::new()))
}

/// A run cut short by a signal; its exit status is 128 + the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped(pub i32);

/// Whether the interrupt handler is in place.
static CAUGHT: AtomicBool = AtomicBool::new(false);

/// Catch INT and TERM for the rest of the process (the first call does it).
pub fn install() {
    if CAUGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let Ok(pipe) = crate::sys::SignalPipe::install(&[libc::SIGINT, libc::SIGTERM]) else {
        return;
    };
    std::thread::spawn(move || {
        loop {
            for sig in pipe.wait() {
                if STOPPED.swap(sig, Ordering::SeqCst) != 0 {
                    std::process::exit(128 + sig);
                }
                // Under the bell's lock: a sleeper between its check and its
                // wait holds it, so the wakeup cannot fall in that gap.
                let (lock, cv) = bell();
                let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
                cv.notify_all();
            }
        }
    });
}

/// `Err` once an interrupt arrived.
///
/// # Errors
/// The run was interrupted.
pub fn check() -> Result<(), Stopped> {
    match STOPPED.load(Ordering::SeqCst) {
        0 => Ok(()),
        sig => Err(Stopped(sig)),
    }
}

/// Wait `d`, or less when an interrupt arrives.
///
/// # Errors
/// The run was interrupted.
pub fn sleep(d: Duration) -> Result<(), Stopped> {
    let end = Instant::now() + d;
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

/// Ring the bell: every waiter ([`sleep`], [`wait_until`]) looks again now.
pub fn ring() {
    let (lock, cv) = bell();
    let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
    cv.notify_all();
}

/// Wait until `done` holds or `limit` has passed: `Ok(true)` once it holds,
/// `Ok(false)` at the limit. It is asked again only when the bell rings — an
/// interrupt, or a [`ring`] from whoever makes it hold — never on a timer.
///
/// # Errors
/// The run was interrupted.
pub fn wait_until(limit: Duration, done: impl Fn() -> bool) -> Result<bool, Stopped> {
    let end = Instant::now() + limit;
    let (lock, cv) = bell();
    let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
    loop {
        check()?;
        if done() {
            return Ok(true);
        }
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

/// Wait until the wall clock reads `t` (seconds since the epoch).
///
/// # Errors
/// The run was interrupted.
pub fn until(t: f64) -> Result<(), Stopped> {
    let d = t - crate::util::now();
    if d > 0.0 {
        sleep(Duration::from_secs_f64(d))
    } else {
        check()
    }
}
