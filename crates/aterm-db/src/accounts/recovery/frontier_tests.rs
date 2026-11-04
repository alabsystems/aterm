// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{
    Scope,
    tests::{
        Lab,
        frontier::{first_records, receive, wide_lab, wires},
    },
};
use astream_wire::sync::MAX_PARENTS;
use aterm_secrets::commons::roster::Role;
use std::collections::BTreeSet;

const NOW: u64 = 20_000 * 86_400 + 100;

/// Every writer's request in today's table, all received by Mac 0. Macs 0, 1 and the last, past
/// `LEGACY_MAX_MEMBERS`, request through the product; the others seal the record a request writes.
fn independent_requests(lab: Lab) -> (Lab, Scope) {
    let last = lab.paths.len() - 1;
    for paths in [&lab.paths[0], &lab.paths[1], &lab.paths[last]] {
        Recovery::at(paths.clone())
            .request_at([1; 32], [2; 32], [3; 16], NOW)
            .unwrap();
    }
    let period = RECOVERIES.epoch_of(NOW);
    let scope = RECOVERIES.scope(lab.bootstrap(0).trust(), period).unwrap();
    let mut incoming = wires(&lab, 1, scope);
    incoming.extend(wires(&lab, last, scope));
    incoming.extend(first_records(&lab, 2..last, scope, |_, session| {
        encode(&command(session.actor(), [1; 32], [2; 32], [3; 16], period)).unwrap()
    }));
    receive(&lab, 0, scope, &incoming);
    (lab, scope)
}

/// A request reads through a chain of joins, with no wide frontier needed to make one: past two
/// parent windows a write leaves two joins (the store's
/// `wide_append_rejections_preserve_capacity_for_a_complete_edit` proves that count and its bound
/// at 129 writers). NEGATIVE CONTROL: with `documents/log.rs` no longer decoding a merge, the
/// second request's read of its own table refuses the history as withheld.
#[test]
fn recovery_reads_through_a_chain_of_joins() {
    let lab = Lab::new(&[Role::Writer, Role::Writer]);
    let db = Recovery::at(lab.paths[0].clone());
    db.request_at([1; 32], [2; 32], [3; 16], NOW).unwrap();
    let scope = RECOVERIES
        .scope(lab.bootstrap(0).trust(), RECOVERIES.epoch_of(NOW))
        .unwrap();
    {
        let bootstrap = lab.bootstrap(0);
        let session = Session::open(&bootstrap).unwrap();
        let directory = Directory::new(&lab.paths[0], bootstrap.trust());
        let mut store = Store::open(&session, &directory.path(scope).unwrap(), scope).unwrap();
        for request in [83, 84] {
            let merge = crate::database::Command::Merge {
                request: [request; 16],
            };
            crate::documents::application::append(&session, &mut store, &merge.encode().unwrap())
                .unwrap();
        }
    }
    db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
    assert_eq!(db.pending_at(NOW).unwrap().len(), 2);
    assert_eq!(wires(&lab, 0, scope).len(), 4);
}

/// Requests stay readable on a frontier wider than the parent window, a request lands only when
/// its join fits both the table's and the writer's slots, and a replica converges on the same
/// wires and requests. What needs a second join is held elsewhere: its count, refusals and bound
/// by the store's `wide_append_rejections_preserve_capacity_for_a_complete_edit` at 129 writers,
/// reads through it by `recovery_reads_through_a_chain_of_joins`; no application request runs on a
/// frontier of 127 heads or more. NEGATIVE CONTROL: with `needed = 1`, the first capped request
/// lands in today's table and the "one total slot" assertion fails.
#[test]
fn recovery_frontier_admission_reserves_both_total_and_writer_slots() {
    let writers = MAX_PARENTS + 1;
    let (lab, scope) = independent_requests(wide_lab(writers));
    let original = wires(&lab, 0, scope);
    let next_scope = RECOVERIES
        .scope(lab.bootstrap(0).trust(), RECOVERIES.epoch_of(NOW) + 1)
        .unwrap();
    let db = Recovery::at(lab.paths[0].clone());
    assert_eq!(db.pending_at(NOW).unwrap().len(), writers);

    db.request_with_caps_at([4; 32], [5; 32], [6; 16], NOW, 66, 3)
        .unwrap();
    assert_eq!(
        wires(&lab, 0, scope),
        original,
        "one total slot cannot hold a merge and request"
    );
    let first_next = wires(&lab, 0, next_scope);
    assert_eq!(first_next.len(), 1);
    db.request_with_caps_at([4; 32], [5; 32], [6; 16], NOW, 66, 3)
        .unwrap();
    assert_eq!(wires(&lab, 0, next_scope), first_next);

    db.request_with_caps_at([7; 32], [8; 32], [9; 16], NOW, 67, 2)
        .unwrap();
    assert_eq!(
        wires(&lab, 0, scope),
        original,
        "one writer slot cannot hold a merge and request"
    );
    let next = wires(&lab, 0, next_scope);
    assert_eq!(next.len(), 2);

    db.request_with_caps_at([10; 32], [11; 32], [12; 16], NOW, 67, 3)
        .unwrap();
    let joined = wires(&lab, 0, scope);
    assert_eq!(joined.len(), 67);
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    assert_eq!(store.retained_by(&session, session.actor()).unwrap(), 3);
    let actor = session.actor();
    drop((store, session));
    assert_eq!(db.pending_at(NOW).unwrap().len(), 68);
    db.request_with_caps_at([10; 32], [11; 32], [12; 16], NOW, 0, 0)
        .unwrap();
    assert_eq!(wires(&lab, 0, scope), joined, "retry needs no new capacity");
    assert_eq!(wires(&lab, 0, next_scope), next);

    // A replica holding today's table alone reads what Mac 0 reads of it, by identity and not only
    // by count, its joined request included.
    receive(&lab, 1, scope, &joined);
    assert_eq!(wires(&lab, 1, scope), joined);
    let identities = |requests: &[Request]| {
        requests
            .iter()
            .map(|request| {
                (
                    request.requester.actor(),
                    request.account,
                    request.decision,
                    request.attempt,
                )
            })
            .collect::<BTreeSet<_>>()
    };
    let peer = identities(&Recovery::at(lab.paths[1].clone()).pending_at(NOW).unwrap());
    assert_eq!(peer.len(), writers + 1);
    assert!(peer.is_subset(&identities(&db.pending_at(NOW).unwrap())));
    assert!(peer.contains(&(actor, [10; 32], [11; 32], [12; 16])));
}
