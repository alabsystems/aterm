// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use super::*;

/// A warm-up killed at its bound while the program check holds it says so in one line
/// naming its pid, and one that is not held says nothing. (`sleep 30` outlives the 50 ms
/// bound by construction.) The reader is asked while the run still runs: the fake answers
/// only while the program check's own reader still reads the pid, which on macOS a killed
/// and reaped child no longer does.
#[test]
fn a_warm_up_killed_while_held_says_so_once() {
    let sleep = Path::new("/bin/sleep");
    let bound = Duration::from_millis(50);
    let asked = std::cell::Cell::new(None);
    let mut lines = Vec::new();
    run_once_judged(
        sleep,
        &["30"],
        bound,
        &|pid| {
            asked.set(Some(pid));
            aterm_uds::process::at_program_check(i32::try_from(pid).unwrap_or(0)).map(|_| true)
        },
        &mut |line| lines.push(line),
    );
    let pid = asked.get().expect("the killed run was asked about");
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(
        lines[0].starts_with("first_exec: macOS's program check held /bin/sleep")
            && lines[0].contains(&format!("(pid {pid})")),
        "{lines:?}"
    );
    let mut quiet = Vec::new();
    run_once_judged(sleep, &["30"], bound, &|_| Some(false), &mut |line| {
        quiet.push(line);
    });
    assert_eq!(quiet, Vec::<String>::new());
    // `Duration::MAX` budget does not overflow `Instant::now() + budget`, and a gap larger
    // than `budget` is clamped to the remaining deadline.
    assert_eq!(within(Duration::MAX, Duration::ZERO, || Some(7)), Some(7));
    let began = Instant::now();
    assert_eq!(
        within(
            Duration::from_millis(10),
            Duration::from_secs(30),
            || None::<()>
        ),
        None
    );
    assert!(began.elapsed() < MUST_HAPPEN);
}

/// A peer whose end another descriptor still holds reads open, and the wait returns once
/// that last copy lets go, 300 ms on: an assertion at once fails there.
#[test]
fn the_peers_close_is_waited_for_until_its_last_copy_lets_go() {
    let (near, far) = aterm_uds::kept_out(aterm_uds::CtlStream::pair).unwrap();
    let copy = aterm_uds::kept_out(|| far.try_clone()).unwrap();
    drop(far);
    assert!(
        !aterm_uds::hangup::peer_closed(&near),
        "the copy holds the peer open"
    );
    let holder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        drop(copy);
    });
    until_peer_closed(&near);
    holder.join().unwrap();
}
