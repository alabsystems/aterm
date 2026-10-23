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
pub fn hand_off(settled: impl FnMut() -> bool + Send + 'static) {
    let mut ledger = LEDGER
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ledger.waiting.push(Box::new(settled));
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
        let mut ledger = LEDGER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = ledger.waiting.len();
        ledger.waiting.retain_mut(|settled| !settled());
        if ledger.waiting.is_empty() {
            ledger.running = false;
            return;
        }
        pause = if ledger.waiting.len() < before {
            FIRST_LOOK
        } else {
            (pause * 2).min(SLOWEST_LOOK)
        };
    }
}
