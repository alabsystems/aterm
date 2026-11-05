// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A member's desk takes a seat's `Node` record (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
//! §4.4, B3): a seated Mac whose node key the other member's log binds to no one sends its signed
//! `Node` record on `atf-ask/1` ([`desk::offer_node`]), so it can bind its key and join `atm/1`.
//! Two real vault states on one loopback server; nothing here touches a live vault.

use super::log_entries_tests::two_macs;
use super::*;
use crate::members::desk::{self, Desk, Local, NodeTaken};
use crate::membership_log::{Body, MemberId, NodeStanding, node_proof_message};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use std::sync::Mutex;

fn node_pair(tag: u8) -> Ed25519KeyPair {
    Ed25519KeyPair::from_seed_unchecked(&[tag ^ 0x5a; 32]).unwrap()
}

/// `paths`' seat binds `pair`'s key in its own log, posting it nowhere.
fn publish_node(paths: &Paths, pair: &Ed25519KeyPair) -> [u8; 32] {
    let key: [u8; 32] = pair.public_key().as_ref().try_into().unwrap();
    publish_local_node_key(paths, key, |network, actor| {
        pair.sign(&node_proof_message(network, actor))
            .as_ref()
            .try_into()
            .unwrap()
    })
    .expect("publishes its node key")
    .expect("a Node record");
    key
}

fn standing(paths: &Paths, key: &[u8; 32]) -> NodeStanding {
    load_records(paths)
        .unwrap()
        .log()
        .expect("a log")
        .membership()
        .node(key)
}

fn seat(paths: &Paths, key: &[u8; 32]) -> MemberId {
    match standing(paths, key) {
        NodeStanding::Member(actor) => actor,
        other => panic!("its own log binds the key to no seat: {other:?}"),
    }
}

fn desk_of(paths: &Paths) -> Mutex<Desk> {
    Mutex::new(Desk::new(
        Local::of(paths).unwrap().expect("a member's desk"),
    ))
}

/// `record` offered to `desk` over a socket pair, as an `atf-ask/1` connection carries it: what
/// the sender heard, and whether the desk's serve ended well.
fn offered(desk: &Mutex<Desk>, record: &[u8]) -> (crate::Result<NodeTaken>, bool) {
    let (mut ours, mut theirs) = std::os::unix::net::UnixStream::pair().unwrap();
    for end in [&ours, &theirs] {
        end.set_read_timeout(Some(aterm_spec::wait::MUST_HAPPEN))
            .unwrap();
    }
    std::thread::scope(|scope| {
        let served = scope.spawn(|| {
            desk::serve_with(
                desk,
                &mut ours,
                std::net::IpAddr::from([192, 0, 2, 7]),
                &|_| Err(crate::Error::new("no phrase runs here")),
                None,
            )
            .is_ok()
        });
        let taken = desk::offer_node(&mut theirs, record);
        (taken, served.join().unwrap())
    })
}

/// m7 binds its node key in its own log and posts it nowhere, so m22's log binds the key to no
/// one; m22's desk takes the record, its log then binds the key to m7's seat, and the same record
/// again is `already`.
///
/// NEGATIVE CONTROL: the desk answering before it saves (`return Ok(NodeTaken::Bound)` in place
/// of `client::save_records_unlocked`) leaves m22's log binding no one, and the standing
/// assertion fails.
#[test]
fn a_seats_node_record_binds_its_key_at_a_members_desk_and_again_is_already() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let n7 = publish_node(m7, &node_pair(7));
    let seat7 = seat(m7, &n7);
    assert_eq!(standing(m22, &n7), NodeStanding::Unknown, "posted nowhere");
    let record = desk::node_record(m7, &n7)
        .unwrap()
        .expect("m7's log binds its key");
    assert_eq!(desk::node_record(m7, &[7; 32]).unwrap(), None, "no record");

    let desk = desk_of(m22);
    let (taken, served) = offered(&desk, &record);
    assert_eq!(taken.unwrap(), NodeTaken::Bound);
    assert!(served);
    assert_eq!(standing(m22, &n7), NodeStanding::Member(seat7));

    let (again, served) = offered(&desk, &record);
    assert_eq!(again.unwrap(), NodeTaken::Already);
    assert!(served);
    macs.server.stop();
    drop(macs.root);
}

/// m7 rotates its node key twice in its own log. Its current `Node` record, sent alone, waits on
/// the one it rotates, which m22's log lacks; once that one arrives, m22's log takes both, the
/// first key stands replaced and the second bound, and the first record again is refused as a key
/// its seat replaced.
#[test]
fn a_rotation_waits_on_the_record_it_rotates_and_binds_once_that_arrives() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let first = publish_node(m7, &node_pair(70));
    let first_record = desk::node_record(m7, &first).unwrap().expect("bound");
    let second = publish_node(m7, &node_pair(71));
    let seat7 = seat(m7, &second);
    let second_record = desk::node_record(m7, &second).unwrap().expect("bound");
    assert_eq!(desk::node_record(m7, &first).unwrap(), None, "replaced");

    let desk = desk_of(m22);
    let (waiting, _) = offered(&desk, &second_record);
    assert_eq!(waiting.unwrap(), NodeTaken::Waiting);
    assert_eq!(standing(m22, &second), NodeStanding::Unknown);

    let (replaced, _) = offered(&desk, &first_record);
    assert_eq!(replaced.unwrap(), NodeTaken::Replaced);
    assert_eq!(standing(m22, &second), NodeStanding::Member(seat7));
    assert_eq!(standing(m22, &first), NodeStanding::Retired(seat7));

    let (again, served) = offered(&desk, &first_record);
    let refused = again.expect_err("a replaced key is refused");
    assert!(
        refused.to_string().contains("replaced"),
        "the refusal names it: {refused}"
    );
    assert!(!served);
    let (current, _) = offered(&desk, &second_record);
    assert_eq!(current.unwrap(), NodeTaken::Already);
    macs.server.stop();
    drop(macs.root);
}

/// A record that binds no node key is refused, and so is a `Node` record for a key m22's log
/// binds to m22's own seat (m7 holds the same key pair and signs it in its own log): m22's log
/// keeps the key m22's alone.
///
/// NEGATIVE CONTROL: the desk skipping its standing check (`NodeStanding::Member(_) |
/// NodeStanding::Contested => {}`) takes m7's record into m22's store, which then reads the key
/// `Contested`, and its refusal names no other seat: the second `contains` assertion fails.
#[test]
fn a_desk_refuses_a_record_that_binds_no_key_and_a_key_another_seat_holds() {
    let _answering = ServersAnswering::set();
    let macs = two_macs(&[]);
    let (m22, m7) = (&macs.m22, &macs.m7);
    let desk = desk_of(m22);

    let store = load_records(m7).unwrap();
    let other = store
        .log()
        .unwrap()
        .records()
        .find(|record| !matches!(record.body(), Body::Node { .. }))
        .expect("the genesis at least")
        .bytes()
        .to_vec();
    let (taken, served) = offered(&desk, &other);
    let refused = taken.expect_err("no Node record");
    assert!(
        refused.to_string().contains("not a Node record"),
        "{refused}"
    );
    assert!(!served);

    let shared = node_pair(22);
    let n22 = publish_node(m22, &shared);
    let seat22 = seat(m22, &n22);
    publish_node(m7, &shared);
    let claimed = desk::node_record(m7, &n22)
        .unwrap()
        .expect("m7's log binds it to m7");
    let (taken, served) = offered(&desk, &claimed);
    let refused = taken.expect_err("another seat's key");
    assert!(refused.to_string().contains("another seat"), "{refused}");
    assert!(!served);
    assert_eq!(standing(m22, &n22), NodeStanding::Member(seat22));
    macs.server.stop();
    drop(macs.root);
}
