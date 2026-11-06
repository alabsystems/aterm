// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`WakeDeadline`]: a deadline its host slept past has passed, and the sleep a test
//! says its host slept reaches only the deadlines made under its seam ([`SleepSeam`]).

use super::*;

const HOUR: Duration = Duration::from_secs(3600);

/// A DEADLINE COUNTS THE TIME ITS HOST SLEPT (KI-1105-14): an hour's deadline whose host then
/// slept two hours has passed and leaves nothing, though the clock that stops in sleep leaves it
/// most of the hour; so has a sub-wait bounded by it, and a wait on it ends at its first look.
/// CONTROL: the same deadline before the sleep has not passed and leaves most of the hour, and an
/// hour's deadline made on another thread, which holds no seam, has not passed either.
#[test]
fn a_deadline_its_host_slept_past_has_passed() {
    assert!(
        aterm_uds::process::continuous_ns().is_some(),
        "this host has a clock that counts through sleep"
    );
    let _seam = SleepSeam::hold();
    let deadline = WakeDeadline::after(HOUR);
    let sub = deadline.within(2 * HOUR);
    assert!(deadline.counts_sleep());
    assert!(
        !deadline.passed(),
        "CONTROL: an hour's deadline, just taken"
    );
    assert!(deadline.remaining() > HOUR / 2, "CONTROL: most of the hour");
    assert!(
        !sub.passed() && sub.remaining() <= HOUR,
        "a sub-wait never outlasts its deadline"
    );
    assert_eq!(deadline.tick(), SLEEP_CHECK, "CONTROL: a wait on it ticks");

    host_slept(2 * HOUR);
    assert!(deadline.passed(), "the host slept past it");
    assert_eq!(deadline.remaining(), Duration::ZERO);
    assert_eq!(deadline.tick(), Duration::ZERO);
    assert!(sub.passed(), "a sub-wait bounded by it");
    assert!(
        deadline.within(HOUR).passed(),
        "a sub-wait taken after the sleep starts no new budget"
    );
    assert!(deadline.instant() <= Instant::now());
    let (_tx, rx) = mpsc::channel::<()>();
    assert_eq!(deadline.recv(&rx), Err(mpsc::RecvTimeoutError::Timeout));
    let elsewhere = std::thread::spawn(|| WakeDeadline::after(HOUR).passed())
        .join()
        .expect("the other thread");
    assert!(
        !elsewhere,
        "CONTROL: a deadline made where no seam is held counts no sleep this test said"
    );
}

/// A DEADLINE TAKES ITS SEAM TO EVERY THREAD THAT READS IT: an hour's deadline made under a seam,
/// waited on by another thread, ends that thread's wait once this test says, from a third thread,
/// that the host slept two hours. NEGATIVE CONTROL: a deadline that read its seam on the thread
/// that made it alone waited the hour out, which the one-minute hang detector reports.
#[test]
fn a_deadline_takes_its_seam_to_the_thread_that_waits_on_it() {
    let seam = SleepSeam::hold();
    let deadline = WakeDeadline::after(HOUR);
    let (_tx, rx) = mpsc::channel::<()>();
    let (said, waited) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let _ = said.send(deadline.recv(&rx));
    });
    let sleeper = seam.sleeper();
    std::thread::spawn(move || sleeper.slept(2 * HOUR))
        .join()
        .expect("the sleeper");
    assert_eq!(
        waited
            .recv_timeout(aterm_spec::wait::MUST_HAPPEN)
            .expect("the wait read its seam on its own thread"),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    waiter.join().expect("the waiter");
}

/// A WAIT ON A DEADLINE THAT HAS NOT PASSED TAKES WHAT COMES: a message queued before the wait is
/// taken, and senders that are gone end it as such, not as a timeout.
#[test]
fn a_wait_on_a_deadline_takes_its_message_or_its_senders_end() {
    let deadline = WakeDeadline::after(HOUR);
    let (tx, rx) = mpsc::channel();
    tx.send(7).expect("send");
    assert_eq!(deadline.recv(&rx), Ok(7));
    drop(tx);
    assert_eq!(
        deadline.recv(&rx),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
}

/// THE SEAM REACHES ONLY THE DEADLINES MADE UNDER IT: a deadline made before a seam was held, or
/// after it dropped, reads the host's own clocks, while one made under it keeps it.
#[test]
fn a_seam_reaches_only_the_deadlines_made_under_it() {
    let before = WakeDeadline::after(HOUR);
    let under = {
        let _seam = SleepSeam::hold();
        let under = WakeDeadline::after(HOUR);
        host_slept(2 * HOUR);
        assert!(under.passed(), "made under the seam");
        assert!(!before.passed(), "made before the seam");
        under
    };
    assert!(under.passed(), "it keeps its seam");
    assert!(
        !WakeDeadline::after(HOUR).passed(),
        "made once the seam dropped"
    );
}
