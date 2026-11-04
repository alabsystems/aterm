// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The macOS listener scan against a real kernel, in a test process of its own whose tests run
//! one at a time ([`ONE_AT_A_TIME`]): one holds this process's spawn lock, which every other
//! test's spawns and kept-out sockets would wait on, and counts every thread of this process that
//! waits to hold it.
#![cfg(target_vendor = "apple")]

use std::os::fd::AsRawFd as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use aterm_uds::spawn_lock::seam;

/// A hang detector, never a latency budget.
const HANG: Duration = Duration::from_secs(60);

/// Held by each test for its whole run, so no other test's thread waits on the spawn lock while
/// one counts its waiters.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// A broker's listener bound at its private sibling `<dir>/b.sock.<pid>.0` and linked into place
/// at `<dir>/b.sock`, as astream binds (`Broker::bind_unix`): the listener and the path.
fn broker_in(dir: &std::path::Path) -> (UnixListener, std::path::PathBuf) {
    let path = dir.join("b.sock");
    let private = dir.join(format!("b.sock.{}.0", std::process::id()));
    let broker = UnixListener::bind(&private).expect("bind the private name");
    std::fs::hard_link(&private, &path).expect("link into place");
    std::fs::remove_file(&private).expect("remove the private name");
    (broker, path)
}

/// A broker's listener, bound at its private sibling and linked into place as astream binds
/// (`Broker::bind_unix`), is named by a scan whose connection's `socket(2)` waited on the spawn
/// lock: a creation holds the lock until the scan's connection is seen waiting to hold it, then
/// lets it go. NEGATIVE CONTROL: make the connection's `socket(2)` outside the spawn lock, and
/// the scan answers before any thread waits for the lock.
#[test]
fn a_scan_whose_connection_waits_on_the_spawn_lock_still_names_the_broker() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let me = std::process::id();
    let dir = aterm_tempfile::Builder::new()
        .prefix("stall-")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let (broker, path) = broker_in(dir.path());

    let (held_tx, held) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let creation = std::thread::spawn(move || {
        aterm_uds::kept_out(|| {
            held_tx
                .send(std::thread::current().id())
                .expect("the test waits for the hold");
            let _ = released.recv_timeout(HANG);
            UnixStream::pair()
        })
    });
    let holder = held
        .recv_timeout(HANG)
        .expect("the creation holds the spawn lock");
    let scan = {
        let path = path.clone();
        // The broker is this test's own and serves: its connection waits a hang detector, never
        // the scan's 10 s.
        std::thread::spawn(move || {
            aterm_uds::holders::scan_listener_holders_within(&path, &[], HANG)
        })
    };
    let started = Instant::now();
    while !(seam::exclusive_waiters() > 0 && seam::exclusive_holder() == Some(holder)) {
        if scan.is_finished() {
            panic!(
                "the scan answered before its connection waited on the spawn lock: {:?}",
                scan.join().expect("the scan")
            );
        }
        assert!(
            started.elapsed() < HANG,
            "the scan's connection never waited on the spawn lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    release
        .send(())
        .expect("the creation waits for its release");
    drop(
        creation
            .join()
            .expect("the creation")
            .expect("a kept-out pair"),
    );
    let found = scan.join().expect("the scan").expect("the listener scan");
    assert!(
        found.holds(me),
        "a scan whose connection waited on the spawn lock: {found:?}"
    );
    drop(broker);
}

/// A broker whose accept queue is full is refused (`ECONNREFUSED`) as a socket file whose
/// listener is gone is, but a datagram probe of its path finds a stream socket bound there, so a
/// scan that finds the broker's live row at its sibling and is refused at its path answers
/// unknown (`ConnectionRefused`), never a scan naming no holder. With one queued connection
/// accepted, room for the scan's own, the same scan names the broker. The queue is filled by
/// count, not by time. NEGATIVE CONTROL: read `ECONNREFUSED` as no listener, and the scan of the
/// wedged broker names no holder.
#[test]
fn a_broker_whose_accept_queue_is_full_is_unknown_never_absent() {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
    let dir = aterm_tempfile::Builder::new()
        .prefix("full-")
        .tempdir_in("/tmp")
        .expect("a scratch folder short enough for sun_path");
    let (broker, path) = broker_in(dir.path());
    // SAFETY: the descriptor is the broker's listening socket, open while `broker` lives.
    let listened = unsafe { libc::listen(broker.as_raw_fd(), 1) };
    assert_eq!(listened, 0, "{}", std::io::Error::last_os_error());
    let mut queued = Vec::new();
    let refused = loop {
        match aterm_uds::connect::until(&path, Instant::now() + HANG) {
            Ok(stream) => queued.push(stream),
            Err(e) => break e,
        }
        assert!(queued.len() < 64, "the accept queue never filled");
    };
    assert_eq!(
        refused.raw_os_error(),
        Some(libc::ECONNREFUSED),
        "{refused}"
    );
    let e = aterm_uds::holders::scan_listener_holders(&path, &[])
        .expect_err("a scan refused by a full accept queue");
    assert_eq!(e.kind(), std::io::ErrorKind::ConnectionRefused, "{e}");
    drop(broker.accept().expect("a queued connection"));
    let found =
        aterm_uds::holders::scan_listener_holders(&path, &[]).expect("the scan with room queued");
    assert!(found.holds(std::process::id()), "{found:?}");
    drop((queued, broker));
}
