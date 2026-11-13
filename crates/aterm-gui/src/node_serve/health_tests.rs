// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn health_requires_the_expected_authenticated_node_and_a_complete_census() {
    let expected = NodeIdentity::from_seed(&"1".repeat(32), &[1; 32]).unwrap();
    let other = NodeIdentity::from_seed(&"2".repeat(32), &[2; 32]).unwrap();
    let census = aterm_agent::fleet_census::SwarmCensus::default().to_json();
    for (node, reply, succeeds) in [
        (expected.clone(), Some(format!("OK {census}\n")), true),
        (other.clone(), Some(format!("OK {census}\n")), false),
        (other, Some("ERR no Commons\n".to_owned()), false),
        (expected.clone(), None, false),
        (expected.clone(), Some("OK {}\n".to_owned()), false),
        (expected.clone(), Some("ERR no Commons\n".to_owned()), true),
        (expected.clone(), Some("ERR cut sho".to_owned()), false),
        (expected.clone(), Some("what is this\n".to_owned()), false),
    ] {
        let secret = aterm_net::fleet::FleetSecret::new(&[0x5a; 24]).unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(Stop::new());
        let serve_stop = Arc::clone(&stop);
        let listening = aterm_net::fleet::Listening {
            secret: secret.clone(),
            scope: FleetScope::Facts,
            node: Some(node),
            serves: &[FleetOp::Census],
            slots: 2,
            per_source: 2,
            lifetime: aterm_spec::wait::MUST_HAPPEN,
        };
        let server = std::thread::spawn(move || {
            aterm_net::fleet::serve_with_handshake(
                &listener,
                &listening,
                &serve_stop,
                aterm_spec::wait::MUST_HAPPEN,
                move |served, _| {
                    served.answer(|stream| {
                        if let Some(reply) = &reply {
                            stream.write_all(reply.as_bytes())?;
                            stream.flush()?;
                        }
                        Ok(())
                    })
                },
                |_| {},
            );
        });
        let proved = prove(addr, &secret, &expected, aterm_spec::wait::MUST_HAPPEN);
        stop.set();
        server.join().unwrap();
        assert_eq!(proved.is_ok(), succeeds, "{proved:?}");
    }
}
