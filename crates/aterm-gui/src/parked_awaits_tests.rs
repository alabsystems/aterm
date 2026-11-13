// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::time::{Duration, Instant};

use aterm_vitals::jobs::JobProc;

use super::{Parked, mark, timeout_ms};
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
