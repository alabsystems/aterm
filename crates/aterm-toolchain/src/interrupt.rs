// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! AN INTERRUPT ENDS A WAIT (`docs/DESIGN-adaptive-build-2026-10-24.md`, WAIT-INTERRUPT).
//!
//! A run that waits for room in the capacity ledger or for a named lock has started nothing, so
//! SIGINT, SIGTERM, SIGHUP or SIGQUIT then should end it at once as NOT RUN, its place in the
//! queue given up. SIGQUIT (a terminal's `Ctrl-\`) is among them because its default action dumps
//! core: a build's front waiting for its claim would otherwise die of it as a crash of the aterm
//! image, when it had started nothing to crash. The tools catch interrupts their own ways
//! (verify ends its stage groups and dies of the signal, the release steps and the lab set a flag
//! their work looks at, a bench keeps the default), and none of those reaches a loop that sleeps between looks, nor leaves its queue
//! entry behind it. So a wait holds a [`Watch`]: while one lives, the process's own handling of
//! those four signals is set aside for this module's, which notes the first that comes; the
//! wait looks at it between short naps ([`Watch::nap`]); and the last watch to end puts the
//! process's own handling back. A signal the process ignores (a background job's SIGINT) stays
//! ignored. The wait that ends on the signal takes it ([`Watch::take`]), and its caller ends as
//! NOT RUN; one no wait took (it came as the wait ended otherwise, or after a wait took one and
//! before its watch ended) is sent again once the process's own handling is back, so no
//! interrupt is lost and a second one ends the process as its own handling would.

use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The longest a wait naps between two looks at the interrupt: it ends well within a second.
pub const NAP: Duration = Duration::from_millis(50);

/// The first interrupt no wait has taken ([`Watch::take`]) since the outermost live watch began;
/// 0 for none.
static CAUGHT: AtomicI32 = AtomicI32::new(0);

/// The live watches, and what the outermost set aside.
static HELD: Mutex<Held> = Mutex::new(Held {
    depth: 0,
    #[cfg(unix)]
    saved: Vec::new(),
});

struct Held {
    depth: usize,
    /// Each signal this module catches, with the action it set aside.
    #[cfg(unix)]
    saved: Vec<(libc::c_int, libc::sigaction)>,
}

/// The signals a wait ends on.
#[cfg(unix)]
const SIGNALS: [libc::c_int; 4] = [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    // SAFETY: installed with SA_SIGINFO, so the kernel supplies a live packet.
    unsafe { aterm_uds::signal_origin::capture(sig, info) };
    let _ = CAUGHT.compare_exchange(0, sig, Ordering::SeqCst, Ordering::SeqCst);
}

/// `SIGTERM`, `SIGINT`, `SIGHUP`, `SIGQUIT`, else `signal N`.
#[must_use]
pub fn name(sig: i32) -> String {
    match sig {
        1 => "SIGHUP".to_owned(),
        2 => "SIGINT".to_owned(),
        3 => "SIGQUIT".to_owned(),
        15 => "SIGTERM".to_owned(),
        n => format!("signal {n}"),
    }
}

/// A wait in progress: SIGINT, SIGTERM, SIGHUP and SIGQUIT, each the process does not ignore,
/// are noted for it ([`Watch::caught`]) instead of reaching the process's own handling, until
/// the last live watch ends.
#[derive(Debug)]
pub struct Watch(());

impl Watch {
    /// Begin a wait. The outermost watch sets aside the process's handling of each signal it
    /// does not ignore, as it reads at this moment; the last one to end puts it back, unless a
    /// handler was installed over this module's meanwhile, which stays.
    #[must_use]
    pub fn enter() -> Watch {
        #[cfg(unix)]
        if let Err(why) = aterm_uds::signal_origin::start() {
            eprintln!("signal provenance unavailable: {why}");
        }
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        if held.depth == 0 {
            CAUGHT.store(0, Ordering::SeqCst);
            #[cfg(unix)]
            for sig in SIGNALS {
                // SAFETY: a zeroed `sigaction` is a valid value; with a null new action the call
                // only reads the current one into `old`, which outlives it.
                let mut old: libc::sigaction = unsafe { std::mem::zeroed() };
                if unsafe { libc::sigaction(sig, std::ptr::null(), &raw mut old) } != 0
                    || old.sa_sigaction == libc::SIG_IGN
                {
                    continue;
                }
                // SAFETY: as above for the zeroed value; the SA_SIGINFO handler lives for
                // the whole program and uses only atomics and a nonblocking pipe write.
                // `sa_mask` is emptied before the kernel reads it.
                let installed = unsafe {
                    let mut new: libc::sigaction = std::mem::zeroed();
                    new.sa_sigaction = on_signal as *const () as libc::sighandler_t;
                    new.sa_flags = libc::SA_RESTART | libc::SA_SIGINFO;
                    libc::sigemptyset(&raw mut new.sa_mask);
                    libc::sigaction(sig, &raw const new, std::ptr::null_mut()) == 0
                };
                if installed {
                    held.saved.push((sig, old));
                }
            }
        }
        held.depth += 1;
        Watch(())
    }

    /// The interrupt that came since the outermost live watch began and no wait took, if one did.
    #[must_use]
    pub fn caught(&self) -> Option<i32> {
        match CAUGHT.load(Ordering::SeqCst) {
            0 => None,
            sig => {
                #[cfg(unix)]
                let _ = aterm_uds::signal_origin::records();
                Some(sig)
            }
        }
    }

    /// [`Self::caught`], taken by this wait, which ends on it: it is not sent again when the
    /// last watch ends. One that comes after it is noted afresh, and sent again then.
    #[must_use]
    pub fn take(&self) -> Option<i32> {
        match CAUGHT.swap(0, Ordering::SeqCst) {
            0 => None,
            sig => {
                #[cfg(unix)]
                let _ = aterm_uds::signal_origin::records();
                Some(sig)
            }
        }
    }

    /// Sleep `d`, ending early once an interrupt came or `stop` says to, looking every [`NAP`]:
    /// `true` when it ended early.
    pub fn nap(&self, d: Duration, stop: &dyn Fn() -> bool) -> bool {
        let end = Instant::now().checked_add(d);
        loop {
            if self.caught().is_some() || stop() {
                return true;
            }
            let now = Instant::now();
            if let Some(end) = end {
                if now >= end {
                    return false;
                }
                std::thread::sleep((end - now).min(NAP));
            } else {
                std::thread::sleep(NAP);
            }
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
        held.depth = held.depth.saturating_sub(1);
        if held.depth > 0 {
            return;
        }
        #[cfg(unix)]
        for (sig, old) in held.saved.drain(..).rev() {
            // SAFETY: a zeroed `sigaction` is a valid value, and a null new action only reads the
            // current one into `now`; `old` is the action `sigaction` itself read for `sig` when
            // the watch began.
            unsafe {
                let mut now: libc::sigaction = std::mem::zeroed();
                let ours = on_signal as *const () as libc::sighandler_t;
                if libc::sigaction(sig, std::ptr::null(), &raw mut now) == 0
                    && now.sa_sigaction == ours
                {
                    libc::sigaction(sig, &raw const old, std::ptr::null_mut());
                }
            }
        }
        let sig = CAUGHT.swap(0, Ordering::SeqCst);
        if sig != 0 {
            #[cfg(unix)]
            let _ = aterm_uds::signal_origin::records();
            resend(sig);
        }
    }
}

/// Send `sig` to this process again, now that its own handling is back.
#[cfg(unix)]
fn resend(sig: i32) {
    // SAFETY: `kill` takes two integers and touches no memory of ours; the pid is this process's.
    unsafe {
        libc::kill(libc::getpid(), sig);
    }
}

#[cfg(not(unix))]
fn resend(_sig: i32) {}

#[cfg(all(test, unix))]
#[path = "interrupt_tests.rs"]
mod tests;
