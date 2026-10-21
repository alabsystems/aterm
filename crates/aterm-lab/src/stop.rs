// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Interrupts (`^C`, TERM, the hang-up of a terminal that closed) as a value
//! the long verbs return through: the first one is noted and every wait of the
//! lab ([`sleep`], the frame recorder's) ends with [`Stopped`], so each verb
//! runs its own cleanup — a lab it brought up comes down, a relay it held is
//! released — and exits 128 + the signal. A second interrupt ends the process
//! at once, after the one act a verb said must happen even then
//! ([`at_hard_exit`]). A signal the lab was started ignoring stays ignored
//! (`nohup`: a closed terminal does not end it; [`crate::sys::SignalPipe`]).

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

/// What a second interrupt runs before the process ends ([`at_hard_exit`]).
static HARD_EXIT: Mutex<Option<fn()>> = Mutex::new(None);

/// Run `undo` when a second interrupt ends the process at once, before it
/// exits: what must be undone even then (`meta-keys` puts the person's input
/// sources back). It runs on the interrupt thread, not the main one. The last
/// call wins.
pub fn at_hard_exit(undo: fn()) {
    *HARD_EXIT.lock().unwrap_or_else(|p| p.into_inner()) = Some(undo);
}

/// Catch INT, TERM and HUP for the rest of the process (the first call does it),
/// but none it was started ignoring.
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

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// The probe's mode, set on a re-run of this test binary.
    const PROBE: &str = "ATERM_LAB_STOP_PROBE";

    /// This test binary re-run as `test` in probe `mode`: its status and stdout.
    fn probe(test: &str, mode: &str) -> (Option<i32>, String) {
        let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
        cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(PROBE, mode);
        // A hang detector, never a budget: past the probe's own minute.
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

    /// `nohup` keeps a lab alive past a closed terminal: a hang-up the process
    /// was started ignoring stays ignored, while TERM is caught as ever. The
    /// control starts with HUP at its default, which the lab catches.
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

    fn hard_exit_act() {
        let _ = crate::sys::write_all(1, b"PROBE the hard exit's act ran\n");
    }

    /// A second interrupt ends the process at once, with 128 + the signal, and
    /// runs the act a verb registered first, even while the main thread is
    /// held where it can put nothing back itself (here a plain sleep, which no
    /// interrupt cuts short). Without the second interrupt's exit the probe would
    /// sleep out its minute and exit 0.
    #[test]
    fn a_second_interrupt_runs_the_hard_exit_act_and_ends_the_run() {
        const TEST: &str =
            "stop::tests::a_second_interrupt_runs_the_hard_exit_act_and_ends_the_run";
        if std::env::var_os(PROBE).is_some() {
            // Interrupts start at their default, whatever the runner left: a
            // background job of a non-interactive shell starts with INT ignored,
            // which `install` keeps ignored (`nohup`), and the probe would sleep.
            crate::sys::start_signal(libc::SIGINT, false);
            super::install();
            super::at_hard_exit(hard_exit_act);
            let me = i32::try_from(std::process::id()).expect("pid");
            crate::sys::kill(me, libc::SIGINT).expect("the first interrupt");
            // Two pending interrupts would merge into one: the second is sent once
            // the first was noted (the lab's own wait ends with it).
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
