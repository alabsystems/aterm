// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn current_protocol_routes_require_a_live_log_key_and_the_existing_node_binding() {
    const HANG: Duration = aterm_spec::wait::MUST_HAPPEN;
    let node = "11111111111111111111111111111111";
    let other_node = "22222222222222222222222222222222";
    let server = NodeIdentity::from_seed(node, &[41; 32]).unwrap();
    let client = NodeIdentity::from_seed(other_node, &[42; 32]).unwrap();
    let secret = FleetSecret::new(&[19; 32]).unwrap();
    for (standing, bound, wrong_claim, expected) in [
        (Some(member::Standing::Member), true, false, true),
        (
            Some(member::Standing::Out("removed".into())),
            true,
            false,
            false,
        ),
        (None, true, false, false),
        (Some(member::Standing::Member), false, false, false),
        (Some(member::Standing::Member), true, true, false),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        aterm_uds::keep_out_of_children(&listener).unwrap();
        let addr = listener.local_addr().unwrap();
        let serving = if wrong_claim {
            NodeIdentity::from_seed(other_node, &[41; 32]).unwrap()
        } else {
            server.clone()
        };
        let secret_server = secret.clone();
        let worker = std::thread::spawn(move || {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            aterm_net::fleet::server_handshake_as(
                tcp,
                &secret_server,
                Some(&serving),
                FleetScope::Facts,
                &[FleetOp::Survey],
                HANG,
            )
            .unwrap();
        });
        let members: member::MemberKeys = standing.into_iter().map(|s| (server.key(), s)).collect();
        let old_endpoint = Peer::own(&server, ("old.invalid", 7100, "0.199.0"), 1);
        let bindings = if bound {
            member::Keyring::of([&old_endpoint])
        } else {
            member::Keyring::default()
        };
        let hint = aterm_secrets::discover::Advert {
            instance: "opaque".into(),
            host: "127.0.0.1".into(),
            port: addr.port(),
            txt: vec![
                ("v".into(), "1".into()),
                ("fleet".into(), addr.port().to_string()),
                ("role".into(), "member".into()),
                ("window".into(), "0".into()),
            ],
        };
        let route = prove_fleet_route(&hint, (&secret, Some(&client)), (&members, &bindings), HANG);
        assert_eq!(
            route.is_some(),
            expected,
            "bound={bound}, wrong_claim={wrong_claim}"
        );
        if expected {
            assert_eq!(route, Some((node.into(), addr)));
        }
        worker.join().unwrap();
    }
}

#[test]
fn only_a_handshake_proving_a_current_members_mesh_id_populates_discovery() {
    const HANG: Duration = Duration::from_secs(60);
    let identity = |seed: u8| {
        let provisional =
            NodeIdentity::from_seed("00000000000000000000000000000000", &[seed; 32]).unwrap();
        NodeIdentity::from_seed(&member::node_principal(provisional.key()), &[seed; 32]).unwrap()
    };
    let server = identity(11);
    let client = identity(12);
    let secret = FleetSecret::new(&[7; 32]).unwrap();
    for (standing, wrong_id, expected) in [
        (Some(member::Standing::Member), false, true),
        (Some(member::Standing::Out("removed".into())), false, false),
        (None, false, false),
        (Some(member::Standing::Member), true, false),
    ] {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        aterm_uds::keep_out_of_children(&listener).unwrap();
        let addr = listener.local_addr().unwrap();
        let secret_server = secret.clone();
        let node = server.clone();
        let (done, waited) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (tcp, _) = aterm_uds::accept(
                &listener,
                aterm_uds::AcceptWait::Forever,
                aterm_uds::AcceptedMode::Blocking,
            )
            .unwrap();
            aterm_net::fleet::server_handshake_as(
                tcp,
                &secret_server,
                Some(&node),
                FleetScope::Facts,
                &[FleetOp::Survey],
                HANG,
            )
            .unwrap();
            done.send(()).unwrap();
        });
        let members: member::MemberKeys = standing
            .into_iter()
            .map(|standing| (server.key(), standing))
            .collect();
        let hint = aterm_secrets::discover::Advert {
            instance: "opaque".into(),
            host: "127.0.0.1".into(),
            port: addr.port(),
            txt: vec![
                ("v".into(), "2".into()),
                (
                    "id".into(),
                    if wrong_id {
                        client.node()
                    } else {
                        server.node()
                    }
                    .into(),
                ),
                ("port".into(), addr.port().to_string()),
            ],
        };
        let answer = prove_discovery_address_within(&hint, &secret, Some(&client), &members, HANG);
        assert_eq!(answer.is_some(), expected);
        if expected {
            assert_eq!(answer, Some((server.node().into(), addr)));
        }
        waited.recv_timeout(HANG).unwrap();
        worker.join().unwrap();
    }
}
