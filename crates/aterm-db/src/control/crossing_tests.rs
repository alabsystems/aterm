// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use aterm_secrets::client::Paths;

fn custody(lab: &Lab, name: &str, crossed: bool) -> Paths {
    let paths = Paths::for_home(&lab.root.path().join(name));
    std::fs::create_dir_all(&paths.dir).unwrap();
    if crossed {
        // The durable mark counts even after the records file is absent or short.
        std::fs::write(paths.records_file().with_extension("crossed"), b"crossed\n").unwrap();
    }
    paths
}

#[test]
fn crossed_custody_refuses_every_legacy_protocol_before_hello() {
    let lab = Lab::new();
    for crossed in [false, true] {
        let paths = custody(&lab, if crossed { "crossed" } else { "plain" }, crossed);
        for protocol in [Protocol::Legacy, Protocol::Repair, Protocol::Credentials] {
            for side in [Side::Initiator, Side::Responder] {
                let endpoint = lab.endpoint(1).with_membership_paths(paths.clone());
                let peer = lab.endpoint(0);
                let proof = NodeProof {
                    node: peer.identity.node().into(),
                    key: peer.identity.key(),
                };
                let mut input = Vec::new();
                send_frame(&mut input, &peer.hello).unwrap();
                let mut stream = Script {
                    input: Cursor::new(input),
                    output: Vec::new(),
                };
                let mut repair = Repair::default();
                let _ = endpoint.connected_until(
                    &mut stream,
                    Some(&proof),
                    side,
                    &mut repair,
                    Instant::now() + EXCHANGE,
                    protocol,
                );
                assert_eq!(
                    stream.output.is_empty(),
                    crossed,
                    "side {side:?}, crossed {crossed}"
                );
            }
        }
    }
}

#[test]
fn crossed_custody_cannot_complete_native_legacy_database_tls() {
    let lab = Lab::new();
    for marked in [0, 1] {
        let mut endpoints = [lab.endpoint(0), lab.endpoint(1)];
        endpoints[marked].paths = Some(custody(&lab, &format!("tls-{marked}"), true));
        for version_two in [false, true] {
            endpoints[0].legacy_projection = version_two;
            endpoints[1].legacy_projection = version_two;
            endpoints[0].legacy_credentials = !version_two;
            endpoints[1].legacy_credentials = !version_two;
            let (server, client, _) = dial(&endpoints[0], &endpoints[1]);
            assert!(server.is_err(), "marked {marked}, v2 {version_two}");
            assert!(client.is_err(), "marked {marked}, v2 {version_two}");
        }
    }
}

#[test]
fn a_responder_rechecks_crossing_after_admission_before_its_hello() {
    let lab = Lab::new();
    for cross_after_admission in [false, true] {
        let paths = custody(&lab, &format!("late-{cross_after_admission}"), false);
        let mark = paths.records_file().with_extension("crossed");
        let endpoint = lab.endpoint(1).with_membership_paths(paths);
        let peer = lab.endpoint(0);
        let proof = NodeProof {
            node: peer.identity.node().into(),
            key: peer.identity.key(),
        };
        let mut input = Vec::new();
        send_frame(&mut input, &peer.hello).unwrap();
        let mut stream = Script {
            input: Cursor::new(input),
            output: Vec::new(),
        };
        let mut repair = Repair::default();
        let _ = observe_peer_cache(
            move |stage| {
                if cross_after_admission
                    && matches!(stage, PeerCacheStage::Acquired(Side::Responder))
                {
                    std::fs::write(&mark, b"crossed\n").unwrap();
                }
            },
            || endpoint.connected(&mut stream, Some(&proof), Side::Responder, &mut repair),
        );
        assert_eq!(stream.output.is_empty(), cross_after_admission);
    }
}

#[test]
fn cached_membership_disk_stops_serving_after_the_durable_mark() {
    use aterm_secrets::membership_log::{Entrant, Record};

    let lab = Lab::new();
    let paths = custody(&lab, "cached", false);
    let genesis = Record::found(
        &[7; 32],
        vec![Entrant {
            request: lab.requests[0].clone(),
            node_key: *lab.endpoint(0).identity.key().bytes(),
            name: "fixture".into(),
            label: "m1".into(),
        }],
        1,
    )
    .unwrap();
    aterm_secrets::client::save_records(&paths, genesis.bytes()).unwrap();
    let disk = MembershipDisk::new(&paths, lab.trust);
    let before = disk.inventory().unwrap();
    assert!(before.contains(genesis.id()));
    assert_eq!(disk.batch(&[*genesis.id()]).unwrap().len(), 1);
    std::fs::write(paths.records_file().with_extension("crossed"), b"crossed\n").unwrap();
    assert!(disk.inventory().is_err());
    assert!(disk.batch(&[*genesis.id()]).is_err());
    assert!(
        disk.select_missing(&before, &BTreeSet::new(), &mut None)
            .is_err()
    );
    assert!(disk.retain(&[genesis.bytes().to_vec()]).is_err());
}
