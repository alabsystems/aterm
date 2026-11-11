// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn unrelated_route_messages_do_not_discard_the_correlated_reply() {
    let expected = Route {
        index: 1,
        source: Some("127.0.0.1".parse().unwrap()),
        direct: false,
    };
    let mut unrelated = 64;
    let actual = receive_route(Instant::now() + Duration::from_secs(60), || {
        if unrelated > 0 {
            unrelated -= 1;
            Ok(None)
        } else {
            Ok(Some(expected.clone()))
        }
    });
    assert_eq!(actual.unwrap(), expected);
}

#[test]
fn route_receive_keeps_the_deadline_and_real_read_errors() {
    let mut calls = 0;
    assert_eq!(
        receive_route(Instant::now(), || {
            calls += 1;
            Ok(None)
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
    assert_eq!(calls, 0);
    assert_eq!(
        receive_route(Instant::now() + Duration::from_secs(60), || {
            calls += 1;
            Err(io::ErrorKind::PermissionDenied.into())
        })
        .unwrap_err()
        .kind(),
        io::ErrorKind::PermissionDenied
    );
    assert_eq!(calls, 1);
}

fn loopback() -> (&'static str, u32) {
    let name = if cfg!(target_os = "macos") {
        "lo0"
    } else {
        "lo"
    };
    (name, index(name).unwrap())
}

#[test]
fn expired_route_lookup_cannot_restart_the_deadline() {
    assert_eq!(
        route(None, "127.0.0.1:9".parse().unwrap(), None, Instant::now())
            .unwrap_err()
            .kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn native_route_observation_survives_scheduling_delay() {
    let (_, index) = loopback();
    let peer = "127.0.0.1:9".parse().unwrap();
    let started = Instant::now() - Duration::from_secs(3);
    let observed = route_at(None, peer, None, started + Duration::from_secs(60), started).unwrap();
    assert_eq!(observed.index, index);
    assert_eq!(
        route_at(
            None,
            peer,
            None,
            Instant::now() + Duration::from_secs(60),
            Instant::now() - Duration::from_secs(60)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn accepted_sockets_acquire_or_preserve_but_never_replace_their_binding() {
    let (name, index) = loopback();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (accepted, _) = aterm_uds::accept(
        &listener,
        aterm_uds::AcceptWait::Forever,
        aterm_uds::AcceptedMode::Blocking,
    )
    .unwrap();
    ensure_binding(&accepted, index, name).unwrap();
    ensure_binding(&accepted, index, name).unwrap();
    restrict_hops(&accepted).unwrap();
    socket_policy(&accepted, Some(index), true).unwrap();
    assert!(ensure_binding(&accepted, index.checked_add(1).unwrap(), name).is_err());
    socket_policy(&accepted, Some(index), true).unwrap();
}
