// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide reaper of children nobody else waits for.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::time::Duration;

type Settled = Box<dyn FnMut() -> bool + Send>;

struct Ledger {
    waiting: Vec<Settled>,
    running: bool,
}

static LEDGER: Mutex<Ledger> = Mutex::new(Ledger {
    waiting: Vec::new(),
    running: false,
});

const FIRST_LOOK: Duration = Duration::from_millis(100);
const SLOWEST_LOOK: Duration = Duration::from_secs(2);

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
const WNOHANG: i32 = 1;

/// Wait off-thread for a child until `settled` returns `true`.
/// Polls must not block. Polling and releasing a finished callback hold no ledger lock.
/// A panicking poll retains its callback for retry; only a returned `true` releases it.
pub fn hand_off(settled: impl FnMut() -> bool + Send + 'static) {
    let settled: Settled = Box::new(settled);
    let mut ledger = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ledger.waiting.push(settled);
    if !ledger.running {
        #[cfg(not(target_arch = "wasm32"))]
        let started = std::thread::Builder::new()
            .name("aterm-reaper".into())
            .spawn(run)
            .is_ok();
        ledger.running = started;
    }
}

/// Reap `pid`, this process's own unreaped child, once it exits.
pub(crate) fn hand_off_pid(pid: i32) {
    hand_off(move || reaped_or_gone(pid));
}

fn reaped_or_gone(pid: i32) -> bool {
    let mut status = 0;
    // SAFETY: `status` is a live local `waitpid` writes; the pid names a child whose handle gave it
    // up to the reaper, so nothing else waits for it.
    let got = unsafe { waitpid(pid, &mut status, WNOHANG) };
    if got == pid {
        crate::deaths::note(pid, status);
    }
    got == pid
        || (got == -1 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted)
}

fn run() {
    let mut pause = FIRST_LOOK;
    loop {
        std::thread::sleep(pause);
        pause = match reap_ready(&LEDGER) {
            None => return,
            Some(true) => FIRST_LOOK,
            Some(false) => (pause * 2).min(SLOWEST_LOOK),
        };
    }
}

/// A panic payload can itself panic when dropped. Contain that destructor too; retain
/// its replacement payload rather than let another destructor unwind pending custody.
fn discard_panic(payload: Box<dyn std::any::Any + Send>) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(payload))) {
        std::mem::forget(payload);
    }
}

/// Poll and release one batch outside the metadata lock. If work remains, return whether
/// any waiter settled; otherwise mark this worker stopped and return `None`.
fn reap_ready(ledger: &Mutex<Ledger>) -> Option<bool> {
    let waiting = {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut ledger.waiting)
    };
    let mut pending = Vec::with_capacity(waiting.len());
    let mut progressed = false;
    for mut settled in waiting {
        match catch_unwind(AssertUnwindSafe(&mut settled)) {
            Ok(true) => {
                progressed = true;
                // Only this positively settled callback moves into the unwind boundary.
                // A destructor cannot unwind the other callbacks or the reaper worker.
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| drop(settled))) {
                    discard_panic(payload);
                }
            }
            Ok(false) => pending.push(settled),
            Err(payload) => {
                // A panic supplies no settlement evidence. Keep this same callback,
                // including its child and guards, in its original queue position.
                pending.push(settled);
                discard_panic(payload);
            }
        }
    }
    let pending = {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Keep older unfinished waiters before arrivals queued during this pass.
        pending.append(&mut ledger.waiting);
        std::mem::swap(&mut pending, &mut ledger.waiting);
        if ledger.waiting.is_empty() {
            ledger.running = false;
            false
        } else {
            true
        }
    };
    pending.then_some(progressed)
}

#[cfg(test)]
#[path = "reaper_tests.rs"]
mod tests;
