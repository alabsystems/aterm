// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn completed_exchange_survives_reset_but_never_revives_a_withdrawn_proof() {
    use std::{
        io::{Read, Write},
        os::fd::AsRawFd,
    };
    for withdrawn in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let connection = Connection::connect(
            listener.local_addr().unwrap(),
            Scope::Loopback,
            Duration::from_secs(60),
        )
        .unwrap();
        let (mut stream, proof) = connection.into_parts();
        stream
            .set_read_timeout(Some(Duration::from_secs(60)))
            .unwrap();
        let mut peer = listener.accept().unwrap().0;
        peer.write_all(b"completed").unwrap();
        let mut bytes = [0; 9];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"completed");
        proof.recheck().unwrap();
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        // SAFETY: peer owns a live socket, and linger is readable for its exact size.
        assert_eq!(
            unsafe {
                libc::setsockopt(
                    peer.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    (&raw const linger).cast(),
                    std::mem::size_of_val(&linger) as libc::socklen_t,
                )
            },
            0
        );
        drop(peer);
        match stream.read(&mut [0]) {
            Ok(0) => {} // Some kernels report the completed read side as EOF.
            Err(error) if error.kind() == io::ErrorKind::ConnectionReset => {}
            other => panic!("reset peer did not close the stream: {other:?}"),
        }
        if withdrawn {
            proof.withdrawn.store(true, Ordering::Release);
        }
        let completed = proof.finish();
        if withdrawn {
            assert!(completed.is_err());
        } else {
            completed.unwrap();
        }
    }
}

#[test]
fn bounded_fleet_listener_applies_link_policy_before_tls() {
    use crate::fleet::{self, FleetOp, FleetScope, FleetSecret, Listening};
    use std::{
        io::{Read, Write},
        sync::{Arc, atomic::AtomicUsize},
        thread,
    };
    let within = aterm_spec::wait::MUST_HAPPEN;
    for scope in [Scope::Lan, Scope::Loopback] {
        let listener = Listener::bind("127.0.0.1:0".parse().unwrap(), scope);
        if scope == Scope::Lan {
            assert!(
                listener.is_err(),
                "LAN policy rejects loopback before listening"
            );
            continue;
        }
        let listener = listener.unwrap();
        let address = listener.local_addr().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let delivered = Arc::new(AtomicUsize::new(0));
        let server = {
            let running = running.clone();
            let delivered = delivered.clone();
            thread::spawn(move || {
                fleet::serve_lan(
                    listener,
                    &Listening {
                        secret: FleetSecret::new(&[7; 32]).unwrap(),
                        scope: FleetScope::Facts,
                        node: None,
                        serves: &[FleetOp::Survey],
                        slots: 2,
                        per_source: 2,
                        lifetime: within,
                    },
                    &running,
                    move |served, _| {
                        delivered.fetch_add(1, Ordering::SeqCst);
                        served.answer(|stream| {
                            let mut request = [0];
                            stream.read_exact(&mut request)?;
                            stream.write_all(&request)?;
                            stream.flush()
                        })
                    },
                    |_| {},
                );
            })
        };
        let tcp = TcpStream::connect_timeout(&address, within).unwrap();
        tcp.set_read_timeout(Some(within)).unwrap();
        tcp.set_write_timeout(Some(within)).unwrap();
        let result = fleet::client_handshake(
            tcp,
            &FleetSecret::new(&[7; 32]).unwrap(),
            FleetOp::Survey,
            within,
        );
        let admitted = result.is_ok();
        if let Ok((mut channel, _)) = result {
            channel
                .stream()
                .get_ref()
                .set_read_timeout(Some(within))
                .unwrap();
            channel
                .stream()
                .get_ref()
                .set_write_timeout(Some(within))
                .unwrap();
            channel.stream().write_all(&[42]).unwrap();
            channel.stream().flush().unwrap();
            let mut answer = [0];
            channel.stream().read_exact(&mut answer).unwrap();
            assert_eq!(answer, [42]);
        }
        running.store(false, Ordering::Release);
        server.join().unwrap();
        assert_eq!(admitted, scope == Scope::Loopback);
        assert_eq!(delivered.load(Ordering::SeqCst), usize::from(admitted));
    }
}
use std::net::{SocketAddrV6, TcpListener};

fn fixture() -> (SocketAddr, SocketAddr, Attachment, Route) {
    let local = "192.0.2.1:1200".parse().unwrap();
    let peer = "192.0.2.2:1300".parse().unwrap();
    let attachment = Attachment {
        index: 3,
        name: "test-physical".into(),
        address: "192.0.2.1".parse().unwrap(),
        netmask: "255.255.255.0".parse().unwrap(),
        flags: (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_MULTICAST) as u32,
        physical: true,
    };
    let route = Route {
        index: 3,
        source: Some(attachment.address),
        direct: true,
    };
    (local, peer, attachment, route)
}
fn v6_fixture() -> (SocketAddr, SocketAddr, Attachment, Route) {
    let (_, _, mut attachment, mut route) = fixture();
    attachment.address = "fe80::1".parse().unwrap();
    attachment.netmask = "ffff:ffff:ffff:ffff::".parse().unwrap();
    route.source = Some(attachment.address);
    let local = SocketAddrV6::new("fe80::1".parse().unwrap(), 1200, 0, 3).into();
    let peer = SocketAddrV6::new("fe80::2".parse().unwrap(), 1300, 0, 3).into();
    (local, peer, attachment, route)
}

#[test]
fn private_addresses_do_not_override_route_or_interface_evidence() {
    let (local, peer, attachment, route) = fixture();
    assert!(validate(local, peer, &attachment, &route).is_ok());
    let mut routed = route.clone();
    routed.direct = false;
    assert!(validate(local, peer, &attachment, &routed).is_err());
    let mut wrong = route.clone();
    wrong.index = 4;
    assert!(validate(local, peer, &attachment, &wrong).is_err());
    for flag in [libc::IFF_UP, libc::IFF_RUNNING, libc::IFF_MULTICAST] {
        let mut down = attachment.clone();
        down.flags &= !(flag as u32);
        assert!(validate(local, peer, &down, &route).is_err());
    }
    for flag in [libc::IFF_LOOPBACK, libc::IFF_POINTOPOINT] {
        let mut tunnel = attachment.clone();
        tunnel.flags |= flag as u32;
        assert!(validate(local, peer, &tunnel, &route).is_err());
    }
    let mut virtual_link = attachment.clone();
    virtual_link.physical = false;
    assert!(validate(local, peer, &virtual_link, &route).is_err());
    for peer in [
        "192.0.3.2:13",
        "192.0.2.0:13",
        "192.0.2.255:13",
        "224.0.0.2:13",
        "0.0.0.2:13",
        "255.255.255.255:13",
    ] {
        assert!(validate(local, peer.parse().unwrap(), &attachment, &route).is_err());
    }
}

#[test]
fn subnet_masks_must_be_contiguous_and_not_default_routes() {
    let local: IpAddr = "192.0.2.1".parse().unwrap();
    let peer: IpAddr = "192.0.2.2".parse().unwrap();
    for mask in ["0.0.0.0", "255.0.255.0", "255.255.255.253"] {
        assert!(!same_subnet(local, peer, mask.parse().unwrap()));
    }
    assert!(same_subnet(local, peer, "255.255.255.0".parse().unwrap()));
    for source in ["192.0.2.0", "192.0.2.255"] {
        assert!(!same_subnet(
            source.parse().unwrap(),
            peer,
            "255.255.255.0".parse().unwrap()
        ));
    }
    assert!(same_subnet(
        "192.0.2.0".parse().unwrap(),
        local,
        "255.255.255.254".parse().unwrap()
    ));
    let local: IpAddr = "2001:db8::1".parse().unwrap();
    let peer: IpAddr = "2001:db8::2".parse().unwrap();
    assert!(same_subnet(
        local,
        peer,
        "ffff:ffff:ffff:ffff::".parse().unwrap()
    ));
    assert!(!same_subnet(
        local,
        peer,
        "ffff:ffff:ffff:fffd::".parse().unwrap()
    ));
    assert!(!same_subnet(local, peer, "::".parse().unwrap()));
}

#[test]
fn ipv6_zone_is_explicit_and_mapped_addresses_are_not_silently_relabelled() {
    assert!(!unambiguous("[fe90::2%3]:80".parse().unwrap()));
    let (local, peer, attachment, route) = v6_fixture();
    assert!(validate(local, peer, &attachment, &route).is_ok());
    for scope in [0, 4] {
        let mut wrong = match peer {
            SocketAddr::V6(peer) => peer,
            _ => unreachable!(),
        };
        wrong.set_scope_id(scope);
        assert!(validate(local, wrong.into(), &attachment, &route).is_err());
    }
    assert!(!unambiguous("[::ffff:192.0.2.2]:1300".parse().unwrap()));
    let mut flow = match peer {
        SocketAddr::V6(peer) => peer,
        _ => unreachable!(),
    };
    flow.set_flowinfo(1);
    assert!(!unambiguous(flow.into()));
    let embedded: IpAddr = "fe80:3::1".parse().unwrap();
    assert_eq!(
        scoped_ip(embedded, 3).unwrap(),
        "fe80::1".parse::<IpAddr>().unwrap()
    );
    assert!(scoped_ip(embedded, 4).is_err());
}

#[test]
fn loopback_is_explicit_and_the_witness_owns_its_exact_connection() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = listener.local_addr().unwrap();
    assert!(Connection::connect(peer, Scope::Lan, Duration::from_secs(1)).is_err());
    let connected = Connection::connect(peer, Scope::Loopback, Duration::from_secs(1)).unwrap();
    let accepted = Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap();
    let (_stream, mut proof) = connected.into_parts();
    proof.recheck().unwrap();
    proof
        .observation
        .peer
        .set_port(proof.observation.peer.port() + 1);
    assert!(proof.recheck().is_err());
    proof.observation.peer = peer;
    assert!(
        proof.recheck().is_err(),
        "a detected change cannot resurrect an old observation"
    );
    accepted.proof.recheck().unwrap();
    assert!(
        Connection::connect(
            "192.0.2.1:9".parse().unwrap(),
            Scope::Loopback,
            Duration::from_secs(1)
        )
        .is_err()
    );
}

#[test]
fn nondefault_socket_routing_cannot_reuse_default_route_evidence() {
    use std::os::fd::AsRawFd;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stream = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let _accepted = listener.accept().unwrap();
    let value = 1_i32;
    // SAFETY: value is a readable integer socket option on this owned live fd.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_DONTROUTE,
                std::ptr::from_ref(&value).cast(),
                std::mem::size_of_val(&value) as _,
            )
        },
        0
    );
    assert!(Connection::admit(stream, Scope::Loopback).is_err());
}

#[test]
fn ipv6_loopback_is_local_without_a_fake_lan_zone() {
    let listener = TcpListener::bind("[::1]:0").unwrap();
    let connection = Connection::connect(
        listener.local_addr().unwrap(),
        Scope::Loopback,
        Duration::from_secs(1),
    )
    .unwrap();
    let accepted = Connection::admit(listener.accept().unwrap().0, Scope::Loopback).unwrap();
    connection.proof.recheck().unwrap();
    accepted.proof.recheck().unwrap();
    assert!(!unambiguous("[2001:db8::1%3]:80".parse().unwrap()));
}

pub fn lan_transition(
    state: &aterm_spec::interp::State,
    action: &str,
    bypass: bool,
) -> aterm_spec::interp::State {
    let (mut local, mut peer, mut attachment, mut route) = fixture();
    let scenario = match action {
        "Physical" => 1,
        "Routed" => {
            route.direct = false;
            2
        }
        "Virtual" => {
            attachment.physical = false;
            3
        }
        "WrongInterface" => {
            route.index = 4;
            4
        }
        "WrongSource" => {
            attachment.address = "192.0.2.9".parse().unwrap();
            5
        }
        "OffLink" => {
            peer = "192.0.3.2:1300".parse().unwrap();
            6
        }
        "Down" => {
            attachment.flags &= !(libc::IFF_UP as u32);
            7
        }
        "MissingScope" => {
            (local, peer, attachment, route) = v6_fixture();
            if let SocketAddr::V6(peer) = &mut peer {
                peer.set_scope_id(0);
            }
            8
        }
        "MappedAddress" => {
            peer = "[::ffff:192.0.2.2]:1300".parse().unwrap();
            9
        }
        "ScopedIPv6" => {
            (local, peer, attachment, route) = v6_fixture();
            10
        }
        _ => panic!("unknown LAN action"),
    };
    let admitted = bypass || validate(local, peer, &attachment, &route).is_ok();
    let mut next = state.clone();
    next.insert("scenario", scenario);
    next.insert("admitted", i64::from(admitted));
    next
}

#[test]
fn lan_policy_model_conforms_and_catches_a_private_address_bypass() {
    let model = aterm_models_core::lan_boundary_model();
    assert!(aterm_spec::interp::prove_and_catch(&model) > 1);
    assert_eq!(
        aterm_spec::interp::fired_actions(&model).len(),
        model.actions.len()
    );
    for state in aterm_spec::interp::reachable_states(&model) {
        for action in &model.actions {
            if model.action_enabled(action.name, &state) {
                assert!(
                    model
                        .successors(action.name, &state)
                        .contains(&lan_transition(&state, action.name, false))
                );
            }
        }
    }
    let state = std::collections::BTreeMap::from([("scenario", 0), ("admitted", 0)]);
    let bad = lan_transition(&state, "Routed", true);
    assert!(!model.successors("Routed", &state).contains(&bad));
    assert!(!model.check_invariant("OnlyVerifiedLink", &bad));
}

#[test]
fn the_loopback_guard_refuses_a_lan_dial_listen_and_join_and_keeps_the_loopback() {
    use crate::loopback::forced;
    use std::net::Ipv4Addr;
    let documentation = Ipv4Addr::new(192, 0, 2, 1);
    for on in [true, false] {
        let refusals = forced(on, || {
            [
                (
                    "dial",
                    Connection::connect(
                        (documentation, 7100).into(),
                        Scope::Lan,
                        Duration::from_secs(60),
                    )
                    .err(),
                ),
                (
                    "listen",
                    Listener::bind((documentation, 0).into(), Scope::Lan).err(),
                ),
                (
                    "multicast",
                    Multicast::bind(documentation, Ipv4Addr::new(239, 255, 0, 1), 0, Scope::Lan)
                        .err(),
                ),
            ]
        });
        for (what, refused) in refusals {
            let refused = refused.unwrap_or_else(|| panic!("{what}: no LAN at 192.0.2.1"));
            let said = refused.to_string();
            assert_eq!(
                said.starts_with("ATERM_DEBUG_LOOPBACK_ONLY=1 refuses ")
                    && said.contains("192.0.2.1"),
                on,
                "{what} (guard {on}): {said}"
            );
        }
    }
    let local = TcpListener::bind("127.0.0.1:0").unwrap();
    forced(true, || {
        Connection::connect(
            local.local_addr().unwrap(),
            Scope::Loopback,
            Duration::from_secs(60),
        )
        .expect("a loopback dial under the guard");
        Listener::bind("127.0.0.1:0".parse().unwrap(), Scope::Loopback)
            .expect("a loopback listener under the guard");
    });
}
