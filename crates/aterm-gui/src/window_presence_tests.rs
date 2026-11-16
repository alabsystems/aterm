// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The window's presence is written at its start, each backstop period, each wake and its quit,
//! never on a minute timer.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use super::*;

const MIN: Duration = Duration::from_secs(60);

/// THE BEAT ON A FAKE CLOCK: a write at its start, none through 59 minutes with nothing rung, one
/// at the backstop's hour, one at a wake, and the quit ends it with no write of the beat's own
/// (the quit wrote). NEGATIVE CONTROL: a [`PRESENCE_EVERY`] of 60 s writes within the 59 minutes.
#[test]
fn the_beat_writes_at_start_each_hour_and_each_wake() {
    let now = Cell::new(Duration::ZERO);
    let script = RefCell::new(vec![(90 * MIN, Heard::Woke), (100 * MIN, Heard::Quit)].into_iter());
    let writes = RefCell::new(Vec::new());
    beat(
        PRESENCE_EVERY,
        || writes.borrow_mut().push(now.get()),
        |every| {
            let due = now.get() + every;
            let mut script = script.borrow_mut();
            match script.as_slice().first().copied() {
                Some((at, heard)) if at < due => {
                    script.next();
                    now.set(at);
                    heard
                }
                _ => {
                    now.set(due);
                    Heard::Due
                }
            }
        },
    );
    let writes = writes.into_inner();
    assert_eq!(
        writes.first(),
        Some(&Duration::ZERO),
        "written at the start"
    );
    assert!(
        !writes
            .iter()
            .any(|&at| at > Duration::ZERO && at <= 59 * MIN),
        "nothing rang through 59 minutes: {writes:?}"
    );
    assert_eq!(
        writes,
        vec![Duration::ZERO, 60 * MIN, 90 * MIN],
        "the hour, the wake"
    );
}

/// A ring heard before a wait ends it at once as a wake, taken, and the quit outranks a ring: a
/// wait after the quit reads the quit whatever rang. A ring that reaches a parked wait is
/// [`ParkBell`]'s own test (`park_bell::tests::a_ring_reaches_a_parked_waiter`). NEGATIVE
/// CONTROL: the ring is taken, so the next wait does not read it again.
#[test]
fn a_ring_ends_the_wait_as_a_wake() {
    let presence = Presence::new();
    presence.ring();
    assert_eq!(presence.wait(PRESENCE_EVERY), Heard::Woke);
    assert_eq!(
        presence.wait(Duration::ZERO),
        Heard::Due,
        "the ring was taken"
    );
    presence.ring();
    presence.quit(|_| {});
    assert_eq!(
        presence.wait(PRESENCE_EVERY),
        Heard::Quit,
        "the quit outranks"
    );
}

/// THE QUIT WRITES ONCE under the state its beat started with, and every wait after it reads the
/// quit. NEGATIVE CONTROLS: a second quit writes nothing, and a presence with no beat (headless,
/// a test) writes nothing at its quit.
#[test]
fn the_quit_writes_once_and_ends_the_beat() {
    let tmp = aterm_tempfile::tempdir().expect("tmp");
    let none = Presence::new();
    none.quit(|_| panic!("no beat started, so nothing is written"));
    let presence = Presence::new();
    presence
        .state
        .set(tmp.path().to_path_buf())
        .expect("one state");
    let writes = Cell::new(0);
    presence.quit(|state| {
        write_now(state);
        writes.set(writes.get() + 1);
    });
    presence.quit(|_| writes.set(writes.get() + 1));
    assert_eq!(writes.get(), 1, "the quit writes once");
    let body = std::fs::read_to_string(tmp.path().join("window-last")).expect("written");
    assert!(
        body.trim().parse::<u64>().is_ok_and(|at| at > 0),
        "{body:?}"
    );
    assert_eq!(presence.wait(PRESENCE_EVERY), Heard::Quit);
    let beat_writes = Cell::new(0);
    beat(
        PRESENCE_EVERY,
        || beat_writes.set(beat_writes.get() + 1),
        |every| presence.wait(every),
    );
    assert_eq!(
        beat_writes.get(),
        1,
        "a beat after the quit writes its start and ends"
    );
}

/// The variable that makes this test's stand-in the window: the state directory its presence
/// writes under.
#[cfg(unix)]
const DIES_STATE: &str = "ATERM_TEST_WINDOW_DIES_STATE";

/// A TERMINATING SIGNAL WRITES THE WINDOW'S PRESENCE AS A QUIT DOES: armed as a windowed instance
/// arms it (`harness_host::arm_claims_off_as_the_window_dies` with `the_window_dies`), a SIGTERM
/// (a shut down or restart with no quit Apple event, a `launchctl bootout` logout) writes
/// `window-last` under the state its beat started with, and the process then dies of that
/// signal. A stand-in process of this test is the window. NEGATIVE CONTROL: leave
/// `window_presence::quit` out of `the_window_dies` and the stand-in dies of its SIGTERM with no
/// `window-last`.
#[cfg(unix)]
#[test]
fn a_terminating_signal_writes_the_windows_presence() {
    use std::os::unix::process::ExitStatusExt as _;
    if let Some(state) = std::env::var_os(DIES_STATE) {
        PRESENCE.state.set(PathBuf::from(state)).expect("one state");
        crate::harness_host::arm_claims_off_as_the_window_dies(crate::the_window_dies);
        let pid = libc::pid_t::try_from(std::process::id()).expect("a pid fits pid_t");
        // SAFETY: a signal to this stand-in process itself.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        // A hang detector: the signal ends this process first.
        std::thread::sleep(aterm_spec::wait::MUST_HAPPEN);
        panic!("the SIGTERM did not end the window's stand-in");
    }
    let scratch = aterm_tempfile::tempdir().expect("scratch");
    let mut stand_in = crate::test_alone::StandIn::this_test();
    stand_in.command().env(DIES_STATE, scratch.path());
    let ran = stand_in.spawn().selected();
    let status = ran.status.expect("the stand-in exited");
    assert_eq!(
        status.signal(),
        Some(libc::SIGTERM),
        "the window did not die of its SIGTERM ({status})\n{}",
        ran.stderr
    );
    let body = std::fs::read_to_string(scratch.path().join("window-last")).unwrap_or_else(|e| {
        panic!(
            "the window died of its SIGTERM with no presence written ({e})\n{}",
            ran.stderr
        )
    });
    assert!(
        body.trim().parse::<u64>().is_ok_and(|at| at > 0),
        "{body:?}"
    );
}
