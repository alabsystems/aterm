// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Process-wide reaper of children nobody else waits for.

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

/// Poll and release one batch outside the metadata lock. If work remains, return whether
/// any waiter settled; otherwise mark this worker stopped and return `None`.
fn reap_ready(ledger: &Mutex<Ledger>) -> Option<bool> {
    let mut waiting = {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut ledger.waiting)
    };
    let before = waiting.len();
    waiting.retain_mut(|settled| !settled());
    let progressed = waiting.len() < before;
    let pending = {
        let mut ledger = ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Keep older unfinished waiters before arrivals queued during this pass.
        waiting.append(&mut ledger.waiting);
        std::mem::swap(&mut waiting, &mut ledger.waiting);
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
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn ledger() -> Arc<Mutex<Ledger>> {
        Arc::new(Mutex::new(Ledger {
            waiting: Vec::new(),
            running: true,
        }))
    }

    #[test]
    fn a_poll_can_enqueue_more_work_without_losing_older_waiters() {
        let ledger = ledger();
        let queue = Arc::clone(&ledger);
        let order = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&order);
        let mut first = true;
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            if first {
                first = false;
                let recorded = Arc::clone(&recorded);
                queue
                    .try_lock()
                    .expect("poll holds no ledger lock")
                    .waiting
                    .push(Box::new(move || {
                        recorded.lock().unwrap().push("new");
                        true
                    }));
                false
            } else {
                recorded.lock().unwrap().push("old");
                true
            }
        }));
        assert_eq!(reap_ready(&ledger), Some(false));
        assert_eq!(ledger.lock().unwrap().waiting.len(), 2);
        assert_eq!(reap_ready(&ledger), None);
        assert_eq!(*order.lock().unwrap(), ["old", "new"]);
        assert!(!ledger.lock().unwrap().running);
    }

    struct ReturnCustody {
        ledger: Arc<Mutex<Ledger>>,
        returned: Arc<AtomicBool>,
    }

    impl Drop for ReturnCustody {
        fn drop(&mut self) {
            if let Ok(mut ledger) = self.ledger.try_lock() {
                ledger.waiting.push(Box::new(|| true));
                self.returned.store(true, Ordering::Release);
            }
        }
    }

    #[test]
    fn releasing_a_finished_callback_can_return_more_custody() {
        let ledger = ledger();
        let returned = Arc::new(AtomicBool::new(false));
        let custody = ReturnCustody {
            ledger: Arc::clone(&ledger),
            returned: Arc::clone(&returned),
        };
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            let _keep = &custody;
            true
        }));
        assert_eq!(reap_ready(&ledger), Some(true));
        assert!(returned.load(Ordering::Acquire));
        assert_eq!(reap_ready(&ledger), None);
        assert!(!ledger.lock().unwrap().running);
    }
}
