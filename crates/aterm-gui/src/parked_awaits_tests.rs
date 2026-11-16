// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_vitals::jobs::JobProc;

use super::{Parked, elsewhere_on, is_elsewhere, mark, timeout_ms};
#[cfg(unix)]
use super::{birth_of, enter_in};

fn member(pid: u32, start: Option<u64>) -> JobProc {
    JobProc {
        pid,
        start,
        ..JobProc::default()
    }
}

/// A parked await marks its own client, by pid and birth, only while it is held: once the server
/// lets it go the same process is no aterm wait. NEGATIVE CONTROLS: another birth at that pid (a
/// reused pid), a member whose birth is unread, and an ordinary process are never marked.
#[test]
fn a_parked_await_marks_its_own_client_only_while_it_is_held() {
    let parked = Parked::new();
    let now = Instant::now();
    let mut procs = vec![
        member(4200, Some(77)),
        member(4200, Some(78)),
        member(4200, None),
        member(4300, Some(77)),
    ];
    let held = parked.hold(4200, 77, now + Duration::from_secs(60));
    mark(&mut procs, &parked, now);
    let marked: Vec<bool> = procs.iter().map(|p| p.aterm_wait).collect();
    assert_eq!(marked, [true, false, false, false]);
    drop(held);
    mark(&mut procs, &parked, now);
    assert!(
        procs.iter().all(|p| !p.aterm_wait),
        "an await the server let go marks nothing"
    );
}

/// An await is parked until its deadline, never past: a server wait that outran what it was asked
/// for vouches for nothing.
#[test]
fn an_await_past_its_deadline_no_longer_counts() {
    let parked = Parked::new();
    let now = Instant::now();
    let until = now + Duration::from_secs(90);
    let _timed = parked.hold(1, 10, until);
    assert!(parked.holds(1, Some(10), now + Duration::from_secs(89)));
    assert!(!parked.holds(1, Some(10), until));
}

/// Clamped to the wire's longest wait, as each handler clamps it, so a 4 h ask parks for 10 min
/// and the client asks again.
#[test]
fn the_timeout_is_read_as_the_await_handlers_read_it() {
    assert_eq!(
        timeout_ms("task @s-1,@s-2 since=0 timeout=14400000"),
        Some(aterm_control::wire::WAIT_MAX_MS)
    );
    assert_eq!(timeout_ms("idle 500 timeout 3000"), Some(3000));
    assert_eq!(timeout_ms("wake label=x"), None);
    assert_eq!(timeout_ms("task @s-1 timeout=soon"), None);
}

/// The server parks an `await` for the kernel's peer of the connection it serves: over a real
/// socket pair this process is the peer, parked by its own birth until the guard drops.
/// NEGATIVE CONTROLS: another verb, and a connection not marked direct, park nothing.
#[cfg(unix)]
#[test]
fn the_server_parks_an_await_for_the_kernels_peer_of_its_connection() {
    use std::os::fd::AsRawFd;
    let (server, _client) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    let _fd = aterm_control::hangup::ServingFd::enter(server.as_raw_fd());
    let parked = Parked::new();
    let me = std::process::id();
    let birth = birth_of(me);
    assert!(birth.is_some(), "this process's birth reads");
    let now = Instant::now();

    assert!(
        enter_in(&parked, "await", "task @s-1 since=0").is_none(),
        "a connection not marked direct names no peer"
    );
    let _direct = crate::control::DirectPeer::enter();
    assert!(enter_in(&parked, "status", "").is_none());
    assert!(!parked.holds(me, birth, now));

    let held = enter_in(&parked, "await", "task @s-1 since=0 timeout=14400000")
        .expect("an await on a direct connection parks");
    assert!(parked.holds(me, birth, Instant::now()));
    let max = Duration::from_millis(aterm_control::wire::WAIT_MAX_MS);
    assert!(
        !parked.holds(me, birth, Instant::now() + max + super::GRACE),
        "a 4 h ask parks no longer than the 10 min its handler waits, and the grace"
    );
    drop(held);
    assert!(!parked.holds(me, birth, Instant::now()));

    let open = enter_in(&parked, "await", "wake").expect("an await with no timeout parks");
    assert!(
        !parked.holds(me, birth, Instant::now() + max + super::GRACE),
        "one with no timeout parks no longer than the longest any wait verb waits"
    );
    drop(open);
}

/// `await elsewhere` holds until its client hangs up, the vouch over: a client still there keeps it
/// past every poll until its timeout, `OK elsewhere status=timeout`. NEGATIVE CONTROLS: another
/// await's words are not this form, and a client that hangs up ends it at once, never at its
/// timeout. Off a serving lane no bell arms, and the lane here polls at HANGUP_POLL.
#[test]
fn an_await_elsewhere_holds_until_its_client_hangs_up_or_its_timeout() {
    use std::sync::atomic::{AtomicU32, Ordering};
    static ASKED: AtomicU32 = AtomicU32::new(0);
    fn polled(left: Duration) -> Duration {
        left.min(aterm_control::hangup::HANGUP_POLL)
    }
    fn there() -> bool {
        false
    }
    fn gone_at_the_third_ask() -> bool {
        ASKED.fetch_add(1, Ordering::SeqCst) >= 2
    }
    let lane = |hung_up: fn() -> bool| crate::park::Lane {
        slice: polled,
        hung_up,
        bell: None,
    };
    assert!(is_elsewhere("elsewhere timeout=600000"));
    assert!(is_elsewhere("elsewhere"));
    assert!(!is_elsewhere("task @s-1 timeout=600000"));
    assert!(!is_elsewhere(""));
    let started = Instant::now();
    let waited = elsewhere_on("elsewhere timeout=450", lane(there));
    assert_eq!(waited, "OK elsewhere status=timeout\n");
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "held to its timeout through every poll"
    );
    let gone = elsewhere_on("elsewhere timeout=600000", lane(gone_at_the_third_ask));
    assert_eq!(gone, crate::control::HUNG_UP_REPLY);
    assert_eq!(
        ASKED.load(Ordering::SeqCst),
        3,
        "it ends at the poll that reads the hangup"
    );
}

/// The `await elsewhere` the hang-up bell rings: its caller is a socket pair's client, its lane a
/// thread the test starts. Each wait for something that must happen is the one-minute hang
/// detector, and no count depends on time.
#[cfg(unix)]
mod belled {
    use std::os::fd::AsRawFd as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use aterm_control::hangup::{HANGUP_POLL, ServingFd, caller_hung_up, hangup_park};
    use aterm_spec::wait::{MUST_HAPPEN, until_within};
    use aterm_uds::CtlStream;
    use aterm_uds::hangup::Bell;

    use aterm_control::park::BellLane;

    use crate::park::Lane;
    use crate::parked_awaits::{elsewhere, elsewhere_on};

    #[track_caller]
    fn until(what: &str, mut seen: impl FnMut() -> bool) {
        until_within(MUST_HAPPEN, what, || seen().then_some(()));
    }

    /// AN `await elsewhere` WHOSE CALLER CLOSES WAKES ON THE RING, though its timeout is ten
    /// minutes: armed on the process's bell, it waits once for all that is left (its bell lane is
    /// asked once, its sliced lane never), and the caller closes only once that arm stands, so
    /// only the ring, which waits in the slot if the wait has not begun, can end it within the
    /// hang detector.
    /// NEGATIVE CONTROL: arming the bell to ring a slot that is not the wait's leaves it waiting
    /// past the hang detector.
    #[test]
    fn an_await_elsewhere_whose_caller_closes_wakes_on_the_ring() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(_left: Duration) -> Duration {
            panic!("an armed await on a live bell is never sliced")
        }
        let (client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let lane = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            elsewhere_on(
                "elsewhere timeout=600000",
                Lane {
                    slice: sliced,
                    hung_up: caller_hung_up,
                    bell: Some(BellLane {
                        bell: Bell::process,
                        slice: belled,
                    }),
                },
            )
        });
        until("the await to arm the bell on its connection", || {
            Bell::process().armed_for_test(served)
        });
        drop(client);
        until("the parked await to hear its caller hang up", || {
            lane.is_finished()
        });
        assert_eq!(
            lane.join().expect("the lane"),
            crate::control::HUNG_UP_REPLY
        );
        assert_eq!(BELLED.load(Ordering::SeqCst), 1, "one wait, to the ring");
    }

    /// AN UNARMED `await elsewhere` IS SLICED AT HANGUP_POLL: on a bell whose thread has ended it
    /// cannot arm, so it waits by its lane's slices, each no longer than HANGUP_POLL, and asks
    /// after its caller between them, ending at the lane's second ask, its bell lane never asked.
    /// NEGATIVE CONTROL: a park that takes its bell lane whether or not the arm stands waits once
    /// for ten minutes and is reported at the hang detector.
    #[test]
    fn an_unarmed_await_elsewhere_is_sliced_at_hangup_poll() {
        static BELLED: AtomicUsize = AtomicUsize::new(0);
        static SLICED: AtomicUsize = AtomicUsize::new(0);
        static LONGEST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        fn belled(left: Duration) -> Duration {
            BELLED.fetch_add(1, Ordering::SeqCst);
            left
        }
        fn sliced(left: Duration) -> Duration {
            SLICED.fetch_add(1, Ordering::SeqCst);
            let slice = hangup_park(left);
            let nanos = u64::try_from(slice.as_nanos()).unwrap_or(u64::MAX);
            LONGEST.fetch_max(nanos, Ordering::SeqCst);
            slice
        }
        fn gone_after_two_slices() -> bool {
            SLICED.load(Ordering::SeqCst) >= 2
        }
        let (_client, server) = CtlStream::pair().expect("socket pair");
        let lane = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            elsewhere_on(
                "elsewhere timeout=600000",
                Lane {
                    slice: sliced,
                    hung_up: gone_after_two_slices,
                    bell: Some(BellLane {
                        bell: Bell::ended_for_test,
                        slice: belled,
                    }),
                },
            )
        });
        until("the unarmed await to end", || lane.is_finished());
        assert_eq!(
            lane.join().expect("the lane"),
            crate::control::HUNG_UP_REPLY
        );
        assert_eq!(
            (BELLED.load(Ordering::SeqCst), SLICED.load(Ordering::SeqCst)),
            (0, 2),
            "a bell that has ended arms nothing, and the await is sliced"
        );
        let longest = Duration::from_nanos(LONGEST.load(Ordering::SeqCst));
        assert!(
            longest <= HANGUP_POLL,
            "a slice of {longest:?} outran HANGUP_POLL"
        );
    }

    /// AN `await elsewhere` ARMS THE PROCESS'S HANG-UP BELL ON ITS SHIPPED LANE: served as the
    /// dispatcher serves it ([`elsewhere`], no lane of the test's own), it arms `Bell::process()`
    /// on its connection while it waits, and a caller that then closes ends it, though its
    /// timeout is ten minutes. The arm is the sign: a shipped lane that armed no bell would hear
    /// the close too, by its HANGUP_POLL slices, so only the arm tells the bell's lane from the
    /// sliced one.
    /// NEGATIVE CONTROL: an [`elsewhere`] whose lane carries no bell lane never arms, and the hang
    /// detector fires before the close.
    #[test]
    fn an_await_elsewhere_arms_the_process_bell_on_its_shipped_lane() {
        let (client, server) = CtlStream::pair().expect("socket pair");
        let served = server.as_raw_fd();
        let lane = std::thread::spawn(move || {
            let _serving = ServingFd::enter(server.as_raw_fd());
            elsewhere("elsewhere timeout=600000")
        });
        until(
            "the await to arm the process bell on its connection",
            || Bell::process().armed_for_test(served),
        );
        drop(client);
        until("the parked await to hear its caller hang up", || {
            lane.is_finished()
        });
        assert_eq!(
            lane.join().expect("the lane"),
            crate::control::HUNG_UP_REPLY
        );
    }
}
