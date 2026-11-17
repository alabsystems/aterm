// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::cell::RefCell;
use std::net::{Ipv4Addr, SocketAddr};

use super::*;

fn key(port: u16) -> Key {
    Key::new(
        Target::Addr(SocketAddr::from((Ipv4Addr::LOCALHOST, port))),
        port,
        ConnectOptions::default(),
    )
}

/// A destination's failures back off across callers and error text changes. Only a down
/// transition and its eventual recovery are logged. NEGATIVE CONTROLS: another destination
/// can dial during that wait, a due retry dials again, and a fresh outage is said again.
#[test]
fn a_dead_peer_backs_off_and_each_change_of_state_is_said_once() {
    let peers = Peers::default();
    let start = Instant::now();
    let lines = RefCell::new(Vec::new());
    let log = |line: std::fmt::Arguments<'_>| lines.borrow_mut().push(line.to_string());
    let refused = (libc::ECONNREFUSED, "connection refused".to_owned());
    let timed_out = (libc::ETIMEDOUT, "readiness deadline".to_owned());
    assert_eq!(
        peers.connect(key(1), || start, || Err::<(), _>(refused.clone()), log),
        Err(refused.clone())
    );
    assert_eq!(lines.borrow().len(), 1);
    assert_eq!(
        peers.connect(key(1), || start, || panic!("not due"), log),
        Err::<(), _>(refused)
    );
    peers.connect(key(2), || start, || Ok(()), log).unwrap();
    assert_eq!(
        peers.connect(
            key(1),
            || start + FIRST_RETRY,
            || Err::<(), _>(timed_out.clone()),
            log
        ),
        Err(timed_out.clone())
    );
    assert_eq!(
        lines.borrow().len(),
        1,
        "a different failure is still the same outage"
    );
    assert_eq!(
        peers.connect(
            key(1),
            || start + FIRST_RETRY * 2,
            || panic!("second retry backs off for two seconds"),
            log
        ),
        Err::<(), _>(timed_out)
    );
    peers
        .connect(key(1), || start + FIRST_RETRY * 3, || Ok(()), log)
        .unwrap();
    assert_eq!(lines.borrow().len(), 2);
    assert!(lines.borrow()[1].ends_with("reachable"));
    let _ = peers.connect(
        key(1),
        || start + FIRST_RETRY * 3,
        || Err::<(), _>((libc::EHOSTUNREACH, "no route".into())),
        log,
    );
    assert_eq!(lines.borrow().len(), 3);
}

/// Cancellation, local policy and descriptor failures say nothing about a destination.
/// NEGATIVE CONTROL: a real refused connect establishes the retry clock at the same instant.
#[test]
fn only_network_failure_establishes_a_peers_retry_clock() {
    let peers = Peers::default();
    let now = Instant::now();
    for code in [
        libc::EINTR,
        libc::EIO,
        libc::EINVAL,
        libc::EACCES,
        libc::EMFILE,
    ] {
        let _ = peers.connect(
            key(1),
            || now,
            || Err::<(), _>((code, "not peer state".into())),
            |_| panic!("not a peer transition"),
        );
        peers
            .connect(
                key(1),
                || now,
                || Ok(()),
                |_| panic!("no outage to recover"),
            )
            .unwrap();
    }
    let _ = peers.connect(
        key(1),
        || now,
        || Err::<(), _>((libc::ECONNREFUSED, "down".into())),
        |_| {},
    );
    assert!(
        peers
            .connect::<()>(key(1), || now, || panic!("a refused peer waits"), |_| {})
            .is_err_and(|(code, _)| code == libc::ECONNREFUSED)
    );
}

/// A concurrent caller never duplicates an in-flight probe of a dead peer; healthy callers
/// remain concurrent. Routing options identify a
/// different path, which can be tried now. The table is bounded despite destination churn,
/// and backoff saturates. NEGATIVE CONTROL: a different path still calls its dial closure.
#[test]
fn in_flight_probes_and_destination_churn_keep_bounded_state() {
    let peers = Peers::default();
    let now = Instant::now();
    peers
        .connect(
            key(1),
            || now,
            || {
                peers.connect(key(1), || now, || Ok(()), |_| {}).unwrap();
                let mut another_path = key(1);
                another_path.options.bound_if = Some(2);
                peers
                    .connect(another_path, || now, || Ok(()), |_| {})
                    .unwrap();
                Ok(())
            },
            |_| {},
        )
        .unwrap();
    // A healthy parallel dial can succeed while an older dial is still running. That
    // older dial's eventual timeout must not replace the newer success with an outage.
    let _ = peers.connect(
        key(1),
        || now,
        || {
            peers.connect(key(1), || now, || Ok(()), |_| {}).unwrap();
            Err::<(), _>((libc::ETIMEDOUT, "an older dial's late failure".into()))
        },
        |_| panic!("a late failure cannot overwrite newer success"),
    );
    let down = (libc::ECONNREFUSED, "down".to_owned());
    let _ = peers.connect(key(1), || now, || Err::<(), _>(down.clone()), |_| {});
    peers
        .connect(
            key(1),
            || now + FIRST_RETRY,
            || {
                assert_eq!(
                    peers.connect(
                        key(1),
                        || now + FIRST_RETRY,
                        || panic!("already probing the dead peer"),
                        |_| {}
                    ),
                    Err::<(), _>(down)
                );
                Ok(())
            },
            |_| {},
        )
        .unwrap();
    for port in 1..=u16::try_from(MAX_PEERS + 1).unwrap() {
        let _ = peers.connect(
            key(port),
            || now,
            || Err::<(), _>((libc::ECONNREFUSED, "down".into())),
            |_| {},
        );
    }
    assert_eq!(peers.0.lock().unwrap().len(), MAX_PEERS);
    peers.connect(key(1), || now, || Ok(()), |_| {}).unwrap();
    let mut time = now;
    for _ in 0..20 {
        let _ = peers.connect(
            key(1),
            || time,
            || Err::<(), _>((libc::ECONNREFUSED, "down".into())),
            |_| {},
        );
        let state = peers.0.lock().unwrap();
        let peer = state.iter().find(|peer| peer.key == key(1)).unwrap();
        assert!(peer.delay <= MAX_RETRY);
        time = peer.retry_at;
    }
    let state = peers.0.lock().unwrap();
    assert_eq!(
        state.iter().find(|peer| peer.key == key(1)).unwrap().delay,
        MAX_RETRY
    );
}
