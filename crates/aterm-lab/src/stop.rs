// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Signal interruption handling and hard-exit cleanup for long lab verbs.

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

static STOPPED: AtomicI32 = AtomicI32::new(0);

fn bell() -> &'static (Mutex<()>, Condvar) {
    static B: OnceLock<(Mutex<()>, Condvar)> = OnceLock::new();
    B.get_or_init(|| (Mutex::new(()), Condvar::new()))
}

/// A run cut short by a signal; its exit status is 128 + the signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stopped(pub i32);

static CAUGHT: AtomicBool = AtomicBool::new(false);

static HARD_EXIT: Mutex<Option<fn()>> = Mutex::new(None);

/// Run `undo` when a second interrupt ends the process at once, before it exits.
pub fn at_hard_exit(undo: fn()) {
    *HARD_EXIT.lock().unwrap_or_else(|p| p.into_inner()) = Some(undo);
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
                if STOPPED.swap(sig, Ordering::SeqCst) != 0 {
                    let undo = *HARD_EXIT.lock().unwrap_or_else(|p| p.into_inner());
                    if let Some(undo) = undo {
                        undo();
                    }
                    std::process::exit(128 + sig);
                }
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

/// Wait `d`, or less when `done` is set (and [`ring`] rung) or an interrupt arrives: `Ok(true)` when `done` ended it.
///
/// # Errors
/// The run was interrupted before `done` was set.
pub fn sleep_unless(d: Duration, done: &AtomicBool) -> Result<bool, Stopped> {
    let end = Instant::now() + d;
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
        sleep(Duration::from_secs_f64(d))
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
    }

    fn hard_exit_act() {
        let _ = crate::sys::write_all(1, b"PROBE the hard exit's act ran\n");
    }

    #[test]
    fn a_second_interrupt_runs_the_hard_exit_act_and_ends_the_run() {
        const TEST: &str =
            "stop::tests::a_second_interrupt_runs_the_hard_exit_act_and_ends_the_run";
        if std::env::var_os(PROBE).is_some() {
            crate::sys::start_signal(libc::SIGINT, false);
            super::install();
            super::at_hard_exit(hard_exit_act);
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
        assert!(out.contains("PROBE the hard exit's act ran"), "{out}");
    }
}
