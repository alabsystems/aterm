// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::sync::mpsc;
use std::time::Instant;
const HANG_DETECTOR: Duration = Duration::from_secs(60);

#[test]
fn loopback_resolves_with_the_native_adapter() {
    let addresses = resolve("localhost", HANG_DETECTOR).unwrap();
    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(IpAddr::is_loopback));
}

#[test]
fn a_lookup_behind_one_still_running_starts_no_second_helper() {
    static FLIGHTS: Flights = Flights::new();
    let helpers = Arc::new(AtomicUsize::new(0));
    let spawn = |helpers: &Arc<AtomicUsize>| {
        let helpers = Arc::clone(helpers);
        move |lookup: Box<dyn FnOnce() + Send>| {
            helpers.fetch_add(1, Ordering::SeqCst);
            std::thread::Builder::new().spawn(lookup).map(drop)
        }
    };
    let at = |port| SocketAddr::from(([192, 168, 64, 17], port));
    let (release, held) = mpsc::channel::<()>();
    let (host, port) = ("held.local", 7484);
    let error = resolve_within_using(
        host,
        port,
        Duration::from_millis(50),
        move |_, port| {
            let _ = held.recv_timeout(HANG_DETECTOR);
            Ok(std::iter::once(at(port)))
        },
        spawn(&helpers),
        &FLIGHTS,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    let error = resolve_within_using(
        host,
        port,
        Duration::from_millis(50),
        |_, _| -> io::Result<std::iter::Empty<SocketAddr>> {
            unreachable!("a lookup behind one under way runs no resolver")
        },
        spawn(&helpers),
        &FLIGHTS,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
    assert_eq!(helpers.load(Ordering::SeqCst), 1, "one helper for the name");

    // One more waits; the held helper answers; the waiter gets its answer.
    let waiter = std::thread::spawn({
        let spawn = spawn(&helpers);
        move || {
            resolve_within_using(
                host,
                port,
                HANG_DETECTOR,
                |_, _| -> io::Result<std::iter::Empty<SocketAddr>> {
                    unreachable!("the waiter runs no resolver")
                },
                spawn,
                &FLIGHTS,
            )
        }
    });
    let started = Instant::now();
    while FLIGHTS.waiting(host, port) == 0 {
        assert!(started.elapsed() < HANG_DETECTOR, "the waiter never waited");
        aterm_uds::deadline::poll_yield();
    }
    release.send(()).unwrap();
    assert_eq!(waiter.join().unwrap().unwrap(), vec![at(port)]);
    assert_eq!(helpers.load(Ordering::SeqCst), 1);

    // Answered, the name is asked afresh; another port is another lookup.
    for port in [port, port + 1] {
        let addrs = resolve_within_using(
            host,
            port,
            HANG_DETECTOR,
            move |_, port| Ok(std::iter::once(at(port))),
            spawn(&helpers),
            &FLIGHTS,
        )
        .unwrap();
        assert_eq!(addrs, vec![at(port)]);
    }
    assert_eq!(helpers.load(Ordering::SeqCst), 3);
}

/// A resolver that unwinds lands its flight as having ended without an answer,
/// so the next lookup of that name starts a helper of its own rather than wait
/// on one nobody will answer.
#[test]
fn a_resolver_that_unwinds_leaves_no_flight_under_way() {
    static FLIGHTS: Flights = Flights::new();
    let spawn = |lookup: Box<dyn FnOnce() + Send>| {
        std::thread::Builder::new()
            .spawn(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(lookup));
            })
            .map(drop)
    };
    let error = resolve_within_using(
        "unwinds.invalid",
        443,
        HANG_DETECTOR,
        |_, _| -> io::Result<std::iter::Empty<SocketAddr>> { panic!("the resolver unwinds") },
        spawn,
        &FLIGHTS,
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Other, "{error}");
    assert_eq!(error.to_string(), "the resolver ended without an answer");
    let addr = SocketAddr::from(([127, 0, 0, 1], 443));
    let addrs = resolve_within_using(
        "unwinds.invalid",
        443,
        HANG_DETECTOR,
        move |_, _| Ok(std::iter::once(addr)),
        spawn,
        &FLIGHTS,
    )
    .unwrap();
    assert_eq!(addrs, vec![addr]);
}
