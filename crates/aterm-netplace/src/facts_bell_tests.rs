// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::io::{ErrorKind, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, BorrowedFd};

use super::*;
use std::cell::Cell;

fn sense() -> Live {
    Live {
        routes: None,
        path: Arc::new(PathState {
            bell: Some(Bell::new().expect("notification pair")),
            ..PathState::default()
        }),
        #[cfg(target_os = "macos")]
        _monitor: None,
    }
}

fn readable(fd: BorrowedFd<'_>) -> bool {
    let mut fd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one live pollfd, polled without waiting.
    let ready = unsafe { libc::poll(&raw mut fd, 1, 0) };
    assert!(ready >= 0);
    ready != 0
}

fn tell(path: &PathState) {
    // The monitor holds the reading while it sets the change flag and rings.
    let _held = path.reading.lock().unwrap();
    path.tell();
}

#[test]
fn stop_wakes_real_condvar_and_registered_poll_without_a_network_change() {
    for mode in 0..3 {
        let sense = sense();
        let path = Arc::clone(&sense.path);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let wake = sense.stop_wake().unwrap();
        let (entered, receiving) = std::sync::mpsc::channel();
        let (answer, result) = std::sync::mpsc::channel();
        let hang = Duration::from_secs(60);
        let outside_hang_detector = Duration::from_secs(3600);
        let worker = std::thread::spawn(move || {
            let entered = std::cell::RefCell::new(Some(entered));
            let mut wait_path = |held: MutexGuard<'_, Option<PathFacts>>, left| {
                if let Some(entered) = entered.borrow_mut().take() {
                    entered.send(()).unwrap();
                }
                path.wait_reading(held, left, &stop);
            };
            let changed = if mode == 0 {
                wait_on_path(
                    &path,
                    &stop,
                    Instant::now() + outside_hang_detector,
                    Instant::now,
                    wait_path,
                )
            } else {
                let routes = aterm_uds::network::RouteChanges::open().unwrap();
                wait_on_routes(
                    &path,
                    &stop,
                    outside_hang_detector,
                    Instant::now,
                    |left| {
                        if mode == 1 {
                            Woke::Failed
                        } else {
                            // Registration uses the actual bell FD; a stop immediately before the
                            // kernel poll must be as observable as one arriving after it began.
                            poll_both_by(&routes, path.bell_fd(), left, |fds, millis| {
                                // This control concerns our private bell, not ambient route
                                // traffic. poll ignores a negative descriptor.
                                fds[0].fd = -1;
                                if let Some(entered) = entered.borrow_mut().take() {
                                    entered.send(()).unwrap();
                                }
                                // SAFETY: this array holds the live bell descriptor and an ignored
                                // negative descriptor; poll receives its actual two-element length.
                                if unsafe { libc::poll(fds.as_mut_ptr(), 2, millis) } < 0 {
                                    Err(std::io::Error::last_os_error())
                                } else {
                                    Ok(())
                                }
                            })
                        }
                    },
                    &mut wait_path,
                )
            };
            answer.send(changed).unwrap();
        });
        let ready = receiving.recv_timeout(hang);
        stopping.store(true, Ordering::SeqCst);
        wake.ring();
        let changed = result.recv_timeout(hang);
        // Rescue is cleanup only: it cannot qualify the automatic stop observation above.
        if changed.is_err() {
            tell(&sense.path);
        }
        worker.join().unwrap();
        ready.unwrap();
        assert!(!changed.unwrap(), "stop is not a network observation");
        assert!(!sense.path.changed.load(Ordering::SeqCst));
    }
}

#[test]
fn a_closed_bell_is_a_failed_wait_not_an_endless_notification() {
    let routes = aterm_uds::network::RouteChanges::open().expect("network event socket");
    let (hear, ring) = std::io::pipe().unwrap();
    drop(ring);
    aterm_uds::deadline::once_released(|| {
        if poll_both(&routes, Some(hear.as_fd()), Duration::ZERO) == Woke::Failed {
            Ok(())
        } else {
            Err(std::io::ErrorKind::WouldBlock.into())
        }
    });
}

#[test]
fn path_changes_consumed_before_poll_leave_no_stale_readiness() {
    let sense = sense();
    for _ in 0..32 {
        tell(&sense.path);
        tell(&sense.path);
        assert!(sense.wait(Duration::ZERO, &AtomicBool::new(false)));
        assert!(
            !readable(sense.path.bell_fd().unwrap()),
            "acknowledged path changes must not leave a stale notification"
        );
        assert!(!sense.wait(Duration::ZERO, &AtomicBool::new(false)));
    }
}

#[test]
fn a_full_bell_never_blocks_the_monitor_and_drains_before_rearming() {
    let bell = Bell::new().unwrap();
    for fd in [bell.hear.as_fd(), bell.ring.as_fd()] {
        // SAFETY: F_GETFL only reads the flags of this live descriptor.
        let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_ne!(flags & libc::O_NONBLOCK, 0, "notification I/O cannot block");
    }
    let fill = |buf: &[u8]| loop {
        match (&bell.ring).write(buf) {
            Ok(n) => assert!(n > 0),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) => panic!("fill notification transport: {e}"),
        }
    };
    fill(&[1; 4096]);
    fill(&[1]);
    bell.ring();
    assert!(readable(bell.hear.as_fd()));
    bell.hush();
    assert!(!readable(bell.hear.as_fd()));
    bell.ring();
    assert!(readable(bell.hear.as_fd()));
    bell.hush();
    assert!(!readable(bell.hear.as_fd()));
}

#[test]
fn draining_the_bell_keeps_the_path_change_latched() {
    let sense = sense();
    tell(&sense.path);
    sense.path.hush();
    assert!(!readable(sense.path.bell_fd().unwrap()));
    let (answer, result) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        answer
            .send(sense.wait(Duration::ZERO, &AtomicBool::new(false)))
            .unwrap();
    });
    assert!(
        result.recv_timeout(Duration::from_secs(60)).unwrap(),
        "draining readiness must not clear the separately latched path change"
    );
    waiter.join().unwrap();
}

/// The waker starts only while the wait backend holds the reading lock. It
/// cannot latch its change until the condition variable releases that lock.
/// Both entry points use the production condition-variable backend.
fn notified_wait(failed_route: bool) {
    let path = sense().path;
    let wake_path = Arc::clone(&path);
    let (entered, waiting) = std::sync::mpsc::channel();
    let hang = Duration::from_secs(60);
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let waker = scope.spawn(move || {
            waiting
                .recv_timeout(hang)
                .expect("the wait backend entered");
            tell(&wake_path);
        });
        let mut entered = Some(entered);
        let wait_path = |held: MutexGuard<'_, Option<PathFacts>>, left| {
            if let Some(entered) = entered.take() {
                entered.send(()).expect("the waker is listening");
            }
            path.wait_reading(held, left, &stop);
        };
        let polls = Cell::new(0);
        let changed = if failed_route {
            wait_on_routes(
                &path,
                &stop,
                hang,
                Instant::now,
                |_| {
                    polls.set(polls.get() + 1);
                    assert_eq!(polls.get(), 1, "a failed route is not polled again");
                    Woke::Failed
                },
                wait_path,
            )
        } else {
            wait_on_path(&path, &stop, Instant::now() + hang, Instant::now, wait_path)
        };
        waker.join().expect("the waker ended");
        assert!(
            changed,
            "the notification, rather than expiry, ended the wait"
        );
        assert_eq!(polls.get(), usize::from(failed_route));
        assert!(!path.changed.load(Ordering::SeqCst));
        assert!(!readable(path.bell_fd().unwrap()));
    });
}

#[test]
fn a_path_notification_wakes_the_failed_route_fallback() {
    notified_wait(true);
}

#[test]
fn a_path_notification_wakes_the_path_only_wait() {
    notified_wait(false);
}

/// A LIVE NUDGE IS LATCHED: a watched file's change told while no one waits (the keeper is
/// reading) ends the next wait on the route socket and the bell at once, answered as a change,
/// and is consumed by it, so the wait after that sleeps again.
/// NEGATIVE CONTROL: a nudge that only rings the bell (`bell.ring()` without `tell`'s change
/// flag) is hushed as a stray ring and the wait sleeps on: "the nudge ended the wait" fails at
/// the hang detector.
#[test]
fn a_nudge_told_before_the_wait_ends_it_as_a_change() {
    let sense = sense();
    let wake = sense.stop_wake().unwrap();
    let stop = AtomicBool::new(false);
    let routes = aterm_uds::network::RouteChanges::open().unwrap();
    wake.nudge();
    let changed = wait_on_routes(
        &sense.path,
        &stop,
        Duration::from_secs(60),
        Instant::now,
        |left| poll_both(&routes, sense.path.bell_fd(), left),
        |held, left| sense.path.wait_reading(held, left, &stop),
    );
    assert!(changed, "the nudge ended the wait");
    let again = wait_on_routes(
        &sense.path,
        &stop,
        Duration::ZERO,
        Instant::now,
        |left| poll_both(&routes, sense.path.bell_fd(), left),
        |held, left| sense.path.wait_reading(held, left, &stop),
    );
    assert!(!again, "the nudge was consumed");
}
