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

    struct Dropped(Arc<AtomicBool>);

    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    struct PanickingPayload(Arc<AtomicBool>);

    impl Drop for PanickingPayload {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            panic!("injected panic payload destructor");
        }
    }

    #[test]
    fn a_panicking_poll_preserves_pending_custody_and_retries() {
        let ledger = ledger();
        let first_dropped = Arc::new(AtomicBool::new(false));
        let second_dropped = Arc::new(AtomicBool::new(false));
        let payload_dropped = Arc::new(AtomicBool::new(false));
        let release_second = Arc::new(AtomicBool::new(false));
        let first = Dropped(Arc::clone(&first_dropped));
        let second = Dropped(Arc::clone(&second_dropped));
        let payload = Arc::clone(&payload_dropped);
        let release = Arc::clone(&release_second);
        let order = Arc::new(Mutex::new(Vec::new()));
        let first_order = Arc::clone(&order);
        let second_order = Arc::clone(&order);
        let mut first_poll = true;
        let queue = Arc::clone(&ledger);
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            let _keep = &first;
            first_order.lock().unwrap().push("first");
            assert!(queue.try_lock().is_ok(), "poll holds no ledger lock");
            if first_poll {
                first_poll = false;
                std::panic::panic_any(PanickingPayload(Arc::clone(&payload)));
            }
            true
        }));
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            let _keep = &second;
            second_order.lock().unwrap().push("second");
            release.load(Ordering::Acquire)
        }));

        assert_eq!(reap_ready(&ledger), Some(false));
        assert!(payload_dropped.load(Ordering::Acquire));
        assert!(!first_dropped.load(Ordering::Acquire));
        assert!(!second_dropped.load(Ordering::Acquire));
        assert_eq!(ledger.lock().unwrap().waiting.len(), 2);
        assert!(ledger.lock().unwrap().running);
        assert_eq!(*order.lock().unwrap(), ["first", "second"]);

        assert_eq!(reap_ready(&ledger), Some(true));
        assert!(first_dropped.load(Ordering::Acquire));
        assert!(!second_dropped.load(Ordering::Acquire));
        assert_eq!(ledger.lock().unwrap().waiting.len(), 1);
        assert_eq!(
            *order.lock().unwrap(),
            ["first", "second", "first", "second"]
        );

        release_second.store(true, Ordering::Release);
        assert_eq!(reap_ready(&ledger), None);
        assert!(second_dropped.load(Ordering::Acquire));
        assert!(!ledger.lock().unwrap().running);
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

    struct PanickingCustody(ReturnCustody);

    impl Drop for PanickingCustody {
        fn drop(&mut self) {
            assert!(
                self.0.ledger.try_lock().is_ok(),
                "drop holds no ledger lock"
            );
            panic!("injected settled callback destructor");
        }
    }

    #[test]
    fn a_panicking_settled_destructor_preserves_pending_and_returned_custody() {
        let ledger = ledger();
        let returned = Arc::new(AtomicBool::new(false));
        let other_dropped = Arc::new(AtomicBool::new(false));
        let release_other = Arc::new(AtomicBool::new(false));
        let custody = PanickingCustody(ReturnCustody {
            ledger: Arc::clone(&ledger),
            returned: Arc::clone(&returned),
        });
        let other = Dropped(Arc::clone(&other_dropped));
        let release = Arc::clone(&release_other);
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            let _keep = &custody;
            true
        }));
        ledger.lock().unwrap().waiting.push(Box::new(move || {
            let _keep = &other;
            release.load(Ordering::Acquire)
        }));

        assert_eq!(reap_ready(&ledger), Some(true));
        assert!(returned.load(Ordering::Acquire));
        assert!(!other_dropped.load(Ordering::Acquire));
        assert_eq!(ledger.lock().unwrap().waiting.len(), 2);
        assert!(ledger.lock().unwrap().running);

        release_other.store(true, Ordering::Release);
        assert_eq!(reap_ready(&ledger), None);
        assert!(other_dropped.load(Ordering::Acquire));
        assert!(!ledger.lock().unwrap().running);
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
