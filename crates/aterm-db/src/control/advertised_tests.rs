// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual advertisement producer, with explicit isolated fleet storage. These are
//! witness-bound signed descriptors and local IP observations, never endpoint authentication.

use super::*;
use aterm_net::fleet_member::{Peer, record, update_peers};
use aterm_secrets::{client, membership_log::NodeStanding};
use std::{collections::BTreeMap, net::IpAddr};

struct Producer {
    lab: enrollment::tests::Lab,
    active: Endpoint,
    pending: Endpoint,
    legacy: Endpoint,
    node: NodeIdentity,
    actor: Actor,
    state: PathBuf,
}

fn publish_node(lab: &enrollment::tests::Lab, node: &NodeIdentity) {
    assert!(
        client::publish_local_node_key(&lab.paths[0], *node.key().bytes(), |network, actor| {
            let message = [network.as_slice(), actor.as_slice()].concat();
            node.sign_record("membership node", &message)
        })
        .unwrap()
        .is_some()
    );
    client::save_records(
        &lab.paths[1],
        &client::load_records(&lab.paths[0]).unwrap().encode(),
    )
    .unwrap();
}

impl Producer {
    fn new() -> Self {
        let lab = enrollment::tests::Lab::new(true);
        let legacy = Endpoint::from_bootstrap(&lab.bootstrap(0))
            .unwrap()
            .unwrap();
        assert!(legacy.snapshot_log.is_none());
        lab.found_log();
        let node = NodeIdentity::from_seed(&"17".repeat(16), &[17; 32]).unwrap();
        // Publish the fleet key before the new device's real enrollment, so the
        // device's historical admission witness includes this binding's ancestry.
        publish_node(&lab, &node);
        let pending = lab.pending();
        assert!(pending.snapshot_log.is_none());
        let (owner, joining) = lab.exchange(&pending);
        assert!(owner.unwrap().sent > 0);
        assert!(joining.unwrap().received > 0);
        let active = Endpoint::from_bootstrap(&lab.bootstrap(1))
            .unwrap()
            .unwrap();
        assert!(!active.pending());
        let actor = lab.bootstrap(0).request().unwrap().binding().actor();
        assert_eq!(
            active
                .snapshot_log
                .as_ref()
                .unwrap()
                .membership()
                .node(node.key().bytes()),
            NodeStanding::Member(actor)
        );
        let state = lab.root.path().join("fleet");
        aterm_secrets::fsx::ensure_private_dir(&state).unwrap();
        Self {
            lab,
            active,
            pending,
            legacy,
            node,
            actor,
            state,
        }
    }

    fn contact(node: &NodeIdentity) -> Peer {
        let mut peer = Peer::own(node, ("member.invalid", 7100, "1.0.0"), 7);
        // This field represents this book's local contact observation. It is
        // deliberately separate from the actual node-signed descriptor.
        peer.addr = Some(IpAddr::from([192, 0, 2, 17]));
        peer
    }

    fn keep(&self, peer: Peer) {
        update_peers(&self.state, |book| {
            book.clear();
            record(book, peer).map(|_| ())
        })
        .unwrap();
    }
}

#[test]
fn advertised_versions_reads_signed_witness_descriptors_and_local_contacts() {
    let producer = Producer::new();
    let contact = Producer::contact(&producer.node);
    assert!(contact.signature_holds());
    producer.keep(contact.clone());
    assert_eq!(
        producer.active.advertised_versions(&producer.state),
        BTreeMap::from([(producer.actor, (contact.addr.unwrap(), "1.0.0".into()))])
    );
    assert!(
        producer
            .pending
            .advertised_versions(&producer.state)
            .is_empty()
    );
    assert!(
        producer
            .legacy
            .advertised_versions(&producer.state)
            .is_empty()
    );

    let mut tampered = contact.clone();
    tampered.version = Some("2.0.0".into());
    assert!(!tampered.signature_holds());
    producer.keep(tampered);
    assert!(
        producer
            .active
            .advertised_versions(&producer.state)
            .is_empty()
    );

    let mut missing = contact.clone();
    missing.addr = None;
    assert!(missing.signature_holds());
    producer.keep(missing);
    assert!(
        producer
            .active
            .advertised_versions(&producer.state)
            .is_empty()
    );

    let unknown = NodeIdentity::from_seed(&"19".repeat(16), &[19; 32]).unwrap();
    producer.keep(Producer::contact(&unknown));
    assert!(
        producer
            .active
            .advertised_versions(&producer.state)
            .is_empty()
    );

    // Changing local contact metadata neither breaks the signature nor proves
    // current reachability. Even an old observation is consumed as a hint.
    let mut moved = contact;
    moved.addr = Some(IpAddr::from([192, 0, 2, 18]));
    moved.seen_ms = Some(0);
    assert!(moved.signature_holds());
    producer.keep(moved);
    assert_eq!(
        producer.active.advertised_versions(&producer.state),
        BTreeMap::from([(
            producer.actor,
            (IpAddr::from([192, 0, 2, 18]), "1.0.0".into())
        )])
    );
}

#[test]
fn advertised_versions_is_historical_witness_evidence_not_a_full_current_roster() {
    let producer = Producer::new();
    let rotated = NodeIdentity::from_seed(&"17".repeat(16), &[18; 32]).unwrap();
    publish_node(&producer.lab, &rotated);
    let bootstrap = producer.lab.bootstrap(1);
    let full = client::trusted_commons_log(&producer.lab.paths[1], bootstrap.trust())
        .unwrap()
        .unwrap()
        .0;
    assert_eq!(
        full.membership().node(rotated.key().bytes()),
        NodeStanding::Member(producer.actor)
    );
    assert_eq!(
        full.membership().node(producer.node.key().bytes()),
        NodeStanding::Retired(producer.actor)
    );

    // Construct again from current local custody: the minimal historical seat
    // witness still predates the later fleet-key rotation in the full log.
    let fresh = Endpoint::from_bootstrap(&bootstrap).unwrap().unwrap();
    let witness = fresh.snapshot_log.as_ref().unwrap().membership();
    assert_eq!(
        witness.node(producer.node.key().bytes()),
        NodeStanding::Member(producer.actor)
    );
    assert_eq!(witness.node(rotated.key().bytes()), NodeStanding::Unknown);
    producer.keep(Producer::contact(&producer.node));
    assert_eq!(fresh.advertised_versions(&producer.state).len(), 1);
    producer.keep(Producer::contact(&rotated));
    assert!(fresh.advertised_versions(&producer.state).is_empty());
}
