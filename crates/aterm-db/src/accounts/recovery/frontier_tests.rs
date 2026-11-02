// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::table::{
    Scope,
    tests::{
        Lab,
        frontier::{receive, wide_lab, wires},
    },
};
use astream_wire::sync::MAX_PARENTS;
use std::collections::BTreeSet;

const NOW: u64 = 20_000 * 86_400 + 100;

fn independent_requests(lab: Lab) -> (Lab, Scope) {
    let writers = lab.paths.len();
    for paths in &lab.paths {
        Recovery::at(paths.clone())
            .request_at([1; 32], [2; 32], [3; 16], NOW)
            .unwrap();
    }
    let scope = RECOVERIES
        .scope(lab.bootstrap(0).trust(), RECOVERIES.epoch_of(NOW))
        .unwrap();
    let incoming = (1..writers)
        .flat_map(|writer| wires(&lab, writer, scope))
        .collect::<Vec<_>>();
    receive(&lab, 0, scope, &incoming);
    (lab, scope)
}

#[test]
fn recovery_requests_continue_after_many_independent_writers() {
    let writers = 2 * MAX_PARENTS + 1;
    let (lab, scope) = independent_requests(wide_lab(writers));
    let db = Recovery::at(lab.paths[0].clone());
    assert_eq!(db.pending_at(NOW).unwrap().len(), writers);
    let bootstrap = lab.bootstrap(0);
    let session = Session::observe(&bootstrap).unwrap();
    let directory = Directory::new(&lab.paths[0], bootstrap.trust());
    let store = Store::observe(&session, &directory.path(scope).unwrap(), scope).unwrap();
    let (_, bound) = store.metadata_with_append_bound(&session).unwrap();
    assert!(bound > 2, "this frontier needs multiple causal joins");
    drop((store, session));

    db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
    let requests = db.pending_at(NOW).unwrap();
    assert_eq!(requests.len(), writers + 1);
    let retained = wires(&lab, 0, scope);
    assert!(
        retained.len() > writers + 2,
        "multiple causal merges are retained"
    );
    assert!(
        retained.len() - writers <= bound,
        "all writes fit the reserved capacity"
    );
    db.request_at([4; 32], [5; 32], [6; 16], NOW).unwrap();
    assert_eq!(wires(&lab, 0, scope), retained);

    receive(&lab, 1, scope, &retained);
    let peer = Recovery::at(lab.paths[1].clone()).pending_at(NOW).unwrap();
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
    assert_eq!(identities(&peer), identities(&requests));
    assert_eq!(wires(&lab, 1, scope), retained);
}

#[test]
fn recovery_frontier_admission_reserves_both_total_and_writer_slots() {
    let (lab, scope) = independent_requests(wide_lab(MAX_PARENTS + 1));
    let original = wires(&lab, 0, scope);
    let next_scope = RECOVERIES
        .scope(lab.bootstrap(0).trust(), RECOVERIES.epoch_of(NOW) + 1)
        .unwrap();
    let db = Recovery::at(lab.paths[0].clone());

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
    drop((store, session));
    assert_eq!(db.pending_at(NOW).unwrap().len(), 68);
    db.request_with_caps_at([10; 32], [11; 32], [12; 16], NOW, 0, 0)
        .unwrap();
    assert_eq!(wires(&lab, 0, scope), joined, "retry needs no new capacity");
    assert_eq!(wires(&lab, 0, next_scope), next);
}
