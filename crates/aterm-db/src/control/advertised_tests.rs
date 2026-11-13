// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The actual advertisement producer, with explicit isolated fleet storage. These are
//! witness-bound signed descriptors and local IP observations, never endpoint authentication.

use super::*;
use aterm_net::fleet_member::{HeldSignatures, Peer, record, signature_checks_here, update_peers};
use aterm_secrets::{client, membership_log::NodeStanding};
use std::{collections::BTreeMap, net::IpAddr};

pub(crate) struct Producer {
    pub(crate) lab: enrollment::tests::Lab,
    pub(crate) active: Endpoint,
    pending: Endpoint,
    legacy: Endpoint,
    pub(crate) node: NodeIdentity,
    actor: Actor,
    pub(crate) state: PathBuf,
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
    pub(crate) fn new() -> Self {
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

    pub(crate) fn contact(node: &NodeIdentity) -> Peer {
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

    /// A book of 85 signed entries: the member's own contact, then 84 nodes the witness binds
    /// to no seat, each at an address of its own.
    pub(crate) fn crowd(&self) -> Vec<Peer> {
        let mut book = vec![Self::contact(&self.node)];
        for n in 100..184u8 {
            let node = NodeIdentity::from_seed(&format!("{n:032x}"), &[n; 32]).unwrap();
            let mut peer = Peer::own(&node, ("member.invalid", 7100, "1.0.0"), 7);
            peer.addr = Some(IpAddr::from([198, 51, 100, n]));
            book.push(peer);
        }
        book
    }

    /// Keep `book` as the fleet book, whole.
    pub(crate) fn keep_all(&self, book: &[Peer]) {
        update_peers(&self.state, |held| {
            held.clear();
            held.extend(book.iter().cloned());
            Ok::<(), String>(())
        })
        .unwrap();
    }
}

/// NEGATIVE CONTROL (2026-11-12): with a kept verdict filed under the node id and key alone
/// (`HeldSignatures`), the entry whose version changed under its old signature took the old
/// verdict and was advertised (the read after `tampered` was not empty).
#[test]
fn advertised_versions_reads_signed_witness_descriptors_and_local_contacts() {
    let producer = Producer::new();
    // One memo for every read of the member's, as its transport holds one: each entry below
    // differs from the one kept before it in a field the check reads or in none.
    let mut held = HeldSignatures::default();
    let contact = Producer::contact(&producer.node);
    assert!(contact.signature_holds());
    producer.keep(contact.clone());
    assert_eq!(
        producer
            .active
            .advertised_versions(&producer.state, &mut held),
        BTreeMap::from([(producer.actor, (contact.addr.unwrap(), "1.0.0".into()))])
    );
    assert!(
        producer
            .pending
            .advertised_versions(&producer.state, &mut HeldSignatures::default())
            .is_empty()
    );
    assert!(
        producer
            .legacy
            .advertised_versions(&producer.state, &mut HeldSignatures::default())
            .is_empty()
    );

    let mut tampered = contact.clone();
    tampered.version = Some("2.0.0".into());
    assert!(!tampered.signature_holds());
    producer.keep(tampered);
    assert!(
        producer
            .active
            .advertised_versions(&producer.state, &mut held)
            .is_empty()
    );

    let mut missing = contact.clone();
    missing.addr = None;
    assert!(missing.signature_holds());
    producer.keep(missing);
    assert!(
        producer
            .active
            .advertised_versions(&producer.state, &mut held)
            .is_empty()
    );

    let unknown = NodeIdentity::from_seed(&"19".repeat(16), &[19; 32]).unwrap();
    producer.keep(Producer::contact(&unknown));
    assert!(
        producer
            .active
            .advertised_versions(&producer.state, &mut held)
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
        producer
            .active
            .advertised_versions(&producer.state, &mut held),
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
    let mut held = HeldSignatures::default();
    let witness = fresh.snapshot_log.as_ref().unwrap().membership();
    assert_eq!(
        witness.node(producer.node.key().bytes()),
        NodeStanding::Member(producer.actor)
    );
    assert_eq!(witness.node(rotated.key().bytes()), NodeStanding::Unknown);
    producer.keep(Producer::contact(&producer.node));
    assert_eq!(
        fresh.advertised_versions(&producer.state, &mut held).len(),
        1
    );
    producer.keep(Producer::contact(&rotated));
    assert!(
        fresh
            .advertised_versions(&producer.state, &mut held)
            .is_empty()
    );
}

/// The member's read of an unchanged 85-entry book checks each signature once: a second read
/// checks none and answers as the first did. An entry whose version changed under its old
/// signature is checked and refused, and stays refused unchecked; the entry it was, put back,
/// is checked again, since only the last book's verdicts are kept. Counted on this thread
/// (`signature_checks_here`).
///
/// NEGATIVE CONTROL (2026-11-12): with `HeldSignatures::judge` never looking a kept verdict up,
/// the second read made 85 checks (`an unchanged book is not checked again`: left 85, right 0).
/// NEGATIVE CONTROL (2026-11-12): with a verdict kept under the node id and key alone, the
/// changed entry took the old verdict and was advertised (`a changed entry is checked`: left 0,
/// right 1).
#[test]
fn an_unchanged_book_is_checked_once_and_a_changed_entry_again() {
    let producer = Producer::new();
    let book = producer.crowd();
    assert_eq!(book.len(), 85);
    producer.keep_all(&book);
    let mut held = HeldSignatures::default();
    let mut read = || {
        let before = signature_checks_here();
        let advertised = producer
            .active
            .advertised_versions(&producer.state, &mut held);
        (advertised, signature_checks_here() - before)
    };
    let member = BTreeMap::from([(producer.actor, (book[0].addr.unwrap(), "1.0.0".to_owned()))]);
    let (first, checks) = read();
    assert_eq!(checks, 85, "a book not read before is checked whole");
    assert_eq!(first, member);
    let (second, checks) = read();
    assert_eq!(checks, 0, "an unchanged book is not checked again");
    assert_eq!(second, member);

    let mut changed = book.clone();
    changed[0].version = Some("2.0.0".into());
    producer.keep_all(&changed);
    let (refused, checks) = read();
    assert_eq!(checks, 1, "a changed entry is checked");
    assert!(refused.is_empty(), "and refused: {refused:?}");
    let (refused, checks) = read();
    assert_eq!(checks, 0, "a failed verdict is kept");
    assert!(refused.is_empty(), "as a failure: {refused:?}");

    producer.keep_all(&book);
    let (back, checks) = read();
    assert_eq!(checks, 1, "only the last book's verdicts are kept");
    assert_eq!(back, member);
}
