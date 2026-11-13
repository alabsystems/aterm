// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

const PROBE: &str = "ATERM_LAB_STOP_PROBE";

fn probe(test: &str, mode: &str) -> (Option<i32>, String) {
    let mut cmd = aterm_uds::spawnfd::command(std::env::current_exe().expect("test binary"));
    cmd.args(["--exact", test, "--nocapture", "--test-threads=1"])
        .env(PROBE, mode);
    let out = aterm_uds::deadline::output_until(&mut cmd, Instant::now() + Duration::from_mins(2))
        .expect("the probe ends within the hang detector");
    (
        out.status.code(),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
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
        // SIGTERM starts at its default, set here, not this test binary's: `install` leaves
        // an ignored one ignored.
        crate::sys::start_signal(libc::SIGTERM, false);
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
    const TEST: &str =
        "stop::tests::a_second_interrupt_runs_every_hard_exit_act_newest_first_and_ends_the_run";
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

/// The stop, a cleanup's grace, then a second interrupt: `held` sends it with the grace held,
/// where it cuts the cleanup short and a third ends the run; `released` once the grace is
/// dropped, where it ends the run itself. Each interrupt is a different signal from the one
/// before it, so each wait says which one ended it.
#[test]
fn a_cleanup_waits_through_the_stop_on_its_own_thread_and_a_third_interrupt_ends_the_run() {
    const TEST: &str = "stop::tests::a_cleanup_waits_through_the_stop_on_its_own_thread_and_a_third_interrupt_ends_the_run";
    if let Some(mode) = std::env::var_os(PROBE) {
        crate::sys::start_signal(libc::SIGINT, false);
        crate::sys::start_signal(libc::SIGTERM, false);
        super::install();
        super::at_hard_exit(earlier_act);
        super::at_hard_exit(later_act);
        let me = i32::try_from(std::process::id()).expect("pid");
        let signal = |sig, what| crate::sys::kill(me, sig).expect(what);
        signal(libc::SIGINT, "the stop");
        assert_eq!(
            super::sleep(Duration::from_mins(1)),
            Err(super::Stopped(libc::SIGINT)),
            "the stop is noted"
        );
        let grace = super::grace();
        assert_eq!(
            super::sleep(Duration::from_millis(30)),
            Ok(()),
            "a cleanup's wait sees through the stop that asked for it"
        );
        assert!(!super::cut(), "the stop cuts no cleanup short");
        let elsewhere = std::thread::spawn(super::check).join().expect("thread");
        assert_eq!(
            elsewhere,
            Err(super::Stopped(libc::SIGINT)),
            "another thread's wait still ends on the stop"
        );
        if mode == "released" {
            drop(grace);
            assert_eq!(
                super::check(),
                Err(super::Stopped(libc::SIGINT)),
                "once its grace is dropped, the thread's waits end on the stop again"
            );
            assert!(!super::cut(), "outside a cleanup nothing is cut");
            println!("PROBE the grace was released");
            signal(libc::SIGTERM, "the second, with no cleanup under way");
        } else {
            signal(libc::SIGTERM, "the cut");
            assert_eq!(
                super::sleep(Duration::from_mins(1)),
                Err(super::Stopped(libc::SIGTERM)),
                "the second interrupt ends the cleanup's wait and names itself"
            );
            assert!(super::cut(), "the cleanup reads its cut");
            println!("PROBE the second interrupt cut the cleanup short");
            signal(libc::SIGINT, "the third, with the cleanup still under way");
        }
        std::thread::sleep(Duration::from_mins(1));
        println!("PROBE still running");
        std::process::exit(0);
    }
    let (code, out) = probe(TEST, "held");
    assert_eq!(
        code,
        Some(128 + libc::SIGINT),
        "a third interrupt ends the run while its cleanup is under way: {out}"
    );
    assert!(
        out.contains("PROBE the second interrupt cut the cleanup short")
            && out.contains("PROBE the earlier act ran")
            && out.contains("PROBE the later act ran"),
        "{out}"
    );
    let (code, out) = probe(TEST, "released");
    assert_eq!(
        code,
        Some(128 + libc::SIGTERM),
        "a second interrupt after the cleanup's grace is dropped ends the run: {out}"
    );
    assert!(
        out.contains("PROBE the grace was released")
            && out.contains("PROBE the earlier act ran")
            && out.contains("PROBE the later act ran"),
        "{out}"
    );
}

/// A second interrupt before the run has acted on the first: with a cleanup ahead, it cuts
/// that cleanup short once it begins, and a third ends the run. Each is a signal of its own
/// and is sent once the one before it was read, since a signal sent while the same one is
/// pending is not delivered twice.
#[test]
fn a_second_interrupt_before_the_cleanup_ahead_begins_cuts_it_short() {
    const TEST: &str =
        "stop::tests::a_second_interrupt_before_the_cleanup_ahead_begins_cuts_it_short";
    if std::env::var_os(PROBE).is_some() {
        for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
            crate::sys::start_signal(sig, false);
        }
        super::install();
        super::at_hard_exit(earlier_act);
        super::at_hard_exit(later_act);
        let me = i32::try_from(std::process::id()).expect("pid");
        let ahead = super::cleanup_ahead();
        crate::sys::kill(me, libc::SIGINT).expect("the stop");
        assert_eq!(
            super::sleep(Duration::from_mins(1)),
            Err(super::Stopped(libc::SIGINT)),
            "the stop is noted"
        );
        crate::sys::kill(me, libc::SIGTERM).expect("the second");
        // Read before the cleanup begins: it has cut the cleanup ahead, or ended the run.
        let hang = Instant::now() + Duration::from_mins(1);
        while super::CUT.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            assert!(Instant::now() < hang, "the second interrupt was never read");
            std::thread::sleep(Duration::from_millis(5));
        }
        let grace = super::grace();
        assert_eq!(
            super::sleep(Duration::from_mins(1)),
            Err(super::Stopped(libc::SIGTERM)),
            "the cleanup begins already cut short"
        );
        println!("PROBE the cleanup ahead was cut short");
        drop(grace);
        drop(ahead);
        crate::sys::kill(me, libc::SIGHUP).expect("the third");
        std::thread::sleep(Duration::from_mins(1));
        println!("PROBE still running");
        std::process::exit(0);
    }
    let (code, out) = probe(TEST, "twice");
    assert_eq!(
        code,
        Some(128 + libc::SIGHUP),
        "the second interrupt cut the cleanup ahead short and the third ended the run: {out}"
    );
    assert!(
        out.contains("PROBE the cleanup ahead was cut short")
            && out.contains("PROBE the earlier act ran")
            && out.contains("PROBE the later act ran"),
        "{out}"
    );
}
