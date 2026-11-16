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

/// [`working`] on a scripted clock: the probe answers from `answer` on, the process runs until
/// `ends`, and `reading` is its tree's progress generation at each offset. What the wait
/// answered, and when.
fn scripted(
    window: Duration,
    guard: Duration,
    answer: Option<Duration>,
    ends: Option<Duration>,
    reading: impl Fn(Duration) -> Option<u64>,
) -> (Result<(), Unanswered>, Duration) {
    let start = Instant::now();
    let at = std::cell::Cell::new(Duration::ZERO);
    let got = working(
        window,
        guard,
        Duration::from_millis(10),
        &mut || start + at.get(),
        &mut || ends.is_none_or(|ends| at.get() < ends),
        &mut || reading(at.get()),
        &mut |gap| at.set(at.get() + gap),
        || {
            answer
                .is_some_and(|answer| at.get() >= answer)
                .then_some(())
        },
    );
    (got, at.get())
}

/// A wait on a working process is held to that process's progress, not to the wall: one that
/// keeps using CPU is waited for through any number of windows, until the probe answers. A
/// starved machine stretches the wall without bound, and the process still advances.
/// NEGATIVE CONTROLS: the same wait on a process that stops advancing ends `Stalled` one
/// window after its last advance, and one that works without the answer ends at the guard.
#[test]
fn a_working_process_is_waited_for_past_any_window_and_a_stalled_one_is_not() {
    let secs = Duration::from_secs;
    let (window, guard) = (secs(60), secs(900));
    // Half a second of CPU every tick: a generation per reading.
    let working_tree = |at: Duration| Some(u64::try_from(at.as_millis() / 500).unwrap());
    let (got, at) = scripted(window, guard, Some(secs(600)), None, working_tree);
    assert_eq!((got, at), (Ok(()), secs(600)), "ten windows of work");

    let (got, at) = scripted(window, guard, Some(secs(600)), None, |_| Some(7));
    assert_eq!(got, Err(Unanswered::Stalled));
    assert_eq!(at, window, "a hang is named after one window");

    // It worked for 200 s and then hung.
    let (got, at) = scripted(window, guard, None, None, |at| {
        Some(u64::try_from(at.min(secs(200)).as_millis() / 500).unwrap())
    });
    assert_eq!((got, at), (Err(Unanswered::Stalled), secs(260)));

    let (got, at) = scripted(window, guard, None, None, working_tree);
    assert_eq!((got, at), (Err(Unanswered::Guard), guard));
}

/// A tree that does not read, or does not compare with the last reading (its members changed),
/// is no evidence of a stall: the window opens again at the next comparable reading.
#[test]
fn an_unread_tree_is_no_stall() {
    let secs = Duration::from_secs;
    let (window, guard) = (secs(60), secs(900));
    let (got, at) = scripted(window, guard, Some(secs(300)), None, |_| None);
    assert_eq!((got, at), (Ok(()), secs(300)));
    // Unread for 100 s, then one constant generation: the window runs from the first reading.
    let (got, at) = scripted(window, guard, None, None, |at| {
        (at >= secs(100)).then_some(3)
    });
    assert_eq!((got, at), (Err(Unanswered::Stalled), secs(160)));
}

/// A process that is gone answers at once, by one more probe: what it did as it ended may be
/// the answer, and without one the wait does not sit out a window on nothing.
#[test]
fn a_process_that_ended_is_asked_once_more_and_not_waited_on() {
    let secs = Duration::from_secs;
    let (window, guard) = (secs(60), secs(900));
    let tree = |at: Duration| Some(u64::try_from(at.as_millis() / 500).unwrap());
    let (got, at) = scripted(window, guard, None, Some(secs(5)), tree);
    assert_eq!((got, at), (Err(Unanswered::Ended), secs(5)));
    // Its last act was the answer: the probe that follows its end reads it.
    let (got, at) = scripted(window, guard, Some(secs(5)), Some(secs(5)), tree);
    assert_eq!((got, at), (Ok(()), secs(5)));
}

/// The live readers: a child that sleeps uses no CPU, so a wait on it ends `Stalled`, and one
/// that exited ends `Ended`. No wall clock decides either: the window is short, and a loaded
/// machine only delays the reading that closes it.
#[test]
fn a_sleeping_child_stalls_and_an_exited_one_ended() {
    let mut sleeper = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("600")
        .spawn()
        .unwrap();
    let got = within_while_working(
        sleeper.id(),
        Duration::from_millis(400),
        WORKING_GUARD,
        Duration::from_millis(10),
        || None::<()>,
    );
    aterm_uds::deadline::kill_within(&mut sleeper, MUST_HAPPEN);
    assert_eq!(got, Err(Unanswered::Stalled));

    let mut gone = aterm_uds::spawnfd::command("/bin/sleep")
        .arg("0")
        .spawn()
        .unwrap();
    let pid = gone.id();
    let _status = gone.wait().unwrap();
    let got = within_while_working(
        pid,
        MUST_HAPPEN,
        WORKING_GUARD,
        Duration::from_millis(10),
        || None::<()>,
    );
    assert_eq!(got, Err(Unanswered::Ended));
}

/// A child whose exit has begun is not yet gone: XNU reads it exited from the moment its exit
/// begins, while its parent can reap it only once the exit finishes, so a wait for what its end
/// does (here its reaping) still waits. Each child is read the moment its input closes, and over
/// enough of them some are read mid-exit. CONTROL: with an exiting child read as gone, the one
/// more probe found nothing to reap and the wait ended `Ended` within these children (measured:
/// `native_act::source_exec`'s source-gone test failed so, one run in three).
#[cfg(unix)]
#[test]
fn a_child_mid_exit_is_waited_for_until_its_end_is_done() {
    for n in 0..300u64 {
        let mut child = aterm_uds::spawnfd::command("/bin/cat")
            .stdin(aterm_uds::spawnfd::Stdio::piped())
            .stdout(aterm_uds::spawnfd::Stdio::piped())
            .spawn()
            .unwrap();
        drop(child.stdin.take());
        // The wait's first reading of the child, a few microseconds later each time: from
        // before cat has woken to its read's end, to after its exit has finished.
        let read_at = Instant::now() + Duration::from_micros(n * 7);
        while Instant::now() < read_at {
            std::hint::spin_loop();
        }
        let got = within_while_working(
            child.id(),
            MUST_HAPPEN,
            WORKING_GUARD,
            Duration::from_millis(1),
            || child.try_wait().unwrap().map(|_| ()),
        );
        assert_eq!(got, Ok(()), "child {n}");
    }
}
