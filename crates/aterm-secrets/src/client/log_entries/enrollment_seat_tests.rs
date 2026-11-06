// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Enrollment judges a seat by the one removal predicate ([`Log::seat_removed`]), the kin verdict
//! included, as the peer check does.

use super::*;
use crate::commons::binding::Request;
use crate::commons::journal::Trust;
use crate::membership_log::{Member, Membership, NodeStanding, Removal};

/// A request on one root, its recipient chosen by `recipient` and its writer key by `writer`.
fn request(recipient: u8, writer: u8) -> Request {
    Request::create(
        [17; 16],
        [34; 32],
        [writer; 16],
        [writer; 16],
        &Identity::from_secret_bytes([0x40 + recipient; 32]),
        &[0x60 + writer; 32],
    )
    .unwrap()
}

/// A log whose evaluation seats `enrolling` (node key `[0x31; 32]`, its own) and a kin seat that
/// shares its recipient with node key `[0x32; 32]`, standing `kin_key`, beside a removed Mac. The
/// enrolling seat's actor is the higher, so it is the recipient's newest holder enrollment reads.
fn kin_log(kin_key: fn(MemberId) -> NodeStanding) -> (Log, Request, Trust) {
    let (a, b) = (request(1, 1), request(1, 2));
    let (enrolling, kin) = if a.binding().actor() > b.binding().actor() {
        (a, b)
    } else {
        (b, a)
    };
    let removed = request(3, 3).binding().actor();
    let genesis = Record::found(
        &[9; 32],
        vec![Entrant {
            request: enrolling.clone(),
            node_key: [0x31; 32],
            name: "m1".to_owned(),
            label: "m1".to_owned(),
        }],
        1,
    )
    .unwrap();
    let entry = *genesis.id();
    let seat = |request: &Request, node_key: [u8; 32]| Member {
        binding: request.binding().clone(),
        node_key: Some(node_key),
        name: String::new(),
        label: String::new(),
        admitted_by: None,
        entry,
    };
    let enrolling_actor = enrolling.binding().actor();
    let membership = Membership::from_parts(
        BTreeMap::from([
            (enrolling_actor, seat(&enrolling, [0x31; 32])),
            (kin.binding().actor(), seat(&kin, [0x32; 32])),
        ]),
        BTreeMap::from([(
            removed,
            Removal {
                by: BTreeSet::new(),
                quorum: false,
            },
        )]),
        BTreeSet::new(),
        Vec::new(),
        BTreeMap::from([
            ([0x31; 32], NodeStanding::Member(enrolling_actor)),
            ([0x32; 32], kin_key(removed)),
        ]),
    );
    let log = Log::new(genesis).unwrap().with_membership(membership);
    let trust = Trust {
        network: *enrolling.binding().network(),
        authority: *enrolling.binding().authority(),
        genesis: [0; 32],
    };
    (log, enrolling, trust)
}

/// A seat whose kin holds a node key a removed Mac bound does not enroll, though its actor, its
/// recipient and its writer key are clear: the guard the peer check shares. NEGATIVE CONTROL: the
/// same kin with the key retired passes the guard and is judged as the existing seat, as this seat
/// was before the guard took the kin verdict.
#[test]
fn enrollment_refuses_a_seat_whose_kin_holds_a_removed_node_key() {
    let root = aterm_tempfile::tempdir().expect("tempdir");
    let paths = Paths::for_home(root.path());

    let (log, enrolling, trust) = kin_log(NodeStanding::Removed);
    let membership = log.membership();
    let binding = enrolling.binding();
    assert!(
        membership.removal(&binding.actor()).is_none()
            && !log.recipient_is_removed(&binding.recipient())
            && !matches!(membership.node(binding.writer()), NodeStanding::Removed(_))
            && membership.kin_removed(binding),
        "only the kin verdict takes the seat out"
    );
    assert!(log.seat_removed(binding));
    let refused = plan_log_enrollment(&paths, &log, trust, binding, false)
        .err()
        .expect("a seat whose kin holds a removed node key does not enroll");
    assert_eq!(
        refused.to_string(),
        "Commons enrollment recipient was removed"
    );

    let (log, enrolling, trust) = kin_log(NodeStanding::Retired);
    assert!(!log.seat_removed(enrolling.binding()));
    // Past the guard, the seat is read as the recipient's existing holder, and enrollment goes on
    // to the roster, which a home holding no vault cannot read.
    let passed = plan_log_enrollment(&paths, &log, trust, enrolling.binding(), false)
        .err()
        .expect("a home holding no vault has no roster to read");
    assert_eq!(
        passed.to_string(),
        "the vault state is busy or unavailable while reading its roster",
        "CONTROL: with the kin's key retired the seat passes the guard"
    );
}
