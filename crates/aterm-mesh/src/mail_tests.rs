// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use std::collections::{BTreeMap, BTreeSet};

use aterm_link::federate::node_of;

use super::handoff_of;

fn key(tag: u8) -> [u8; 32] {
    astream_broker::origin::public_key(&[tag; 32])
}

/// **THE HAND-OFF NAMES EVERY NODE GONE**: a key the log removed and a key a `Node` record
/// rotated away are gone; a current member, this Mac, and a removed key some member's key
/// also names (none can, but the rule holds it) are not. Each rotation names its successor.
#[test]
fn the_hand_off_names_each_removed_and_rotated_node_gone_and_no_member() {
    let (me, b, removed, old, new) = (key(1), key(2), key(3), key(4), key(5));
    let standings = crate::Standings {
        current: [me, b, new].into(),
        removed: [removed, b].into(),
        retired: [old].into(),
        rotated: [(old, new)].into(),
        recency: [(old, 0)].into(),
    };
    let handoff = handoff_of(node_of(&me), &standings);
    assert_eq!(handoff.me, node_of(&me));
    assert_eq!(
        handoff.members,
        [me, b, new].iter().map(node_of).collect::<BTreeSet<_>>()
    );
    assert_eq!(
        handoff.moved,
        BTreeMap::from([(node_of(&old), node_of(&new))])
    );
    assert_eq!(
        handoff.rank,
        BTreeMap::from([(node_of(&old), 0)]),
        "where the rotated node stands among its seat's retired nodes"
    );
    assert_eq!(
        handoff.gone,
        [removed, old].iter().map(node_of).collect::<BTreeSet<_>>(),
        "the removed and the rotated node, and no member"
    );
    let none = handoff_of(node_of(&me), &crate::Standings::default());
    assert!(
        none.gone.is_empty(),
        "a log that took no one out names no one gone"
    );
    assert!(
        none.members.contains(&node_of(&me)),
        "a Mac the log has not seated yet counts in"
    );
}

/// **A REMOVED MAC NAMES ITSELF GONE**: once the log takes this Mac out, its own hand-off
/// counts it no member and names it gone, so its bridge lifts the halts it said, as every
/// other Mac's does (§4.11, "No halt outlives its origin").
/// NEGATIVE CONTROL: the same Mac still seated is a member and not gone.
#[test]
fn a_removed_mac_names_itself_gone() {
    let (me, b) = (key(1), key(2));
    let removed = crate::Standings {
        current: [b].into(),
        removed: [me].into(),
        ..crate::Standings::default()
    };
    let handoff = handoff_of(node_of(&me), &removed);
    assert!(!handoff.members.contains(&node_of(&me)));
    assert_eq!(handoff.gone, [node_of(&me)].into());
    let seated = crate::Standings {
        current: [me, b].into(),
        ..crate::Standings::default()
    };
    let control = handoff_of(node_of(&me), &seated);
    assert!(control.members.contains(&node_of(&me)), "control");
    assert!(control.gone.is_empty(), "control");
}
